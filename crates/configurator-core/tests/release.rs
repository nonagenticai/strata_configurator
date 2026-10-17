//! care-bus release bundle v1: hash parity with care-bus's golden fixture
//! (`crates/carebus-api/tests/fixtures/release-bundle-v1.json` on care-bus-agents
//! `feat/w3-multitenant`, copied byte for byte).

use configurator_core::canonical::canonical_json;
use configurator_core::compile::{EffectiveConfig, Selection, compile};
use configurator_core::compile::{Entity, Relation};
use configurator_core::release::{
    BuildError, BundleV1, HashError, ReleaseBodyV1, SCHEMA_V1, Violation, build_bundle,
    bundle_hash, release_body, validate,
};
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

// --- Task 4: build and validate -----------------------------------------------------

fn build_seeded() -> BundleV1 {
    let cfg = compile_seeds(&retail_customer_care(), false);
    build_bundle(&cfg).unwrap_or_else(|e| panic!("build: {e:#?}"))
}

fn entity_names(b: &BundleV1) -> Vec<&str> {
    b.ontology
        .entities
        .iter()
        .map(|e| e.name.as_str())
        .collect()
}

#[test]
fn seed_v2_builds_a_valid_bundle() {
    let b = build_seeded();
    assert_eq!(validate(&b), Vec::<Violation>::new());
    assert_eq!(b.context_spaces.len(), 5);
    assert_eq!(b.procedures.len(), 6);
    assert_eq!(b.connectors.len(), 1);
    assert_eq!(b.knowledge_stores.len(), 2);
    assert_eq!(b.job_settings.actor.as_deref(), Some("Customer care desk"));
    assert!(b.job_settings.preamble.is_some() && b.job_settings.stance.is_some());
    // Lists come sorted by natural key.
    let keys: Vec<&str> = b.context_spaces.iter().map(|s| s.key.as_str()).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted);
    let names = entity_names(&b);
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
    // It hashes, and the PUT body parses back as v1.
    let body = release_body("cfg-2026-10-18.1", "customer_care_agent", &b).expect("body");
    let again: ReleaseBodyV1 = serde_json::from_value(body).expect("parses back");
    assert_eq!(again.bundle, b);
}

#[test]
fn slice_closure_pulls_in_referenced_entities() {
    let cfg = compile_seeds(&retail_customer_care(), false);
    let slice = &cfg.job.as_ref().expect("job").ontology_slice;
    assert!(!slice.iter().any(|n| n == "ReturnAuthorization"));
    let b = build_bundle(&cfg).expect("builds");
    let names = entity_names(&b);
    for name in [
        "ReturnAuthorization",
        "WarrantyClaim",
        "Shipment",
        "Carrier",
        "Refund",
    ] {
        assert!(names.contains(&name), "{name} missing from {names:?}");
    }
    for name in slice {
        assert!(
            names.contains(&name.as_str()),
            "slice entity {name} missing"
        );
    }
}

#[test]
fn relations_need_both_ends() {
    let mut cfg = compile_seeds(&retail_customer_care(), false);
    cfg.entities.insert(
        "Ghost".into(),
        Entity {
            label: "Ghost".into(),
            synonyms: vec![],
            hidden: false,
        },
    );
    cfg.relations.insert(
        "Customer|haunts|Ghost".into(),
        Relation {
            subject: "Customer".into(),
            predicate: "haunts".into(),
            object: "Ghost".into(),
            cardinality: "1:n".into(),
        },
    );
    let b = build_bundle(&cfg).expect("builds");
    assert!(
        !entity_names(&b).contains(&"Ghost"),
        "Ghost is outside the slice"
    );
    let names = entity_names(&b);
    for r in &b.ontology.relations {
        assert!(names.contains(&r.subject.as_str()) && names.contains(&r.object.as_str()));
    }
    assert!(!b.ontology.relations.iter().any(|r| r.predicate == "haunts"));
}

#[test]
fn hidden_entities_are_dropped() {
    let mut cfg = compile_seeds(&retail_customer_care(), false);
    cfg.entities
        .get_mut("Communication")
        .expect("Communication in the seed")
        .hidden = true;
    let b = build_bundle(&cfg).expect("builds");
    assert!(!entity_names(&b).contains(&"Communication"));
    assert!(
        !b.ontology
            .relations
            .iter()
            .any(|r| r.subject == "Communication" || r.object == "Communication")
    );
}

#[test]
fn empty_job_content_is_refused() {
    let cfg = compile_seeds(&retail_customer_care(), true);
    match build_bundle(&cfg) {
        Err(BuildError::Incomplete(v)) => {
            let paths: Vec<&str> = v.iter().map(|x| x.path.as_str()).collect();
            assert_eq!(paths, ["/bundle/context_spaces", "/bundle/procedures"]);
        }
        other => panic!("expected Incomplete, got {other:?}"),
    }
    // No job at all is incomplete too.
    let sel = Selection {
        job: None,
        ..retail_customer_care()
    };
    assert!(matches!(
        build_bundle(&compile_seeds(&sel, false)),
        Err(BuildError::Incomplete(_))
    ));
}

#[test]
fn fixture_validates_clean() {
    assert_eq!(validate(&typed().bundle), Vec::<Violation>::new());
}

#[test]
fn violations_are_all_listed() {
    let mut v = fixture();
    v["bundle"]["context_spaces"][0]["procedure"] = json!("nope");
    v["bundle"]["ontology"]["relations"][0]["object"] = json!("Ghost");
    // return@1.4.0-beta PUBLISHED next to return@1.3.0: twice per name and per space.
    v["bundle"]["procedures"][1]["status"] = json!("PUBLISHED");
    v["bundle"]["ontology"]["entities"][0]["industry"] = json!("retail");
    v["bundle"]["connectors"][0]["field_mappings"][0]["attribute"] = json!("Customer.nope");
    v["bundle"]["knowledge_stores"][0]["parent"] = json!("faq-policies");
    v["bundle"]["procedures"][0]["steps"][1]["step_number"] = json!(1);
    v["bundle"]["connectors"][0]["timeout_ms"] = json!(50);
    let body: ReleaseBodyV1 = serde_json::from_value(v).expect("still parses");
    let paths: Vec<String> = validate(&body.bundle).into_iter().map(|x| x.path).collect();
    for want in [
        "/bundle/context_spaces/0/procedure",
        "/bundle/ontology/relations/0/object",
        "/bundle/procedures/1/status",
        "/bundle/ontology/entities/0/industry",
        "/bundle/connectors/0/field_mappings/0/attribute",
        "/bundle/knowledge_stores/0/parent",
        "/bundle/knowledge_stores/1/parent",
        "/bundle/procedures/0/steps/1/step_number",
        "/bundle/connectors/0/timeout_ms",
    ] {
        assert!(
            paths.contains(&want.to_owned()),
            "{want} missing in {paths:?}"
        );
    }
    assert_eq!(
        paths
            .iter()
            .filter(|p| p.as_str() == "/bundle/procedures/1/status")
            .count(),
        2
    );
}

#[test]
fn build_is_deterministic() {
    let a = serde_json::to_value(build_seeded()).expect("serialises");
    let b = serde_json::to_value(build_seeded()).expect("serialises");
    assert_eq!(
        bundle_hash(&a).expect("hash"),
        bundle_hash(&b).expect("hash")
    );
}
