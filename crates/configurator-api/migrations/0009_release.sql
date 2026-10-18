-- W4 (configurator plan 03, Tasks 6-7): versioned base releases per (tenant, job) and their pushes to
-- care-bus. Forward-only (pre-prod).
--
-- Table names avoid the SQL keywords TARGET and RELEASE: release_target, config_release, push_step.
-- All three are tenant tables under RLS (the anti-drift test). config_release and push_step are
-- append-only for the runtime role: SELECT and INSERT only. Release numbering is serialised per target
-- with a transaction-scoped advisory lock, so release_target needs no UPDATE grant either.

-- When the tenant was last provisioned into care-bus (tenant and job PUT both succeeded). `tenant` is
-- outside RLS (0003) and the runtime role already holds UPDATE on it.
ALTER TABLE tenant ADD COLUMN carebus_provisioned_at timestamptz;

CREATE TABLE release_target (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant      uuid        NOT NULL REFERENCES tenant (id),
    job         text        NOT NULL CHECK (job ~ '^[a-z][a-z0-9_]{0,62}$'),
    selection   jsonb       NOT NULL CHECK (jsonb_typeof(selection) = 'object'),
    created_by  text        NOT NULL CHECK (created_by <> ''),
    created_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT release_target_job_unique UNIQUE (tenant, job)
);

CREATE TABLE config_release (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant          uuid        NOT NULL REFERENCES tenant (id),
    target          uuid        NOT NULL REFERENCES release_target (id),
    release_id      text        NOT NULL CHECK (release_id ~ '^cfg-[0-9]{4}-[0-9]{2}-[0-9]{2}\.[0-9]+$'),
    hash            text        NOT NULL CHECK (hash ~ '^sha256:[0-9a-f]{64}$'),
    effective_hash  text        NOT NULL,
    -- The compiled job label, sent as care-bus's job label when the release is pushed.
    job_label       text        NOT NULL CHECK (job_label <> ''),
    -- The whole PUT body {schema, release_id, job, hash, bundle}.
    body            jsonb       NOT NULL CHECK (jsonb_typeof(body) = 'object'),
    created_by      text        NOT NULL CHECK (created_by <> ''),
    created_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT config_release_id_unique UNIQUE (target, release_id)
);

CREATE INDEX config_release_target_idx ON config_release (target, created_at DESC, id DESC);

CREATE TABLE push_step (
    id            bigserial   PRIMARY KEY,
    tenant        uuid        NOT NULL REFERENCES tenant (id),
    release       uuid        NOT NULL REFERENCES config_release (id),
    step          text        NOT NULL CHECK (step IN ('tenant', 'job', 'release')),
    -- The release hash for the release step; NULL for the provisioning steps.
    request_hash  text,
    -- care-bus's HTTP status; NULL when care-bus was not reached.
    status        integer,
    ok            boolean     NOT NULL,
    -- care-bus's answer (the release report with its conflicts); never a token.
    response      jsonb,
    error_code    text,
    pushed_by     text        NOT NULL CHECK (pushed_by <> ''),
    pushed_at     timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX push_step_release_idx ON push_step (release, id);

GRANT SELECT, INSERT ON release_target, config_release, push_step TO configurator_app;
GRANT USAGE, SELECT ON SEQUENCE push_step_id_seq TO configurator_app;

ALTER TABLE release_target ENABLE ROW LEVEL SECURITY;
ALTER TABLE config_release ENABLE ROW LEVEL SECURITY;
ALTER TABLE push_step ENABLE ROW LEVEL SECURITY;

CREATE POLICY release_target_read ON release_target FOR SELECT
    USING (tenant = app_current_tenant());
CREATE POLICY release_target_insert ON release_target FOR INSERT
    WITH CHECK (tenant = app_current_tenant());

CREATE POLICY config_release_read ON config_release FOR SELECT
    USING (tenant = app_current_tenant());
CREATE POLICY config_release_insert ON config_release FOR INSERT
    WITH CHECK (tenant = app_current_tenant());

CREATE POLICY push_step_read ON push_step FOR SELECT
    USING (tenant = app_current_tenant());
CREATE POLICY push_step_insert ON push_step FOR INSERT
    WITH CHECK (tenant = app_current_tenant());
