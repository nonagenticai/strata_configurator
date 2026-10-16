use axum::body::Body;
use axum::http::{Request, StatusCode};
use configurator_api::config::Config;
use configurator_api::routes;
use configurator_api::state::AppState;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn config(database_url: &'static str) -> Config {
    Config::from_lookup(|k| {
        match k {
            "DATABASE_URL" => Some(database_url),
            "KEYCLOAK_ISSUER" => Some("https://issuer.test/realms/portal"),
            "KEYCLOAK_JWKS_URL" => Some("https://issuer.test/certs"),
            _ => None,
        }
        .map(String::from)
    })
    .unwrap()
}

/// No database: the pool is lazy and these routes never touch it.
fn app() -> axum::Router {
    routes::router(AppState::lazy(config("postgres://unused@localhost/unused")).unwrap())
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

#[tokio::test]
async fn ready_reports_db_unavailable() {
    // Nothing listens on port 1: the ping fails within the pool's acquire timeout.
    let state = AppState::lazy(config("postgres://u:p@127.0.0.1:1/none")).unwrap();
    let req = Request::builder()
        .uri("/ready")
        .body(Body::empty())
        .unwrap();
    let resp = routes::router(state).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["error"]["code"], "db_unavailable");
}
