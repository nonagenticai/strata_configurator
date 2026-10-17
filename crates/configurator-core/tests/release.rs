//! care-bus release bundle v1: hash parity with care-bus's golden fixture
//! (`crates/carebus-api/tests/fixtures/release-bundle-v1.json` on care-bus-agents
//! `feat/w3-multitenant`, copied byte for byte).

use configurator_core::release::{HashError, ReleaseBodyV1, SCHEMA_V1, bundle_hash, release_body};
use serde_json::{Value, json};

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
