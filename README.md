# strata-configurator

> **RETIRED 2026-10-24.** The configurator was merged into [`nonagenticai/agent`](https://github.com/nonagenticai/agent).
> This repository is published for reference only: it is frozen and not maintained.
> Do not open PRs here.

Multi-tenant configuration model, overlay compiler and read-only model explorer.

- `configurator-core`: pure model validation, compilation, canonical hashing, care-bus projection and seed format.
- `configurator-api`: axum/sqlx, PostgreSQL 18, strict Keycloak JWT authentication, tenant roles and transactional audit. Port **8097**.
- `web/`: SvelteKit explorer with PKCE login, tenant selection, layer revisions and compile provenance. Port **3000**.

The original deployment served both parts from one host: `/api/*`, `/health` and `/ready` route to the API; all other paths route to the UI. `/health` and `/ready` are unauthenticated. Tenant requests require a bare non-nil UUID in `X-Tenant-Id`; the super-admin may omit it to inspect platform standards.

Required API environment variables: `DATABASE_URL`, `KEYCLOAK_ISSUER`, `KEYCLOAK_JWKS_URL`. Optional: `PORT` (8097), `KEYCLOAK_AUDIENCE` (`strata-configurator`), `SUPER_ADMIN_ROLE` (`super_admin`, a client role on the `KEYCLOAK_AUDIENCE` client, read from `resource_access.<audience>.roles`; realm roles are ignored). Migrations and embedded seed versions apply at startup; existing versions are never re-applied over curated data.

Optional, all dark by default (unset = off):
- Invites by email (W1.1), on only when all four are set: `KEYCLOAK_ADMIN_TOKEN_URL`, `KEYCLOAK_ADMIN_CLIENT_ID` (`strata-configurator-admin`), `KEYCLOAK_ADMIN_CLIENT_SECRET`, `KEYCLOAK_ADMIN_API_URL` (`…/admin/realms/portal`). Without them, an `email` in `POST /api/tenants` or `POST /api/tenants/{id}/users` is 503 `invite_unavailable`.
- Licence gate 2 (W2.1): `LICENSE_GATE_2_ENABLED` (`false`), `LICENSE_INTROSPECT_URL` (unset; required when the gate is on — without it every gated request answers 402 `license_unavailable`), `LICENSE_APP_KEY` (`strata-configurator`), `LICENSE_CACHE_TTL_SEC` (`300`).
- Licence issuance on tenant create (W2.2), on only when all four are set: `LICENSES_ADMIN_TOKEN_URL`, `LICENSES_ADMIN_CLIENT_ID` (`strata-configurator-licenses-admin`), `LICENSES_ADMIN_CLIENT_SECRET`, `LICENSES_ADMIN_URL`; plus `LICENSE_PLAN_CODE` (`standard`). The tenant's token is minted as a registered perpetual `internal` token (`token_type=internal&long_lived=true`, owner `strata-configurator`): licences refuses a long-lived `api` token outside its `LONG_LIVED_API_TENANTS`, gives an unregistered one a 365-day expiry (which would 402 every tenant a year after onboarding), and the token is only used server-side for gate 2's introspect, which ignores the token type. The SA needs the `license-admin` realm role for this. The same SA revokes a tenant's licence on offboarding (`offboarding::revoke_tenant_license`, fail-loud 502 `license_provisioning_failed`).
- `LICENSE_TOKEN_ENC_KEY`: base64 of 32 random bytes (`openssl rand -base64 32`), the AES-256-GCM key the tenant licence token is stored under (`v1:` + base64(nonce ‖ ciphertext ‖ tag)). **Required whenever issuance or gate 2 is on** (startup error otherwise); a set but invalid value is always a startup error. A token that will not decrypt (e.g. a rotated key) makes gate 2 answer 402 `license_unreadable` for that tenant. Gate 2 applies to the super-admin acting in a tenant (`X-Active-Tenant-Id`, else their own) as to its members; the platform view is not gated.
- Early-access self-signup (W2.3, after commonpractice `docs/runbooks/early-access-signup.md`): `EARLY_ACCESS_SIGNUP_ENABLED` (`false`; off, every `/api/signup-approvals` route is the unknown-route 404). Intake guard: `SIGNUP_INTAKE_AZP` (comma list, `configurator-signup-intake`) and `SIGNUP_INTAKE_ROLE` (`configurator-signup-intake`, a client role on the `KEYCLOAK_AUDIENCE` client; the intake token must also carry that audience). `SIGNUP_QUEUE_MAX_PENDING` (`500`). Turnstile, on when `TURNSTILE_SECRET` is set: `TURNSTILE_VERIFY_URL` (Cloudflare siteverify), `TURNSTILE_TIMEOUT_MS` (`5000`). Telegram notices, on only when all of `TELEGRAM_BOT_TOKEN`, `TELEGRAM_SIGNUP_CHAT_ID` and `TELEGRAM_SIGNUP_THREAD_ID` are set: `TELEGRAM_API_BASE` (`https://api.telegram.org`), `CONFIGURATOR_PUBLIC_URL` (the UI origin for the Approve/Reject buttons; no buttons when unset). Approving needs the Keycloak admin SA (the requester is invited by email) and issues the licence when issuance is on.
A service account with only some of its four settings is a startup error.

Web runtime configuration: `PUBLIC_KEYCLOAK_URL`, `PUBLIC_KEYCLOAK_REALM`, `PUBLIC_KEYCLOAK_CLIENT_ID` (defaults: `https://auth.nonagentic.ai`, `portal`, `strata-configurator`). API calls use the same origin and fresh bearer tokens.

Rust builds and tests:

```sh
cargo build --locked
cargo test --locked
```

The integration tests use a throwaway PostgreSQL 18 container (Docker), or the server at
`TEST_DATABASE_URL` when it is set. Local web checks:

```sh
cd web
npm ci
npm run ci:verify
npm test
npm run build
npm run format:check
```

Tenant offboarding (super-admin, after commonpractice): `POST /api/tenants/{id}/offboard` `{"confirm_name": "<tenant name>"}` revokes the licence first (502 `license_provisioning_failed` leaves the tenant live; retry), then soft-archives the tenant (`archived_at`, migration 0007) and disables its users in one audited transaction (`tenant.offboard`); 200 with the tenant, idempotent (a repeat is 200 `already_archived: true` without calling licences). Guards: 403 `forbidden`, 400 `invalid_tenant`/`invalid_body`/`confirm_mismatch`, 404 `tenant_not_found`. Archived tenants are left out of `GET /api/tenants` (super-admin: `?include_archived=true`) and every tenant-scoped request for one, the super-admin's included, is 403 `tenant_suspended`. Keycloak identities are never touched (shared portal realm).

Phase 1 authors configuration through APIs and previews it in the UI. Publishing to care-bus/RAG/ingest and conversational generation belong to later phases.

## Licensing

The code runs without a licence server. There is one licence gate, switched by
`LICENSE_GATE_2_ENABLED`, which defaults to `false` — unset means off, and when it is off no
licence endpoint is contacted. Turning it on requires `LICENSE_TOKEN_ENC_KEY` (startup fails
otherwise) and `LICENSE_INTROSPECT_URL` (there is no default); introspection is fail-closed, so a
failed lookup answers 402 `license_unavailable` rather than letting a request through.
`LICENSE_APP_KEY` (`strata-configurator`) and `LICENSE_CACHE_TTL_SEC` (`300`) tune it. Licence
issuance and the other optional integrations are likewise off unless their settings are set (see
above).

## License

Apache 2.0 — see [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).
