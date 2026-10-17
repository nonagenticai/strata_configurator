//! Golden tests for seed v1 (spec 4.6 / 10.5): the retail customer-care compile
//! contains care-bus's 0002 ontology seed, with the recorded model deltas.

use configurator_core::carebus::project_carebus;
use configurator_core::compile::{EffectiveConfig, Selection, compile};
use configurator_core::seed::{self, SeedBundle};
use serde_json::Value;

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

const FIXTURE: &str = include_str!("fixtures/carebus_0002_ontology.json");

fn v1() -> SeedBundle {
    match seed::parse(1, V1) {
        Ok(b) => b,
        Err(e) => panic!("seed v1 does not parse: {e}"),
    }
}

fn compile_v1(b: &SeedBundle, sel: &Selection) -> EffectiveConfig {
    let (rules, rows) = seed::to_rows(b);
    match compile(sel, &b.known(), &rules, &rows) {
        Ok(cfg) => cfg,
        Err(errors) => panic!("seed v1 does not compile: {errors:#?}"),
    }
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

fn electricity(b: &SeedBundle) -> Selection {
    Selection {
        profile: b
            .profile("electricity")
            .cloned()
            .expect("electricity profile template"),
        job: Some("customer_care_agent".into()),
        ..Selection::default()
    }
}

fn section<'a>(doc: &'a Value, name: &str) -> &'a Vec<Value> {
    doc[name]
        .as_array()
        .unwrap_or_else(|| panic!("{name} is not an array"))
}

/// Rewrite the fixture's care-bus rows named in `_deltas` from `from` to `to`
/// (scope/industry), asserting each row really is in its `from` state.
/// Returns the moved entity names.
fn apply_deltas(fixture: &mut Value) -> Vec<String> {
    let deltas = section(fixture, "_deltas").clone();
    let entities = fixture["entities"]
        .as_array_mut()
        .expect("entities is an array");
    let mut moved = Vec::new();
    for d in &deltas {
        let name = d["entity"].as_str().expect("delta names an entity");
        let row = entities
            .iter_mut()
            .find(|e| e["name"] == name)
            .unwrap_or_else(|| panic!("delta entity {name} not in fixture"));
        for field in ["scope", "industry"] {
            assert_eq!(
                row[field], d["from"][field],
                "{name}.{field}: fixture row is not in the delta's from state"
            );
            row[field] = d["to"][field].clone();
        }
        moved.push(name.to_owned());
    }
    moved
}

#[test]
fn golden_retail_customer_care_contains_carebus_0002() {
    let b = v1();
    let cfg = compile_v1(&b, &retail_customer_care());
    assert_eq!(cfg.hash.len(), 64);
    println!("golden retail customer_care_agent hash: {}", cfg.hash);

    let mut fixture: Value = serde_json::from_str(FIXTURE).expect("fixture is JSON");
    let got = serde_json::to_value(project_carebus(&cfg)).expect("projection serializes");

    // The only recorded model delta: Order, Product and Payment move to base.
    let mut moved = apply_deltas(&mut fixture);
    moved.sort_unstable();
    assert_eq!(moved, ["Order", "Payment", "Product"]);

    let sections: [(&str, &[&str], usize); 3] = [
        ("entities", &["name"], 11),
        ("attributes", &["entity", "name"], 37),
        ("relations", &["subject", "predicate", "object"], 10),
    ];
    for (name, id, count) in sections {
        let want = section(&fixture, name);
        assert_eq!(want.len(), count, "fixture {name} count");
        let have = section(&got, name);
        for row in want {
            let found = have.iter().find(|h| id.iter().all(|f| h[*f] == row[*f]));
            assert_eq!(
                found,
                Some(row),
                "{name}: care-bus 0002 row missing or different"
            );
        }
    }
}

#[test]
fn electricity_profile_adds_metering_and_renewal() {
    let b = v1();
    let cfg = compile_v1(&b, &electricity(&b));
    for entity in ["Contract", "Bill", "MeterPoint", "Renewal"] {
        assert!(cfg.entities.contains_key(entity), "entity {entity} missing");
    }
    assert!(cfg.attributes.contains_key("Bill.meter_reading"));
    assert!(cfg.attributes.contains_key("Renewal.notice_period_days"));
    assert!(cfg.relations.contains_key("Order|meteredBy|MeterPoint"));
    // Retail is not selected: its overlay must not leak into this compile.
    assert!(!cfg.entities.contains_key("Shipment"));
}

#[test]
fn seed_render_parse_round_trip() {
    let b = v1();
    let files = seed::render(&b);
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect();
    let again = match seed::parse(1, &refs) {
        Ok(again) => again,
        Err(e) => panic!("rendered seed does not parse: {e}"),
    };
    assert_eq!(again, b);
    // Labels and descriptions survive render -> parse (spec 4.3 axis/axis_value label).
    let customer_type = again
        .axes
        .iter()
        .find(|a| a.key == "customer_type")
        .expect("customer_type axis");
    assert_eq!(customer_type.label.as_deref(), Some("Customer type"));
    assert!(customer_type.description.is_some());
    assert_eq!(
        customer_type.values[0].label.as_deref(),
        Some("Natural person")
    );
}

#[test]
fn seed_v1_has_no_warnings() {
    let b = v1();
    for sel in [retail_customer_care(), electricity(&b)] {
        let cfg = compile_v1(&b, &sel);
        assert!(cfg.warnings.is_empty(), "{sel:?}: {:?}", cfg.warnings);
    }
}

/// The golden hash before W4 (main, CI run 39076619471). Seed v1 has no job
/// content and no job preamble/actor_label, so adding them must not move it.
#[test]
fn phase1_hash_unchanged() {
    let b = v1();
    let cfg = compile_v1(&b, &retail_customer_care());
    assert!(cfg.job_content.is_empty());
    assert_eq!(
        cfg.hash,
        "7a0f9b6ba479f368ef2e8bae39e84653598e8a04941efed30b4ba6a60e0c3125"
    );
}
