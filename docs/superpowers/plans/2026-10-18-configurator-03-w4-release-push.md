# strata-configurator W4: versioned base release push to care-bus. Implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the configurator builds a versioned base release per `(tenant, job)` from its compile output, provisions the tenant and job into care-bus, pushes the release to care-bus's release endpoint as the `release-writer` SA, records the response and conflicts, and reads the release back to detect drift. A super-admin can also adopt an existing tenant id (the platform fr-test tenant) with a licence, as the first managed tenant.

**Architecture:** `configurator-core` gains a typed mirror of care-bus's *release bundle v1* together with its JCS hash, its referential validation, and a pure `build_bundle(&EffectiveConfig)`. The four bundle sections the phase-1 model cannot express (context spaces, procedures, connector slots, knowledge-store layout) become four new overlay kinds, compiled by the same layer and overlay rules. A new seed v2 fills them for `customer_care_agent` from care-bus's golden fixture. `configurator-api` adds three things: `target`, `release` and `push_step` tables under RLS; a care-bus client (a client-credentials token, then `PUT` tenant, user, job and release, and `GET releases/current`); and routes to build, push and drift-check. Tenant creation, signup approval and the new import path provision down to care-bus as a best-effort step that can be retried. A failed provision never fails the create.

**Tech Stack:** Rust (axum 0.8.9, sqlx 0.9.0, serde_json 1.0.154, sha2 already in core, reqwest 0.13.5, testcontainers postgres 18), SvelteKit 2 + Svelte 5 web, vitest. **No local cargo** (hook-enforced): CI on the one W4 PR is the first compile.

**Spec:**
- `docs/superpowers/plans/2026-10-17-configurator-02-tenancy-and-carebus-handoff.md` §4 W4 (and §2.2, §3);
- `docs/superpowers/specs/2026-10-16-configurator-design.md` §3 (target), §6.3 (push), §8 (release), §9 (drift);
- **the authoritative contract:** care-bus-agents branch `feat/w3-multitenant` (draft PR #23), `docs/superpowers/plans/2026-10-17-care-bus-11-w3-base-refinement-release.md` § "Release bundle v1", fixture `crates/carebus-api/tests/fixtures/release-bundle-v1.json` (hash `sha256:79c5b940a6a1b0f3015bd5b818e22ac7c284a72efef6c69c1e276612e8aaa492`), and the provision-down routes in `crates/carebus-api/src/routes/tenancy.rs` (commit 401b350).

Read the contract with `git -C ../care-bus-agents fetch && git -C ../care-bus-agents show origin/feat/w3-multitenant:<path>`. **Never modify care-bus-agents.**

## Contract facts the plan relies on (verified 2026-10-18 on `origin/feat/w3-multitenant` and gitops `origin/main`)

| Item | Value |
|---|---|
| Release push | `PUT /api/jobs/{job}/releases/{release_id}`, header `X-Active-Tenant-Id: <uuid>`, bearer = release-writer SA. Body `{schema:"care-bus.release/1", release_id, job, hash, bundle}`. 201 = applied as a new release; 200 = either a no-op (`applied:false`) or a re-apply of a known release. 409 `release_hash_mismatch`; 400 `invalid_bundle` (`details:[{path,message}]`), `bundle_hash_mismatch`, `release_path_mismatch`, `invalid_release_id`; 404 `job`; 413 over 4 MiB. **Deviation from our spec §6.3:** the tenant goes in the header, not the path. Follow care-bus |
| Read-back | `GET /api/jobs/{job}/releases/current` (release-writer allowed). It returns the same `ReleaseReport` shape with the **open** conflicts. 404 `release` means the job has never had a release |
| Report | `{release_id, hash, applied, previous_release_id, applied_at, applied_by, counts{9 keys}, conflicts:[{id, kind, object, key, tenant_row, base_hash_before, base_hash_after}]}` |
| Provision-down | `PUT /api/tenants/{t}` `{name (1..120), status:"active"\|"suspended"}` (201 created / 200 updated, upsert; **overwrites name**). `PUT /api/tenants/{t}/users/{sub}` `{tenant_role:"tenant_admin"\|"tenant_user", display_name?}` (409 `user_in_other_tenant`). `PUT /api/tenants/{t}/jobs/{key}` `{label, settings?}`: `settings` is only for a NEW job. On an existing job a different `settings` is 409 `job_settings_read_only`, so **we never send `settings`**; the base settings come with the release. All three accept the release-writer (`Provisioner` / `UserManager`) |
| Auth | care-bus reads `resource_access["care-bus"].roles ∋ "release-writer"` and requires `exp, iss, aud, sub`. The configured `KEYCLOAK_AUDIENCE` must be `care-bus` |
| Hash | `"sha256:" + hex(SHA-256(JCS(bundle)))`: keys sorted, compact, strings escaped as serde_json does, integers only (a float is rejected) |
| Keycloak | gitops #4751 (`ed686e9e1a`) created the portal-realm confidential SA client **`strata-configurator-release-writer`**, holding `care-bus/release-writer` and its own `aud=care-bus` mapper. Its secret is "operator-set, stored SOPS per consumer", **not yet in any secret**. `KC_HOSTNAME` is fixed, so a token minted at the in-cluster token URL still carries `iss=https://auth.nonagentic.ai/realms/portal` |
| care-bus live | W3 is **not deployed**. Live `carebus-api` runs `KEYCLOAK_AUDIENCE=care-bus-admin` (it flips with the W3 deploy). Until then every release-writer call returns 401 |
| fr-test tenant in licences | `0195f48b-d000-7000-8000-000000000099`, name `fr-test-user`, `owner_key=platform`. `tenant_app_grant` exists for commonpractice, fr, merqi and w2v, **not for strata-configurator**, so licence issuance for it returns 409 `tenant_owned_by_other_app` (licences `PUT /tenants/{id}`) until a grant exists |

## Bundle field → configurator source (gap table)

| Bundle v1 field | Configurator source today | W4 action |
|---|---|---|
| `job_settings.stance` | `Job.stance` (job overlay) | map |
| `job_settings.actor` | `Job.actor` is a role word (`customer`), not the prompt text care-bus uses (`Customer care desk`) | **gap**: new optional job field `actor_label` → `actor` (Task 2) |
| `job_settings.preamble` | none | **gap**: new optional job field `preamble` (Task 2) |
| `ontology.entities[]` (name, scope, industry, department, label, synonyms) | `project_carebus` (carebus.rs) | map. `hidden` entities are dropped. The slice is the closure defined in Global Constraints |
| `entities[].attributes[]` | `EffectiveConfig.attributes` (same `data_type` enum) | nest under the entity |
| `ontology.relations[]` | `EffectiveConfig.relations` | kept only when both ends are in the slice |
| `context_spaces[]` | **none** (spec §6: generated from approved use cases in phase 3) | **gap**: new overlay kind `context_space` (Task 2) + seed v2 (Task 3) |
| `procedures[]` (+ `steps`, `plan`) | **none** (phase 3: SOPs) | **gap**: new overlay kind `procedure`, key `name@version`. `plan` is checked only as `{phases:[…]}`, because the full `carebus::domain::Plan` check stays with care-bus (no shared code) |
| `connectors[]` slots + `field_mappings` | `Job.tools` holds names only | **gap**: new overlay kind `connector_slot`. The typed struct has no secret field, so a secret cannot be expressed |
| `knowledge_stores[]` | `Job.stores` holds names only | **gap**: new overlay kind `knowledge_store` |
| `release_id`, `job`, `hash`, `schema` | none | the release builder (Task 4/6) |
| target `(tenant, job, selection)` | none (compile takes the selection per request) | new `target` table (Task 6) |

The phase-1 seed ontology differs from care-bus 0002 by the recorded deltas in `crates/configurator-core/tests/golden.rs` (for example `Order` is `generic` here and `industry/retail` there). Release 1 therefore **changes** base rows relative to the golden fixture. That is expected. We prove **hash-function parity** on the fixture itself, not byte equality of our build with it.

## Global Constraints

- One PR for all of W4 (branch `feat/w4-release-push`). CI runs only on `pull_request`, so open the PR as a **draft after Task 1's commit** and push after each task. Rust is verified only by CI job `api` (`cargo test --workspace --locked`); web by job `web`. Locally run only `cd web && npm run ci:verify && npm test && npm run lint`.
- Forward-only migrations `0008_job_content_kinds.sql` (Task 3) and `0009_release.sql` (Task 6). Pre-prod: no back-compat shims.
- Unversioned routes `/api/<resource>`.
- Every new table with a `tenant` column has RLS + policies (the anti-drift test in `tests/rls.rs` must keep passing), and the needed grants to `configurator_app`. `push_step` and `release` are append-only for the runtime role: `SELECT, INSERT` only.
- **Release id** = `cfg-YYYY-MM-DD.N` (UTC date; N = 1 + the number of this target's releases with that date prefix, under the target row lock). The `cfg-` prefix is never used by care-bus's own `bootstrap-*` releases or by its hand-pushed fixture `2026-10-20.1` (care-bus plan 11 Task 8), so we never hit its 409 `release_hash_mismatch` by accident. The id must match `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`.
- **Hash** = `"sha256:" + sha256_hex(canonical_json(bundle))`, reusing `configurator_core::canonical`. Before hashing, refuse any non-integer number (`HashError::Float { path }`).
- **Ontology slice** = `job.ontology_slice` ∪ every entity named in `context_spaces[].entities` ∪ every entity in `connectors[].field_mappings[].attribute`. An empty or absent `ontology_slice` means every effective entity. Hidden entities are excluded. A relation is kept only when both its ends are in the slice.
- `job_settings` keys are omitted when their source is `None` (`skip_serializing_if`). Every other nullable bundle field (`industry`, `department`, `base_url`, `rate_limit`, `parent`, `description`, `pddl_domain`) **serialises as `null`, never omitted**. Every bundle struct is `#[serde(deny_unknown_fields)]`.
- The care-bus client is configured all-or-none by `CAREBUS_API_URL`, `CAREBUS_TOKEN_URL`, `CAREBUS_CLIENT_ID`, `CAREBUS_CLIENT_SECRET`. A partial set fails startup. With none set, push and drift return 503 `carebus_unconfigured` and provision-down is skipped (logged).
- Provision-down is **best effort**: tenant create, signup approve and import succeed even when care-bus fails. The failure is audited as `carebus.provision_failed` and `tenant.carebus_provisioned_at` stays NULL. Every push first re-provisions the tenant and job, and both are idempotent upserts.
- Roles: target, build and push need `tenant_admin` (a super-admin acting in the tenant counts). Reads need `tenant_user`. Import and provision retry are super-admin only. Licence gate 2 applies as it does on every tenant route.
- No secret or token in logs, audit metadata, `push_step.response` or API responses. The care-bus bearer is never stored.

## Review Focus

1. **Hash drift between configurator and care-bus.** Possible causes: a nullable field omitted instead of `null`, a float, or a key order coming from `preserve_order`. Expected: our hash of the copied golden fixture equals its committed hash, and a typed round-trip (fixture → `ReleaseBodyV1` → JSON) has the same hash. Pinned in Task 1 (`golden_fixture_hash_matches_carebus`, `typed_roundtrip_preserves_hash`, `null_fields_are_serialised`).
2. **Release 1 deletes care-bus's working base.** An empty `context_spaces` or `procedures` section would delete the 5 spaces and 6 procedures care-bus runs today. Expected: build refuses (422 `bundle_incomplete`) when the job has no context space or no PUBLISHED procedure. Pinned in Task 4 (`empty_job_content_is_refused`).
3. **Re-push and concurrency.** Two pushes of one release, or a re-push after a 5xx, must not create a second release id or a duplicate `release` row. Building twice with no change returns the existing release. Pinned in Task 6 (`rebuild_without_change_returns_same_release`) and Task 7 (`repush_records_noop`).
4. **Wrong tenant on the wire.** A tenant_admin of A pushing a release of tenant B by id must get 404, and the header sent to care-bus must always be the release's own tenant. Pinned in Task 7 (`push_sends_release_tenant_header`, `cross_tenant_release_is_404`).
5. **Import of an existing id.** Re-importing must be idempotent; an import whose name collides, or whose id belongs to a live tenant under another name, must be refused; a licence failure must create nothing. Pinned in Task 9 (`import_is_idempotent`, `import_name_mismatch_is_409`, `import_licence_refused_creates_nothing`).

---

## File structure

| File | Responsibility |
|---|---|
| `crates/configurator-core/src/release.rs` (new) | bundle v1 types, `bundle_hash`, `release_body`, `validate`, `build_bundle` |
| `crates/configurator-core/tests/fixtures/carebus-release-bundle-v1.json` (new) | byte copy of care-bus's golden fixture |
| `crates/configurator-core/tests/release.rs` (new) | parity, build and validation tests against seed v1+v2 |
| `crates/configurator-core/src/{overlay,compile,canonical,seed}.rs` | four new kinds, two new job fields, `EffectiveConfig.job_content` |
| `seed/v2/customer_care_agent.toml` (new) | job-layer rows for `customer_care_agent` |
| `crates/configurator-api/migrations/0008_job_content_kinds.sql`, `0009_release.sql` (new) | kind CHECK; target/release/push_step, `tenant.carebus_provisioned_at` |
| `crates/configurator-api/src/carebus_client.rs` (new) | SA token + the five care-bus calls |
| `crates/configurator-api/src/provision.rs` (new) | best-effort provision-down of tenant, first admin, job |
| `crates/configurator-api/src/db/releases.rs` (new) | target/release/push_step queries |
| `crates/configurator-api/src/routes/releases.rs` (new) | targets, releases, push, drift |
| `crates/configurator-api/src/routes/{compile,tenants,signups}.rs` | compile extracted for reuse; import route; provision-down hooks |
| `crates/configurator-api/tests/{releases,carebus_provision}.rs` (new), `tests/common/carebus_mock.rs` (new) | API tests against a local axum mock of care-bus + Keycloak token |
| `web/src/routes/releases/+page.svelte` (new), `web/src/routes/tenants/+page.svelte`, `web/src/lib/api.ts` | release page; import form |
| `web/tests/unit/Releases.test.ts` (new) | page tests |

---

### Task 1: Bundle v1 types and hash parity with care-bus

**Files:**
- Create: `crates/configurator-core/src/release.rs`, `crates/configurator-core/tests/fixtures/carebus-release-bundle-v1.json`, `crates/configurator-core/tests/release.rs`
- Modify: `crates/configurator-core/src/lib.rs` (`pub mod release;`)

**Interfaces — Produces:**
```rust
pub const SCHEMA_V1: &str = "care-bus.release/1";
// All #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] #[serde(deny_unknown_fields)], field names/types exactly as bundle v1:
pub struct ReleaseBodyV1 { pub schema: String, pub release_id: String, pub job: String, pub hash: String, pub bundle: BundleV1 }
pub struct BundleV1 { pub job_settings: JobSettingsV1, pub ontology: OntologyV1, pub context_spaces: Vec<ContextSpaceV1>,
                      pub procedures: Vec<ProcedureV1>, pub connectors: Vec<ConnectorSlotV1>, pub knowledge_stores: Vec<KnowledgeStoreV1> }
pub struct JobSettingsV1 { preamble/actor/stance: Option<String> }        // skip_serializing_if = "Option::is_none", default
pub struct OntologyV1 { pub entities: Vec<EntityV1>, pub relations: Vec<RelationV1> }
pub struct EntityV1 { name, scope, industry: Option<String>, department: Option<String>, label, synonyms: Vec<String>, attributes: Vec<AttributeV1> }
pub struct AttributeV1 { name, data_type, pii: bool, synonyms: Vec<String> }
pub struct RelationV1 { subject, predicate, object, cardinality }
pub struct ContextSpaceV1 { key, area, label, request_type, required: Vec<String>, entities: Vec<String>, procedure: String }
pub struct ProcedureV1 { name, version, status, context_space, domain, description: Option<String>, pddl_domain: Option<String>,
                         tool_requirements, success_criteria, reviewers: Vec<String>, plan: serde_json::Value, steps: Vec<StepV1> }
pub struct StepV1 { step_number: i64, phase: i64, name, action_type, worker, mcp_tool, performed_by,
                    preconditions, postconditions: Vec<String>, parameters: BTreeMap<String, String>, gate: bool }
pub struct ConnectorSlotV1 { id, name, system, auth_type, base_url: Option<String>, timeout_ms: i64, rate_limit: Option<String>, field_mappings: Vec<FieldMappingV1> }
pub struct FieldMappingV1 { backend_field, attribute }
pub struct KnowledgeStoreV1 { key, label, parent: Option<String>, kind, areas: Vec<String>, description: Option<String> }
#[derive(Debug, thiserror::Error)] pub enum HashError { #[error("non-integer number at {path}")] Float { path: String } }
pub fn bundle_hash(bundle: &serde_json::Value) -> Result<String, HashError>;          // "sha256:<64 hex>"
pub fn release_body(release_id: &str, job: &str, bundle: &BundleV1) -> Result<serde_json::Value, HashError>;
```
Check the step and connector field types against the fixture before writing them (for example, whether `phase` is nullable). Copy the fixture with `git -C ../care-bus-agents show origin/feat/w3-multitenant:crates/carebus-api/tests/fixtures/release-bundle-v1.json > crates/configurator-core/tests/fixtures/carebus-release-bundle-v1.json`. Add `thiserror` to core only if it is not already a dependency there; otherwise implement `Display` by hand.

- [ ] **Step 1: Write the failing tests** in `tests/release.rs`:
```rust
const FIXTURE: &str = include_str!("fixtures/carebus-release-bundle-v1.json");
#[test] fn golden_fixture_hash_matches_carebus() {
    let body: Value = serde_json::from_str(FIXTURE).unwrap();
    assert_eq!(body["hash"], "sha256:79c5b940a6a1b0f3015bd5b818e22ac7c284a72efef6c69c1e276612e8aaa492");
    assert_eq!(bundle_hash(&body["bundle"]).unwrap(), body["hash"].as_str().unwrap());
}
#[test] fn typed_roundtrip_preserves_hash() {
    let body: ReleaseBodyV1 = serde_json::from_str(FIXTURE).unwrap();
    let again = serde_json::to_value(&body.bundle).unwrap();
    assert_eq!(bundle_hash(&again).unwrap(), body.hash);
}
#[test] fn null_fields_are_serialised() {
    let body: ReleaseBodyV1 = serde_json::from_str(FIXTURE).unwrap();
    let v = serde_json::to_value(&body.bundle).unwrap();
    assert!(v["connectors"][0]["base_url"].is_null() && v["connectors"][0].get("base_url").is_some());
    assert!(v["ontology"]["entities"][0].get("industry").is_some());
}
#[test] fn floats_are_refused() { assert!(matches!(bundle_hash(&json!({"a": [1, {"t": 1.5}]})), Err(HashError::Float { .. }))); }
#[test] fn secret_ref_in_slot_does_not_parse() {
    let mut v: Value = serde_json::from_str(FIXTURE).unwrap();
    v["bundle"]["connectors"][0]["secret_ref"] = json!("k/v");
    assert!(serde_json::from_value::<ReleaseBodyV1>(v).is_err());
}
#[test] fn release_body_embeds_its_hash() {
    let body: ReleaseBodyV1 = serde_json::from_str(FIXTURE).unwrap();
    let out = release_body("cfg-2026-10-18.1", "customer_care_agent", &body.bundle).unwrap();
    assert_eq!(out["schema"], SCHEMA_V1); assert_eq!(out["release_id"], "cfg-2026-10-18.1");
    assert_eq!(out["hash"], body.hash.as_str());
}
```
- [ ] **Step 2:** Implement `release.rs` (types, then `bundle_hash` = walk for floats → `canonical::canonical_json` → `canonical::sha256_hex` with the `sha256:` prefix).
- [ ] **Step 3: Commit, push, open the draft PR** (`gh pr create --draft`, body ending with the attribution line). Run: CI job `api`. Expected: the 6 tests PASS (a compile failure counts as FAIL; fix and push again).
```bash
git add crates/configurator-core && git commit -m "feat(core): care-bus release bundle v1 types and hash parity with the golden fixture"
```

### Task 2: Four job-content overlay kinds and two job fields

**Files:**
- Modify: `crates/configurator-core/src/overlay.rs` (Kind, key validation, field validation), `crates/configurator-core/src/compile.rs` (state, `EffectiveConfig`, `Job`, ErrorCode), `crates/configurator-core/src/canonical.rs` (`effective_hash`), `crates/configurator-core/src/seed.rs` (kind names)
- Test: unit tests in `overlay.rs` and `compile.rs`

**Interfaces — Produces:**
```rust
pub enum Kind { Entity, Attribute, Relation, Job, ContextSpace, Procedure, ConnectorSlot, KnowledgeStore }   // serde snake_case
// keys: context_space = ContextSpaceV1.key regex ^[a-z0-9-]+(:[a-z0-9-]+){1,3}$; procedure = "<name>@<version>";
//       connector_slot = ^[a-z][a-z0-9-]{0,62}$; knowledge_store = ^[a-z][a-z0-9-]{0,62}$
pub struct JobContent { pub context_spaces: BTreeMap<String, ContextSpaceV1>, pub procedures: BTreeMap<String, ProcedureV1>,
                        pub connectors: BTreeMap<String, ConnectorSlotV1>, pub knowledge_stores: BTreeMap<String, KnowledgeStoreV1> }
impl JobContent { pub fn is_empty(&self) -> bool }
// EffectiveConfig gains `#[serde(default, skip_serializing_if = "JobContent::is_empty")] pub job_content: JobContent`
// Job gains `preamble: Option<String>`, `actor_label: Option<String>` (skip_serializing_if none); JOB_FIELDS gains both as optional Str.
// ErrorCode gains `KindNotAllowedInLayer`.
```
Rules (decided):
- A row's `fields` plus its key must deserialise into the V1 item struct. The key supplies `key`, `name`+`version`, or `id`. A failure is `InvalidRow` with the serde message.
- `patch` merges the top-level fields shallowly into the current object, and the result is re-validated.
- `tombstone` removes the object.
- The four kinds are allowed only in `job` and `tenant` layers; anywhere else is `KindNotAllowedInLayer`.
- They do not cascade and are not dangling-checked in compile. Referential checks live in `release::validate` (Task 4).
- `effective_hash` includes `job_content` only when it is non-empty, and `job.preamble`/`actor_label` only when set. Phase-1 hashes therefore do not move.

- [ ] **Step 1: Write the failing tests:**
  - `context_space_key_validation`: `retail:order-support:returns` ok; `Retail:x` and `retail` rejected;
  - `procedure_fields_must_parse_as_v1`: missing `steps` → `InvalidRow`;
  - `job_content_compiles_in_job_layer`: add a context_space in `job:customer_care_agent`; selection job = that job; it is in `job_content.context_spaces`;
  - `tenant_patch_overrides_context_space_label`;
  - `tombstone_removes_procedure`;
  - `job_content_in_industry_layer_is_refused` → `KindNotAllowedInLayer`;
  - `phase1_hash_unchanged`: the existing `golden.rs` hash assertions are untouched and still pass;
  - `job_preamble_and_actor_label_are_optional`.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Push. Run: CI job `api`. Expected: new tests PASS, and `tests/golden.rs` PASS unchanged.
- [ ] **Step 4: Commit** `feat(core): job-content overlay kinds (context_space, procedure, connector_slot, knowledge_store), job preamble/actor_label`.

### Task 3: Seed v2 (customer_care_agent job content) and migration 0008

**Files:**
- Create: `seed/v2/customer_care_agent.toml`, `crates/configurator-api/migrations/0008_job_content_kinds.sql`
- Modify: `crates/configurator-api/src/seeder.rs` (register v2), `crates/configurator-core/tests/release.rs`, `crates/configurator-api/tests/seeder.rs`

Seed v2 has one layer, `scope = "job"`, `job = "customer_care_agent"`, containing:
- `add` rows for the fixture's 5 context spaces, 6 procedures (with steps and plan), the `crm-core` connector slot and the 2 knowledge stores;
- one `patch` of `kind = "job"`, key `customer_care_agent`, with `preamble` and `actor_label` taken from the fixture's `job_settings`. `stance` stays the v1 value.

Generate the TOML once from the fixture with a throwaway script kept in the scratchpad, not committed. Use TOML tables (`[layer.row.fields]`, `[[layer.row.fields.steps]]`), not inline tables.

`0008`: `ALTER TABLE overlay_row DROP CONSTRAINT <kind check>` and re-add it with the four new kinds. Look up the constraint name in `0001` (it is the unnamed column CHECK, so `overlay_row_kind_check`).

- [ ] **Step 1: Write the failing tests:**
  - core `seed_v2_job_content_equals_fixture_sections`: compile seed v1+v2 rows (v1 `known()`) for the `golden.rs` `retail_customer_care()` selection. `job_content` as V1 vectors, sorted as in the fixture, equals the fixture's `context_spaces`, `procedures`, `connectors` and `knowledge_stores` by canonical JSON;
  - api `seeder.rs::v2_applies_after_v1`: `seed_version` holds 1 and 2, and a second start is a no-op.
- [ ] **Step 2:** Implement (seed file, seeder registration, migration).
- [ ] **Step 3:** Push. Run: CI job `api`. Expected: PASS, including `tests/rls.rs` (anti-drift is unchanged because overlay_row already has RLS).
- [ ] **Step 4: Commit** `feat(seed): v2 customer_care_agent job content from the care-bus golden fixture`.

### Task 4: Bundle builder and referential validation

**Files:**
- Modify: `crates/configurator-core/src/release.rs`, `crates/configurator-core/tests/release.rs`

**Interfaces — Produces:**
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize)] pub struct Violation { pub path: String, pub message: String }  // JSON pointer under /bundle
pub fn validate(b: &BundleV1) -> Vec<Violation>;                         // care-bus plan 11 Task 3 checks, minus the Plan type check
pub enum BuildError { Incomplete(Vec<Violation>), Invalid(Vec<Violation>) }
pub fn build_bundle(e: &EffectiveConfig) -> Result<BundleV1, BuildError>;
```
`validate` covers:
- the regexes and length bounds of bundle v1;
- unique natural keys per list, and `request_type` unique;
- scope/industry/department consistency;
- relation ends in entities;
- context-space `entities` ⊆ entities, and `procedure` names a bundle procedure;
- a procedure's `context_space` exists;
- at most one PUBLISHED per name and per context space;
- step numbers unique and ≥ 1;
- each field-mapping `attribute` is an `Entity.attribute` in the bundle;
- a knowledge-store `parent` exists, is a `group`, and there are no cycles;
- `plan` is an object with a `phases` array;
- every `timeout_ms` is in 100..=600000.

`build_bundle`:
- returns `Incomplete` when `e.job` is None, `job_content.context_spaces` is empty, or there is no PUBLISHED procedure;
- otherwise maps the gap table, applies the slice closure, and sorts lists by natural key;
- returns `Invalid` when `validate` reports anything.

- [ ] **Step 1: Write the failing tests:**
  - `seed_v2_builds_a_valid_bundle` (retail_customer_care → `Ok`, `validate` empty, 5 spaces / 6 procedures);
  - `slice_closure_pulls_in_referenced_entities` (`ReturnAuthorization` is in, though it is not in `ontology_slice`);
  - `relations_need_both_ends`;
  - `hidden_entities_are_dropped`;
  - `empty_job_content_is_refused` (seed v1 only → `Incomplete`);
  - `fixture_validates_clean` (`validate(fixture.bundle)` empty);
  - `violations_are_all_listed` (fixture with `context_spaces[0].procedure = "nope"` and `relations[0].object = "Ghost"` → both paths, as in care-bus's test);
  - `build_is_deterministic` (two builds → same `bundle_hash`).
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Push. Run: CI job `api`. Expected: PASS.
- [ ] **Step 4: Commit** `feat(core): release bundle builder from compile output, with care-bus's referential checks`.

### Task 5: care-bus client (SA token and the five calls)

**Files:**
- Create: `crates/configurator-api/src/carebus_client.rs`, `crates/configurator-api/tests/common/carebus_mock.rs`
- Modify: `src/config.rs` (`CarebusConfig`, all-or-none), `src/state.rs` (`pub carebus: Option<Arc<CarebusClient>>`), `src/lib.rs`, `tests/common/mod.rs`

**Interfaces — Produces:**
```rust
pub struct CarebusConfig { pub api_url: String, pub token_url: String, pub client_id: String, pub client_secret: String }   // Debug redacts secret
pub struct CarebusClient { /* ServiceToken + reqwest::Client + api_url */ }
pub enum CarebusError { Unavailable(String), Rejected { status: u16, code: Option<String>, body: serde_json::Value } }
pub struct CarebusResponse { pub status: u16, pub body: serde_json::Value }
impl CarebusClient {
  pub async fn put_tenant(&self, tenant: Uuid, name: &str, status: &str) -> Result<CarebusResponse, CarebusError>;
  pub async fn put_user(&self, tenant: Uuid, sub: &str, role: &str) -> Result<CarebusResponse, CarebusError>;
  pub async fn put_job(&self, tenant: Uuid, job: &str, label: &str) -> Result<CarebusResponse, CarebusError>;   // never sends settings
  pub async fn put_release(&self, tenant: Uuid, job: &str, release_id: &str, body: &serde_json::Value) -> Result<CarebusResponse, CarebusError>;
  pub async fn current_release(&self, tenant: Uuid, job: &str) -> Result<Option<serde_json::Value>, CarebusError>;  // 404 release → None
}
```
- Every call sends `Authorization: Bearer <ServiceToken::get()>`.
- `put_release` and `current_release` also send `X-Active-Tenant-Id: <tenant>`.
- Classification: 2xx → Ok. 4xx → `Rejected`, with `code` taken from `body.error.code`. Transport errors and 5xx → `Unavailable`. A 401 clears the token cache once and retries a single time.
- The mock is a local axum server serving a Keycloak `/token` and the five care-bus routes. It records every request (path, headers, body) and can be switched to fail with a given status.

- [ ] **Step 1: Write the failing tests** (in `tests/carebus_provision.rs`, client-level):
  - `client_sends_bearer_and_tenant_header`;
  - `put_job_never_sends_settings`;
  - `release_404_is_none`;
  - `rejected_carries_code` (mock 409 `{"error":{"code":"release_hash_mismatch"}}`);
  - `unavailable_on_5xx`;
  - `token_cached_across_calls` (1 token request for 3 calls);
  - `partial_carebus_config_fails_startup` (in `config.rs` unit tests, as for `LICENSES_ADMIN`).
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Push. Run: CI job `api`. Expected: PASS.
- [ ] **Step 4: Commit** `feat(api): care-bus client for the release-writer SA (tenant, user, job, release, read-back)`.

### Task 6: Targets and releases: schema, build route

**Files:**
- Create: `crates/configurator-api/migrations/0009_release.sql`, `src/db/releases.rs`, `src/routes/releases.rs`, `tests/releases.rs`
- Modify: `src/routes/compile.rs` (extract the compile body), `src/routes/mod.rs`, `src/db/mod.rs`, `tests/common/mod.rs` (`TENANT_TABLES` if listed)

**Schema (0009):**
```sql
ALTER TABLE tenant ADD COLUMN carebus_provisioned_at timestamptz;
CREATE TABLE target  (id uuid PK DEFAULT gen_random_uuid(), tenant uuid NOT NULL REFERENCES tenant(id), job text NOT NULL,
                      selection jsonb NOT NULL CHECK (jsonb_typeof(selection)='object'), created_by text NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
                      UNIQUE (tenant, job));
CREATE TABLE release (id uuid PK DEFAULT gen_random_uuid(), tenant uuid NOT NULL REFERENCES tenant(id), target uuid NOT NULL REFERENCES target(id),
                      release_id text NOT NULL CHECK (release_id ~ '^cfg-[0-9]{4}-[0-9]{2}-[0-9]{2}\.[0-9]+$'), hash text NOT NULL CHECK (hash ~ '^sha256:[0-9a-f]{64}$'),
                      effective_hash text NOT NULL, body jsonb NOT NULL, created_by text NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
                      UNIQUE (target, release_id));
CREATE TABLE push_step (id bigserial PK, tenant uuid NOT NULL REFERENCES tenant(id), release uuid NOT NULL REFERENCES release(id),
                        step text NOT NULL CHECK (step IN ('tenant','job','release')), request_hash text, status integer, ok boolean NOT NULL,
                        response jsonb, error_code text, pushed_by text NOT NULL, pushed_at timestamptz NOT NULL DEFAULT now());
```
RLS (`tenant = app_current_tenant()`) on all three, mirroring 0003's policies. Grants: `SELECT, INSERT` on `target` and `release`; `SELECT, INSERT` on `push_step` plus its sequence.

**Routes:**
- `GET /api/targets` (tenant_user);
- `POST /api/targets` `{job, selection:{industry?, departments[], profile_id?|profile?, dimensions{}}}` (tenant_admin). The selection's `job` and `tenant` are set by the server. The handler compiles once and refuses `compile_failed` with 422 and `invalid_selection` with 400. A second target for the same job is 409 `target_exists`;
- `POST /api/targets/{id}/releases` (tenant_admin): compile, `build_bundle`, then look at the latest release. With the same hash: 200 with the existing release. Otherwise insert under `SELECT … FROM target WHERE id=$1 FOR UPDATE` → 201. Errors: 422 `bundle_incomplete` / `bundle_invalid` with `errors: [Violation]`;
- `GET /api/targets/{id}/releases` (tenant_user, newest first, `Page`);
- `GET /api/releases/{id}` (tenant_user): release + body + push steps.

Extract from `routes/compile.rs`:
```rust
pub async fn compile_selection(tx: &mut PgConnection, tenant: Option<Uuid>, req: SelectionRequest) -> Result<EffectiveConfig, ApiError>;
```
`/api/compile` keeps its behaviour, and the existing `tests/compile.rs` passes unchanged.

- [ ] **Step 1: Write the failing tests** (`tests/releases.rs`, DB via testcontainers as in `tests/layers.rs`):
  - `create_target_and_build_release_1`: 201, `release_id` `cfg-<today>.1`, hash matches `^sha256:`, the stored `body.hash` equals `bundle_hash(body.bundle)`;
  - `rebuild_without_change_returns_same_release` (200, same id, 1 row);
  - `rebuild_after_tenant_patch_is_release_2` (tenant-layer patch of a context-space label → `.2`);
  - `build_without_job_content_is_422` (a target for `employee_care_agent` → `bundle_incomplete`);
  - `tenant_user_cannot_build` (403);
  - `cross_tenant_target_is_404`;
  - `rls_hides_other_tenant_releases` (raw SQL as `configurator_app`, as in `tests/rls.rs`);
  - anti-drift `every_tenant_table_has_rls` still passes.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Push. Run: CI job `api`. Expected: PASS.
- [ ] **Step 4: Commit** `feat(api): targets and versioned release build from compile output`.

### Task 7: Push a release; record the response and conflicts

**Files:** `src/routes/releases.rs`, `src/db/releases.rs`, `src/provision.rs` (create), `tests/releases.rs`

**Interfaces — Produces:**
```rust
// provision.rs
pub async fn ensure_tenant_and_job(st: &AppState, tenant: &Tenant, job: &str, job_label: &str) -> Result<Vec<StepOutcome>, CarebusError>;
pub struct StepOutcome { pub step: &'static str, pub status: u16, pub ok: bool, pub body: Value, pub error_code: Option<String> }
```
`POST /api/releases/{id}/push` (tenant_admin):
1. Return 503 `carebus_unconfigured` if there is no client. An archived tenant gets 403 `tenant_suspended` (existing guard).
2. `put_tenant(tenant.id, tenant.name, "active")`, then `put_job(tenant, target.job, job.label)`. On success set `tenant.carebus_provisioned_at` (owner pool).
3. `put_release(tenant, job, release_id, body)`.
4. Insert one `push_step` per call, including failures, each with `request_hash`: the release hash for the release step, NULL for the others. Audit `release.push` with `{release_id, hash, status, conflicts: n}` and no bodies.

Response 200 `{release, steps:[…], carebus: <care-bus report or null>}`. Errors:

| care-bus result | Our response |
|---|---|
| `Rejected` 409 `release_hash_mismatch` | 409 `carebus_release_conflict` |
| other `Rejected` | 502 `carebus_rejected` with `details {step, status, code}` |
| `Unavailable` | 502 `carebus_unavailable` |

The steps are recorded before the error returns. Conflicts are read from `push_step.response.conflicts` and are not copied elsewhere.

- [ ] **Step 1: Write the failing tests** (mock care-bus):
  - `push_provisions_then_puts_release`: mock order is tenant, job, release. The release PUT path is `/api/jobs/customer_care_agent/releases/cfg-…`, `X-Active-Tenant-Id` = tenant, and the body hash = the stored hash. There are 3 push_steps, all ok;
  - `push_sends_release_tenant_header` (super-admin acting in tenant B pushes B's release → header B);
  - `cross_tenant_release_is_404`;
  - `repush_records_noop` (mock answers 200 `applied:false` → 200, 6 push_steps, still 1 release row);
  - `conflicts_are_returned_and_stored` (mock 201 with 1 `base_removed` conflict → it is in the response and in `push_step.response`);
  - `hash_mismatch_is_409` (mock 409);
  - `carebus_down_is_502_and_recorded` (mock 503 on the release → 502, release step `ok=false`);
  - `unconfigured_is_503`;
  - `no_token_in_push_step_or_audit` (grep the stored JSON for the mock token string → absent).
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Push. Run: CI job `api`. Expected: PASS.
- [ ] **Step 4: Commit** `feat(api): push a release to care-bus (provision tenant+job, PUT release, record steps and conflicts)`.

### Task 8: Drift read-back

**Files:** `src/routes/releases.rs`, `tests/releases.rs`

`GET /api/targets/{id}/drift` (tenant_user) → 200:
```json
{"status": "in_sync|drift|never_pushed|carebus_has_no_release|carebus_unavailable",
 "carebus": {"release_id","hash","applied_at","conflicts":[...]} | null,
 "last_pushed": {"release_id","hash","pushed_at"} | null,
 "latest_built": {"release_id","hash"} | null,
 "config_changed": bool}
```
- `last_pushed` is the newest release whose `release` push_step is ok.
- `drift` means care-bus's `(release_id, hash)` ≠ `last_pushed`.
- `config_changed` means a fresh compile+build hash ≠ `latest_built.hash`. A build error counts as `true`.
- `carebus_unavailable` reports the upstream failure as a status, not as a 502.

- [ ] **Step 1: Write the failing tests:**
  - `in_sync_after_push`;
  - `drift_when_carebus_holds_other_release`;
  - `never_pushed`;
  - `carebus_404_is_no_release`;
  - `config_changed_after_tenant_patch`;
  - `open_conflicts_surface_in_drift`.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Push. Run: CI job `api`. Expected: PASS.
- [ ] **Step 4: Commit** `feat(api): drift read-back against care-bus releases/current`.

### Task 9: Tenant import with a fixed id, and provision-down on create, approve and import

**Files:** `src/routes/tenants.rs`, `src/routes/signups.rs` (replace the `// W4:` marker), `src/provision.rs`, `src/db/tenants.rs`, `tests/carebus_provision.rs`, `tests/license_invite.rs` (mock reuse)

**Interfaces:**
- `create_tenant` takes the id from `Origin`: `Origin::Manual | Signup(Uuid)` mint `Uuid::new_v4()`; the new `Origin::Import(Uuid)` uses that id. Audit `tenant.import` vs `tenant.create`.
- `POST /api/tenants/import` (super-admin) `{id, name, first_admin_sub? | first_admin_email?}`. The `id` must be bare and non-nil, checked with `parse_tenant_id`.
  - Absent locally: the normal create path with the fixed id. The licence is issued first, and a failure → 502 `licences_unavailable` with nothing created (licences 409 `tenant_owned_by_other_app` lands here too, with the upstream reason logged). Then 201 `{tenant, imported: true, carebus}`.
  - Present, same name, a licence row exists: 200 `{tenant, imported: false, carebus}`. Only the care-bus provision is re-run.
  - Present, same name, no licence row: issue the licence now (the repair path), audited `license.issue`, 200.
  - Present with another name, or archived: 409 `tenant_exists` / 403 `tenant_suspended`.
- `provision::tenant_created(st, &Tenant, first_admin_sub) -> CarebusProvision` (`{ok: bool, steps: [StepOutcome]}`) calls `put_tenant` (active), then `put_user(first_admin, "tenant_admin")`. Best effort: it sets `carebus_provisioned_at` on success, and audits `carebus.provision_failed` (metadata `{step, status, code}`) on failure. It is called after the commit by create, signup approve and import, and its result is returned as `carebus` in each response.
- `POST /api/tenants/{id}/carebus-provision` (super-admin): retries the same function, 200 with the outcome.

- [ ] **Step 1: Write the failing tests:**
  - `import_creates_tenant_with_fixed_id_and_licence` (licences mock is called with that id);
  - `import_is_idempotent` (second call 200 `imported:false`, still 1 licence);
  - `import_without_licence_row_repairs`;
  - `import_name_mismatch_is_409`;
  - `import_licence_refused_creates_nothing` (licences mock 409 → 502, no tenant row);
  - `import_requires_super_admin`;
  - `nil_or_braced_id_is_400`;
  - `create_provisions_down` (mock sees PUT tenant + PUT user tenant_admin; `carebus_provisioned_at` set);
  - `create_survives_carebus_down` (201, `carebus.ok=false`, audit `carebus.provision_failed`, column NULL);
  - `approve_provisions_down`;
  - `retry_route_provisions`.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Push. Run: CI job `api`. Expected: PASS, and the existing `tests/signups.rs` and `tests/license_invite.rs` PASS.
- [ ] **Step 4: Commit** `feat(api): super-admin tenant import with a fixed id; provision tenant and first admin down to care-bus`.

### Task 10: Web release page and import form

**Files:**
- Create: `web/src/routes/releases/+page.svelte`, `web/tests/unit/Releases.test.ts`
- Modify: `web/src/lib/api.ts` (types `Target`, `Release`, `PushStep`, `Conflict`, `Drift`), `web/src/routes/+layout.svelte` (nav link), `web/src/routes/tenants/+page.svelte` (super-admin "Import existing tenant": id, name, first admin; shows `carebus.ok` and a "Retry care-bus provision" button), `web/tests/unit/Tenants.test.ts`

The page has four parts:
- a target list, plus a create form with the job select and the selection fields used on the compile page;
- per target, the releases newest first, with Build and Push buttons (`tenant_admin` only);
- the push steps of the selected release, and its conflicts table (kind, object, key, tenant_row);
- a drift panel with a status badge and a "Check" button.

Errors show their `code` and `message`; `bundle_incomplete` and `bundle_invalid` list each violation path.

- [ ] **Step 1: Write the failing tests:**
  - `lists targets and releases`;
  - `build shows violations on 422`;
  - `push renders conflicts`;
  - `push button hidden for tenant_user`;
  - `drift badge shows status`;
  - Tenants: `import posts fixed id and shows carebus outcome`.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Run: `cd web && npm run ci:verify && npm test && npm run lint`. Expected: 0 errors and 0 warnings; all tests PASS.
- [ ] **Step 4: Commit** `feat(web): release page (targets, build, push, conflicts, drift) and tenant import`. Push; CI job `web` PASS.

### Task 11: gitops-production PR (one PR, in a worktree of gitops-production)

Before you start, check that no open gitops PR touches these files: `gh pr list -R nonagenticai/gitops-production --search "strata-configurator OR care-bus"`.

1. **Secret.** Add `CAREBUS_CLIENT_SECRET` to `clusters/strato/services/strata-configurator/secrets.sops.yaml` (`sops --set`). The value is the client secret of portal-realm client `strata-configurator-release-writer`: read it with kcadm (`get clients/<uuid>/client-secret`). If it was never set, regenerate it; the client is new and has no other consumer.
2. **Env** on the API container in `clusters/strato/services/strata-configurator/api.yaml`:
   - `CAREBUS_API_URL=http://carebus-api.care-bus.svc.cluster.local:8095`
   - `CAREBUS_TOKEN_URL=http://keycloak.keycloak.svc.cluster.local:8080/realms/portal/protocol/openid-connect/token` (keycloak egress and the cross-namespace AuthZ already exist for this pod)
   - `CAREBUS_CLIENT_ID=strata-configurator-release-writer`
   - `CAREBUS_CLIENT_SECRET` via `secretKeyRef strata-configurator-secrets/CAREBUS_CLIENT_SECRET`
3. **Egress.** Add NetworkPolicy `allow-api-egress-care-bus` to `clusters/strato/services/strata-configurator/networkpolicy.yaml`:
   - pod-scoped `app: strata-configurator-api`;
   - target: namespace `care-bus`, pod `app: carebus-api`, ports 8095 + 15008;
   - comment it in the house style: HBONE :15008 is load-bearing and already admitted by `default-restrict`; this rule names the dependency and admits the plaintext post-DNAT :8095. care-bus is waypoint-fronted, and the waypoint pod's :15008 is inside the existing `10.42.0.0/16:15008` arm.
4. **care-bus ingress.** In `clusters/strato/services/care-bus/networkpolicy.yaml` `default-restrict`, add an ingress arm from namespace `strata-configurator`, port 8095 (same shape as the enterprise-mcp arm).
5. **care-bus AuthZ: no change needed today.** No ALLOW policy selects `carebus-api` (`services-istio/care-bus/authz.yaml` only scopes demo-backends and redis), so the configurator's mesh identity is already admitted. The release-writer client role is the real gate. Record in the PR body: if the care-bus W3 deploy adds a `carebus-api` ALLOW, it must list `cluster.local/ns/strata-configurator/sa/default`. The configurator's own `strata-configurator-callers` is unchanged, because the configurator is the caller here.
6. **Validate locally:**
   - `kustomize build clusters/strato/services/strata-configurator` and `…/care-bus`;
   - gitops-production's local validators and tests (`tests/` runner as in its CLAUDE.md);
   - `sops -d` round-trip of the new key, with nothing printed.
   - Then open the PR, merge it, `flux reconcile`, and verify the env on the pod (`kubectl -n strata-configurator get deploy strata-configurator-api -o yaml | grep CAREBUS_`, values except the secret).

Not in this PR, and a prerequisite elsewhere:
- (a) the care-bus W3 deploy flips `carebus-api` `KEYCLOAK_AUDIENCE` to `care-bus` (care-bus's session);
- (b) the licences grant `tenant_app_grant(0195f48b-d000-7000-8000-000000000099, strata-configurator)` needs owner action, see Owner decisions.

### Task 12: Live verification (exit criteria), after W3 is live and both PRs are merged and rolled

The pods must run the merge-SHA images. Check `imageID` against the merge SHA build; Flux lags promotion by about 13 minutes.

- [ ] Mint a token with the release-writer SA (`curl` client_credentials from a pod in `strata-configurator`). Decode it without printing it: `aud` contains `care-bus`, `iss=https://auth.nonagentic.ai/realms/portal`, and `resource_access["care-bus"].roles` = `["release-writer"]`.
- [ ] Import: as super-admin, `POST /api/tenants/import` with `{id:"0195f48b-d000-7000-8000-000000000099", name:<care-bus's current name for it>, first_admin_sub:<fr-test user sub>}`. Expected:
  - 201, `carebus.ok=true`, a licence row present;
  - a re-run returns 200 `imported:false`;
  - care-bus `GET /api/tenants` (as owner super_admin) shows the tenant unchanged in status.
- [ ] Target + release 1:
  - `POST /api/targets {job:"customer_care_agent", selection:{industry:"retail", departments:["faq","logistics","order-support","payments"]}}` → 201;
  - `POST /api/targets/{id}/releases` → 201 `cfg-<date>.1`;
  - push → 200, `carebus.applied=true`, `previous_release_id` is `bootstrap-0009` (or care-bus's hand-pushed `2026-10-20.1`).
- [ ] **care-bus answers a return request with it:**
  - care-bus `GET /api/jobs/customer_care_agent/releases/current` shows `cfg-<date>.1` with our hash;
  - a console return request (care-bus web `/chat`, or its bus check from plan 13) resolves end-to-end via `retail:order-support:returns` and procedure `return@1.3.0`.
- [ ] **A tenant refinement survives release 2:**
  - as owner in care-bus (super_admin, header = the tenant), override the `Order` entity label;
  - in the configurator, add a tenant-layer patch of a context-space label, build `cfg-<date>.2`, and push;
  - care-bus still serves the overridden `Order` label;
  - drift = `in_sync`.
- [ ] **A removed base object shows as a conflict:**
  - in care-bus, override entity `Carrier`;
  - in the configurator, tombstone `Carrier` in the tenant layer, together with the context space and relation that reference it (otherwise build refuses with `bundle_invalid`, which is correct);
  - build and push release 3. The response conflicts include `{kind:"base_removed", object:"ontology_entity", key:{name:"Carrier"}}`, and the configurator's release page and drift panel show it.
- [ ] Negative: a request from a pod in a namespace other than strata-configurator or care-bus to `carebus-api:8095/api/jobs/x/releases/y` without a release-writer token → 401 or 403.
- [ ] Record evidence (commands + outputs, no secrets) in the PR body; then mark the PR ready, merge, and remove the worktree.

---

## Owner decisions needed (genuine)

1. **Licence for the adopted tenant.**
   - Facts: licences holds `0195f48b-…-099` under `owner_key=platform`, with grants only for commonpractice, fr, merqi and w2v. The configurator's import, which issues a licence as a normal create does, will get 409 `tenant_owned_by_other_app` until a `tenant_app_grant` for `strata-configurator` exists.
   - Who: the owner, or the W6 licences seeds, as the platform owner (`PUT /tenants/{id}/grants/strata-configurator`).
   - Recommendation: add the grant. Do not special-case import to skip the licence.
2. **Interim source of job content.**
   - Facts: context spaces, procedures, connector slots, knowledge stores and the preamble have no source in the phase-1 model. Per spec they come from the phase-3 funnel.
   - Plan: four new overlay kinds, seeded (seed v2) from care-bus's golden fixture. Tenants can patch or tombstone them, and phase 3 later generates rows of the same kinds.
   - Why: without them, release 1 deletes care-bus's working base and the return request stops resolving.
   - Confirm, or choose to defer W4's live push until phase 3.
3. **Rollback by re-push.** We rely on care-bus plan 11's open question 1 default (a): any known release with its original hash re-applies. If care-bus picks (b) or (c), the rollback UI changes. W4 ships no rollback button; re-push of an older release is possible via the API.

## Gaps recorded, not solved in W4

- Offboarding does not suspend the tenant in care-bus (a `put_tenant(…, "suspended")` hook is the follow-up).
- Pushes to strata_rag and strata_ingest (spec §6.3 steps 1–2) wait for W5's cell pattern.
- The approval lifecycle (use cases `approved` → release) arrives with phase 3. Until then a `tenant_admin` builds and pushes directly; both actions are audited with their actor.
- care-bus's `GET …/release-conflicts` (history incl. resolved) is JobMember-only, so the SA cannot read it. Drift shows only the open conflicts from `releases/current`.
