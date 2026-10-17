-- Tenant offboarding (POST /api/tenants/{id}/offboard), after commonpractice `services/offboarding.ts`
-- (`offboardTenantWrites`: `offboarded_at = COALESCE(offboarded_at, now())`). Forward-only (pre-prod).
--
-- A tenant is soft-archived, never deleted: deleting it would cascade `users` and `tenant_license` and
-- break the append-only audit trail's subject. `archived_at` is written once (the route sets it with
-- COALESCE, so a repeat offboard keeps the original instant); NULL = live. An archived tenant is
-- hidden from tenant lists by default, and every tenant-scoped request for it is refused (403
-- `tenant_suspended`, commonpractice's code for a workspace it has offboarded).
--
-- `tenant` stays outside RLS (identity and tenancy are resolved before a tenant is known, 0003), so
-- this column needs no policy; the runtime role already holds SELECT/UPDATE on the table.

ALTER TABLE tenant ADD COLUMN archived_at timestamptz;

-- The default tenant listing is the live tenants, newest first.
CREATE INDEX tenant_live_created_idx ON tenant (created_at DESC, id DESC) WHERE archived_at IS NULL;
