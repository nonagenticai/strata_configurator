//! The care-bus client (W4) against the local care-bus mock: bearer token, tenant header, error
//! classification, token caching; and provision-down on tenant create, import and the retry route,
//! behind CAREBUS_PROVISION_ENABLED.
mod common;

use axum::http::StatusCode;
use common::carebus_mock::{CB_SECRET, CB_TOKEN, CarebusMock};
use common::client::root;
use configurator_api::carebus_client::{CarebusClient, CarebusError};
use configurator_api::config::CarebusConfig;
use configurator_core::release::{ReleaseBodyV1, release_body};
use serde_json::{Value, json};
use uuid::Uuid;

const FIXTURE: &str =
    include_str!("../../configurator-core/tests/fixtures/carebus-release-bundle-v1.json");
const JOB: &str = "customer_care_agent";
const RELEASE: &str = "cfg-2026-10-18.1";

fn client(mock: &CarebusMock) -> CarebusClient {
    let cfg = CarebusConfig {
        api_url: mock.url.clone(),
        token_url: format!("{}/token", mock.url),
        client_id: "strata-configurator-release-writer".into(),
        client_secret: CB_SECRET.into(),
    };
    CarebusClient::new(&cfg, reqwest::Client::new())
}

/// The golden fixture's bundle as our release `RELEASE`.
fn body() -> Value {
    let golden: ReleaseBodyV1 = serde_json::from_str(FIXTURE).expect("fixture parses");
    release_body(RELEASE, JOB, &golden.bundle).expect("release body")
}

#[tokio::test]
async fn client_sends_bearer_and_tenant_header() {
    let mock = CarebusMock::start().await;
    let c = client(&mock);
    let t = Uuid::new_v4();
    let r = c.put_tenant(t, "Acme", "active").await.expect("tenant");
    assert_eq!(r.status, 201);
    let r = c
        .put_release(t, JOB, RELEASE, &body())
        .await
        .expect("release");
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.body["applied"], true);
    assert_eq!(r.body["release_id"], RELEASE);

    let seen = mock.requests();
    assert_eq!(seen.len(), 2);
    for req in &seen {
        assert_eq!(
            req.auth.as_deref(),
            Some(format!("Bearer {CB_TOKEN}").as_str())
        );
    }
    assert_eq!(seen[0].path, format!("/api/tenants/{t}"));
    assert_eq!(
        seen[0].tenant, None,
        "provisioning names the tenant in the path"
    );
    assert_eq!(seen[0].body, json!({"name": "Acme", "status": "active"}));
    assert_eq!(seen[1].path, format!("/api/jobs/{JOB}/releases/{RELEASE}"));
    assert_eq!(seen[1].tenant, Some(t.to_string()));
    assert_eq!(seen[1].body, body());
}

#[tokio::test]
async fn put_job_never_sends_settings() {
    let mock = CarebusMock::start().await;
    let c = client(&mock);
    let t = Uuid::new_v4();
    c.put_job(t, JOB, "Customer care agent").await.expect("job");
    c.put_user(t, "alice@example.test", "tenant_admin")
        .await
        .expect("user");
    let seen = mock.requests();
    assert_eq!(seen[0].path, format!("/api/tenants/{t}/jobs/{JOB}"));
    assert_eq!(seen[0].body, json!({"label": "Customer care agent"}));
    assert_eq!(
        seen[1].path,
        // The client percent-encodes the sub; the mock records the decoded path.
        format!("/api/tenants/{t}/users/alice@example.test")
    );
    assert_eq!(seen[1].body, json!({"tenant_role": "tenant_admin"}));
}

#[tokio::test]
async fn release_404_is_none() {
    let mock = CarebusMock::start().await;
    let c = client(&mock);
    let t = Uuid::new_v4();
    assert_eq!(c.current_release(t, JOB).await.expect("read-back"), None);
    c.put_release(t, JOB, RELEASE, &body()).await.expect("push");
    let current = c
        .current_release(t, JOB)
        .await
        .expect("read-back")
        .expect("a current release");
    assert_eq!(current["release_id"], RELEASE);
    assert_eq!(current["hash"], body()["hash"]);
    // Another 404 (an unknown job) is not "no release".
    mock.answer(
        "current",
        404,
        json!({"error": {"code": "not_found", "message": "job not found"}}),
    );
    assert!(matches!(
        c.current_release(t, JOB).await,
        Err(CarebusError::Rejected { status: 404, .. })
    ));
}

#[tokio::test]
async fn rejected_carries_code() {
    let mock = CarebusMock::start().await;
    let c = client(&mock);
    mock.answer(
        "release",
        409,
        json!({"error": {"code": "release_hash_mismatch", "message": "x"}}),
    );
    match c.put_release(Uuid::new_v4(), JOB, RELEASE, &body()).await {
        Err(CarebusError::Rejected { status, code, .. }) => {
            assert_eq!(status, 409);
            assert_eq!(code.as_deref(), Some("release_hash_mismatch"));
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
    // A body with a wrong hash is refused by the mock as care-bus refuses it.
    mock.clear_answer("release");
    let mut bad = body();
    bad["hash"] = json!(format!("sha256:{}", "0".repeat(64)));
    match c.put_release(Uuid::new_v4(), JOB, RELEASE, &bad).await {
        Err(CarebusError::Rejected { status, code, .. }) => {
            assert_eq!(
                (status, code.as_deref()),
                (400, Some("bundle_hash_mismatch"))
            );
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[tokio::test]
async fn unavailable_on_5xx() {
    let mock = CarebusMock::start().await;
    let c = client(&mock);
    mock.answer("tenant", 503, json!({"error": {"code": "unavailable"}}));
    assert!(matches!(
        c.put_tenant(Uuid::new_v4(), "Acme", "active").await,
        Err(CarebusError::Unavailable(_))
    ));
    // Nothing listening at all is unavailable too.
    let gone = CarebusConfig {
        api_url: "http://127.0.0.1:9".into(),
        token_url: format!("{}/token", mock.url),
        client_id: "strata-configurator-release-writer".into(),
        client_secret: CB_SECRET.into(),
    };
    let c = CarebusClient::new(&gone, reqwest::Client::new());
    assert!(matches!(
        c.put_tenant(Uuid::new_v4(), "Acme", "active").await,
        Err(CarebusError::Unavailable(_))
    ));
}

#[tokio::test]
async fn token_cached_across_calls() {
    let mock = CarebusMock::start().await;
    let c = client(&mock);
    let t = Uuid::new_v4();
    c.put_tenant(t, "Acme", "active").await.expect("1");
    c.put_job(t, JOB, "Care").await.expect("2");
    c.current_release(t, JOB).await.expect("3");
    assert_eq!(mock.grants(), 1);
}

#[tokio::test]
async fn a_401_refreshes_the_token_once() {
    let mock = CarebusMock::start().await;
    let c = client(&mock);
    let t = Uuid::new_v4();
    c.put_tenant(t, "Acme", "active").await.expect("warm");
    mock.reject_tokens(1);
    c.put_tenant(t, "Acme", "active").await.expect("retried");
    assert_eq!(mock.grants(), 2);
    // Two 401s in a row: the retry is not repeated, the caller gets the refusal.
    mock.reject_tokens(2);
    assert!(matches!(
        c.put_tenant(t, "Acme", "active").await,
        Err(CarebusError::Rejected { status: 401, .. })
    ));
}

// ---- Task 9: provision-down on tenant create, import and the retry route ----

async fn env(mock: &CarebusMock, enabled: bool) -> common::TestEnv {
    let mut cfg = mock.config();
    if enabled {
        cfg.push(("CAREBUS_PROVISION_ENABLED", "true".into()));
    }
    common::env_with(&cfg).await
}

async fn provisioned_at_set(e: &common::TestEnv, t: &str) -> bool {
    sqlx::query_scalar("SELECT carebus_provisioned_at IS NOT NULL FROM tenant WHERE id = $1::uuid")
        .bind(t)
        .fetch_one(common::pool(e))
        .await
        .unwrap()
}

async fn audited(e: &common::TestEnv, action: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_event WHERE action = $1")
        .bind(action)
        .fetch_one(common::pool(e))
        .await
        .unwrap()
}

#[tokio::test]
async fn provision_off_by_default_makes_no_carebus_call() {
    let mock = CarebusMock::start().await;
    let e = env(&mock, false).await;
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/tenants",
        json!({"name": "Acme", "first_admin_sub": "alice"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(
        b["carebus"],
        json!({"ok": false, "skipped": true, "steps": []})
    );
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/tenants/import",
        json!({"id": "0195f48b-d000-7000-8000-000000000099", "name": "fr-test-user",
               "first_admin_sub": "fr-admin"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(b["carebus"]["skipped"], true);
    assert!(mock.requests().is_empty(), "no care-bus call while off");
    assert_eq!(audited(&e, "carebus.provision_failed").await, 0);
    let t = b["tenant"]["id"].as_str().unwrap();
    let (s, b) = root(
        &e.app,
        "POST",
        &format!("/api/tenants/{t}/carebus-provision"),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["error"]["code"], "carebus_provision_disabled");
}

#[tokio::test]
async fn create_provisions_down() {
    let mock = CarebusMock::start().await;
    let e = env(&mock, true).await;
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/tenants",
        json!({"name": "Acme", "first_admin_sub": "alice"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let t = b["id"].as_str().unwrap();
    assert_eq!(b["carebus"]["ok"], true, "{b}");
    assert_eq!(b["carebus"]["steps"].as_array().unwrap().len(), 2);
    let seen: Vec<(String, Value)> = mock
        .requests()
        .into_iter()
        .map(|r| (r.path, r.body))
        .collect();
    assert_eq!(
        seen,
        [
            (
                format!("/api/tenants/{t}"),
                json!({"name": "Acme", "status": "active"})
            ),
            (
                format!("/api/tenants/{t}/users/alice"),
                json!({"tenant_role": "tenant_admin"})
            ),
        ]
    );
    assert!(provisioned_at_set(&e, t).await);
}

#[tokio::test]
async fn create_survives_carebus_down() {
    let mock = CarebusMock::start().await;
    let e = env(&mock, true).await;
    mock.answer("tenant", 503, json!({}));
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/tenants",
        json!({"name": "Acme", "first_admin_sub": "alice"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(b["carebus"]["ok"], false);
    assert_eq!(b["carebus"]["skipped"], false);
    let t = b["id"].as_str().unwrap().to_owned();
    assert!(!provisioned_at_set(&e, &t).await);
    assert_eq!(audited(&e, "carebus.provision_failed").await, 1);

    // The retry route provisions once care-bus is back.
    mock.clear_answer("tenant");
    let (s, b) = root(
        &e.app,
        "POST",
        &format!("/api/tenants/{t}/carebus-provision"),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["ok"], true, "{b}");
    assert!(provisioned_at_set(&e, &t).await);
    let last = mock.requests().pop().unwrap();
    assert_eq!(last.path, format!("/api/tenants/{t}/users/alice"));
}

#[tokio::test]
async fn retry_route_needs_the_super_admin() {
    let mock = CarebusMock::start().await;
    let e = env(&mock, true).await;
    let (_, b) = root(
        &e.app,
        "POST",
        "/api/tenants",
        json!({"name": "Acme", "first_admin_sub": "alice"}),
    )
    .await;
    let t = b["id"].as_str().unwrap();
    let (s, _) = common::client::member(
        &e.app,
        "POST",
        &format!("/api/tenants/{t}/carebus-provision"),
        "alice",
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}
