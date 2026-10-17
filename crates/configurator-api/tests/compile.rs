mod common;
use axum::http::StatusCode;
use common::client::*;
use configurator_core::{
    compile::{Selection, compile},
    layer::LayerKey,
    seed,
};
use serde_json::{Value, json};

fn golden() -> Value {
    json!({"industry":"retail", "departments":["faq","logistics","order-support","payments"], "job":"customer_care_agent"})
}

#[tokio::test]
async fn golden_selection_hash_matches_core() {
    let e = seeded().await;
    let b = configurator_api::seeder::embedded().unwrap().remove(0);
    let sel = Selection {
        industry: Some("retail".into()),
        departments: ["faq", "logistics", "order-support", "payments"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        job: Some("customer_care_agent".into()),
        ..Default::default()
    };
    let (rules, rows) = seed::to_rows(&b);
    let expected = compile(&sel, &b.known(), &rules, &rows).unwrap();
    let (s, actual) = root(&e.app, "POST", "/api/compile", golden()).await;
    assert_eq!(s, StatusCode::OK, "{actual}");
    assert_eq!(actual["hash"], expected.hash);
    println!("GOLDEN_HASH={}", expected.hash);
    assert_eq!(
        actual["provenance"]["entity:Order"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn unknown_industry_400_names_key() {
    let e = seeded().await;
    for b in [
        json!({"industry":"retial"}),
        json!({"departments":["missing"]}),
        json!({"job":"missing"}),
        json!({"job":"customer_care_agent","dimensions":{"channel":"missing"}}),
    ] {
        let (s, out) = root(&e.app, "POST", "/api/compile", b).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{out}");
        assert_eq!(out["error"]["code"], "invalid_selection");
        assert!(!out["errors"].as_array().unwrap().is_empty());
    }
}

#[tokio::test]
async fn tenant_patch_of_tombstoned_entity_422() {
    let e = seeded().await;
    tenant(&e.app, "alice").await;
    let (_, layer) = member(
        &e.app,
        "POST",
        "/api/layers",
        "alice",
        json!({"scope":"tenant"}),
    )
    .await;
    let uri = format!("/api/layers/{}/rows", layer["id"].as_str().unwrap());
    let (s, patch) = member(
        &e.app,
        "POST",
        &uri,
        "alice",
        json!({"kind":"entity", "key":"Order", "op":"patch", "fields":{"label":"Our order"}}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{patch}");
    let (_, base) = root(&e.app, "GET", "/api/layers?scope=base", Value::Null).await;
    let uri = format!(
        "/api/layers/{}/rows/entity/Order",
        base["items"][0]["id"].as_str().unwrap()
    );
    let (s, out) = root(
        &e.app,
        "PUT",
        &uri,
        json!({"op":"tombstone", "expected_rev":1}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    let (s, out) = member(&e.app, "POST", "/api/compile", "alice", json!({})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{out}");
    assert!(
        out["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|err| err["row_id"] == patch["id"] && err["code"] == "patch_missing")
    );
}

#[tokio::test]
async fn carebus_view_shape() {
    let e = seeded().await;
    let (s, out) = root(&e.app, "POST", "/api/compile?view=carebus", golden()).await;
    assert_eq!(s, StatusCode::OK, "{out}");
    for key in ["entities", "attributes", "relations"] {
        assert!(out[key].is_array());
    }
    assert!(
        out["entities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["name"] == "Order" && r["scope"] == "generic")
    );
}

#[tokio::test]
async fn tenant_layer_of_other_tenant_never_applied() {
    let e = seeded().await;
    let a = tenant(&e.app, "alice").await;
    tenant(&e.app, "bob").await;
    let (_, l) = member(
        &e.app,
        "POST",
        "/api/layers",
        "alice",
        json!({"scope":"tenant"}),
    )
    .await;
    let (s, out) = member(&e.app, "POST", &format!("/api/layers/{}/rows", l["id"].as_str().unwrap()), "alice", json!({"kind":"entity", "key":"Customer", "op":"patch", "fields":{"label":"Private label"}})).await;
    assert_eq!(s, StatusCode::CREATED, "{out}");
    let (_, own) = member(&e.app, "POST", "/api/compile", "alice", json!({})).await;
    let (s, other) = member(&e.app, "POST", "/api/compile", "bob", json!({})).await;
    assert_eq!(s, StatusCode::OK, "{other}");
    assert_eq!(own["entities"]["Customer"]["label"], "Private label");
    let label = other["entities"]["Customer"]["label"].as_str();
    assert!(
        label.is_some_and(|l| l != "Private label"),
        "bob compiles the Customer entity without alice's label: {label:?}"
    );
    let (s, _) = member(&e.app, "POST", "/api/compile", "bob", json!({"tenant":a})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn private_profile_and_dimension_never_compile_for_other_tenant() {
    let e = seeded().await;
    tenant(&e.app, "alice").await;
    tenant(&e.app, "bob").await;
    let (_, profile) = member(
        &e.app,
        "POST",
        "/api/profiles",
        "alice",
        json!({"name":"Private", "values":{}}),
    )
    .await;
    let (s, out) = member(
        &e.app,
        "POST",
        "/api/compile",
        "bob",
        json!({"profile_id":profile["id"]}),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{out}");
    let (s, out) = member(
        &e.app,
        "POST",
        "/api/dimensions/department/values",
        "alice",
        json!({"key":"private", "label":"Private"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{out}");
    let (s, out) = member(
        &e.app,
        "POST",
        "/api/compile",
        "bob",
        json!({"industry":"retail", "departments":["private"]}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{out}");
    let (s, out) = member(
        &e.app,
        "POST",
        "/api/compile",
        "alice",
        json!({"industry":"retail", "departments":["private"]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
}

#[tokio::test]
async fn export_round_trips() {
    let e = seeded().await;
    let (s, out) = root(
        &e.app,
        "POST",
        "/api/axes/payment_method/values",
        json!({"key":"export_test", "label":"Curated label", "description":"Curated description"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{out}");
    tenant(&e.app, "alice").await;
    let (s, _) = member(&e.app, "GET", "/api/platform/export", "alice", Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, out) = root(&e.app, "GET", "/api/platform/export", Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["version"], 2);
    let files: Vec<_> = out["files"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str().unwrap()))
        .collect();
    let parsed = seed::parse(2, &files).unwrap();
    let (_, exported) = seed::to_rows(&parsed);
    let keys: Vec<LayerKey> = parsed
        .layers
        .iter()
        .map(|l| l.layer.clone())
        .chain(parsed.rules.iter().map(|r| LayerKey {
            scope: configurator_core::layer::Scope::Profile,
            rule: Some(r.rule.key.clone()),
            industry: None,
            department: None,
            job: None,
            dimension: None,
            tenant: None,
        }))
        .collect();
    let current = configurator_api::db::rows::load_for_layers(&e.state.owner, &keys)
        .await
        .unwrap();
    let signature = |rows: Vec<configurator_core::overlay::OverlayRow>| {
        let mut values: Vec<_> = rows
            .into_iter()
            .map(|r| json!([r.layer, r.kind, r.key, r.op, r.fields]).to_string())
            .collect();
        values.sort();
        values
    };
    assert_eq!(signature(exported), signature(current));
    let value = parsed
        .axes
        .iter()
        .find(|a| a.key == "payment_method")
        .unwrap()
        .values
        .iter()
        .find(|v| v.key == "export_test")
        .unwrap();
    assert_eq!(value.label.as_deref(), Some("Curated label"));
    assert_eq!(value.description.as_deref(), Some("Curated description"));
}
