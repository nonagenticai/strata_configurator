use axum::body::Body;
use axum::http::{Request, StatusCode};
use configurator_api::config::Config;
use configurator_api::routes;
use configurator_api::state::AppState;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn app() -> axum::Router {
    let cfg = Config::from_lookup(|k| {
        match k {
            "DATABASE_URL" => Some("postgres://unused@localhost/unused"),
            "KEYCLOAK_ISSUER" => Some("https://issuer.test/realms/portal"),
            "KEYCLOAK_JWKS_URL" => Some("https://issuer.test/certs"),
            _ => None,
        }
        .map(String::from)
    })
    .unwrap();
    routes::router(AppState::new(cfg))
}

#[tokio::test]
async fn health_is_unauthenticated() {
    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let resp = app().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v, serde_json::json!({"status": "ok"}));
}

#[tokio::test]
async fn unknown_route_is_a_404_envelope() {
    let req = Request::builder()
        .uri("/api/nope")
        .body(Body::empty())
        .unwrap();
    let resp = app().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["error"]["code"], "not_found");
}
