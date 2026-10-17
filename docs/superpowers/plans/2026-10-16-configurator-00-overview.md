# strata-configurator — implementation overview (all phases)

> **For agentic workers:** this is the phase map. Execute one phase plan at a time
> (REQUIRED SUB-SKILL: superpowers:subagent-driven-development or superpowers:executing-plans).
> Only phase 1 has a task-level plan today; write each later phase's plan when its
> prerequisites have landed, from the spec and the code as it then is on `origin/main`.

**Spec:** `docs/superpowers/specs/2026-10-16-configurator-design.md` (rev 3, 2026-10-17).
**Next-session plan (W0–W6):** `2026-10-17-configurator-02-tenancy-and-carebus-handoff.md`.

## Why only phase 1 is detailed

Phases 2–6 push into care-bus, strata_rag and strata_ingest. Five of them depend on changes in
those repos (P0–P8, spec §11) that do not exist yet. Their interfaces will be decided in those
repos' own plans. A task-level plan written now would describe APIs that are not built and
would be wrong by the time it runs.

## Phases

Each phase is one PR in `nonagenticai/strata_configurator`, and ends **live-verified** in the
strato cluster (not "CI green").

| # | Phase | Needs | Plan file | Exit (live) |
|---|---|---|---|---|
| 1 | Model + compiler + read-only UI | nothing | `2026-10-16-configurator-01-model-compiler.md` | `configurator.internal` serves; seeds v1 applied; `POST /api/compile` for the retail golden selection returns the same hash as CI; UI shows provenance |
| 1a | Identity, tenancy, RLS (W1): `users` replaces `member`, `super_admin` client role, `X-Active-Tenant-Id`, `configurator_app` + RLS | phase 1; W0 waypoint fix (gitops) | `2026-10-17-configurator-02-…` §4 W1 | normal users see only their tenant; RLS test green; super-admin switch audited |
| 1b | Licensing gate 2 + early-access signup (W2), both dark | 1a's `users` model; licences registration (W6) | `2026-10-17-configurator-02-…` §4 W2 | introspect returns `active` for a test tenant; an approved signup creates tenant + `tenant_admin` + licence |
| 2a | Targets + rag store layout | 1a (rag itself is multi-tenant) | `…-02a-targets-rag.md` | a release creates the store tree in rag for a test tenant; re-push is a no-op; orphan store reported |
| 2b | care-bus release push (W4) | 1a, **P0, P4** | `…-02b-carebus-release.md` | release 1 (versioned base bundle) for a test tenant's `customer_care_agent` job; care-bus answers a return request with it; a tenant refinement survives release 2; a removed base object shows as a conflict |
| 3 | Conversation + funnel + approval; configurator metering outbox (first LLM calls) | phase 2b; an LLM provider configured | `…-03-funnel.md` | electricity tenant: job agent → tracks A/B/C → use cases → SOP links → approve → release onto existing request types; LLM usage reaches `billing:costs` when metering is on |
| 4 | New request types live | **P1, P2** (P3 done) | `…-04-new-request-types.md` | a generated `uc_billing_dispute` becomes a live care-bus context space and is answered |
| 5 | Knowledge bootstrap | **P7**, then **P8**; owner's standard-source list | `…-05-knowledge.md` | standard sources ingested with ontology tags; rag queries filtered by use-case metadata |
| 6 | Second job | **P5, P6** | `…-06-employee-care.md` | `employee_care_agent` end to end for one tenant |

Drift (spec §9) is built incrementally: downstream read-back in 2a/2b, upstream (SOP, tool
schema, ontology) staleness in 3 when approvals exist.

## Work items in other repos

Each is that repo's own unit of work: its own plan, its own PR. The configurator plan for the
phase that needs it starts only after it is **merged and live**.

| # | Repo | Item | Needed by |
|---|---|---|---|
| P0 | care-bus-agents | Multi-tenant **and multi-job** (W3): `tenant_id` + `job_id` on every config table with RLS + anti-drift test, own `users` table + `care-bus` client role `super_admin`, tenants provisioned by the configurator, base vs refinement layers, `(tenant, job)`-scoped streams/audit | 2b |
| P4 | care-bus-agents | Release endpoint accepted only from a client-credentials SA with the `release-writer` client role; `aud`/`iss` required | 2b |
| P1 | care-bus-agents | `POST/PUT /api/context-spaces` | 4 |
| P2 | care-bus-agents | Request types/areas/required slots data-driven from `context_spaces` (`domain.rs`, `tools.rs`) | 4 |
| P3 | care-bus-agents | Write API for ontology relations — **done** (verified 2026-10-17) | — |
| P7 | strata_ingest | `metadata` object on `POST /api/ingest/jobs`, merged into delivered documents | 5 |
| P8 | strata_ingest | Classify stage tagging output with ontology entities from a pushed tag scheme | 5 |
| P5 | care-bus-agents | Tool catalogue + side-effect flags from config, not `mcp.rs` constants | 6 |
| P6 | care-bus-agents | Job settings (stance/preamble, actor) from config, not `interaction.rs` | 6 |

P0 is the critical path: it gates every care-bus push. It waits for the owner's other care-bus session to finish (plan 02 §8).

## Order of work

1. Phase 1 — done, live. W0 waypoint fix (gitops).
2. Phase 1a ∥ 1b. Phase 2a as soon as 1a is live.
3. P0 + P4 (care-bus W3) ∥ 1b.
4. Phase 2b when 1a, P0 and P4 are live.
5. Phase 3 (+ configurator metering). Meanwhile P1/P2 in care-bus, P7 in ingest.
6. Phases 4, 5, 6 in the order their prerequisites land.

## Owner inputs needed, and when

- **Phase 3:** which LLM provider the generator uses (the `tenant/providers` `generate` slot). ANSWER: same as care-bus-agent tenant-provided/configured commercial provider selection with local models via pgx:vllm/laya and mlx_api
- **Phase 5:** the standard sources per job/industry (owner curates; spec §14 open item 1).
- **First customer-hardware target:** reachability/auth for remote push (spec §14 open item 2).
