# strata-configurator

Multi-tenant configuration model, overlay compiler and read-only model explorer.

- `configurator-core`: pure model validation, compilation, canonical hashing, care-bus projection and seed format.
- `configurator-api`: axum/sqlx, PostgreSQL 18, strict Keycloak JWT authentication, tenant roles and transactional audit. Port **8097**.
- `web/`: SvelteKit explorer with PKCE login, tenant selection, layer revisions and compile provenance. Port **3000**.

The application is served at `https://configurator.internal`. `/api/*`, `/health` and `/ready` route to the API; all other paths route to the UI. `/health` and `/ready` are unauthenticated. Tenant requests require a bare non-nil UUID in `X-Tenant-Id`; the super-admin may omit it to inspect platform standards.

Required API environment variables: `DATABASE_URL`, `KEYCLOAK_ISSUER`, `KEYCLOAK_JWKS_URL`. Optional: `PORT` (8097), `KEYCLOAK_AUDIENCE` (`strata-configurator`), `SUPER_ADMIN_ROLE` (`config-super-admin`). Migrations and embedded seed versions apply at startup; existing versions are never re-applied over curated data.

Optional, all dark by default (unset = off):
- Invites by email (W1.1), on only when all four are set: `KEYCLOAK_ADMIN_TOKEN_URL`, `KEYCLOAK_ADMIN_CLIENT_ID` (`strata-configurator-admin`), `KEYCLOAK_ADMIN_CLIENT_SECRET`, `KEYCLOAK_ADMIN_API_URL` (`…/admin/realms/portal`). Without them, an `email` in `POST /api/tenants` or `POST /api/tenants/{id}/users` is 503 `invite_unavailable`.
- Licence gate 2 (W2.1): `LICENSE_GATE_2_ENABLED` (`false`), `LICENSE_INTROSPECT_URL` (`http://licenses.licenses.svc.cluster.local:8544/license/introspect`), `LICENSE_APP_KEY` (`strata-configurator`), `LICENSE_CACHE_TTL_SEC` (`300`).
- Licence issuance on tenant create (W2.2), on only when all four are set: `LICENSES_ADMIN_TOKEN_URL`, `LICENSES_ADMIN_CLIENT_ID` (`strata-configurator-licenses-admin`), `LICENSES_ADMIN_CLIENT_SECRET`, `LICENSES_ADMIN_URL`; plus `LICENSE_PLAN_CODE` (`standard`).
- Early-access self-signup (W2.3, after commonpractice `docs/runbooks/early-access-signup.md`): `EARLY_ACCESS_SIGNUP_ENABLED` (`false`; off, every `/api/signup-approvals` route is the unknown-route 404). Intake guard: `SIGNUP_INTAKE_AZP` (comma list, `configurator-signup-intake`) and `SIGNUP_INTAKE_ROLE` (`configurator-signup-intake`, a client role on the `KEYCLOAK_AUDIENCE` client; the intake token must also carry that audience). `SIGNUP_QUEUE_MAX_PENDING` (`500`). Turnstile, on when `TURNSTILE_SECRET` is set: `TURNSTILE_VERIFY_URL` (Cloudflare siteverify), `TURNSTILE_TIMEOUT_MS` (`5000`). Telegram notices, on only when all of `TELEGRAM_BOT_TOKEN`, `TELEGRAM_SIGNUP_CHAT_ID` and `TELEGRAM_SIGNUP_THREAD_ID` are set: `TELEGRAM_API_BASE` (`https://api.telegram.org`), `CONFIGURATOR_PUBLIC_URL` (the UI origin for the Approve/Reject buttons; no buttons when unset). Approving needs the Keycloak admin SA (the requester is invited by email) and issues the licence when issuance is on.
A service account with only some of its four settings is a startup error.

Web runtime configuration: `PUBLIC_KEYCLOAK_URL`, `PUBLIC_KEYCLOAK_REALM`, `PUBLIC_KEYCLOAK_CLIENT_ID` (defaults: `https://auth.nonagentic.ai`, `portal`, `strata-configurator`). API calls use the same origin and fresh bearer tokens.

Rust builds/tests run in CI only. Local web checks:

```sh
cd web
npm ci
npm run ci:verify
npm test
npm run build
npm run format:check
```

Spec: `docs/superpowers/specs/2026-10-16-configurator-design.md`.
Plan: `docs/superpowers/plans/2026-10-16-configurator-01-model-compiler.md`.
Deployment manifests and the selective Keycloak setup job live in `gitops-production/clusters/strato/services/strata-configurator`, `services-istio/strata-configurator` and `services/keycloak/realm-patch-job-strata-configurator.yaml`.

Phase 1 authors configuration through APIs and previews it in the UI. Publishing to care-bus/RAG/ingest and conversational generation belong to later phases.
