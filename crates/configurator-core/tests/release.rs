//! care-bus release bundle v1: hash parity with care-bus's golden fixture
//! (`crates/carebus-api/tests/fixtures/release-bundle-v1.json` on care-bus-agents
//! `feat/w3-multitenant`, copied byte for byte).

use configurator_core::canonical::canonical_json;
use configurator_core::compile::{EffectiveConfig, Selection, compile};
use configurator_core::release::{HashError, ReleaseBodyV1, SCHEMA_V1, bundle_hash, release_body};
use configurator_core::seed;
use serde::Serialize;
use serde_json::{Value, json};

const V1: &[(&str, &str)] = &[
    ("axes.toml", include_str!("../../../seed/v1/axes.toml")),
    (
        "dimensions.toml",
        include_str!("../../../seed/v1/dimensions.toml"),
    ),
    ("base.toml", include_str!("../../../seed/v1/base.toml")),
    ("retail.toml", include_str!("../../../seed/v1/retail.toml")),
    (
        "electricity.toml",
        include_str!("../../../seed/v1/electricity.toml"),
    ),
    ("jobs.toml", include_str!("../../../seed/v1/jobs.toml")),
];

const V2: &[(&str, &str)] = &[(
    "customer_care_agent.toml",
    include_str!("../../../seed/v2/customer_care_agent.toml"),
)];

const FIXTURE: &str = include_str!("fixtures/carebus-release-bundle-v1.json");
const GOLDEN_HASH: &str = "sha256:79c5b940a6a1b0f3015bd5b818e22ac7c284a72efef6c69c1e276612e8aaa492";

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).expect("fixture is JSON")
}

fn typed() -> ReleaseBodyV1 {
    serde_json::from_str(FIXTURE).expect("fixture parses as ReleaseBodyV1")
}

#[test]
fn golden_fixture_hash_matches_carebus() {
    let body = fixture();
    assert_eq!(body["hash"], GOLDEN_HASH);
    assert_eq!(
        bundle_hash(&body["bundle"]).expect("hashes"),
        body["hash"].as_str().expect("hash is a string")
    );
}

#[test]
fn typed_roundtrip_preserves_hash() {
    let body = typed();
    let again = serde_json::to_value(&body.bundle).expect("bundle serialises");
    assert_eq!(bundle_hash(&again).expect("hashes"), body.hash);
    assert_eq!(again, fixture()["bundle"]);
}

#[test]
fn null_fields_are_serialised() {
    let body = typed();
    let v = serde_json::to_value(&body.bundle).expect("bundle serialises");
    let slot = &v["connectors"][0];
    assert!(slot.get("base_url").is_some() && slot["base_url"].is_null());
    assert!(slot.get("rate_limit").is_some() && slot["rate_limit"].is_null());
    assert!(v["ontology"]["entities"][0].get("industry").is_some());
    assert!(v["ontology"]["entities"][0].get("department").is_some());
    assert!(v["procedures"][0].get("description").is_some());
    assert!(v["procedures"][0].get("pddl_domain").is_some());
    assert!(v["knowledge_stores"][0].get("parent").is_some());
    // job_settings keys are omitted when unset, never null.
    let mut bare = body.bundle.clone();
    bare.job_settings.preamble = None;
    let v = serde_json::to_value(&bare).expect("bundle serialises");
    assert!(v["job_settings"].get("preamble").is_none());
    // An unflagged step does not grow a side_effect key.
    assert!(v["procedures"][0]["steps"][0].get("side_effect").is_none());
}

#[test]
fn floats_are_refused() {
    let err = bundle_hash(&json!({"a": [1, {"t": 1.5}]}));
    match err {
        Err(HashError::Float { path }) => assert_eq!(path, "/a/1/t"),
        other => panic!("expected a float error, got {other:?}"),
    }
    assert!(bundle_hash(&json!({"a": [1, -2, {"t": 3}]})).is_ok());
}

#[test]
fn secret_ref_in_slot_does_not_parse() {
    let mut v = fixture();
    v["bundle"]["connectors"][0]["secret_ref"] = json!("k/v");
    assert!(serde_json::from_value::<ReleaseBodyV1>(v).is_err());
}

#[test]
fn release_body_embeds_its_hash() {
    let body = typed();
    let out = release_body("cfg-2026-10-18.1", "customer_care_agent", &body.bundle)
        .expect("release body");
    assert_eq!(out["schema"], SCHEMA_V1);
    assert_eq!(out["release_id"], "cfg-2026-10-18.1");
    assert_eq!(out["job"], "customer_care_agent");
    assert_eq!(out["hash"], body.hash.as_str());
    assert_eq!(out["bundle"], fixture()["bundle"]);
    // The body itself parses back as a v1 release body.
    let again: ReleaseBodyV1 = serde_json::from_value(out).expect("parses back");
    assert_eq!(again.bundle, body.bundle);
}

fn retail_customer_care() -> Selection {
    Selection {
        industry: Some("retail".into()),
        departments: ["faq", "logistics", "order-support", "payments"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        job: Some("customer_care_agent".into()),
        ..Selection::default()
    }
}

/// Seed v1 + v2 compiled for `sel`, with `v1_only` leaving v2 out.
fn compile_seeds(sel: &Selection, v1_only: bool) -> EffectiveConfig {
    let v1 = seed::parse(1, V1).unwrap_or_else(|e| panic!("seed v1: {e}"));
    let (rules, mut rows) = seed::to_rows(&v1);
    if !v1_only {
        let v2 = seed::parse(2, V2).unwrap_or_else(|e| panic!("seed v2: {e}"));
        rows.extend(seed::to_rows(&v2).1);
    }
    compile(sel, &v1.known(), &rules, &rows).unwrap_or_else(|e| panic!("compile: {e:#?}"))
}

fn canon<T: Serialize>(v: &T) -> String {
    canonical_json(&serde_json::to_value(v).expect("serialises"))
}

#[test]
fn seed_v2_job_content_equals_fixture_sections() {
    let cfg = compile_seeds(&retail_customer_care(), false);
    let jc = &cfg.job_content;
    let want = typed().bundle;

    let mut spaces: Vec<_> = jc.context_spaces.values().cloned().collect();
    let mut want_spaces = want.context_spaces.clone();
    spaces.sort_by(|a, b| a.key.cmp(&b.key));
    want_spaces.sort_by(|a, b| a.key.cmp(&b.key));
    assert_eq!(spaces.len(), 5);
    assert_eq!(canon(&spaces), canon(&want_spaces));

    let mut procedures: Vec<_> = jc.procedures.values().cloned().collect();
    let mut want_procedures = want.procedures.clone();
    procedures.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    want_procedures.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    assert_eq!(procedures.len(), 6);
    assert_eq!(canon(&procedures), canon(&want_procedures));

    let connectors: Vec<_> = jc.connectors.values().cloned().collect();
    assert_eq!(canon(&connectors), canon(&want.connectors));

    let mut stores: Vec<_> = jc.knowledge_stores.values().cloned().collect();
    let mut want_stores = want.knowledge_stores.clone();
    stores.sort_by(|a, b| a.key.cmp(&b.key));
    want_stores.sort_by(|a, b| a.key.cmp(&b.key));
    assert_eq!(canon(&stores), canon(&want_stores));

    let job = cfg.job.as_ref().expect("job compiled");
    assert_eq!(job.preamble, want.job_settings.preamble);
    assert_eq!(job.actor_label, want.job_settings.actor);
    // The stance stays seed v1's.
    let v1_job = compile_seeds(&retail_customer_care(), true)
        .job
        .expect("v1 job");
    assert_eq!(job.stance, v1_job.stance);
    assert!(cfg.warnings.is_empty(), "{:?}", cfg.warnings);
}
