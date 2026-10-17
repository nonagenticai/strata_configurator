use super::keys::{super_admin, token};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use uuid::Uuid;

pub async fn request(
    app: &Router,
    method: &str,
    uri: &str,
    auth: &str,
    tenant: Option<Uuid>,
    body: Value,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {auth}"));
    if let Some(id) = tenant {
        req = req.header("x-tenant-id", id.to_string());
    }
    super::send(
        app,
        req.header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

pub async fn root(app: &Router, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    request(app, method, uri, &super_admin("root"), None, body).await
}

pub async fn tenant(app: &Router, name: &str) -> Uuid {
    let (s, b) = root(
        app,
        "POST",
        "/api/tenants",
        json!({"name": name, "first_admin_sub": name}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    Uuid::parse_str(b["id"].as_str().unwrap()).unwrap()
}

pub async fn member(
    app: &Router,
    method: &str,
    uri: &str,
    sub: &str,
    tenant: Uuid,
    body: Value,
) -> (StatusCode, Value) {
    request(app, method, uri, &token(sub, &[]), Some(tenant), body).await
}

pub async fn seeded() -> super::TestEnv {
    let e = super::env().await;
    configurator_api::seeder::apply(&e.state.db, &configurator_api::seeder::embedded().unwrap())
        .await
        .unwrap();
    e
}
