# strata-configurator — next session: waypoint fix, commonpractice tenancy model, care-bus multi-tenant/multi-job handoff

Status: **plan for the next session**, written 2026-10-17. It supersedes the parts of the
spec (`docs/superpowers/specs/2026-10-16-configurator-design.md`, rev 2) and of the overview
(`2026-10-16-configurator-00-overview.md`) that are listed in §3. The first task of the next
session is spec rev 3 (W1.0), which folds these decisions into the spec.

## 1. Verified state (2026-10-17)

| Fact | Evidence |
|---|---|
| Phase 1 (tasks 1–13) merged to `main` as PR #1, merge `9ed503e` | `git log origin/main` |
| CI green on `main` (run 39037486530) and on the gitops PR (run 39036232211, branch `feat/strata-configurator`, merged as #4750 `003355071d`) | `gh run view` |
| API and web pods Running on worker3. `https://configurator.internal/ready` returns 200 `{"status":"ready"}` | curl from master |
| **The waypoint is down.** Gateway `strata-configurator-waypoint` has `PROGRAMMED=False` and its deployment is 0/1. The pod is rejected with: `violates PodSecurity "restricted:latest": seccompProfile … istio-proxy` | `kubectl -n strata-configurator get gateway,deploy,rs` |
| Root cause: the namespace has `pod-security.kubernetes.io/enforce: restricted`. Every namespace that runs a waypoint (care-bus, strata-rag, enterprise-mcp) uses `enforce: baseline` + `warn: restricted`, and so does commonpractice. Istio 1.30.4 waypoint pods do not set `seccompProfile` | `gitops-production` origin/main `clusters/strato/services/*/namespace.yaml` |
| Consequence: `services-istio/strata-configurator/authz.yaml` has two ALLOW AuthorizationPolicies with `targetRefs: kind: Service`. Service-targeted policies are enforced only by a waypoint, so **today the API caller allow-list is not enforced**. Ingress still works, which is why `/ready` answers | manifest + live state |
| Not verified by me: the Task 13 live golden-hash compare and the UI provenance screenshot (codex ran Task 13) | — |

care-bus-agents on `origin/main` (checked 2026-10-17):

| Item | State |
|---|---|
| P0 multi-tenancy | **missing.** `config.rs:21-24` says it is "one tenant's cell by design". There is no tenant column. Calls to rag/ingest send a single `CAREBUS_TENANT_ID` env as `X-Tenant-Id` |
| P4 write-capable service account | **missing.** Static service tokens always get `care-viewer`, and there is no client-credentials path |
| P1 context-space write API | **missing.** Only GET exists (`routes/ontology.rs:73`) |
| P2 data-driven request types/areas/slots | **missing.** `domain.rs` hardcodes `AREAS`, `TOOLS`, `enum RequestType` and `static_plan` |
| P3 ontology relation writes | **done** (`routes/ontology.rs:77-82`) |
| P5 tool catalogue and side-effect flags from config | **missing** |
| P6 job settings (preamble, actor) from config | **missing** |
| Job concept | **none.** The word "job" is only used for ingest jobs |
| Connectors | exist (`routes/connectors.rs`: name, system, base_url, auth_type, secret_ref, field mappings), admin UI at `web/src/routes/admin/backends`. Not tenant- or job-keyed |
| Knowledge | stores and sources exist (`routes/knowledge.rs`, `routes/sources.rs`, admin UI `admin/knowledge`). Not tenant- or job-keyed |
| **Auth defect** | `auth.rs:115-118` sets issuer and audience but not `set_required_spec_claims`. In jsonwebtoken 9 this means a token **without** `aud` or `iss` is accepted. The configurator had the same defect and fixed it in `c3be65e`. care-bus still has it |

## 2. Owner decisions (2026-10-17)

1. **Copy commonpractice's setup** for strata_configurator and care-bus. This covers patterns only; no code is shared:
   - **Overlay + tombstones.** Base rows carry no tenant. Tenant rows override, extend or hide them, and the effective view is compiled at read time (commonpractice `ontology-service.ts` `compileEffective`, `hidden` tombstones).
   - **Postgres RLS.** A non-bypass app role, the `app.current_tenant` GUC, a policy on every `tenant_id` table, and a CI anti-drift test that every such table has RLS and a policy (commonpractice `migrations/0028_rls.sql`, `0029`, `0032`, `0057`).
   - **Identity, as commonpractice's CODE does it.** Its SPEC (Phase 4 decision 2) says "Keycloak `tenant_id` claim". That was superseded, as verified in code on origin/main.
     - `backend/src/services/keycloak-admin.ts:1-7` says "CP NEVER writes realm roles or the `tenant_id` user attribute — tenancy and roles live in CP's own `users` table".
     - `cp-identity.ts:1-7`: Keycloak **only authenticates**. CP drops the token's `tenant_id` claim and every role except `super_admin`. `users.tenant_id` and `users.tenant_role` in CP's own DB are the authority, **one tenant per identity**.
     - A super-admin switches tenant with `X-Active-Tenant-Id`; each switch is audited and one tenant is active at a time.
     - CP creates Keycloak identities (create user plus action email, disable) through a portal-realm client-credentials SA (`commonpractice-admin`, `keycloak-admin.ts:9,368`).
     - So the configurator moves from its `member(tenant, sub, role)` many-to-many to a CP-style `users(sub, tenant_id, tenant_role)` table, one tenant per user.
   - **Deployment shape.** Namespace `enforce: baseline` + `warn: restricted`, ambient mesh.
   - **Billing / metering.** Use a local usage ledger plus a `metering_outbox` in the same transaction. A drainer XADDs to the Redis stream `billing:costs`, and billing-service prices the events (commonpractice `host/metering-emit.ts`, `host/metering-outbox-drainer.ts`, migration `0090`).
   - **Licensing.** An in-app "gate 2": introspect the tenant's licence at `licenses.licenses.svc.cluster.local:8544/license/introspect`. It is fail-closed and cached for 300 s. Issue a licence on onboarding and revoke it on offboarding through a `secure-apps` client-credentials service account (commonpractice `middleware/license-gate.ts`, `services/license-client.ts`, `services/licenses-admin.ts`).
   - **Early access (confirmed, Q1).** This is commonpractice's *early-access self-signup*, copied to the point that it can be switched on after testing. A website or chat backend calls an intake route (client-credentials client and role). The row lands in a signup-approvals queue, with optional Turnstile and Telegram notification. A super-admin approves it, which creates the tenant, issues the licence and sets a default plan. Everything ships dark behind env vars (commonpractice `docs/runbooks/early-access-signup.md`, `routes/onboarding.ts:243`, `services/plan-posture.ts`).
2. **Handoff: push a versioned base.** The configurator pushes an approved release (snapshot N) to care-bus. care-bus stores that base separately from the tenant admin's refinements. Applying release N+1 keeps the refinements and reports conflicts. care-bus keeps running when the configurator is down.
3. **Jobs live in care-bus.** One care-bus becomes multi-tenant and multi-job. A tenant can reach all of its job scopes, and context spaces, procedures, connectors, knowledge stores and model routes are keyed by `(tenant, job)`.
4. **Identity:** adopt commonpractice's model as its code implements it (above), with per-app roles (§3).

Consistency notes against earlier rulings in memory:
- *apps-are-cells* (2026-10-08) said "no new central coupling". The owner has now explicitly chosen central licensing and billing as in commonpractice. To keep customer-hardware installs possible, both ship **dark, behind flags**, exactly as commonpractice does: `LICENSE_GATE_2_ENABLED` and `METERING_EMIT_ENABLED`.
- *care-bus direction* (2026-10-16) said commonpractice is a "reference only". That still holds: these are copied patterns, not shared code.
- Metering applies only to LLM and GPU calls. The configurator makes **no LLM calls until phase 3** (the generator), so its metering pipeline lands with phase 3. care-bus already makes LLM calls and needs metering now.

## 3. What this changes in the spec and overview

| Spec rev 2 | Becomes (rev 3) |
|---|---|
| §10.2: tenant roles viewer < editor < approver < admin in the configurator DB, `X-Tenant-Id` header, super-admin = Keycloak role `config-super-admin` | An app-owned `users(sub, tenant_id, tenant_role)` table, one tenant per identity, replacing `member`. Keycloak only authenticates, plus a per-app `super_admin` client role. `X-Active-Tenant-Id` is for super-admin switching only. Approval (spec §8) is done by `tenant_admin` |
| §10.4: isolation by query predicates | Postgres RLS on every tenant table, plus a CI anti-drift test |
| §2 / overview: "care-bus has no tenants"; P0 = tenant column + `X-Tenant-Id` | P0 = tenant **and job** scope with RLS, and care-bus gets its own `users` table (tenant ids provisioned by the configurator). Base and refinement are separate layers in care-bus |
| §11 P4: service account with care-admin write | A client-credentials SA with a dedicated **release-writer** role, valid only for the release endpoint, with `aud`/`iss` required |
| §12: phases 2b/4/6 | Reordered in §5 below |
| (absent) | Licensing gate 2, metering outbox, early-access signup |

**Roles (owner, 2026-10-17, answer to Q2).** Every app has its **own Keycloak client roles**. No role overlaps between commonpractice, w2v, conveyor, merqi, strata_configurator and care-bus-agents. The configurator and the agents (care-bus) each get their **own dedicated `tenant_admin` and `tenant_user` roles**. This prepares a future separation between the *configuration* organisation and the *run* organisation: a person can administer a tenant's configuration without administering its running agents, and the reverse.
- `tenant_admin` and `tenant_user` live in **each app's own `users` table**. That is how commonpractice does it, and it guarantees no overlap: a role in the configurator's DB means nothing to care-bus.
- The only Keycloak role per app is a **client role `super_admin`** on that app's own client (`resource_access.<client>.roles`, never `realm_access`): `strata-configurator` and `care-bus`. Never the shared realm `super_admin`, because commonpractice reads that one.
- Configurator: `tenant_user` = read only. `tenant_admin` = edit and approve releases (spec §8).
- care-bus: its own `tenant_admin` (refines base config, connectors, knowledge) and `tenant_user` (uses the agents). It also gets the client-credentials client role `release-writer` for the configurator's SA.

**Tenant id (owner, answer to Q3).** **The configurator defines the tenant id.** It creates the tenant uuid and shares it with the agents (care-bus) through provision-down: tenant creation and release pushes. care-bus never mints tenant ids.
- No Keycloak attribute needs writing (Q5 answered by commonpractice's code). The configurator mints the tenant uuid, writes its own `users` row and creates or invites the Keycloak identity through its own portal-realm admin SA, as commonpractice's `commonpractice-admin` does.
- care-bus receives the tenant **and** its first care-bus `tenant_admin` by provision-down (a tenant-create call to care-bus, authenticated by the configurator's SA). After that, care-bus admins manage care-bus users themselves (the run organisation).
- **strata_rag / strata_ingest are different.** They are shared by many apps, so they do **not** adopt the claim model. They follow the multi-app **cell pattern** that w2v is building and merqi should follow (W5).

## 4. Workstreams

Each workstream is one PR per repo (PR discipline). Rust is verified in CI only (no local cargo).

### W0 — waypoint fix (gitops-production, first, small)
1. `clusters/strato/services/strata-configurator/namespace.yaml`: change `pod-security.kubernetes.io/enforce` from `restricted` to `baseline`, and keep `warn: restricted`. This matches care-bus, strata-rag, enterprise-mcp and commonpractice. Rejected alternative: keep `restricted` and add a Gateway `spec.infrastructure.parametersRef` ConfigMap that patches `seccompProfile: RuntimeDefault` into the waypoint deployment. No other namespace does this, and per the cluster rule a one-off pattern would have to land in the archetype first.
2. Before pushing, check that no open gitops PR touches that file. Run gitops-production's local validators, open the PR, merge it, and reconcile Flux.
3. Verify live:
   - the Gateway reports `PROGRAMMED=True` and the waypoint pod is Running;
   - `/ready` returns 200 through ingress;
   - **negative test:** a pod in a namespace that is not on the `authz.yaml` allow-list gets RBAC denied on `strata-configurator-api:8097`, while an allowed caller gets 200. This proves the policies are now enforced.
4. **Check the same failure elsewhere.** `strata-ingest` is also `restricted` but has no waypoint, so it is not affected. Grep for any other namespace that combines `use-waypoint` with `enforce: restricted`. A one-off check found only strata-configurator.

### W1 — configurator: commonpractice identity, tenancy and RLS (strata_configurator)
0. **Spec rev 3** (doc commit): fold in §2 and §3.
1. Keycloak, via a gitops realm-patch job modelled on `realm-patch-job-strata-configurator.yaml`:
   - client role `super_admin` on client `strata-configurator`, and the owner user gets it (replacing the realm role `config-super-admin`);
   - a portal-realm client-credentials admin SA `strata-configurator-admin`, scoped to create users and send action emails, modelled on commonpractice's `commonpractice-admin`.
2. API auth:
   - read only `sub` and the client role `super_admin` from the token;
   - `TenantCtx` comes from `users` by `sub`; for `super_admin` it comes from `X-Active-Tenant-Id` (bare non-nil uuid, audited switch);
   - replace `member` with `users(sub pk, tenant_id, tenant_role tenant_admin|tenant_user)`, one tenant per user; the user routes become invite (creates the Keycloak identity through the admin SA), change role, and disable. Tenant creation (super-admin, or early-access approval) mints the tenant uuid: the configurator defines tenant ids (§3);
   - migration `0003`, forward-only, since pre-prod.
   - **Landed (API, dark):** invite by `email` on `POST /api/tenants` (`first_admin_email`) and `POST /api/tenants/{id}/users` via the `strata-configurator-admin` SA (`src/keycloak_admin.rs`, `KEYCLOAK_ADMIN_*`); find-or-create, action email only for a new identity, no realm roles or attributes written. Still needed: the gitops realm patch that creates the SA, and its secret.
3. RLS:
   - add a non-bypass role `configurator_app` (DB owner ≠ runtime role);
   - add `SET LOCAL app.current_tenant` per request transaction;
   - add policies on `tenant`, `layer` (where `tenant IS NULL OR tenant = current`), `overlay_row` (through its layer), `dimension_value`, `profile` and `audit_event`;
   - platform rows (tenant NULL) are readable by all and writable only under a super-admin GUC flag;
   - add a CI anti-drift test that every table with a `tenant` column has RLS enabled and at least one policy (commonpractice's verification pattern);
   - the provisioning script must create two DB users, the owner (migrations) and the app role. Check that `scripts/host-configs/worker4/postgres/provision-service-db.sh` supports this, or extend it **in gitops first**.
4. Tests:
   - rewrite the auth, tenancy and layer tests around the `users` table and the `super_admin` client role;
   - keep the Review Focus tests: cross-tenant id → 404, and concurrent PUT → 200/409;
   - add an RLS test in which raw SQL as the app role, with tenant A set, cannot see tenant B's rows.
5. Web: drop the tenant picker for normal users (one tenant per user); a super-admin gets a tenant switcher.

### W2 — configurator: licensing gate 2, early-access signup, metering scaffold
1. Licence gate 2 (Rust port of commonpractice `license-gate.ts`):
   - introspect, cache 300 s, fail closed;
   - error codes 402 `license_required|inactive|expired|unreadable|unavailable` and 403 `license_app_mismatch|tenant_mismatch`;
   - exempt: `/health`, `/ready`, super-admin routes and `/api/license/status`;
   - env `LICENSE_GATE_2_ENABLED`, `LICENSE_INTROSPECT_URL`, `LICENSE_APP_KEY=strata-configurator`, `LICENSE_CACHE_TTL_SEC`.
   - Licences side: W6.
2. Issuance: on tenant create, `POST /licenses/user/{sub}` through a `secure-apps` client-credentials SA (`strata-configurator-licenses-admin`), with the secret in SOPS and egress NetworkPolicy plus AuthZ entries in gitops. Revoke on offboarding.
3. Early-access signup:
   - `POST /api/signup-approvals/intake` (client-credentials client `configurator-signup-intake` with a role) writes to a `signup_approvals` table;
   - a super-admin approval UI;
   - approval creates the tenant, issues the licence and **provisions down**, i.e. creates the tenant in care-bus once W3 lands;
   - Turnstile and Telegram are optional and dark by default.
4. Metering: build the table and outbox scaffold only when phase 3 brings LLM calls. Record that here so the next session does not build it early.
5. Rollout order, as in commonpractice: backfill licences, smoke-test introspect, then flip `LICENSE_GATE_2_ENABLED`.
6. **Landed (API, dark):** gate 2 (`src/license.rs`, applied in `tenancy.rs` for non-super-admin callers, `GET /api/license/status`) and issuance on tenant create (`src/licenses_admin.rs`, token in owner-only `tenant_license`, migration 0004; a failed issuance fails the create with 502 `licences_unavailable`, as commonpractice does). Not yet: revoke on offboarding, token encryption at rest, the tenant-level licence fallback, the gitops SA/secret/NetworkPolicy, W6 seeds.
7. **Landed (W2.3, API + web, dark behind `EARLY_ACCESS_SIGNUP_ENABLED`):** early-access signup after commonpractice (`src/routes/signups.rs`, `src/signup.rs`, owner-only `signup_approvals`, migration 0005; web `Signups` page for the super-admin). Intake `POST /api/signup-approvals/intake` (client `configurator-signup-intake` + client role of the same name, super-admin tokens refused; 202 `queued|already_pending`; Turnstile and Telegram optional). Approve runs the same `create_tenant` path as `POST /api/tenants` (invite, licence, revoke on failure, audit `signup.approve`) under a row lock (second approve 409 `approval_not_pending`); reject audits `signup.reject`. Not yet: provision-down to care-bus (`// W4:` in `approve`), the Keycloak client/role, the gitops env/secret/egress, commonpractice's untrusted-row digest and domain auto-routing (no anonymous path exists here).

### W3 — care-bus: multi-tenant, multi-job, base vs refinement (care-bus-agents; its own plan file in that repo)
1. Fix the `aud`/`iss` defect now: `set_required_spec_claims(&["exp","iss","aud","sub"])` plus tests for missing and wrong aud/iss. This is independent and small; it can go first as its own commit in care-bus's W3 PR, or as a separate PR if the owner wants it shipped sooner.
2. Identity: an own `users(sub, tenant_id, tenant_role)` table plus a `care-bus` client role `super_admin`, the same model as W1; the configurator provisions tenants into it. Drop the "one tenant's cell" assumption and the single `CAREBUS_TENANT_ID`. Calls to rag/ingest forward the request's tenant.
3. Schema: add `tenant_id` + `job_id` to every config table (context_spaces, procedures, connectors, connector_field_mappings, knowledge stores, sources, model routes, ontology tables), plus RLS and the anti-drift test. Add a new `job` table (tenant, key, label, settings jsonb for preamble/actor/stance) to cover P6. Make streams and audit tenant- and job-scoped (`bus:<tenant>:<job>:*` or equivalent).
4. **Base vs refinement (the core of the handoff).** Every config row carries `origin = base | tenant`:
   - base rows carry `release_id`;
   - the tenant admin edits only create `origin=tenant` rows, as overrides, extensions or `hidden` tombstones, with the same natural key as the base row (commonpractice's `tenantRowForGlobal` predicate);
   - the effective view = base(current release) ⊕ tenant rows, computed at read time, the same as commonpractice `compileEffective`.
5. Release endpoint: `PUT /api/tenants/{tenant}/jobs/{job}/releases/{release_id}` (tenant in the path: the SA token carries none), authenticated by a client-credentials SA with the client role `release-writer` (P4). The same role, and nothing else, also covers tenant provisioning `PUT /api/tenants/{tenant}` (tenant + first `tenant_admin`).
   - Body: the configurator's release bundle (ontology slice, context spaces/use cases, procedures, job settings, connector *slots* without secrets, knowledge store layout), plus its hash.
   - Idempotent on `(tenant, job, release_id, hash)`; a re-push is a no-op.
   - The apply is transactional: it swaps the base rows to the new release and reports a conflict for every tenant row whose base target disappeared or changed. Conflicts are kept, flagged in the admin UI and returned in the response.
   - `GET /api/tenants/{tenant}/jobs/{job}/releases/current` exposes the current release for read-back (drift).
6. Data-driven substrate (P1, P2, P5): request types, areas and slots come from the effective context spaces; tools and side-effect flags come from config; this removes the `domain.rs` constants. This is the largest item; split it in the care-bus plan.
7. Admin UI: everything is job-scoped; refinements of connectors (credentials, endpoints, field mappings) and knowledge areas sit on top of the base; base and tenant values are shown side by side, with unhide/revert.
8. Licensing gate 2 (app key `care-bus`) and metering of LLM calls through the outbox → `billing:costs` (care-bus makes LLM calls today). Both ship dark behind flags.

### W4 — configurator phase 2b: release push to care-bus (strata_configurator)
Needs W1 and W3.5 live.
- `target` and `release` tables;
- the release bundle builder from compile output;
- push with an SA token;
- record the response and conflicts;
- drift read-back;
- the UI release page.

Exit (live): push release 1 for a test tenant's `customer_care_agent` job; care-bus answers a return request with it; a tenant refinement survives release 2; a removed base object shows as a conflict.

### W6 — licences service (repo `nonagenticai/licenses`, local `/Users/georgedekker/Movies/nonagenticai/licenses`), owned by this workstream (owner, Q4/Q6)
1. **Register the apps** in `scripts/seed_apps.py`. `APPS` starts at line 80; the commonpractice entry at line 143 (`issuers=("commonpractice",)`, plan `standard`, perpetual) is the model. Add:
   - `strata-configurator` and `care-bus`, each with its own issuer, audience `human`;
   - one **test plan** (`standard`, perpetual, the default quota and token budget) that works now.
   Real plan types come later.
2. **Offerings** (`scripts/seed_offerings.py`): `byo_mode = byo_required`, matching care-bus's existing model where every user is **BYO by default** and local platform providers are used only for local testing. Do not add a platform-paid offering yet.
3. **`actor_sub` on the bundle-token mint.**
   - Code: `internal_mint_tenant_bundle_token` at `main.py:10431` (route at :10390, auth `require_bundle_token_minter` in `auth/dependencies.py:196`, minter scoping in `models.py:1806-1835, 2434-2471`).
   - Accept an `actor_sub`, so a tenant-x-app licence can carry the human actor for members who exist only in the calling app's DB.
   - This unblocks worker_management's `tenant_licence` mode, which was reverted to delegation in gitops#4720 because of `unknown_subject`, and is what w2v, merqi and later rag/ingest need.
   - Tests start from `tests/unit/test_dependency_licences.py:190`.
   - Verify live by re-flipping one WM caller to `tenant_licence`; coordinate with the w2v session that owns gitops#4720.
4. Apply seeds and migrations in the licences deploy, then check that `POST /license/introspect` for a newly issued `strata-configurator` licence returns `active` with `app.key == strata-configurator`.

### W5 — strata_rag / strata_ingest: shared multi-app dependencies (cell pattern, as w2v)
These services are shared by many apps (w2v, merqi, configurator/care-bus, …). Owner, 2026-10-17: they use **the same implementation w2v is building** (not finished), i.e. the way apps *should* use worker_management.

The reference pattern (plan `~/.claude/plans/w2v-cell-architecture-template.md` rev 5, implemented in w2v_orchestrator):
- **Provision down.** The top app owns the tenant and PUTs each dependency's config subtree to that dependency's tenant-config API.
- **Manifest and slots.** `GET /api/provider-requirements` returns a manifest, so the top GUI is generated from what each dependency needs. `PUT /api/tenant/connections` writes slots in bulk, all-or-nothing, with `expected_version`, and reports per slot whether it was created, updated, deleted or unchanged. `PUT /api/tenant/connections/{slot}` runs policy and SSRF checks, then a live test, then stores an immutable version. Keys are write-only.
- **Resolution.** A tenant slot wins, else the deployment default. A tenant slot that fails is an error and never falls back to a platform key.
- **Run pinning.** A run pins its slot versions for its lifetime.
- **Auth: tenant-x-app licence.** The caller presents the end tenant's own minted licence for that app, via `POST {licenses}/internal/tenants/{t}/bundle-token?app=<app>`, cached per (tenant, app) and failing loud (see `w2v_common/auth/bundle_tokens.py`). There are no shared service-account tokens and no `X-Effective-Tenant-Id` delegation. This follows the owner ruling of 2026-10-15: "no app-to-app licences, only tenant-x-app licences".

**Known blocker (same as w2v and WM).** worker_management's tenant_licence mode was reverted to delegation (gitops#4720). WM checks the human actor against the licences service, but members exist only in the app, so the check returns `unknown_subject`. The fix needs `actor_sub` on the licence mint, and that is licences-owner work. merqi has not adopted the pattern either: it uses shared tokens or delegation, has no provision-down and keeps providers in env.

What strata_rag and strata_ingest already have:
- `GET/PUT /api/tenant/providers` (rag: embed, rerank, chat; ingest: ocr, vision, transcribe), with keys write-only and AES-GCM sealed, an SSRF guard, a live probe, and `source` reported as tenant, default or unconfigured.
- Tenant scope via static env tokens: rag uses `<tenant>:<token>` plus `X-Tenant-Id`, ingest uses a token→tenant map.

What is missing:
- the provider-requirements manifest;
- the bulk connections PUT with versions and `expected_version`, and per-slot test and DELETE;
- tenant-x-app licence auth instead of static `STRATA_API_TOKENS` / `STRATA_INGEST_API_TOKENS`;
- tenant register and revoke;
- run pinning;
- per-tenant usage metering with actor and project attribution.

W5 = bring rag and ingest to this pattern, **after** w2v's version stabilises (so they copy the finished contract) and after the licences `actor_sub` fix. Then the configurator (or care-bus, per job) provisions rag/ingest slots down for its tenants. The overview's P7/P8 items (ingest metadata, classify stage) are unchanged.

## 5. Order

1. W0, today: small, and it unblocks enforced authz.
2. W1.0 (spec rev 3), then W1 ∥ W2. care-bus W3 waits for the owner's other care-bus session (brief in §8); its aud/iss fix is item 2 of that brief.
3. W3 (plan written in care-bus-agents first, from its `origin/main`) ∥ W2.
4. W4 once W1 and W3.5 are live.
5. Phase 3 (conversation/funnel; LLM = the care-bus tenant-provided commercial provider, or local pgx vllm / mlx_api, per the owner note on main `7076f27`), then metering for the configurator.

## 6. Questions

Answered by the owner on 2026-10-17:
- **Q1. Early access: yes, copy commonpractice exactly.** The configurator must be able to do early-access self-signup with the same setup commonpractice uses: intake client, approval queue, super-admin approval, tenant + licence + default plan. It is switched on when testing is done.
- **Q2. Roles: per-app client roles**, with no overlap between apps, and dedicated `tenant_admin`/`tenant_user` roles for the configurator and separately for the agents. See §3.
- **Q3. Tenant id: defined by the configurator**, shared with the agents. rag and ingest follow the w2v cell pattern (§4 W5).
- **care-bus (W3) is not touched in this session.** The owner has another session making final care-bus changes. Hand that session the brief in §8. The further care-bus design (W3 as a plan in care-bus-agents) comes after that session finishes.

- **Q4. Licences:** I do the registrations (W6) and define one test plan that works. Plan types come later. BYO by default, as care-bus already does; local platform providers only for local testing.
- **Q5. Tenant id onto Keycloak:** not needed. commonpractice's code never writes `tenant_id` to Keycloak (§2). If something is still unclear, repos that exist only on mini3 (`~/Movies` there, `$HOME=/Users/mini3`) may help.
- **Q6. Licences `actor_sub` fix:** owned by this workstream (W6.3).

Still open: none blocking. Content questions (relations for Party, Contract and Bill) stay in the phase-1 ledger.

## 8. Brief for the care-bus-agents session (owner relays)

The text to paste is kept in §8.1 so it lives alongside the plan.

### 8.1 Paste this
```
Context: strata_configurator (repo nonagenticai/strata_configurator, phase 1 live at
configurator.internal) will become the system that DEFINES tenants and pushes a versioned
BASE configuration per (tenant, job) into care-bus. care-bus will become multi-tenant AND
multi-job, and its admin UI will refine that base per job (backend connectors, knowledge
areas). The design for that comes after your session. It is in strata_configurator
docs/superpowers/plans/2026-10-17-configurator-02-tenancy-and-carebus-handoff.md §4 W3
(branch docs/next-session-plan). Please keep your final changes compatible with it.

1. Don't build in the opposite direction. No new single-tenant assumptions (no more global
   CAREBUS_TENANT_ID uses, no new unscoped config tables, no hardcoded areas/request types
   in domain.rs), and no tenant concept invented inside care-bus. Tenant ids will come from
   the configurator via provision-down (tenant create + pushed releases). Like
   commonpractice's code, Keycloak will only AUTHENTICATE. Tenant and role will live in a
   care-bus `users(sub, tenant_id, tenant_role)` table, one tenant per user.
2. Auth defect, verified on origin/main: crates/carebus-api/src/auth.rs ~115-118 sets
   issuer and audience but never calls `set_required_spec_claims`. In jsonwebtoken 9 a token
   WITHOUT `aud` or `iss` therefore passes (any portal-realm token from another client).
   Fix: `v.set_required_spec_claims(&["exp","iss","aud","sub"])` plus HTTP tests for an
   expired token, missing or wrong aud, missing or wrong iss, missing sub, HS256 and
   alg=none, all 401. strata_configurator fixed the same copied code in commit c3be65e
   (tests/auth.rs `claim_checks_401`), so copy that. If it doesn't fit your session, say
   so and it goes into W3.
3. Roles (owner decision): care-bus roles do NOT overlap with any other app's.
   `tenant_admin` and `tenant_user` will live in care-bus's own users table; the only
   Keycloak role will be a `care-bus` CLIENT role `super_admin`
   (resource_access, never realm_access). If you touch role handling, move toward this. The current care-admin / care-viewer realm roles will be
   replaced in W3.
4. Please end your session with a short note listing:
   (a) the final origin/main SHA;
   (b) every config table and every admin route you added or changed;
   (c) what still reads CAREBUS_TENANT_ID;
   (d) whether you changed connectors, knowledge stores/sources, procedures or
       model-routes schemas.
   I will design W3 (multi-tenant/multi-job, base vs refinement, release endpoint with a
   release-writer client-credentials role, RLS, licence gate 2, metering outbox) from that
   state.
```

## 7. Where things are

- Spec: `docs/superpowers/specs/2026-10-16-configurator-design.md` (rev 2; rev 3 pending, W1.0).
- Plans: `docs/superpowers/plans/2026-10-16-configurator-00-overview.md`, `…-01-model-compiler.md` (done), this file.
- care-bus vision: care-bus-agents `docs/superpowers/specs/2026-10-16-interaction-layer-vision.md`.
- commonpractice reference files (read via `git show origin/main:`): `backend/src/ontology/ontology-service.ts`, `backend/src/middleware/{admin-guards,license-gate}.ts`, `backend/src/services/{license-client,licenses-admin,plan-posture}.ts`, `backend/src/host/{metering-emit,metering-outbox-drainer}.ts`, `migrations/{0018,0023,0024,0028,0029,0032,0090}`, `SPEC-PHASE4-admin-tenant-isolation.md`, `docs/runbooks/early-access-signup.md`.
- gitops: `clusters/strato/services/strata-configurator/`, `services-istio/strata-configurator/`, `services/keycloak/realm-patch-job-strata-configurator.yaml`; commonpractice licence and metering wiring in `services/commonpractice/deployment.yaml:314-364, 488-540`, `networkpolicy.yaml:91-108`.
- Live: `https://configurator.internal`; kubectl through master (see Movies/CLAUDE.md).
