-- W2 (plan 2026-10-17 §4 W2.1/W2.2): the licence issued to each tenant, after commonpractice's
-- `tenants.license_token_encrypted` (0031). Licence gate 2 introspects `token` on tenant requests.
--
-- Owner-only: the runtime role gets no grant, so no request (not even one with a SQL bug) can read a
-- licence token; the gate and the tenant create read and write it through the owner connection.
-- commonpractice encrypts the token at rest; here the owner-only grant is the protection (follow-up).

CREATE TABLE tenant_license (
    tenant      uuid        PRIMARY KEY REFERENCES tenant (id) ON DELETE CASCADE,
    license_id  text        NOT NULL CHECK (license_id <> ''),
    subject_id  text        NOT NULL CHECK (subject_id <> ''),
    holder_sub  text        NOT NULL CHECK (holder_sub <> ''),
    token       text        NOT NULL CHECK (token <> ''),
    issued_at   timestamptz NOT NULL DEFAULT now()
);

-- It has a tenant column, so it is under RLS like every tenant table (the anti-drift test); with no
-- grant to the runtime role the policy is defence in depth.
ALTER TABLE tenant_license ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_license_read ON tenant_license FOR SELECT
    USING (tenant = app_current_tenant());
