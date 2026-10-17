mod common;
use axum::http::StatusCode;
use common::client::*;
use serde_json::{Value, json};

#[tokio::test]
async fn axis_value_parent_must_exist() {
    let e = common::env().await;
    let (s, _) = root(
        &e.app,
        "POST",
        "/api/axes",
        json!({"key":"payment", "cardinality":"one"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/axes/payment/values",
        json!({"key":"wallet.mobile", "parent":"wallet"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    assert_eq!(b["error"]["code"], "invalid_axis");
    for body in [
        json!({"key":"wallet"}),
        json!({"key":"wallet.mobile", "parent":"wallet", "label":"Mobile", "description":"Phone wallet"}),
    ] {
        let (s, b) = root(&e.app, "POST", "/api/axes/payment/values", body).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
    }
    let (_, b) = root(&e.app, "GET", "/api/axes?limit=1", Value::Null).await;
    assert_eq!(b["items"][0]["values"][1]["label"], "Mobile");
}

#[tokio::test]
async fn rule_with_unknown_value_rejected() {
    let e = seeded().await;
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/axis-rules",
        json!({"key":"bad", "priority":10, "condition":{"eq":["order_type","missing"]}}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    assert_eq!(b["error"]["code"], "invalid_condition");
    assert!(b["error"]["message"].as_str().unwrap().contains("missing"));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM layer WHERE rule = 'bad'")
        .fetch_one(&e.state.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn rule_creates_profile_layer() {
    let e = seeded().await;
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/axis-rules",
        json!({"key":"extra", "priority":20, "condition":{"eq":["order_type","continuous"]}}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let uri = format!("/api/layers/{}", b["layer_id"].as_str().unwrap());
    let (_, layer) = root(&e.app, "GET", &uri, Value::Null).await;
    assert_eq!(layer["scope"], "profile");
    assert_eq!(layer["rule"], "extra");
}

#[tokio::test]
async fn tenant_dimension_value_invisible_to_other_tenant() {
    let e = seeded().await;
    let a = tenant(&e.app, "alice").await;
    let b = tenant(&e.app, "bob").await;
    let (s, body) = member(
        &e.app,
        "POST",
        "/api/dimensions/department/values",
        "alice",
        a,
        json!({"key":"private", "label":"Private"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    let (_, own) = member(
        &e.app,
        "GET",
        "/api/dimensions/department/values",
        "alice",
        a,
        Value::Null,
    )
    .await;
    let (_, other) = member(
        &e.app,
        "GET",
        "/api/dimensions/department/values",
        "bob",
        b,
        Value::Null,
    )
    .await;
    assert!(
        own["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["key"] == "private")
    );
    assert!(
        !other["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["key"] == "private")
    );
    let (_, platform) = root(
        &e.app,
        "GET",
        "/api/dimensions/department/values",
        Value::Null,
    )
    .await;
    assert!(
        !platform["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["key"] == "private")
    );
}

#[tokio::test]
async fn profile_one_axis_two_values_400() {
    let e = seeded().await;
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/profiles",
        json!({"name":"bad", "values":{"order_type":["discrete","continuous"]}}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    assert_eq!(b["error"]["code"], "invalid_profile");
}

#[tokio::test]
async fn seeded_electricity_template_listed() {
    let e = seeded().await;
    let (s, b) = root(&e.app, "GET", "/api/profiles", Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert!(
        b["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "electricity" && p["tenant"].is_null())
    );
}

#[tokio::test]
async fn profile_visibility_permissions_and_audit() {
    let e = seeded().await;
    let a = tenant(&e.app, "alice").await;
    let b = tenant(&e.app, "bob").await;
    let (_, p) = member(
        &e.app,
        "POST",
        "/api/profiles",
        "alice",
        a,
        json!({"name":"Private", "values":{}}),
    )
    .await;
    let uri = format!("/api/profiles/{}", p["id"].as_str().unwrap());
    for method in ["GET", "PUT"] {
        let (s, body) = member(
            &e.app,
            method,
            &uri,
            "bob",
            b,
            json!({"name":"stolen", "values":{}}),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{body}");
    }
    let (s, body) = member(
        &e.app,
        "PUT",
        &uri,
        "alice",
        a,
        json!({"name":"Updated", "values":{}}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let (s, _) = member(
        &e.app,
        "POST",
        "/api/axes",
        "alice",
        a,
        json!({"key":"no", "cardinality":"one"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_event WHERE tenant = $1 AND action LIKE 'profile.%'",
    )
    .bind(a)
    .fetch_one(&e.state.db)
    .await
    .unwrap();
    assert_eq!(count, 2);
    let (_, list) = member(&e.app, "GET", "/api/profiles", "alice", a, Value::Null).await;
    let template = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["tenant"].is_null())
        .unwrap();
    let (s, _) = member(
        &e.app,
        "PUT",
        &format!("/api/profiles/{}", template["id"].as_str().unwrap()),
        "alice",
        a,
        json!({"name":"no", "values":{}}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}
