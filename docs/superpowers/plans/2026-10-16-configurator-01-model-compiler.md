# strata-configurator phase 1 — model, compiler, read-only UI — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A live, multi-tenant `strata-configurator` service that stores the layered standard model (ontology, axes, rules, dimensions, profiles, job definitions), seeds it from versioned files, compiles any selection into a hashed `EffectiveConfig` with provenance, and shows it in a read-only UI.

**Architecture:** Two Rust crates. `configurator-core` is pure (no IO): types, field validation, axis conditions, the overlay compiler, canonical hashing, the care-bus projection and the seed format. `configurator-api` is axum + sqlx over Postgres 18: tenancy/auth, CRUD over overlay rows, the seeder, and `/api/compile`, which loads rows and calls the core compiler. A SvelteKit `web/` app renders layers and compile results.

**Tech Stack:** Rust edition 2024, axum 0.8.9, sqlx 0.9.0, jsonwebtoken =9.3.1, testcontainers-modules 0.15.0 — every shared dependency pinned exactly as in care-bus `crates/carebus-api/Cargo.toml` @ 3b88445. New deps: `sha2`, `toml` (pin exact latest at implementation time, `cargo search` is allowed). SvelteKit with care-bus `web/package.json` versions.

**Spec:** `docs/superpowers/specs/2026-10-16-configurator-design.md` (rev 2). Overview: `2026-10-16-configurator-00-overview.md`.

## Global Constraints

- **No local cargo** (owner ruling 2026-10-16, hook-enforced): never run `cargo build/check/test/clippy/run`. `cargo fmt`, `cargo metadata`, `cargo search`, `cargo update -p` are allowed. Rust is verified by pushing to the open PR and reading the CI job log.
- **One PR for this whole plan.** Task 1 opens it (draft). Every later task is commits pushed to the same branch. A failing CI run is fixed with another commit, never a second PR.
- Each task's commit message names the task. A task is done only when CI on its head commit is green **and** the job log lists that task's test names as `ok`.
- API port **8097** (re-check free in `gitops-production/clusters/strato` and locally before Task 13). Never 8000. Web port 3000 in-container.
- Routes unversioned `/api/<resource>`. Lists: `Paged{items,next}`, `limit` default 50, max 500, cursor `before`. Errors: `{"error":{"code","message"}}`.
- Postgres 18 only; tests use ONE container per test binary from `registry.internal:5000/library/postgres:18-alpine` and a fresh DB per test — copy care-bus `crates/carebus-api/tests/common/mod.rs`, do not reinvent it.
- Migrations only, `sqlx::migrate!("./migrations")` at startup. No `create_all`.
- No shared library with care-bus/rag/ingest. Copy code (auth verifier, test harness), never depend on it.
- Tenant ids: bare non-nil uuid. Nil or malformed → 400, never 500.
- No LLM calls anywhere in phase 1.
- Super-admin = Keycloak portal realm role `config-super-admin`. Tenant roles `viewer < editor < approver < admin` live in the configurator DB.

## Review Focus

1. **Cross-tenant access by id.** Tenant A requests a layer/row id that belongs to tenant B → 404 (not 403, not B's data). Test in Task 9.
2. **Concurrent edits of one object.** Two `PUT`s on the same `(layer, kind, key)` with the same `expected_rev` → exactly one 200, the other 409 `revision_conflict`; never 500 from the partial unique index. Test in Task 9.
3. **Unknown selection keys.** Compile with `industry: "retial"` or an unknown department/job/axis value → 400 `invalid_selection` naming the key; never a smaller compile that looks valid. Test in Task 4 (core) and Task 11 (HTTP).
4. **Tenant patch of an object a later seed tombstones.** A tenant `patch` on `Order.status` after a platform tombstone of `Order` → compile 422 with `patch_missing` on the tenant row id; never a panic or a silent drop. Test in Task 4.
5. **Key casing/shape.** `order` vs `Order`, `Order.Status`, `Order|paid by|Payment` → 400 `invalid_key` on write, so two spellings of one object can never coexist. Test in Task 2.

---

## File structure

```
Cargo.toml                         workspace: crates/configurator-core, crates/configurator-api
Dockerfile.rust                    care-bus pattern, PACKAGE arg
.github/workflows/ci.yml           rust-service.yml@main (api, runs workspace tests) + ts-service.yml@main (web)
seed/v1/*.toml                     axes, dimensions, base, retail, electricity, jobs
crates/configurator-core/src/
  lib.rs
  layer.rs        Scope, LayerKey, validation
  overlay.rs      Kind, Op, OverlayRow, key + field validation per kind
  axis.rs         Axis, Profile, Condition (parse/validate/eval)
  compile.rs      Selection, AxisRule, compile(), CompileError, EffectiveConfig
  canonical.rs    canonical JSON + sha256
  carebus.rs      projection to care-bus ontology tables
  seed.rs         SeedBundle, parse(), render()
crates/configurator-core/tests/
  fixtures/carebus_0002_ontology.json
  golden.rs
crates/configurator-api/
  migrations/0001_core.sql
  src/{main,lib,config,state,error,page,audit,auth,tenancy,seeder}.rs
  src/db/{mod,layers,rows,axes,dimensions,profiles,tenants}.rs
  src/routes/{mod,health,me,tenants,layers,axes,dimensions,profiles,compile,platform}.rs
  tests/common/{mod.rs,keys.rs}
  tests/{health,seeder,auth,layers,catalog,compile}.rs
web/                               SvelteKit, read-only
```

---

### Task 1: Repository, workspace, CI, health

**Files:**
- Create: `Cargo.toml`, `Cargo.lock` (via `cargo generate-lockfile`, allowed), `.gitignore`, `README.md` (5 lines: what, port, spec link), `Dockerfile.rust`, `.dockerignore`, `.github/workflows/ci.yml`
- Create: `crates/configurator-core/{Cargo.toml,src/lib.rs}`, `crates/configurator-api/{Cargo.toml,src/main.rs,src/lib.rs,src/config.rs,src/state.rs,src/error.rs,src/routes/mod.rs,src/routes/health.rs}`
- Test: `crates/configurator-api/tests/health.rs`

**Interfaces:**
- Produces: `configurator_api::config::Config::from_env() -> anyhow::Result<Config>` (fields: `database_url`, `port: u16` default 8097, `keycloak_issuer`, `keycloak_audience` default `strata-configurator`, `keycloak_jwks_url`, `super_admin_role` default `config-super-admin`); `configurator_api::routes::router(state: AppState) -> axum::Router`; `configurator_api::error::ApiError` (`IntoResponse` → `{"error":{"code","message"}}`).

- [ ] **Step 1:** Create GitHub repo `nonagenticai/strata_configurator` (private, default branch `main`, delete-branch-on-merge on). `git init` in the existing `strata_configurator/` dir (it holds only `docs/`), commit the docs on `main`, push, then branch `phase-1-model-compiler`.
- [ ] **Step 2: Write the failing test** `health_is_unauthenticated`: `GET /health` → 200 `{"status":"ok"}` with no `Authorization` header. (`/ready` comes in Task 7.)
- [ ] **Step 3:** Implement the workspace, both crates, `Config`, `ApiError`, `router` with `/health` only, `main.rs` binding `0.0.0.0:{port}`.
- [ ] **Step 4:** `ci.yml`: one `api` job calling `nonagenticai/gitops-production/.github/workflows/rust-service.yml@main` with `image_name: strata-configurator-api`, `cargo_package: configurator-api`, `test_command: 'cargo test --workspace --locked'`, `prepull_images: registry.internal:5000/library/postgres:18-alpine`, `dockerfile_path: Dockerfile.rust`, `buildx_extra_args: "--build-arg PACKAGE=configurator-api"`, `build_smoke` as care-bus api's (binary `configurator-api`). No deploy inputs until Task 13.
- [ ] **Step 5:** Commit, push, open draft PR. Expected: CI `api` green, log shows `health_is_unauthenticated ... ok`.

---

### Task 2: Core types — layers, overlay rows, key and field validation

**Files:**
- Create: `crates/configurator-core/src/layer.rs`, `crates/configurator-core/src/overlay.rs`
- Test: unit tests in each file (`#[cfg(test)]`)

**Interfaces:**
- Produces:
  - `enum Scope { Base, Profile, Industry, IndustryDept, Job, Tenant }` (serde `snake_case`), `Scope::rank(&self) -> u8` in that order.
  - `struct LayerKey { scope, industry: Option<String>, department: Option<String>, job: Option<String>, rule: Option<String>, dimension: Option<(String, String)>, tenant: Option<Uuid> }`; `LayerKey::validate(&self) -> Result<(), LayerError>` enforcing spec §4.1's required-keys table exactly (any extra key is an error).
  - `enum Kind { Entity, Attribute, Relation, Job }`, `enum Op { Add, Patch, Tombstone }`.
  - `struct OverlayRow { id: Uuid, layer: LayerKey, kind: Kind, key: String, op: Op, fields: serde_json::Map<String, Value> }`.
  - `fn validate_key(kind: Kind, key: &str) -> Result<(), FieldError>` and `fn validate_fields(kind: Kind, op: Op, fields: &Map) -> Result<(), Vec<FieldError>>`.
  - `struct FieldError { code: &'static str, field: Option<String>, message: String }` — codes `invalid_key`, `unknown_field`, `missing_field`, `invalid_value`, `fields_on_tombstone`, `empty_patch`.

Key shapes: entity `^[A-Z][A-Za-z0-9]{0,63}$`; attribute `<Entity>.<name>` with name `^[a-z][a-z0-9_]{0,63}$`; relation `<Entity>|<predicate>|<Entity>` with predicate `^[a-z][A-Za-z0-9]{0,63}$`; job `^[a-z][a-z0-9_]{0,63}$`.
Field schemas: entity `label` (string, required on add), `synonyms` (string[]), `hidden` (bool). Attribute `data_type` (required on add; one of spec §4.2's 8), `pii` (bool), `synonyms`. Relation `cardinality` (required on add; `1:1|1:n|n:1|n:m`). Job `label`, `actor` (both required on add), `stance`, `ontology_slice` (entity names[]), `channels` (string[]), `escalation` (object), `use_cases`, `sops`, `tools`, `stores` (string[]). Tombstone: no fields. Patch: ≥1 known field.

- [ ] **Step 1: Write failing tests:** `layer_tenant_requires_tenant_only`, `layer_industry_dept_requires_both`, `layer_extra_key_rejected` (base + industry → error); `key_shapes` table test: accepts `Order`, `Order.order_id`, `Order|paidBy|Payment`, `customer_care_agent`; rejects `order`, `Order.Status`, `Order|paid by|Payment`, `Order.`, `` (Review Focus 5); `add_entity_needs_label`, `patch_unknown_field_rejected`, `tombstone_with_fields_rejected`, `empty_patch_rejected`, `attribute_data_type_enum`.
- [ ] **Step 2:** Implement. Use `std` only for key checks (no regex dep).
- [ ] **Step 3:** Commit, push. Expected: CI green, the 9 test names `ok`.

---

### Task 3: Axis conditions and profiles

**Files:**
- Create: `crates/configurator-core/src/axis.rs`

**Interfaces:**
- Produces:
  - `enum Cardinality { One, Many }`; `struct AxisValue { key: String /* full dotted path */, parent: Option<String> }`; `struct Axis { key: String, cardinality: Cardinality, values: Vec<AxisValue> }`.
  - `struct Profile(pub BTreeMap<String, BTreeSet<String>>)`; `Profile::validate(&self, axes: &[Axis]) -> Result<(), AxisError>` (unknown axis/value; >1 value on a `One` axis).
  - `enum Condition { Eq { axis, value }, In { axis, values }, And(Vec<Condition>), Or(Vec<Condition>), Not(Box<Condition>) }`, serde JSON form `{"eq":["order_type","continuous"]}`, `{"in":["payment_type",["balancing"]]}`, `{"and":[…]}`, `{"or":[…]}`, `{"not":{…}}`.
  - `Condition::validate(&self, axes: &[Axis]) -> Result<(), AxisError>`; `Condition::eval(&self, profile: &Profile) -> bool`.
  - `fn validate_axis(axis: &Axis) -> Result<(), AxisError>`: parent must exist in the same axis, key must equal `parent + "." + leaf`, no cycles.

- [ ] **Step 1: Write failing tests:** `eq_matches_descendant` (`payment_method = digital_wallet` holds for profile `digital_wallet.mobile`; `digital` does not match `digital_wallet`), `in_any`, `and_or_not`, `unknown_axis_rejected_on_validate`, `one_axis_two_values_rejected`, `json_round_trip`, `axis_parent_must_prefix_key`.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Commit, push. Expected: CI green, 7 names `ok`.

---

### Task 4: Compiler

**Files:**
- Create: `crates/configurator-core/src/compile.rs`

**Interfaces:**
- Consumes: Task 2 types, Task 3 `Profile`, `Condition`.
- Produces:
  - `struct Selection { industry: Option<String>, departments: BTreeSet<String>, job: Option<String>, profile: Profile, dimensions: BTreeMap<String, String>, tenant: Option<Uuid> }`.
  - `struct AxisRule { key: String, priority: i32, condition: Condition }`.
  - `struct Known { industries: BTreeSet<String>, departments: BTreeSet<String>, jobs: BTreeSet<String>, dimensions: BTreeMap<String, BTreeSet<String>>, axes: Vec<Axis> }` — what exists, for selection validation (filled by the API from the DB).
  - `fn compile(sel: &Selection, known: &Known, rules: &[AxisRule], rows: &[OverlayRow]) -> Result<EffectiveConfig, Vec<CompileError>>`.
  - `struct CompileError { code: ErrorCode, row_id: Option<Uuid>, kind: Option<Kind>, key: Option<String>, message: String }`; `enum ErrorCode { InvalidSelection, DuplicateAdd, PatchMissing, TombstoneMissing, DanglingReference, InvalidRow }` (serde `snake_case`).
  - `struct EffectiveConfig { selection: Selection, entities: BTreeMap<String, Entity>, attributes: BTreeMap<String, Attribute>, relations: BTreeMap<String, Relation>, job: Option<Job>, provenance: BTreeMap<String /* "entity:Order" */, Vec<Origin>>, warnings: Vec<Warning>, hash: String }`; `struct Origin { row_id: Uuid, layer: LayerKey, op: Op }`; typed `Entity { label, synonyms, hidden }`, `Attribute { entity, name, data_type, pii, synonyms }`, `Relation { subject, predicate, object, cardinality }`, `Job { key, label, actor, stance, ontology_slice, channels, escalation, use_cases, sops, tools, stores }`.
  - `struct Warning { code: &'static str /* "slice_entity_missing" */, key: String }`.
  - `fn layer_order(sel: &Selection, rules: &[AxisRule]) -> Vec<LayerKey>` (public for the API's row loader).

Algorithm (spec §5): validate selection against `known` first (every error, then stop). Build the ordered layer list: base; profile layers of rules whose condition holds, by `(priority, key)`; industry; one industry_dept per selected department in key order; job (unqualified), then job layers qualified by a dimension `k=v` that the selection holds, by `(k, v)`; tenant. Apply each layer's rows in kind order entity → attribute → relation → job. Tombstoning an entity removes its attributes and every relation touching it, and records them so a later attribute/relation `add` on that entity is `dangling_reference`, while a later `patch`/`tombstone` is `patch_missing`/`tombstone_missing`. After all layers: any attribute/relation whose entity is absent is `dangling_reference`; job `ontology_slice` names that are absent become warnings. Hash is filled by Task 5; leave `hash: String::new()` here.

- [ ] **Step 1: Write failing tests** (hand-built rows, no seeds):
  - `layer_order_full_selection` asserts the exact `Vec<LayerKey>` for a selection with 2 rules (one true, one false), industry, departments `{b, a}` (→ a before b), job, dimension `channel=voice`, tenant.
  - `patch_replaces_field_whole` (synonyms `[x,y]` patched to `[z]` → `[z]`).
  - `add_existing_is_duplicate_add`; `two_departments_adding_same_entity_is_duplicate_add`.
  - `tombstone_cascades_attributes_and_relations`.
  - `tenant_patch_after_platform_tombstone_is_patch_missing` — error carries the tenant row's id (Review Focus 4).
  - `errors_are_collected` (3 independent bad rows → 3 errors).
  - `unknown_selection_keys_rejected` — unknown industry, department, job, dimension value, axis value: each yields `invalid_selection` naming the key (Review Focus 3).
  - `rule_false_layer_not_applied`; `provenance_lists_add_then_patch`.
  - `slice_entity_missing_is_warning_not_error`.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Commit, push. Expected: CI green, 12 names `ok`.

---

### Task 5: Canonical hash and care-bus projection

**Files:**
- Create: `crates/configurator-core/src/canonical.rs`, `crates/configurator-core/src/carebus.rs`
- Modify: `crates/configurator-core/src/compile.rs` (set `hash` at the end of `compile`)

**Interfaces:**
- Produces:
  - `fn canonical_json(v: &serde_json::Value) -> String` — object keys sorted recursively, no whitespace. `fn sha256_hex(s: &str) -> String`.
  - `EffectiveConfig.hash` = `sha256_hex(canonical_json({selection, entities, attributes, relations, job}))` — provenance and warnings excluded, so a revision that changes nothing does not change the hash.
  - `struct CarebusOntology { entities: Vec<CbEntity>, attributes: Vec<CbAttribute>, relations: Vec<CbRelation> }` with field names equal to care-bus columns (`name, scope, industry, department, label, synonyms, hidden` / `entity, name, data_type, pii, synonyms` / `subject, predicate, object, cardinality`), each `Vec` sorted by its primary key; `fn project_carebus(e: &EffectiveConfig) -> CarebusOntology` with the scope mapping of spec §5.4 (uses the first `Origin` with `op = Add`).

- [ ] **Step 1: Write failing tests:** `hash_is_stable_under_row_permutation` (same rows in 10 rotations of order → one hash), `hash_ignores_provenance` (same content via a different row id → same hash), `hash_changes_with_content`, `canonical_sorts_nested_keys`, `projection_scope_mapping` (one entity per scope incl. tenant add with one vs two departments).
- [ ] **Step 2:** Implement (`sha2` dep, exact pin).
- [ ] **Step 3:** Commit, push. Expected: CI green, 5 names `ok`.

---

### Task 6: Seed format, seed v1 content, golden test

**Files:**
- Create: `crates/configurator-core/src/seed.rs`
- Create: `seed/v1/axes.toml`, `seed/v1/dimensions.toml`, `seed/v1/base.toml`, `seed/v1/retail.toml`, `seed/v1/electricity.toml`, `seed/v1/jobs.toml`
- Create: `crates/configurator-core/tests/fixtures/carebus_0002_ontology.json`, `crates/configurator-core/tests/golden.rs`

**Interfaces:**
- Produces:
  - `struct SeedBundle { version: u32, axes: Vec<Axis>, dimensions: Vec<Dimension>, rules: Vec<SeedRule>, profiles: Vec<SeedProfile>, layers: Vec<SeedLayer> }`; `struct Dimension { key, label, values: Vec<(String key, String label)> }`; `struct SeedRule { rule: AxisRule, rows: Vec<RowSpec> }`; `struct SeedProfile { name: String, values: Profile }`; `struct SeedLayer { layer: LayerKey, rows: Vec<RowSpec> }`; `struct RowSpec { kind, key, op, fields }`.
  - `fn parse(version: u32, files: &[(&str /* name */, &str /* toml */)]) -> Result<SeedBundle, SeedError>` — validates every key/field (Task 2), axis (Task 3) and rule condition; errors name file + key.
  - `fn render(b: &SeedBundle) -> Vec<(String, String)>` — inverse of `parse`, one file per section as in `seed/v1/`.
  - `fn to_rows(b: &SeedBundle) -> (Vec<AxisRule>, Vec<OverlayRow>)` with row ids `Uuid::new_v5(NAMESPACE_OID, "v{version}/{layer}/{kind}/{key}")` so seed rows are reproducible.

TOML shape (one example; all sections follow it):
```toml
[[layer]]
scope = "industry"
industry = "retail"
  [[layer.row]]
  kind = "entity"
  key = "Shipment"
  op = "add"
  fields = { label = "Shipment", synonyms = ["parcel", "package", "zending"] }
```

Seed v1 content (transcribe from the owner's notes `~/Documents/01Quant/objects.txt`, `object_bill.txt`, `object_contract.txt`; commonpractice seeds are a checklist only):
- `axes.toml`: the 9 axes of spec §4.3 with V§3.1 values (customer_type 6 values; product_type service, physical, virtual, access_lease, freemium; offering_type incl. `availability.*`, `combinatory.*`, `consumption.usage_based`; order_type discrete/recurring/continuous; renewal_type none, customer_initiated, provider.explicit, provider.implicit; delivery_type; delivery_method; payment_type singleton, recurring.fixed, recurring.variable, balancing; payment_method cash, card, bank_transfer, digital_wallet.*). Cardinality: customer_type, product_type, payment_method `many`; others `one`.
- `dimensions.toml`: `department` (values `order-support`, `logistics`, `payments`, `faq`), `channel` (`chat`, `voice`, `email`), `region` (`eu`, `us`).
- `base.toml`: base layer per spec §4.2 — Customer, Communication, Policy exactly as care-bus 0002; Order, Product, Payment with care-bus 0002's attributes except `Order.shipment_id`, and generic synonyms only (Order `[purchase]`, Product `[item, article]`, Payment `[charge, transaction]`); plus Party, ProductOffering, Renewal, Delivery, PaymentMethod, Contract (+ ContractParty, Term, Obligation, ContractLineItem, PaymentTerm, Amendment), Bill (+ BillLineItem, PaymentRecord, Adjustment) with attributes from the notes; base relations `Customer places Order`, `Order contains Product`, `Order paidBy Payment`, `Customer receives Communication`.
- `retail.toml`: industry `retail` adds Shipment, `Order.shipment_id`, relation `Order shippedVia Shipment`, and patches Order/Product/Payment `synonyms` to care-bus 0002's full lists; industry_dept layers: order-support (ReturnAuthorization, WarrantyClaim, `ReturnAuthorization for Order`, `WarrantyClaim covers Product`, `Policy governs ReturnAuthorization`), logistics (Carrier, `Shipment carriedBy Carrier`), payments (Refund, `Refund reverses Payment`). A relation lives in the layer that adds its later entity.
- `electricity.toml`: profile template `electricity` = `{order_type: [continuous], renewal_type: [provider.implicit], product_type: [access_lease], payment_type: [balancing]}`; rule `metered_supply` (priority 100, `and(eq order_type continuous, eq payment_type balancing)`) adding MeterPoint, MeterReading, `Bill.meter_reading`, `Order meteredBy MeterPoint`; rule `implicit_renewal` (`eq renewal_type provider.implicit`) adding `Renewal.notice_period_days`.
- `jobs.toml`: base jobs `customer_care_agent` (actor `customer`, stance "aftersales is presales: every interaction is a brand moment", slice Customer, Order, Product, Payment, Communication, Policy) and `employee_care_agent` (actor `employee`, slice Party, Communication, Policy).

- [ ] **Step 1:** Write the fixture: every row of care-bus `crates/carebus-api/migrations/0002_seed.sql` @ 3b88445 (11 entities, 37 attributes, 10 relations) as `CarebusOntology` JSON, with Order/Product/Payment changed to `scope: "generic", industry: null` — and a top-level `"_deltas"` array naming exactly those three moves.
- [ ] **Step 2: Write failing tests** in `golden.rs`:
  - `golden_retail_customer_care_contains_carebus_0002`: parse `seed/v1/*` via `include_str!`, compile selection `{industry: retail, departments: {faq, logistics, order-support, payments}, job: customer_care_agent}`, project; every fixture row is present and field-equal; extra rows allowed.
  - `electricity_profile_adds_metering_and_renewal`: compile `{profile: electricity, job: customer_care_agent}` → entities include Contract, Bill, MeterPoint, Renewal; attribute `Bill.meter_reading` present; no errors.
  - `seed_render_parse_round_trip`: `parse(render(parse(v1))) == parse(v1)`.
  - `seed_v1_has_no_warnings`: both compiles above return zero warnings.
- [ ] **Step 3:** Implement `seed.rs` (`toml` dep, exact pin) and write the seed files.
- [ ] **Step 4:** Commit, push. Expected: CI green, 4 names `ok`. Record the golden compile's `hash` from the test output (`println!` it) — Task 13 compares the live hash to it.

---

### Task 7: Schema, test harness, seeder, `/ready`

**Files:**
- Create: `crates/configurator-api/migrations/0001_core.sql`, `crates/configurator-api/src/seeder.rs`, `crates/configurator-api/src/db/mod.rs`
- Create: `crates/configurator-api/tests/common/mod.rs` (copy of care-bus harness, adapted to `configurator_api`), `crates/configurator-api/tests/seeder.rs`
- Modify: `src/state.rs`, `src/main.rs`, `src/routes/health.rs`

**Interfaces:**
- Consumes: `seed::{parse, to_rows}`.
- Produces:
  - Tables exactly as spec §10.4 phase-1 list: `tenant(id uuid pk check (id <> '00000000-0000-0000-0000-000000000000'), name, created_at)`, `member(tenant, sub, role member_role, pk(tenant, sub))`, `layer(id, scope, industry, department, job, rule, dimension_key, dimension_value, tenant, UNIQUE NULLS NOT DISTINCT over all key columns, CHECKs mirroring LayerKey::validate)`, `overlay_row(id, layer_id, kind, key, op, fields jsonb, rev int, is_current bool, author, created_at; unique (layer_id, kind, key) WHERE is_current; unique (layer_id, kind, key, rev))`, `axis`, `axis_value(axis, key, label, parent, description)`, `axis_rule(key pk, priority, condition jsonb, layer_id unique)`, `dimension`, `dimension_value(dimension, key, label, tenant, UNIQUE NULLS NOT DISTINCT (dimension, key, tenant))`, `profile(id, tenant null, name, values jsonb, UNIQUE NULLS NOT DISTINCT (tenant, name))`, `seed_version(version pk, sha256, applied_at)`, `audit_event(id bigserial, at, actor, tenant, action, object, before_hash, after_hash)`.
  - `enum SeedStatus { Ready, Conflict { version: u32, key: String } }` in `AppState` (`Arc<RwLock<…>>`).
  - `seeder::apply(pool: &PgPool, bundles: &[SeedBundle]) -> Result<Vec<u32> /* applied */, SeedError>` — per version: skip if in `seed_version`; else one transaction inserting axes, dimensions, rules (+ their profile layers), profiles, layers, rows (`author = "seed:v{N}"`), then the `seed_version` row; any unique violation on a current row → rollback, `SeedError::Conflict { version, key }`.
  - `GET /ready` → 200 when DB pings and `SeedStatus::Ready`; 503 `{"error":{"code":"seed_conflict",…}}` naming the key; 503 `db_unavailable` when the ping fails.

- [ ] **Step 1: Write failing tests:** `migrations_apply_on_empty_db`; `seed_v1_applies_once` (second `apply` returns `[]`, row counts unchanged); `seed_conflict_rolls_back_and_names_key` (pre-insert a base `entity:Order` row, apply v1 → `Conflict{key:"entity:Order"}`, zero rows from v1 present, `seed_version` empty); `ready_reports_seed_conflict` (state with `Conflict` → 503 `seed_conflict`).
- [ ] **Step 2:** Implement. `main.rs`: migrate, apply embedded seed bundles (`include_str!` of `seed/v*/`), set `SeedStatus`, serve even on conflict (so `/ready` can report it).
- [ ] **Step 3:** Commit, push. Expected: CI green, 4 names `ok`.

---

### Task 8: Auth, tenancy, `/api/me`, audit

**Files:**
- Create: `src/auth.rs` (copy care-bus `crates/carebus-api/src/auth.rs` verifier: JWKS cache + static-key mode; drop service tokens), `src/tenancy.rs`, `src/audit.rs`, `src/db/tenants.rs`, `src/routes/{me,tenants}.rs`
- Create: `tests/common/keys.rs` (RSA test key + `token(sub, roles) -> String`, as care-bus tests do), `tests/auth.rs`

**Interfaces:**
- Produces:
  - `struct Caller { sub: String, actor: String, super_admin: bool }` — axum extractor; missing/invalid token → 401.
  - `enum Role { Viewer, Editor, Approver, Admin }` (`Ord` in that order, serde `snake_case`).
  - `struct TenantCtx { caller: Caller, tenant: Uuid, role: Role }` — extractor reading `X-Tenant-Id` (spec §10.2: exactly one header, bare non-nil uuid; missing → 400 `tenant_required`; malformed/nil/duplicated → 400 `invalid_tenant`; not a member → 403 `not_a_member`; super-admin → `Role::Admin` in any existing tenant; unknown tenant → 404). `TenantCtx::require(&self, min: Role) -> Result<(), ApiError>` (403 `forbidden`).
  - `fn require_super_admin(c: &Caller) -> Result<(), ApiError>`.
  - `audit::record(tx, actor: &str, tenant: Option<Uuid>, action: &str, object: &str, before: Option<&Value>, after: Option<&Value>)` storing canonical-JSON sha256 hashes.
  - Routes: `GET /api/me` → `{sub, super_admin, memberships:[{tenant, name, role}]}`; `POST /api/tenants {name, first_admin_sub}` (super-admin) → 201 tenant; `GET /api/tenants` (super-admin: all; others: own); `GET|POST /api/tenants/{id}/members`, `DELETE /api/tenants/{id}/members/{sub}` (admin of that tenant or super-admin; removing the last admin → 409 `last_admin`).

- [ ] **Step 1: Write failing tests:** `no_token_401`; `bad_signature_401`; `me_lists_memberships`; `only_super_admin_creates_tenants`; `tenant_header_errors` (table: missing → 400 `tenant_required`; `not-a-uuid`, nil uuid, two headers, `Bearer x` → 400 `invalid_tenant`); `non_member_403`; `super_admin_acts_in_any_tenant`; `last_admin_cannot_be_removed`; `writes_are_audited` (tenant create → one `audit_event` with actor).
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Commit, push. Expected: CI green, 9 names `ok`.

---

### Task 9: Layers and overlay rows API

**Files:**
- Create: `src/db/{layers,rows}.rs`, `src/routes/layers.rs`, `src/page.rs` (Paged cursor helper)
- Test: `tests/layers.rs`

**Interfaces:**
- Consumes: `LayerKey::validate`, `validate_key`, `validate_fields`, `TenantCtx`, `Caller`, `audit::record`.
- Produces:
  - `GET /api/layers?scope=&industry=&job=` (Paged; platform layers + the caller tenant's own layer), `POST /api/layers` (body = `LayerKey` minus `tenant`; scope `tenant` uses `TenantCtx.tenant`, needs `editor`; any other scope needs super-admin; duplicate → 409 `layer_exists`).
  - `GET /api/layers/{id}/rows?kind=` (current rows, Paged), `POST /api/layers/{id}/rows {kind,key,op,fields}` → 201 with `rev: 1`; existing current row → 409 `row_exists`.
  - `PUT /api/layers/{id}/rows/{kind}/{key} {op, fields, expected_rev}` → new revision; `expected_rev` ≠ current → 409 `revision_conflict` (Review Focus 2: also when the partial unique index fires — map that DB error to 409).
  - `DELETE /api/layers/{id}/rows/{kind}/{key}?expected_rev=` → retire (row stays as history).
  - `GET …/{kind}/{key}/revisions` → all revisions, newest first.
  - `db::rows::load_for_layers(pool, layers: &[LayerKey]) -> Vec<OverlayRow>` (current rows only) — used by Task 11.
  - A layer of another tenant is **404** on every route (Review Focus 1).

- [ ] **Step 1: Write failing tests:** `editor_writes_own_tenant_layer`; `viewer_cannot_write_403`; `tenant_cannot_write_platform_layer_403`; `super_admin_writes_base`; `invalid_key_400` (`order`); `invalid_fields_400_lists_all`; `revision_history_kept`; `concurrent_put_one_wins` (two concurrent `PUT`s, same `expected_rev` → statuses `{200, 409}`); `other_tenants_layer_is_404` (GET layer, GET rows, POST row, PUT row); `retire_then_re_add`.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Commit, push. Expected: CI green, 10 names `ok`.

---

### Task 10: Axes, rules, dimensions, profiles API

**Files:**
- Create: `src/db/{axes,dimensions,profiles}.rs`, `src/routes/{axes,dimensions,profiles}.rs`
- Test: `tests/catalog.rs`

**Interfaces:**
- Consumes: `validate_axis`, `Condition::validate`, `Profile::validate`.
- Produces:
  - `GET|POST /api/axes`, `POST /api/axes/{key}/values` (super-admin writes; parent must exist; 400 `invalid_axis`).
  - `GET|POST /api/axis-rules {key, priority, condition}` (super-admin) — creates the rule and its `profile` layer in one transaction; response includes `layer_id`. Invalid condition → 400 `invalid_condition` naming the unknown axis/value.
  - `GET|POST /api/dimensions` (super-admin), `GET|POST /api/dimensions/{key}/values` — a super-admin write without `X-Tenant-Id` creates a standard value; an `editor` write creates a value for its tenant; GET returns standard + caller-tenant values only.
  - `GET|POST /api/profiles`, `GET|PUT /api/profiles/{id}` — tenant profiles need `editor`; templates (`tenant` null) need super-admin; values validated against axes.

- [ ] **Step 1: Write failing tests:** `axis_value_parent_must_exist`; `rule_with_unknown_value_rejected`; `rule_creates_profile_layer`; `tenant_dimension_value_invisible_to_other_tenant`; `profile_one_axis_two_values_400`; `seeded_electricity_template_listed`.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Commit, push. Expected: CI green, 6 names `ok`.

---

### Task 11: Compile endpoint and platform export

**Files:**
- Create: `src/routes/{compile,platform}.rs`
- Test: `tests/compile.rs`

**Interfaces:**
- Consumes: `compile`, `layer_order`, `project_carebus`, `db::rows::load_for_layers`, `seed::render`.
- Produces:
  - `POST /api/compile` body `{industry?, departments?, job?, profile_id? | profile?, dimensions?}` (viewer; `tenant` = `TenantCtx.tenant`; super-admin may omit `X-Tenant-Id` to compile platform-only). Builds `Known` from the DB (industries = industry layers; departments = `department` dimension values visible to the tenant; jobs = base `job` rows; dimensions; axes). 200 → `EffectiveConfig`; `invalid_selection` errors → 400 with all of them; other compile errors → 422 `{"error":{"code":"compile_failed","message"},"errors":[CompileError…]}`. `?view=carebus` → `CarebusOntology`.
  - `GET /api/platform/export` (super-admin) → `{"version": <max seed_version + 1>, "files": {"axes.toml": "…", …}}` from current platform rows via `render`.

- [ ] **Step 1: Write failing tests:** `golden_selection_hash_matches_core` (seeded DB, same selection as Task 6 → same `hash` as the core golden test computes in-process); `unknown_industry_400_names_key` (`"retial"`); `tenant_patch_of_tombstoned_entity_422` (super-admin tombstones `Order` at base, tenant had patched `Order` → 422 with the tenant row id); `carebus_view_shape`; `tenant_layer_of_other_tenant_never_applied`; `export_round_trips` (export → `seed::parse` → `to_rows` equals the DB's platform rows by `(layer, kind, key, op, fields)`).
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Commit, push. Expected: CI green, 6 names `ok`.

---

### Task 12: Read-only web UI

**Files:**
- Create: `web/` — copy care-bus `web/` scaffolding (package.json versions, svelte.config, vite config, Dockerfile, keycloak-js setup, prettier, tsconfig), strip all care-bus routes.
- Create: `web/src/lib/api.ts`, `web/src/routes/+layout.svelte` (login, tenant picker from `/api/me`), `web/src/routes/layers/+page.svelte`, `web/src/routes/compile/+page.svelte`, `web/src/lib/Provenance.svelte`
- Test: `web/src/lib/Provenance.test.ts`, `web/src/lib/api.test.ts`
- Modify: `.github/workflows/ci.yml` (add `web` job: `ts-service.yml@main`, `image_name: strata-configurator-web`, `working_directory: web`, `docker_build: true`, `dockerfile_path: Dockerfile`, `node_version: "24"`)

**Interfaces:**
- Consumes: `/api/me`, `/api/layers`, `/api/layers/{id}/rows`, `/api/profiles`, `/api/compile`.
- Produces: pages — **Layers** (tree by scope; rows with op and rev; revisions drawer), **Compile** (selection form; entities/attributes/relations table; per object a provenance badge list "base add → industry/retail patch → tenant patch"; hash; errors list with row links). No write controls in phase 1.

- [ ] **Step 1: Write failing tests:** `Provenance renders origins in layer order with op labels`; `api sends X-Tenant-Id and Bearer`; `api surfaces error.code`.
- [ ] **Step 2:** Implement. Run locally (allowed — not cargo): `cd web && npm ci && npm run ci:verify && npm test && npm run format:check`.
- [ ] **Step 3:** Commit, push. Expected: CI `api` and `web` green.

---

### Task 13: Deploy and live verification

**Files (gitops-production, own PR, worktree `.worktrees/strata-configurator`):**
- Create: `clusters/strato/services/strata-configurator/{namespace,registry-secret,secrets.sops,networkpolicy,api,web,waypoint-gateway,kustomization}.yaml` — copy the care-bus shapes (`clusters/strato/services/care-bus/`), api `containerPort: 8097`, web 3000, Flux image policies `strata-configurator-api`/`-web`, VirtualService host `configurator.internal`.
- Create: `clusters/strato/services-istio/strata-configurator/{authz,peerauth,kustomization}.yaml` (copy care-bus).
- Create: `clusters/strato/services/keycloak/realm-patch-job-strata-configurator.yaml` (copy care-bus's): portal client `strata-configurator` (public, PKCE, redirect `https://configurator.internal/*`), audience mapper, realm role `config-super-admin` granted to the owner's user.
- Modify: `strata_configurator/.github/workflows/ci.yml` — add `deploy_namespace: strata-configurator` to both jobs.

- [ ] **Step 1:** Re-check port 8097 free (`git grep -n '\b8097\b' origin/main -- clusters/strato` empty; `lsof -nP -iTCP:8097` empty). Check `gh pr list -R nonagenticai/gitops-production --search strata-configurator` for an existing PR first.
- [ ] **Step 2:** Generate the DB password into `secrets.sops.yaml` (`DATABASE_URL` → `postgres.internal:5432/strata_configurator`), then provision the DB on the primary: `sops -d --extract '["stringData"]["DB_PASSWORD"]' … | ssh -i ~/.ssh/strato-vps admin@100.113.32.48 'bash /home/admin/gitops-production/scripts/host-configs/worker4/postgres/provision-service-db.sh strata_configurator strata_configurator'`. Verify born-locked: `psql -tAc "select datacl from pg_database where datname='strata_configurator'"` shows no `=c/` PUBLIC entry.
- [ ] **Step 3:** Run gitops-production's local CI/linters for the changed paths, open the gitops PR, merge when green, confirm Flux reconciled (`kubectl -n strata-configurator get pods` Running, image digest = the merge-sha image).
- [ ] **Step 4: Live verification** (all must hold; report each with its output):
  - `curl -s https://configurator.internal/ready` → 200.
  - `select version from seed_version` → `1`.
  - `POST /api/compile` with the golden selection as the owner → `hash` equals the value recorded in Task 6.
  - UI: log in as the owner through the browser lease (`browser-testing` skill), open Compile, run the golden selection, screenshot the provenance badges for `Order` (base add → retail patch).
- [ ] **Step 5:** Mark the strata_configurator PR ready, merge when CI is green, verify the merge-sha images rolled. Then `git worktree remove` + delete the branch.

---

## Self-review notes

- Spec coverage for phase 1 (§12.1): repo/DB/auth/tenants (T1, T7, T8, T13), layers/rows (T2, T9), axes/rules/dimensions/profiles (T3, T10), jobs (kind `job`, T2/T4/T6), seeds (T6, T7), compile + carebus view (T4, T5, T11), export (T11), golden test (T6, T11), read-only UI (T12), live (T13). Use cases, SOPs, tools, knowledge, targets, releases, drift, conversation, funnel: phases 2–6 (overview).
- Names used across tasks: `LayerKey`, `OverlayRow`, `Kind`, `Op`, `Profile`, `Condition`, `AxisRule`, `Selection`, `Known`, `compile`, `layer_order`, `EffectiveConfig.hash`, `project_carebus`, `SeedBundle`, `parse`, `render`, `to_rows`, `seeder::apply`, `TenantCtx`, `Role`, `load_for_layers` — each defined once in its task's Interfaces block.
