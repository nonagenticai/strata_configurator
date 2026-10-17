# strata-configurator

Multi-tenant configuration model, overlay compiler and read-only model explorer.

- `configurator-core`: pure model validation, compilation, canonical hashing, care-bus projection and seed format.
- `configurator-api`: axum/sqlx, PostgreSQL 18, strict Keycloak JWT authentication, tenant roles and transactional audit. Port **8097**.
- `web/`: SvelteKit explorer with PKCE login, tenant selection, layer revisions and compile provenance. Port **3000**.

The application is served at `https://configurator.internal`. `/api/*`, `/health` and `/ready` route to the API; all other paths route to the UI. `/health` and `/ready` are unauthenticated. Tenant requests require a bare non-nil UUID in `X-Tenant-Id`; the super-admin may omit it to inspect platform standards.

Required API environment variables: `DATABASE_URL`, `KEYCLOAK_ISSUER`, `KEYCLOAK_JWKS_URL`. Optional: `PORT` (8097), `KEYCLOAK_AUDIENCE` (`strata-configurator`), `SUPER_ADMIN_ROLE` (`config-super-admin`). Migrations and embedded seed versions apply at startup; existing versions are never re-applied over curated data.

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
