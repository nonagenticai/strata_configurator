-- W2.3 (plan 2026-10-17 §2 "Early access", §4 W2.3): the early-access signup queue, after
-- commonpractice `signup_approvals` (0027 + 0105 company/source + 0106 trust + 0119 message).
-- Forward-only (pre-prod).
--
-- A website or chat backend submits a signup through `POST /api/signup-approvals/intake` with its
-- client-credentials token; the row waits here `pending` until a super-admin approves it (which
-- creates the tenant through the same path as `POST /api/tenants`, and records it in `tenant`) or
-- rejects it. The requester has no identity yet: the row stores no `sub`, and approval invites the
-- address, so accepting the invite proves the person owns it.
--
-- Platform data, owner-only like `tenant_license` (0004): the runtime role gets no grant, because
-- every route that touches the queue (intake and the super-admin queue) runs on the owner pool.

CREATE TABLE signup_approvals (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    email       text        NOT NULL CHECK (email <> ''),
    -- Requester-typed, normalised to one printable line by the route (commonpractice
    -- `untrusted-text.ts`), at most 160 characters.
    name        text        NOT NULL CHECK (char_length(name) BETWEEN 1 AND 160),
    company     text        CHECK (company IS NULL OR char_length(company) BETWEEN 1 AND 160),
    -- Free text with line breaks kept; never logged and never sent to Telegram.
    message     text        CHECK (message IS NULL OR char_length(message) <= 2000),
    source      text        NOT NULL CHECK (source IN ('web', 'chat')),
    -- The trusted path that admitted the row. Only the intake writes here, so always 'intake'; kept
    -- so the column means what commonpractice's does if another path is ever added.
    trust       text        NOT NULL DEFAULT 'intake' CHECK (trust IN ('intake')),
    -- The intake client (`azp`) that submitted it.
    intake_client text      NOT NULL CHECK (intake_client <> ''),
    status      text        NOT NULL DEFAULT 'pending'
                            CHECK (status IN ('pending', 'approved', 'rejected')),
    -- The tenant an approval created; NULL until then (and for rejected rows).
    tenant      uuid        REFERENCES tenant (id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    decided_at  timestamptz,
    -- The super-admin's token `sub`.
    decided_by  text,
    CHECK ((status = 'pending') = (decided_at IS NULL)),
    CHECK (status = 'approved' OR tenant IS NULL)
);

-- One PENDING row per address (case-insensitive), so a re-submit is a no-op while it waits; a decided
-- row does not block a later signup (commonpractice `signup_approvals_pending_email`).
CREATE UNIQUE INDEX signup_approvals_pending_email
    ON signup_approvals (lower(email)) WHERE status = 'pending';
-- The queue listing: newest first, optionally by status.
CREATE INDEX signup_approvals_status_created_idx
    ON signup_approvals (status, created_at DESC, id DESC);

-- It has a tenant column, so it is under RLS like every tenant table (the anti-drift test); with no
-- grant to the runtime role the policy is defence in depth, as for `tenant_license`.
ALTER TABLE signup_approvals ENABLE ROW LEVEL SECURITY;
CREATE POLICY signup_approvals_read ON signup_approvals FOR SELECT
    USING (tenant = app_current_tenant());
