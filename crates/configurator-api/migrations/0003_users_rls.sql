-- W1 (plan 2026-10-17 §2/§4): commonpractice identity and Postgres RLS. Forward-only (pre-prod).
--
-- Identity: Keycloak only authenticates. Tenancy and roles live here, in `users`: one tenant per
-- identity, replacing the many-to-many `member`.
--
-- Isolation: the runtime connects as the non-bypass role `configurator_app` and sets the
-- transaction-local GUC `app.current_tenant`; the policies below then hide every other tenant's rows.
-- RLS is ENABLEd, not FORCEd: the owner (migrations, seeder, super-admin platform work) bypasses it.

CREATE TYPE tenant_role AS ENUM ('tenant_admin', 'tenant_user');

CREATE TABLE users (
    sub          text        PRIMARY KEY CHECK (sub <> ''),
    tenant       uuid        NOT NULL REFERENCES tenant (id) ON DELETE CASCADE,
    tenant_role  tenant_role NOT NULL,
    disabled_at  timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX users_tenant_idx ON users (tenant);

-- One row per subject: the highest member role wins (admin → tenant_admin, anything else →
-- tenant_user); between equal roles the earliest membership wins.
INSERT INTO users (sub, tenant, tenant_role, created_at)
SELECT DISTINCT ON (sub)
       sub,
       tenant,
       CASE WHEN role = 'admin' THEN 'tenant_admin'::tenant_role ELSE 'tenant_user'::tenant_role END,
       created_at
FROM member
ORDER BY sub, role DESC, created_at, tenant;

-- `member_sub_idx` (0002) goes with the table.
DROP TABLE member;
DROP TYPE member_role;

-- `tenant.scope_switch` events carry the request (method, path) that switched.
ALTER TABLE audit_event
    ADD COLUMN metadata jsonb CHECK (metadata IS NULL OR jsonb_typeof(metadata) = 'object');

-- The runtime role. In production it is pre-provisioned (with LOGIN) by the DB provisioning script;
-- a migrating owner without CREATEROLE only gets a notice here, and the GRANTs below then fail loudly
-- if the role really is missing. `unique_violation` covers two migrations racing on one cluster.
DO $$
BEGIN
    CREATE ROLE configurator_app NOLOGIN NOSUPERUSER NOCREATEROLE NOCREATEDB NOBYPASSRLS;
EXCEPTION
    WHEN duplicate_object OR unique_violation THEN NULL;
    WHEN insufficient_privilege THEN RAISE NOTICE 'configurator_app must be pre-provisioned';
END
$$;

GRANT USAGE ON SCHEMA public TO configurator_app;
GRANT SELECT, INSERT, UPDATE, DELETE
    ON tenant, users, layer, overlay_row, dimension_value, profile, audit_event
    TO configurator_app;
-- Platform tables: the runtime reads them; only the owner writes them.
GRANT SELECT ON axis, axis_value, axis_rule, dimension, seed_version TO configurator_app;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO configurator_app;

-- The tenant a transaction acts in; NULL when unset (or reset to '' after a local set_config).
CREATE FUNCTION app_current_tenant() RETURNS uuid
    LANGUAGE sql STABLE
    AS $$ SELECT nullif(current_setting('app.current_tenant', true), '')::uuid $$;

GRANT EXECUTE ON FUNCTION app_current_tenant() TO configurator_app;

ALTER TABLE layer ENABLE ROW LEVEL SECURITY;
ALTER TABLE overlay_row ENABLE ROW LEVEL SECURITY;
ALTER TABLE dimension_value ENABLE ROW LEVEL SECURITY;
ALTER TABLE profile ENABLE ROW LEVEL SECURITY;

-- Platform rows (tenant NULL) are readable by everyone and writable only by the owner.
CREATE POLICY layer_read ON layer FOR SELECT
    USING (tenant IS NULL OR tenant = app_current_tenant());
CREATE POLICY layer_insert ON layer FOR INSERT
    WITH CHECK (tenant = app_current_tenant());
CREATE POLICY layer_update ON layer FOR UPDATE
    USING (tenant = app_current_tenant())
    WITH CHECK (tenant = app_current_tenant());
CREATE POLICY layer_delete ON layer FOR DELETE
    USING (tenant = app_current_tenant());

CREATE POLICY dimension_value_read ON dimension_value FOR SELECT
    USING (tenant IS NULL OR tenant = app_current_tenant());
CREATE POLICY dimension_value_insert ON dimension_value FOR INSERT
    WITH CHECK (tenant = app_current_tenant());
CREATE POLICY dimension_value_update ON dimension_value FOR UPDATE
    USING (tenant = app_current_tenant())
    WITH CHECK (tenant = app_current_tenant());
CREATE POLICY dimension_value_delete ON dimension_value FOR DELETE
    USING (tenant = app_current_tenant());

CREATE POLICY profile_read ON profile FOR SELECT
    USING (tenant IS NULL OR tenant = app_current_tenant());
CREATE POLICY profile_insert ON profile FOR INSERT
    WITH CHECK (tenant = app_current_tenant());
CREATE POLICY profile_update ON profile FOR UPDATE
    USING (tenant = app_current_tenant())
    WITH CHECK (tenant = app_current_tenant());
CREATE POLICY profile_delete ON profile FOR DELETE
    USING (tenant = app_current_tenant());

-- `overlay_row` has no tenant column: it is visible when its layer is (the subquery runs under
-- `layer`'s own policies), and writable when its layer is the current tenant's (commonpractice 0057).
CREATE POLICY overlay_row_read ON overlay_row FOR SELECT
    USING (EXISTS (SELECT 1 FROM layer l WHERE l.id = overlay_row.layer_id));
CREATE POLICY overlay_row_write ON overlay_row FOR ALL
    USING (EXISTS (SELECT 1 FROM layer l
                   WHERE l.id = overlay_row.layer_id AND l.tenant = app_current_tenant()))
    WITH CHECK (EXISTS (SELECT 1 FROM layer l
                        WHERE l.id = overlay_row.layer_id AND l.tenant = app_current_tenant()));
