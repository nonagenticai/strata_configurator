//! The care-bus client (W4) against the local care-bus mock: bearer token, tenant header, error
//! classification, token caching. No database.
mod common;

use common::carebus_mock::{CB_SECRET, CB_TOKEN, CarebusMock};
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
        format!("/api/tenants/{t}/users/alice%40example.test")
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
