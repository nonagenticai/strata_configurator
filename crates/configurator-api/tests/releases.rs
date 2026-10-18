//! W4 release targets, builds, pushes to care-bus and drift, against the seeded database and the
//! local care-bus mock (`common::carebus_mock`).
mod common;

use axum::http::StatusCode;
use common::carebus_mock::CarebusMock;
use common::client::{member, tenant};
use configurator_core::release::bundle_hash;
use serde_json::{Value, json};
use uuid::Uuid;

const JOB: &str = "customer_care_agent";

fn golden() -> Value {
    json!({"industry": "retail", "departments": ["faq", "logistics", "order-support", "payments"]})
}

/// Seeds v1 + v2 applied; the care-bus client points at `mock`.
async fn seeded(mock: &CarebusMock) -> common::TestEnv {
    let e = common::env_with(&mock.config()).await;
    configurator_api::seeder::apply(
        &e.state.owner,
        &configurator_api::seeder::embedded().unwrap(),
    )
    .await
    .unwrap();
    e
}

fn code(b: &Value) -> &Value {
    &b["error"]["code"]
}

async fn create_target(e: &common::TestEnv, sub: &str, job: &str, selection: Value) -> String {
    let (s, b) = member(
        &e.app,
        "POST",
        "/api/targets",
        sub,
        json!({"job": job, "selection": selection}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    b["id"].as_str().unwrap().to_owned()
}

async fn build(e: &common::TestEnv, sub: &str, target: &str) -> (StatusCode, Value) {
    member(
        &e.app,
        "POST",
        &format!("/api/targets/{target}/releases"),
        sub,
        Value::Null,
    )
    .await
}

async fn today(e: &common::TestEnv) -> String {
    sqlx::query_scalar("SELECT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD')")
        .fetch_one(common::pool(e))
        .await
        .unwrap()
}

async fn count(e: &common::TestEnv, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(common::pool(e))
        .await
        .unwrap()
}

/// A tenant-layer patch of one context space's label, as `sub`.
async fn patch_space_label(e: &common::TestEnv, sub: &str, label: &str) {
    let (s, l) = member(
        &e.app,
        "POST",
        "/api/layers",
        sub,
        json!({"scope": "tenant"}),
    )
    .await;
    let layer = if s == StatusCode::CREATED {
        l["id"].as_str().unwrap().to_owned()
    } else {
        // The tenant layer exists already: find it.
        let (_, list) = member(&e.app, "GET", "/api/layers?scope=tenant", sub, Value::Null).await;
        list["items"][0]["id"].as_str().unwrap().to_owned()
    };
    let (s, b) = member(
        &e.app,
        "POST",
        &format!("/api/layers/{layer}/rows"),
        sub,
        json!({"kind": "context_space", "key": "retail:faq:general", "op": "patch",
               "fields": {"label": label}}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
}

#[tokio::test]
async fn create_target_and_build_release_1() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    tenant(&e.app, "alice").await;
    let target = create_target(&e, "alice", JOB, golden()).await;

    let (s, r) = build(&e, "alice", &target).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!(r["release_id"], format!("cfg-{}.1", today(&e).await));
    let hash = r["hash"].as_str().unwrap();
    assert!(hash.starts_with("sha256:") && hash.len() == 71, "{hash}");

    let (s, shown) = member(
        &e.app,
        "GET",
        &format!("/api/releases/{}", r["id"].as_str().unwrap()),
        "alice",
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{shown}");
    let body = &shown["body"];
    assert_eq!(body["schema"], "care-bus.release/1");
    assert_eq!(body["release_id"], r["release_id"]);
    assert_eq!(body["job"], JOB);
    assert_eq!(body["hash"], hash);
    assert_eq!(bundle_hash(&body["bundle"]).unwrap(), hash);
    assert_eq!(body["bundle"]["procedures"].as_array().unwrap().len(), 6);
    assert_eq!(shown["push_steps"], json!([]));

    // One target per job.
    let (s, b) = member(
        &e.app,
        "POST",
        "/api/targets",
        "alice",
        json!({"job": JOB, "selection": golden()}),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::CONFLICT, &json!("target_exists"))
    );
    // An unknown industry is refused when the target is created.
    let (s, b) = member(
        &e.app,
        "POST",
        "/api/targets",
        "alice",
        json!({"job": "employee_care_agent", "selection": {"industry": "nowhere"}}),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, &json!("invalid_selection"))
    );
    let (s, list) = member(&e.app, "GET", "/api/targets", "alice", Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["items"][0]["selection"], golden());
}

#[tokio::test]
async fn rebuild_without_change_returns_same_release() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    tenant(&e.app, "alice").await;
    let target = create_target(&e, "alice", JOB, golden()).await;
    let (s, first) = build(&e, "alice", &target).await;
    assert_eq!(s, StatusCode::CREATED, "{first}");
    let (s, again) = build(&e, "alice", &target).await;
    assert_eq!(s, StatusCode::OK, "{again}");
    assert_eq!(again["id"], first["id"]);
    assert_eq!(again["release_id"], first["release_id"]);
    assert_eq!(count(&e, "SELECT count(*) FROM config_release").await, 1);
}

#[tokio::test]
async fn rebuild_after_tenant_patch_is_release_2() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    tenant(&e.app, "alice").await;
    let target = create_target(&e, "alice", JOB, golden()).await;
    let (_, first) = build(&e, "alice", &target).await;
    patch_space_label(&e, "alice", "FAQ (Acme)").await;
    let (s, second) = build(&e, "alice", &target).await;
    assert_eq!(s, StatusCode::CREATED, "{second}");
    assert_eq!(second["release_id"], format!("cfg-{}.2", today(&e).await));
    assert_ne!(second["hash"], first["hash"]);
    let (s, list) = member(
        &e.app,
        "GET",
        &format!("/api/targets/{target}/releases"),
        "alice",
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{list}");
    let ids: Vec<&Value> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| &r["release_id"])
        .collect();
    assert_eq!(
        ids,
        [&second["release_id"], &first["release_id"]],
        "newest first"
    );
}

#[tokio::test]
async fn build_without_job_content_is_422() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    tenant(&e.app, "alice").await;
    let target = create_target(&e, "alice", "employee_care_agent", json!({})).await;
    let (s, b) = build(&e, "alice", &target).await;
    assert_eq!(
        (s, code(&b)),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            &json!("bundle_incomplete")
        ),
        "{b}"
    );
    let paths: Vec<&str> = b["errors"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["path"].as_str())
        .collect();
    assert_eq!(paths, ["/bundle/context_spaces", "/bundle/procedures"]);
    assert_eq!(count(&e, "SELECT count(*) FROM config_release").await, 0);
}

#[tokio::test]
async fn tenant_user_cannot_build() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    let t = tenant(&e.app, "alice").await;
    let (s, b) = member(
        &e.app,
        "POST",
        &format!("/api/tenants/{t}/users"),
        "alice",
        json!({"sub": "carol", "role": "tenant_user"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let target = create_target(&e, "alice", JOB, golden()).await;
    let (s, b) = build(&e, "carol", &target).await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, &json!("forbidden")));
    let (s, b) = member(
        &e.app,
        "POST",
        "/api/targets",
        "carol",
        json!({"job": "employee_care_agent"}),
    )
    .await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, &json!("forbidden")));
    // Reading is fine.
    let (s, b) = member(&e.app, "GET", "/api/targets", "carol", Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn cross_tenant_target_is_404() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    tenant(&e.app, "alice").await;
    tenant(&e.app, "bob").await;
    let target = create_target(&e, "alice", JOB, golden()).await;
    let (_, r) = build(&e, "alice", &target).await;
    let (s, b) = build(&e, "bob", &target).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, &json!("target_not_found"))
    );
    let (s, b) = member(
        &e.app,
        "GET",
        &format!("/api/targets/{target}/releases"),
        "bob",
        Value::Null,
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, &json!("target_not_found"))
    );
    let (s, b) = member(
        &e.app,
        "GET",
        &format!("/api/releases/{}", r["id"].as_str().unwrap()),
        "bob",
        Value::Null,
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, &json!("release_not_found"))
    );
    let (_, list) = member(&e.app, "GET", "/api/targets", "bob", Value::Null).await;
    assert_eq!(list["items"], json!([]));
}

#[tokio::test]
async fn rls_hides_other_tenant_releases() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    let a = tenant(&e.app, "alice").await;
    let b = tenant(&e.app, "bob").await;
    let target = create_target(&e, "alice", JOB, golden()).await;
    let (s, _) = build(&e, "alice", &target).await;
    assert_eq!(s, StatusCode::CREATED);

    // Raw SQL as configurator_app (the runtime pool), under each tenant.
    let seen = |t: Uuid| {
        let db = e.state.db.clone();
        async move {
            let mut tx = configurator_api::db::tenant_tx(&db, t).await.unwrap();
            let mut out = Vec::new();
            for sql in [
                "SELECT count(*) FROM release_target",
                "SELECT count(*) FROM config_release",
                "SELECT count(*) FROM push_step",
            ] {
                let n: i64 = sqlx::query_scalar(sql).fetch_one(&mut *tx).await.unwrap();
                out.push(n);
            }
            out
        }
    };
    assert_eq!(seen(a).await, [1, 1, 0]);
    assert_eq!(seen(b).await, [0, 0, 0]);
    // Without a tenant the runtime role sees nothing.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM config_release")
        .fetch_one(&e.state.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
    // B cannot write a row for A.
    let mut tx = configurator_api::db::tenant_tx(&e.state.db, b)
        .await
        .unwrap();
    let denied = sqlx::query(
        "INSERT INTO release_target (tenant, job, selection, created_by) \
         VALUES ($1, 'employee_care_agent', '{}', 'bob')",
    )
    .bind(a)
    .execute(&mut *tx)
    .await;
    assert!(
        matches!(&denied, Err(sqlx::Error::Database(d)) if d.code().as_deref() == Some("42501")),
        "{denied:?}"
    );
}
