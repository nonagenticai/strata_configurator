# strata-configurator — design spec

Status: **rev 2 — owner decisions applied (§14).** No code yet.
Date: 2026-10-16
Parent direction: `care-bus-agents/docs/superpowers/specs/2026-10-16-interaction-layer-vision.md`
(called "the vision" below; § numbers prefixed `V§` refer to it).
Implementation plans: `docs/superpowers/plans/2026-10-16-configurator-00-overview.md` (all phases)
and `…-01-model-compiler.md` (phase 1, task level).

Facts about the neighbour apps were read from `origin/main` on 2026-10-16:
care-bus-agents @ 3b88445, strata_rag @ bd98d38, strata_ingest @ ae99442.

---

## 1. Purpose

strata-configurator is a separate Rust cell (V§4.1). It takes a client from
*"any job, in any department, in any industry"* to *"this job, here"*, and
produces **configuration**, not prose, for the runtime cells:

- **care-bus-agents** gets context spaces, procedures, connector field mappings, model routes, sources and ontology.
- **strata_rag** gets the store layout and retrieval metadata conventions.
- **strata_ingest** gets ingest jobs and the classification tags to put on delivered documents.

It owns the **generic → specific model**: the base ontology, classifier axes,
industry/department/job overlays, dimensions, job definitions, use cases and SOP links.
It also owns the approval workflow that decides what goes live.

### 1.1 Non-goals

- It does not run interactions. care-bus stays the runtime and reads only its own DB.
- It does not ingest or store knowledge. That stays in strata_ingest and strata_rag.
- It has no graph or entity-extraction service. KAG, karma and hirag stay out (owner, 2026-10-16).
- It shares no library with care-bus, rag or ingest, and nothing with `commonpractice`.
  `commonpractice` is a reference only (V§6.1). Wire contracts are copied as structs and covered by contract tests.
- It does not hold job, industry or department knowledge in code. That knowledge is seed data (§4.6).

---

## 2. Decision on open question V§7.1 — where configuration lives

**Decided: the configurator owns the authored configuration. Each runtime cell owns its
runtime copy. Approved configuration is provisioned down by the same pattern that already
exists for providers.** There is **one central, multi-tenant configurator**. The owner, as
**super-admin**, manages the standard assets in it (§10.2).

| Concern | Owner |
|---|---|
| Layers, overlays, axes, dimensions, job definitions, use cases, SOP links, drafts, approvals, history | configurator DB |
| Effective (compiled) config actually used at runtime | each runtime cell's own DB |
| Moving approved config from one to the other | configurator **push** to each cell's admin/tenant API |

Why:

1. **It is already the pattern.** `PUT /api/tenant/providers` on strata_rag and strata_ingest
   is a top app provisioning its subtree down to a dependency, which is the apps-are-cells rule.
   care-bus already pushes provider config this way.
2. **The runtime must not depend on the configurator.** care-bus answers customers. If the
   configurator is down, or unreachable in a customer-hardware deployment, nothing at runtime breaks.
3. **Standard assets are shared, runtime data is not.** All apps become multi-tenant (owner,
   2026-10-16), care-bus included. The generic layers are shared by every tenant of every
   runtime instance, so they belong in one place that the super-admin curates, not copied
   into each care-bus.
4. **Blast radius.** A bad overlay edit is a draft in the configurator. It only reaches a runtime
   cell through an explicit, approved, recorded release (§8).

What this means: care-bus's existing admin pages stay valid for **runtime-local edits**
(e.g. an operator tweaking a model route). The configurator **detects** those edits as drift
(§9) rather than forbidding them.

---

## 3. Position in the estate

```
                ┌────────────────────── strata-configurator (this spec) ─────────────────────┐
  owner/admin ─▶│ web UI ─ conversational job agent ─ funnel runner ─ compiler ─ release     │
                │                       │                                     │              │
                │                 configurator DB                       push adapters      │
                └─────────────────────────────────────────────────────────────┬────────────┘
                     ┌──────────────────────────────┬─────────────────────────┤
                     ▼                              ▼                         ▼
          care-bus carebus-api :8095      strata_rag :8092          strata_ingest :9135
          (/api/ontology, /api/procedures,  (/api/stores,           (/api/ingest/jobs,
           /api/connectors, /api/model-     /api/tenant/providers)   /api/tenant/providers)
           routes, /api/sources, …)
```

A **deployment target** is the unit the configurator provisions:
`(tenant, care-bus base URL, strata_rag base URL, strata_ingest base URL)`. Every cell is
multi-tenant, so a target addresses one tenant inside each cell, with the **same tenant uuid**
in all of them (the configurator's tenant id is passed down as `X-Tenant-Id`, as rag and
ingest already accept). A tenant with runtime instances in several regions (EU, US) or on
customer hardware has one target per instance. A customer-hardware instance is just a target
whose base URLs are outside the cluster; the configurator never ships with a deployment.

---

## 4. Domain model

Everything below is **data**. A deployment feels custom but is assembled from standard
layers (V§1).

### 4.1 Layers and scopes

Every authored object carries a **layer key**:

```
layer = (scope, industry?, department?, job?, rule?, dimension?, tenant?)
scope ∈ { base, profile, industry, industry_dept, job, tenant }
```

| scope | required keys | written by |
|---|---|---|
| base | — | super-admin |
| profile | `rule` (the axis rule that owns the layer, §4.3) | super-admin |
| industry | `industry` | super-admin |
| industry_dept | `industry`, `department` | super-admin |
| job | `job`, optional `dimension` (`key=value`, e.g. `channel=voice`) | super-admin |
| tenant | `tenant` | that tenant's editors |

Any other key set is a 400. This extends care-bus's
`ontology_scope = generic | industry | industry_dept`. care-bus's `generic` maps to the
configurator's `base`. `profile`, `job` and `tenant` are new.

**Storage shape.** All layered objects (entities, attributes, relations, job definitions, and
later use-case seeds) are rows of one table, `overlay_row(layer, kind, key, op, fields jsonb,
rev, is_current)`. The compiler is generic over `kind`; each kind has a typed field schema that
is validated on write and on compile. A layer holds **at most one current row per
`(kind, key)`**. An edit writes a new revision and retires the previous one, so revisions are
history, never extra overlay steps. Keys: entity `Order`, attribute `Order.order_id`, relation
`Order|paidBy|Payment`, job `customer_care_agent`.

### 4.2 Ontology

- **Entity**: name, label, synonyms, `hidden`.
- **Attribute**: entity, name, data_type, `pii`, synonyms.
- **Relation**: subject, predicate, object, cardinality.

These are the same shapes as care-bus's `ontology_entities`, `ontology_attributes` and
`ontology_relations`, so the push is a direct mapping.

Each entity, attribute and relation is an **overlay row** at a layer. An overlay can:
- `add` a new object;
- `patch` named fields of an object inherited from a lower layer. A patch **replaces** each
  named field whole (a `synonyms` patch carries the full new list). There is no deep merge;
- `tombstone` hide an inherited object at and above this layer.

Attribute `data_type` ∈ `string | text | enum | money | number | boolean | date | datetime`.
Relation `cardinality` ∈ `1:1 | 1:n | n:1 | n:m`.

**Base seed** (§4.6) is the core business objects from V§3.1: Customer (the party in the
customer role, generic in care-bus today), Party (person, people, company, group), Product,
ProductOffering, Order, Renewal, Delivery, Payment, PaymentMethod, Contract (header, parties,
terms, obligations, line items, payment terms, amendments, linkages), Bill (header, line items,
payment records, renewal, additional), Communication, Policy.
care-bus's retail slice (Shipment, ReturnAuthorization, WarrantyClaim, Carrier, Refund, and
retail patches of Order/Product/Payment) becomes the `industry = retail` overlay, so the
existing care-bus seed is one compiled output of the configurator. One deliberate model change:
Order, Product and Payment move from care-bus scope `industry` to `base`, because the vision
makes them core business objects. The golden test (§10.5) records that as an expected delta.

### 4.3 Classifier axes (proposal for open question V§7.2)

**Decided: axes are classifier tables in the configurator ontology. A profile is a tenant's
selection of axis values. The two options in the vision become one design.**

- `axis`: key (e.g. `customer_type`, `product_type`, `offering_type`, `order_type`,
  `renewal_type`, `delivery_type`, `delivery_method`, `payment_type`, `payment_method`),
  label, `cardinality` (`one` | `many`).
- `axis_value`: axis, key, label, parent (for sub-kinds such as
  `payment_method.digital_wallet.mobile`), description.
- **Axis rule**: a profile-scope overlay that applies when a condition over axis values holds.
  Example: `order_type = continuous AND payment_type = balancing` → add `Bill.meter_reading`,
  add relation `Order —metered_by→ MeterPoint`, enable the use-case seed
  `usage_dispute`.

  Conditions are a small boolean language over axis values: `=`, `in`, `and`, `or`, `not`.
  They are evaluated at compile time only. There is no runtime expression engine.
  `axis = v` holds when the profile selects `v` **or a descendant of `v`**
  (`payment_method = digital_wallet` holds for `digital_wallet.mobile`). A condition naming an
  unknown axis or value is rejected when the rule is written, not at compile time.
  Each rule owns one `profile`-scope layer; its rows apply when its condition holds. Rules
  apply in `(priority, key)` order, both stored on the rule.

- **Profile**: a tenant's (or a template's) chosen values per axis.
  Example: electricity = `{order_type: continuous, renewal_type: provider.implicit,
  product_type: [access], payment_type: balancing}`.

This keeps axes as first-class ontology data, which the vision asks for. Profiles stay
cheap: a profile is only a selection, and the rules carry the meaning.

### 4.4 Dimensions (second layer, V§4.1 / V§6.5)

- `dimension`: key (`department`, `region`, `channel`, `segment`, …), label. Super-admin only.
- `dimension_value`: dimension, key, label, `tenant` (NULL = a standard value the super-admin
  curates; set = a value only that tenant sees, e.g. its own department names).

`department` is a dimension too. This replaces a hardcoded department list.
`industry_dept` overlays and job overlays may be qualified by any dimension value, e.g. a job
overlay for `channel = voice`.

### 4.5 Job definitions (V§4.2 / V§6.3)

A **job definition** is a layered object like the ontology:

| Field | Meaning |
|---|---|
| `key`, `label` | e.g. `customer_care_agent`, `employee_care_agent` |
| `actor` | whom the job serves (customer, employee, supplier, patient…). Drives the generalised generator tracks (V§4.3) |
| `stance` | the behavioural brief, e.g. *aftersales is presales* (V§4.3) |
| `ontology_slice` | the entities and relations the job may touch (a selection over the effective ontology) |
| `use_cases[]` | references to use cases (§4.7) |
| `sops[]` | references to SOPs (§4.8) |
| `tools[]` | the MCP tools and public info services the job may call (§4.9) |
| `stores[]` | the knowledge store roles the job reads (§4.10) |
| `channels[]`, `dimensions` | dimension qualifiers |
| `escalation` | human hand-off rules |

Generic jobs (`customer_care_agent`, `employee_care_agent`) are base-layer seeds. Industry,
department and tenant layers refine them with the same `add`/`patch`/`tombstone` semantics.

A job definition is **created through conversation** (§7) and **edited in the UI** by a person
or an agent afterwards. Every edit is a new revision of that layer's row.

### 4.6 Seed data and where it comes from

The base layer, the axes, the dimensions, the generic jobs, the retail industry overlay and
an `electricity` profile template ship as **versioned seed files** in the repo
(`seed/v<N>/*.toml`), applied at startup by a migration-like seeder.

- **Source:** the owner's notes `~/Documents/01Quant/objects.txt`, `object_bill.txt`,
  `object_contract.txt` and `use_case_generator.txt`. They are not in a repo. They are
  transcribed into the seed files once.
- **Seeds bootstrap; the DB is the source of truth afterwards.** The super-admin curates the
  standard assets (platform layers, axes, dimensions, standard sources, tool catalogue, generator
  prompts) through the API/UI. A platform **export** (`GET /api/platform/export`, seed-file
  format) lets the owner snapshot them into the repo as the next seed version.
- **Insert-only per seed version.** A seed version is applied once, in one transaction, and
  recorded in `seed_version`. It adds rows; it never edits rows. A version that would `add` a
  key that already exists fails the transaction and the service does not become ready (`/ready`
  503 `seed_conflict`, naming the key). It never half-applies.
- The `commonpractice` seeds (Contract, Clause, Obligation, Invoice, Payment, Quote…) are used
  only as a **completeness checklist** during transcription.

### 4.7 Use cases (V§4.3 step 3)

A use case has:
- `id`, `title`, `brief`;
- `actors`;
- `preconditions`, `trigger`;
- `main_flow[]`, `exception_flows[]`, `postconditions`;
- `stakeholders`;
- `origin` (`journey_activity` | `service_request`, plus the generator node it came from);
- `data_sets[]` (information objects and example data);
- `data_operations[]` (track C: the operation, its success payload schema and its error states);
- `layer`;
- `status` (§8).

Track outputs are stored as first-class rows, so harmonisation and validation can be
re-run and diffed:
- `journey_event`, `journey_activity` (company/customer), `information_object`;
- `service`, `request_topic`, `request`;
- `data_category`, `data_operation`.

### 4.8 SOPs (V§4.3 step 5 / V§6.4)

- `sop_source`: where SOPs live. It has `kind` (`strata_store` | `url` | `document`),
  a locator (a strata_rag store id + document id, or a URL) and an `authoritative` flag.
- `sop`: title, version/etag, `sop_source`, a content hash at link time, and an owner.
- `use_case_sop`: a many-to-many link with a `governs` note.

**A use case with no SOP link cannot leave `draft`.**

### 4.9 Tool catalogue (V§6.6)

- `tool_server`: an MCP server, with URL, auth type and `secret_ref`. URLs live in this
  **shared catalogue**, never in code.
- `tool`: server, name, description, input schema (from `tools/list`), `side_effect` flag,
  `idempotency` requirement, and `kind` (`backend` | `public_info`).
- The catalogue is refreshed from each server's `tools/list` and stored. A human marks
  `side_effect`. This replaces the hardcoded `SIDE_EFFECT_TOOLS` in care-bus `mcp.rs` once
  care-bus reads it (§11).

**Public info services** (registries, weather, carriers, tax rates) are tools on our MCP
server with `kind = public_info`. They live in the same catalogue for every tenant.

### 4.10 Knowledge layout (V§4.1)

- `knowledge_kind`: a taxonomy node, with `authority` (`authoritative` | `reference` |
  `anecdotal`) and `epistemic_status` (e.g. `normative` for SOPs and policy, `descriptive`
  for product data, `procedural` for how-tos). This is the "ontology, taxonomy and
  epistemology" of V§4.1.
- `store_template`: a tree of router and leaf nodes, each tagged with a `knowledge_kind`.
  It compiles to strata_rag `POST /api/stores` calls (§6.2).
- `standard_source`: a curated bootstrap source (URL set, public dataset, SOP template pack),
  with `knowledge_kind`, `jobs[]` and `industries[]`. Standard sources are platform assets:
  **the owner (super-admin) curates them** (V§7.3, decided). Tenants do not add standard
  sources; they add their own documents and SOP sources at the tenant layer.
- `tag_scheme`: which ontology keys go on rag document `metadata`, e.g.
  `{"entities":["Order","Refund"], "use_cases":["uc-…"], "kind":"sop", "authority":"authoritative"}`.

---

## 5. Compilation — the effective view

### 5.1 Function

```
effective(industry, departments, job, profile, dims, tenant) =
    base
  ⊕ profile_rules(profile)              # axis rules whose condition holds
  ⊕ industry overlay
  ⊕ industry × dept cells                # one per selected department, in key order
  ⊕ job overlay (+ dimension-qualified job overlays)
  ⊕ tenant overrides / tombstones
```

This is V§3.2, with dimension-qualified job overlays added, and a **set** of departments:
a job usually spans several (retail customer care covers faq, logistics, order-support and
payments). Two selected departments that `add` the same object is a `duplicate_add` error.
Every key the selection names (industry, department, job, dimension value, axis value) must
exist; an unknown one is a 400 `invalid_selection`, never a silently thinner compile.

### 5.2 Rules

- **Order is fixed** as listed. Within a layer there is at most one current row per object
  (§4.1), so there is no intra-layer order. Profile layers apply in rule `(priority, key)` order.
- `add` of an object that already exists → a compile **error**, not a silent override.
- `patch` or `tombstone` of an object that does not exist → error.
- An attribute or relation whose entity is not in the effective view → error
  (`dangling_reference`), unless the entity was tombstoned (then it cascades, below).
- Errors are **collected, not first-fail**: a compile returns every error with its row id, so
  one UI round trip shows them all.
- `tombstone` removes the object and everything that depends on it:
  - attributes of a tombstoned entity, and relations that point at it, are removed;
  - a use case whose required entities are gone is **flagged**, not silently dropped.
- **Deterministic.** The output is canonical JSON, sorted, and hashed with sha256 to give the
  **effective hash**. The same inputs always give the same hash.
- The compiler is a pure function over rows that were already loaded. It does no IO.
  This makes it unit-testable without a DB.

### 5.3 Output

`EffectiveConfig` holds:
- ontology, axes and profile;
- job definition;
- use cases (with SOP links);
- tool grants;
- knowledge layout;
- a **provenance map** (object → contributing layer rows).

The UI uses provenance to show "inherited from industry/retail, patched by tenant".

Phase 1 compiles ontology, profile and job definition only. Use cases, tool grants and
knowledge layout join `EffectiveConfig` in the phase that introduces them (§12).

### 5.4 care-bus projection

`EffectiveConfig` projects to care-bus's ontology tables (`ontology_entities`,
`ontology_attributes`, `ontology_relations`, `carebus-api/migrations/0001_schema.sql`).
care-bus's `scope` column comes from the layer of the object's `add` row:
base/profile → `generic`; industry → `industry`; industry_dept → `industry_dept`;
job/tenant → the narrowest scope the selection names (industry + exactly one department → `industry_dept`,
industry → `industry`, else `generic`). `industry`/`department` are filled to match the scope.
This mapping is lossy only for job/tenant adds, and care-bus never reads scope at runtime.

---

## 6. Emission — from effective config to cell configuration (V§4.3 step 4)

Each approved use case becomes:

| Target | Output | Existing API (verified) | Gap |
|---|---|---|---|
| care-bus context space | `{key, area, label, request_type, required[], entities[], procedure}` | `GET /api/context-spaces` only | **no write API** (§11) |
| care-bus procedure | name, PDDL action steps, `tool_requirements`, `success_criteria`, `plan` jsonb (= `carebus::domain::Plan`) | `POST /api/procedures` (new version) + `POST …/versions/{v}/status` | none for the write. The runtime only executes request types in the closed `RequestType` enum (§11) |
| care-bus ontology | entities and attributes | `POST/PUT /api/ontology/entities` | relations are read-only (§11) |
| care-bus connector mappings | `backend_field → Entity.attribute` | `PUT /api/connectors/{id}` (mappings in body, delete-then-insert) | none |
| care-bus sources | website/api/file/datatable source with field→ontology mapping | `POST/PUT /api/sources` | none |
| care-bus model routes | per `model_use` | `PUT /api/model-routes/{use}` | none |
| strata_rag stores | store tree from `store_template` | `POST /api/stores` (`parent_id`, `kind`, `name`, `description`); tenant from token/`X-Tenant-Id` | none |
| strata_rag retrieval scoping | `metadata` tags per document; query uses `metadata @>` | `QueryRequest.metadata` containment already exists | V§5's "no metadata" is outdated — `documents.metadata jsonb` landed in rag migration 0004 |
| strata_ingest jobs | bootstrap ingest of `standard_source` and SOP sources | `POST /api/ingest/jobs` (`store_id`, `url(s)`, `source_id`, `options`) | **no job-level metadata/tags field**; jobs deliver only the structured-source mapping as metadata (§11) |
| strata_ingest classification | ontology tagging step | none (Detect = file type only) | **new Classify stage** between Convert/Ocr and Bundle (§11) |

### 6.1 Naming emitted for care-bus

- `context_space.key = {industry}:{area}:{space}`.
  The `retail:` prefix stays valid because it is just the retail industry key.
- `request_type` = a stable slug of the use case (`uc_<slug>`).
- `area` = the journey stage or service the use case came from.

### 6.2 strata_rag layout

The store template compiles to a router/leaf tree under one root per deployment target.
- Store names are deterministic: `<knowledge_kind>/<slug>`.
- Re-provisioning is **idempotent**: rag has a unique constraint on `(tenant, parent_id, name)`.
  The pusher looks a store up before it creates one, and only `PATCH`es the description.
- Stores are **never deleted** by a push. A store that is no longer in the template is
  reported as orphaned.

### 6.3 Push semantics

- A **release** (§8) is pushed per target, per cell, in order:
  1. rag stores;
  2. ingest bootstrap jobs;
  3. care-bus ontology;
  4. connectors and mappings;
  5. context spaces;
  6. procedures (created DRAFT → `PUBLISHED` last).
- Every call is idempotent. The configurator keeps a `push_step` row per call, holding the
  request hash and the response.
- A failed step stops the release at that point. A re-push resumes from the failed step.
- Partial state is visible in the UI and is never reported as live.
- Procedures go out as new versions. The previous PUBLISHED version stays until the new one
  is published, so care-bus always has exactly one PUBLISHED version per context space.
  This keeps its unique partial index satisfied.
- Credentials are never pushed as values. Pushes carry `secret_ref` names, as care-bus already
  requires. Source credentials go into care-bus's sealed `credentials` field only when a person
  enters them in the configurator UI. They are forwarded in the same request and **not stored**
  in the configurator.

---

## 7. Conversational job definition (V§4.2)

- An agent loop runs in the configurator. It is server-side and streams over SSE to the UI.
- Its tools are typed configurator operations only:
  - `search_layers`, `propose_profile`, `set_axis_value`;
  - `select_job_template`, `patch_job_field`;
  - `list_tools`, `list_standard_sources`;
  - `compile_preview`.

  It cannot call runtime cells.
- The output is a **draft job-definition revision** at the tenant layer, plus a profile.
  Each agent turn that changes state writes a revision with `author = agent:<run_id>`.
- The UI shows the job definition as a form, with provenance badges. People and agents edit
  the same rows.
- The conversation transcript is stored with the run for audit. It is not a source of truth:
  only the revisions are.

---

## 8. The funnel and approval lifecycle (V§4.3)

The funnel is a **persisted, resumable procedure** (`funnel_run` + `funnel_stage` rows).
It is not one long LLM call.

| Stage | Input → output | LLM? | Human gate |
|---|---|---|---|
| 1. Profile | conversation/form → profile + job draft | yes (§7) | confirm profile |
| 2. Compile | layers → `EffectiveConfig` + hash | no | — |
| 3a/b/c | generator tracks A, B and C, **in parallel**, on the effective ontology and job actor | yes | — |
| 3d | harmonise A↔B, validate data sets against both, relate data sets | yes, plus deterministic checks | review diff |
| 3e | generate use cases per activity and per request | yes | — |
| 4. Emit | use case → context space / procedure / mappings / stores / ingest jobs | **no** (deterministic templates over use case + effective ontology) | — |
| 5. SOP link | use case → SOPs + source location | yes for suggestions (rag search over SOP stores), a person confirms | link required |
| 6. Approve | configured use case → `approved` | no | **required** |
| 7. Release | approved set → push (§6.3) | no | **required** (`approver` role) |

Rules:

- **Use case status:** `generated → draft → configured → approved → live`, plus `rejected` and
  `stale` (§9). `configured` needs slots, a procedure, connectors and stores to be filled, and
  at least one SOP link.
- **The client only edits the delta** (V§4.3 step 6). The UI shows each use case as
  inherited/generated vs tenant-edited.
- The generator's tracks generalise by the job's **actor** (V§4.3): journey of the actor,
  services offered to the actor, data accessible to the actor. Prompts are seed data per
  generator version, not code.
- Generated output is **validated against JSON schemas** before it is stored. Invalid output is
  retried at most N times, then the stage fails loudly. It is never stored half-valid.
- **No scheduled generation.** LLM stages run only from an explicit user action.
  No cron and no background trigger may cause LLM or GPU calls.
- **Release** = an immutable snapshot of `{effective_hash, approved use cases, emitted payloads, target}`.
  Rollback = re-push an earlier release.
- **Approve and release are two actions, not two people** (owner, 2026-10-16). One person with
  the `approver` role may do both. Each action records its own actor (`approved_by`,
  `released_by`), so a per-tenant "releaser must differ from approver" policy can be added
  later without a data migration. That policy is **not built** now.

---

## 9. Drift detection (proposal for open question V§7.4)

Every approved use case pins the **input hashes** it was approved against:
- the `effective_hash` of its ontology slice;
- each linked SOP's content hash/etag;
- the tool schema hashes of its granted tools;
- the generator/seed version.

A drift check recomputes these:
- **Upstream drift.** If the ontology, an SOP, a tool schema or a seed changed, the use case
  becomes `stale` and shows a diff. A stale use case is never auto-updated and never
  auto-pushed.
- **Downstream drift.** The configurator reads back each target cell (`GET` on the same
  endpoints it pushes to) and compares with the last release. A local edit in care-bus admin
  is shown as drift, which the user can **adopt** (it becomes a tenant overlay) or **overwrite**
  (on the next release).

Drift checks run on demand and on a timer. Drift checks are pure HTTP/DB reads and hashing,
with **no LLM**, so a timer is allowed. SOP hashes come from rag document `content_hash` and
`GET` on the source URL. The timer interval is config, not code.

---

## 10. Service design

### 10.1 HTTP API (unversioned `/api/<resource>`, `Paged{items,next}`, `limit` 50 / max 500, cursor `before`, errors `{"error":{"code","message"}}`)

- **Identity and tenants**
  - `GET /api/me` (sub, `super_admin`, memberships)
  - `GET|POST /api/tenants` (super-admin), `GET|POST /api/tenants/{id}/members`,
    `DELETE /api/tenants/{id}/members/{sub}` (super-admin or that tenant's `admin`)
- **Layers and overlay rows** (one generic surface for every layered kind, §4.1)
  - `GET|POST /api/layers`
  - `GET|POST /api/layers/{id}/rows?kind=` — POST body `{kind, key, op, fields}`
  - `PUT /api/layers/{id}/rows/{kind}/{key}` (new revision), `DELETE …` (retire the row — this
    *reverts* the overlay; hiding an inherited object is a `tombstone` row, not a DELETE)
  - `GET /api/layers/{id}/rows/{kind}/{key}/revisions`
- **Platform assets**
  - `GET /api/platform/export` (super-admin; seed-file format, §4.6)
- **Axes and dimensions**
  - `GET|POST /api/axes`, `/api/axes/{key}/values`
  - `GET|POST /api/axis-rules`
  - `GET|POST /api/dimensions`, `/api/dimensions/{key}/values`
- **Profiles**
  - `GET|POST /api/profiles`, `GET|PUT /api/profiles/{id}`
- **Job definitions** are overlay rows of kind `job` (layers API above).
- **Compilation**
  - `POST /api/compile` (body: selection) → `EffectiveConfig` + hash + provenance, or 422 with
    every compile error. `?view=carebus` returns the §5.4 projection instead.
- **Conversation**
  - `POST /api/conversations`, `POST /api/conversations/{id}/messages` (SSE response)
- **Funnel**
  - `POST /api/funnel-runs`, `GET /api/funnel-runs/{id}`
  - `POST /api/funnel-runs/{id}/stages/{stage}/run|approve`
- **Use cases**
  - `GET /api/use-cases?status=&job=`, `GET|PUT /api/use-cases/{id}`
  - `POST /api/use-cases/{id}/approve|reject`
- **SOPs**
  - `GET|POST /api/sop-sources`, `GET|POST /api/sops`
  - `PUT /api/use-cases/{id}/sops`
- **Tools**
  - `GET|POST /api/tool-servers`, `POST /api/tool-servers/{id}/refresh`, `GET /api/tools`
- **Knowledge**
  - `GET|POST /api/knowledge-kinds|store-templates|standard-sources|tag-schemes`
- **Targets and releases**
  - `GET|POST /api/targets`, `POST /api/targets/{id}/test`
  - `GET|POST /api/releases`, `POST /api/releases/{id}/push`, `GET /api/releases/{id}/steps`
- **Drift**
  - `GET /api/drift?target=`, `POST /api/drift/{id}/adopt|overwrite`
- **Health**
  - `GET /health`, `GET /ready` (no auth)

### 10.2 Tenancy and auth

- **Identity** comes from the shared Keycloak **portal realm** (all apps log in there), with a
  new public client `strata-configurator` (PKCE for the UI) and audience `strata-configurator`
  for API tokens. Keycloak only authenticates.
- **Super-admin** is the one platform-level role, the Keycloak realm role
  `config-super-admin`, held by the owner. It writes everything that is shared: base, profile,
  industry, industry_dept and job layers; axes, axis rules, dimensions and standard dimension
  values; and (later) standard sources, the tool catalogue, knowledge kinds, store templates and
  generator prompts. It also creates tenants and their first admin.
- **Tenants are owned by the configurator**, as cells do (`tenant`, `member` tables). Tenant
  roles live in the configurator DB, not in Keycloak:
  - `viewer` — read and compile;
  - `editor` — write that tenant's layer and its dimension values;
  - `approver` — editor + approve and release (§8);
  - `admin` — approver + manage members.
- The caller names its tenant in `X-Tenant-Id` (the rag/ingest convention): exactly one header,
  a bare non-nil uuid. Missing on a tenant-scoped route → 400 `tenant_required`; malformed or nil
  → 400 `invalid_tenant`; not a member → 403. Super-admin may act in any tenant.
- Every write records an `audit_event` (actor, action, object, before/after hash).
- **Outbound to targets:**
  - **care-bus:** a Keycloak client-credentials service account with care-admin, plus
    `X-Tenant-Id` once care-bus is multi-tenant (P0). care-bus's static service tokens are
    read-only today (§11).
  - **rag/ingest:** a tenant-scoped bearer token per target (`<tenant>:<token>`, the existing
    rag/ingest format).
  - Target secrets are stored as `secret_ref` names that resolve to k8s Secret keys mounted
    into the pod, never as DB values. Exception: per-target tokens that a person enters in the
    UI are sealed with AES-256-GCM under `CONFIGURATOR_SECRET_KEY`, with the target id as AAD
    (the care-bus sources pattern). They are write-only, and missing key → 503 `not_configured`.
- **SSRF guard** on every user-supplied URL (tool servers, SOP URLs, targets in non-cluster
  mode): block private, CGNAT, loopback and link-local addresses (`not ip.is_global`), pin DNS,
  no redirects, 5 s timeout, 256 KiB body cap. Same rules as care-bus sources.
  Cluster-internal targets are allow-listed by config, not by user input.

### 10.3 LLM usage

- Only through a configured provider, using the `tenant/providers` shape. One capability,
  `generate`, with `base_url`, `dialect`, `model`, `regions`, `zdr`, `timeout_ms`,
  `concurrency` and a sealed `api_key`.
- In production with no provider → 409 `provider_not_configured`. There is no silent fallback.
- Every LLM call records the model, token usage and the stage it ran for, on the run row.
- Prompts are versioned seed data. A generator run pins `generator_version`.

### 10.4 Storage

- Postgres 18, own DB `strata_configurator`, own role. It is created with
  `provision-service-db.sh` (born locked).
- sqlx 0.9, `sqlx::migrate!("./migrations")` at startup.
- Main tables:
  - `tenant`, `member`;
  - `layer`, `overlay_row` (all layered kinds, §4.1);
  - `axis`, `axis_value`, `axis_rule`;
  - `dimension`, `dimension_value`;
  - `profile` (`tenant` NULL = a template such as `electricity`);
  - `seed_version`;
  - `conversation`, `conversation_message`;
  - `funnel_run`, `funnel_stage`;
  - generator track tables (§4.7);
  - `use_case`, `use_case_sop`, `sop_source`, `sop`;
  - `tool_server`, `tool`;
  - `knowledge_kind`, `store_template`, `standard_source`, `tag_scheme`;
  - `target`;
  - `release`, `push_step`;
  - `drift_finding`;
  - `audit_event`.
- **No `create_all`.** Migrations only. Each migration is tested by `upgrade` from empty and
  from the previous release.

### 10.5 Stack and repo layout (matches care-bus / strata conventions)

- Rust edition 2024, axum 0.8.9, sqlx 0.9.0, reqwest, rmcp (MCP `tools/list` client for catalogue refresh), jsonwebtoken. All dependencies use exact pins.
- Workspace crates:
  - `crates/configurator-core`: domain types, overlay compiler, emitter, drift hashing. Pure, with no IO.
  - `crates/configurator-api`: axum service, DB, auth, push adapters, LLM client.
- `web/`: SvelteKit (same versions as care-bus web), adapter-node, keycloak-js, port 3000 in-container.
- **Ports:** API **8097**. It was checked free in gitops `clusters/strato` and locally on
  2026-10-16. Re-verify when deploying; never use 8000.
- **Images** come from the mirrored `registry.internal:5000` base images, with a single
  `Dockerfile.rust` with a `PACKAGE` arg (the care-bus pattern).
- **CI:** `.github/workflows/ci.yml` calls gitops-production `rust-service.yml@main` (api) and
  `ts-service.yml@main` (web).
  - **Deploy:** namespace `strata-configurator`.
  - **Manifests:** `gitops-production/clusters/strato/services/strata-configurator/`.
- **Tests:**
  - **DB tests:** one Postgres testcontainer per test binary and a fresh DB per test (copy the
    care-bus `tests/common/mod.rs` harness; never one container per test).
  - **Push contract tests:** fake care-bus/rag/ingest upstreams in `tests/common/fakes.rs`,
    whose request/response structs are copied from each cell's Rust response types.
  - **Compiler and emitter:** property tests (determinism, the tombstone cascade) and golden
    tests, including **"compiling base ⊕ retail ⊕ customer_care, projected to care-bus (§5.4),
    contains every ontology row of care-bus's 0002 seed field-for-field"**, with the three
    documented scope moves (§4.2) as the only deltas. The procedure/context-space half of that
    golden test arrives with use-case emission (phase 3). It proves the model is complete
    enough to replace a hand seed.
- No local cargo builds (owner ruling 2026-10-16). Rust is verified in CI only.

---

## 11. Prerequisites in the other cells

Each of these is its own work item in that cell's repo. The configurator is useful before all
of them land, because it can author, compile and preview without them. It can only **release**
what the target can accept.

| # | Repo | Change | Blocks |
|---|---|---|---|
| P0 | care-bus | **Multi-tenancy** (owner, 2026-10-16: all apps become multi-tenant): tenant column on ontology, context spaces, procedures, connectors, sources, model routes; tenant from `X-Tenant-Id`/token; streams and audit tenant-scoped. Today care-bus has no tenant anywhere (verified @ 3b88445) and `ontology_entities.name` is a global PK | any push to care-bus |
| P1 | care-bus | Write API for context spaces (`POST/PUT /api/context-spaces`) | emitting new context spaces |
| P2 | care-bus | Make `RequestType`/`area`/`space`/required params **data-driven** from `context_spaces` (`domain.rs:44-77`, `tools.rs:38-50`); intake classifies into loaded request types | any use case beyond the 5 hardcoded ones going *live* |
| P3 | care-bus | Write API for ontology relations | pushing relations |
| P4 | care-bus | Service-account write access (care-admin via client credentials) for the configurator | any push to care-bus |
| P5 | care-bus | MCP tool catalogue + `SIDE_EFFECT_TOOLS` + method→tool mapping from config instead of `mcp.rs` constants | tool grants per job |
| P6 | care-bus | Job-level settings (stance/preamble, actor instead of `customer_id`) from config instead of `interaction.rs` | non-customer-care jobs |
| P7 | strata_ingest | `metadata` (object) on `POST /api/ingest/jobs`, merged into every delivered document | ontology-tagged bootstrap ingest |
| P8 | strata_ingest | Classify stage (between conversion and Bundle) that tags output with ontology entities from a pushed tag scheme | automatic tagging of client documents |
| P9 | strata_rag | Nothing required for v1 (stores + `metadata @>` filter exist). Later: index on `documents.metadata` (GIN) if filter latency needs it | — |

The vision's V§5 is partly outdated. strata_rag **does** have a tenant (`stores.tenant`,
migration 0005) and document metadata with a containment filter (migration 0004).

---

## 12. Phasing

Each phase is shippable on its own and verified live.

1. **Model + compiler.**
   - Repo, DB, auth, tenants, layers, overlay rows, axes, rules, dimensions, profiles, jobs,
     seeds (base + retail + electricity template + `customer_care_agent`, `employee_care_agent`).
   - `POST /api/compile` (+ `?view=carebus`), platform export.
   - The golden test against the care-bus seed.
   - Read-only UI with provenance. Live in the cluster.
2. **Targets + release for what exists today.**
   - Targets; rag store layout first (rag is multi-tenant today, so this needs nothing).
   - care-bus ontology entities, connector mappings, sources and model routes; procedures for
     the existing 5 request types. Needs P0 and P4.
   - Downstream drift read-back.
3. **Conversation + funnel.**
   - Job agent, generator tracks A/B/C, harmonise, use cases, SOP linking, approval.
   - Releases of use cases mapping onto existing request types only.
4. **New request types live.** Needs P1 and P2: generated use cases become live care-bus context spaces.
5. **Knowledge bootstrap.**
   - Standard sources, ingest jobs with tags, retrieval scoping by metadata.
   - Needs P7, then P8.
6. **Second job.** `employee_care_agent` end to end. Needs P5 and P6. This proves the job axis.

---

## 13. Acceptance (for the spec as a whole)

- A new tenant with profile *electricity* (continuous order, provider-implicit renewal, access
  product, balancing payment) and job `customer_care_agent` gets, without code changes:
  - a compiled ontology that includes Contract, Bill, metering and renewal;
  - generated, SOP-linked, approved use cases;
  - a pushed release in which care-bus answers a billing-dispute request through a published
    procedure, using rag documents filtered by the use case's metadata tags.
- Changing a linked SOP marks its use cases `stale` within one drift interval, and the next
  release is refused for those use cases until they are re-approved.
- An edit in care-bus admin shows up as downstream drift and can be adopted or overwritten.

---

## 14. Decisions and what is still open

Owner decisions, 2026-10-16:

1. §2 — the configurator owns the authored config and provisions it down. One central,
   multi-tenant configurator. (The rev-1 proposal; the owner proceeded on it without objection.)
2. §4.3 — classifier tables + profile-as-selection + axis rules. (Same.)
3. V§7.3 — **the owner curates the standard data sources**, as super-admin (§4.10, §10.2).
4. Four-eyes — **not required.** One approver may approve and release. Kept possible as a future
   per-tenant option by recording both actors (§8). Not built.
5. Customer hardware — **all apps become multi-tenant**; the owner as super-admin manages the
   standard assets centrally. A customer-hardware runtime is a target (§3). This adds P0
   (care-bus multi-tenancy) as a prerequisite for every care-bus push.

Still open:

1. Which concrete standard sources bootstrap which jobs (content, not mechanism). Needed by
   phase 5; the owner supplies them.
2. Network reachability and auth for pushing into a customer-hardware target (outbound HTTPS
   from the cluster with a per-target token is the assumption). Needed only when the first such
   target exists.
