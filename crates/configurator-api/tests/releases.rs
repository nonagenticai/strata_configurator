//! W4 release targets, builds, pushes to care-bus and drift, against the seeded database and the
//! local care-bus mock (`common::carebus_mock`).
mod common;

use axum::http::StatusCode;
use common::carebus_mock::CarebusMock;
use common::carebus_mock::{CB_SECRET, CB_TOKEN};
use common::client::{member, request, tenant};
use common::keys::super_admin;
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
    // Stored normalised: the job is the target's, empty dimensions are explicit.
    let mut stored = golden();
    stored["dimensions"] = json!({});
    assert_eq!(list["items"][0]["selection"], stored);
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

// --- Task 7: push ----------------------------------------------------------------------------------

/// `sub`'s tenant (created with `sub` as its first admin), a target for the golden selection and its
/// release 1: (tenant, target id, release).
async fn released(e: &common::TestEnv, sub: &str) -> (Uuid, String, Value) {
    let t = tenant(&e.app, sub).await;
    let target = create_target(e, sub, JOB, golden()).await;
    let (s, r) = build(e, sub, &target).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    (t, target, r)
}

async fn push(e: &common::TestEnv, sub: &str, release: &Value) -> (StatusCode, Value) {
    member(
        &e.app,
        "POST",
        &format!("/api/releases/{}/push", release["id"].as_str().unwrap()),
        sub,
        Value::Null,
    )
    .await
}

/// (step, ok, status, error_code) of one push step.
type Step = (String, bool, Option<i32>, Option<String>);

/// Every push step, in order.
async fn steps(e: &common::TestEnv) -> Vec<Step> {
    sqlx::query_as("SELECT step, ok, status, error_code FROM push_step ORDER BY id")
        .fetch_all(common::pool(e))
        .await
        .unwrap()
}

fn step(name: &str, ok: bool, status: Option<i32>, code: Option<&str>) -> Step {
    (name.into(), ok, status, code.map(str::to_owned))
}

#[tokio::test]
async fn push_provisions_then_puts_release() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    let (t, _, r) = released(&e, "alice").await;
    let (s, out) = push(&e, "alice", &r).await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["carebus"]["applied"], true);
    assert_eq!(out["carebus"]["release_id"], r["release_id"]);
    assert_eq!(out["steps"].as_array().unwrap().len(), 3);

    let seen = mock.requests();
    let rid = r["release_id"].as_str().unwrap();
    let paths: Vec<(&str, &str)> = seen
        .iter()
        .map(|q| (q.method.as_str(), q.path.as_str()))
        .collect();
    assert_eq!(
        paths,
        [
            ("PUT", format!("/api/tenants/{t}").as_str()),
            ("PUT", format!("/api/tenants/{t}/jobs/{JOB}").as_str()),
            ("PUT", format!("/api/jobs/{JOB}/releases/{rid}").as_str()),
        ]
    );
    assert_eq!(seen[0].body, json!({"name": "alice", "status": "active"}));
    assert_eq!(seen[1].body, json!({"label": "Customer care agent"}));
    assert_eq!(seen[2].tenant, Some(t.to_string()));
    assert_eq!(seen[2].body["hash"], r["hash"]);
    assert_eq!(
        bundle_hash(&seen[2].body["bundle"]).unwrap(),
        r["hash"].as_str().unwrap()
    );

    assert_eq!(
        steps(&e).await,
        [
            step("tenant", true, Some(201), None),
            step("job", true, Some(201), None),
            step("release", true, Some(201), None),
        ]
    );
    let hashes: Vec<Option<String>> =
        sqlx::query_scalar("SELECT request_hash FROM push_step ORDER BY id")
            .fetch_all(common::pool(&e))
            .await
            .unwrap();
    assert_eq!(hashes, [None, None, r["hash"].as_str().map(str::to_owned)]);
    let provisioned: bool =
        sqlx::query_scalar("SELECT carebus_provisioned_at IS NOT NULL FROM tenant WHERE id = $1")
            .bind(t)
            .fetch_one(common::pool(&e))
            .await
            .unwrap();
    assert!(provisioned);
    assert_eq!(
        count(
            &e,
            "SELECT count(*) FROM audit_event WHERE action = 'release.push'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn push_sends_release_tenant_header() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    released(&e, "alice").await;
    let (b, _, rb) = released(&e, "bob").await;
    // The super-admin, acting in B through the header, pushes B's release.
    let (s, out) = request(
        &e.app,
        "POST",
        &format!("/api/releases/{}/push", rb["id"].as_str().unwrap()),
        &super_admin("root"),
        Some(b),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    let seen = mock.requests();
    let put = seen
        .iter()
        .find(|q| q.path.starts_with("/api/jobs/"))
        .unwrap();
    assert_eq!(put.tenant, Some(b.to_string()));
    assert_eq!(seen[0].path, format!("/api/tenants/{b}"));
}

#[tokio::test]
async fn cross_tenant_release_is_404() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    let (_, _, ra) = released(&e, "alice").await;
    tenant(&e.app, "bob").await;
    let (s, b) = push(&e, "bob", &ra).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, &json!("release_not_found"))
    );
    assert!(mock.requests().is_empty(), "nothing reached care-bus");
    assert_eq!(count(&e, "SELECT count(*) FROM push_step").await, 0);
}

#[tokio::test]
async fn repush_records_noop() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    let (_, _, r) = released(&e, "alice").await;
    let (s, _) = push(&e, "alice", &r).await;
    assert_eq!(s, StatusCode::OK);
    let (s, out) = push(&e, "alice", &r).await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["carebus"]["applied"], false);
    assert_eq!(out["steps"].as_array().unwrap().len(), 6);
    assert_eq!(count(&e, "SELECT count(*) FROM push_step").await, 6);
    assert_eq!(count(&e, "SELECT count(*) FROM config_release").await, 1);
    let last = steps(&e).await.pop().unwrap();
    assert_eq!(last, step("release", true, Some(200), None));
}

#[tokio::test]
async fn conflicts_are_returned_and_stored() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    let (_, _, r) = released(&e, "alice").await;
    let conflict = json!({
        "id": 17, "kind": "base_removed", "object": "ontology_entity", "key": {"name": "Carrier"},
        "tenant_row": "override", "base_hash_before": format!("sha256:{}", "a".repeat(64)),
        "base_hash_after": null
    });
    mock.set_conflicts(vec![conflict.clone()]);
    let (s, out) = push(&e, "alice", &r).await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["carebus"]["conflicts"], json!([conflict]));
    let stored: Value =
        sqlx::query_scalar("SELECT response -> 'conflicts' FROM push_step WHERE step = 'release'")
            .fetch_one(common::pool(&e))
            .await
            .unwrap();
    assert_eq!(stored, json!([conflict]));
}

#[tokio::test]
async fn hash_mismatch_is_409() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    let (_, _, r) = released(&e, "alice").await;
    mock.answer(
        "release",
        409,
        json!({"error": {"code": "release_hash_mismatch", "message": "known release, other hash"}}),
    );
    let (s, b) = push(&e, "alice", &r).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::CONFLICT, &json!("carebus_release_conflict")),
        "{b}"
    );
    assert_eq!(
        steps(&e).await.pop().unwrap(),
        step("release", false, Some(409), Some("release_hash_mismatch"))
    );
    // Any other refusal is a 502 naming the step.
    mock.answer(
        "release",
        400,
        json!({"error": {"code": "invalid_bundle", "message": "x", "details": []}}),
    );
    let (s, b) = push(&e, "alice", &r).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_GATEWAY, &json!("carebus_rejected")),
        "{b}"
    );
    assert_eq!(
        b["errors"],
        json!([{"step": "release", "status": 400, "code": "invalid_bundle"}])
    );
}

#[tokio::test]
async fn carebus_down_is_502_and_recorded() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    let (_, _, r) = released(&e, "alice").await;
    mock.answer("release", 503, json!({"error": {"code": "unavailable"}}));
    let (s, b) = push(&e, "alice", &r).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_GATEWAY, &json!("carebus_unavailable")),
        "{b}"
    );
    assert_eq!(
        steps(&e).await,
        [
            step("tenant", true, Some(201), None),
            step("job", true, Some(201), None),
            step("release", false, None, Some("carebus_unavailable")),
        ]
    );
    // A failed provision stops before the release.
    mock.clear_answer("release");
    mock.answer("tenant", 503, json!({}));
    let (s, _) = push(&e, "alice", &r).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
    assert_eq!(
        steps(&e).await.pop().unwrap(),
        step("tenant", false, None, Some("carebus_unavailable"))
    );
}

#[tokio::test]
async fn unconfigured_is_503() {
    let e = common::client::seeded().await;
    let (_, _, r) = released(&e, "alice").await;
    let (s, b) = push(&e, "alice", &r).await;
    assert_eq!(
        (s, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            &json!("carebus_unconfigured")
        )
    );
    assert_eq!(count(&e, "SELECT count(*) FROM push_step").await, 0);
}

#[tokio::test]
async fn no_token_in_push_step_or_audit() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    let (_, _, r) = released(&e, "alice").await;
    let (s, out) = push(&e, "alice", &r).await;
    assert_eq!(s, StatusCode::OK);
    for secret in [CB_TOKEN, CB_SECRET] {
        assert!(!out.to_string().contains(secret), "response leaks {secret}");
        let n: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM push_step WHERE response::text LIKE '%' || $1 || '%') \
                  + (SELECT count(*) FROM audit_event WHERE coalesce(metadata::text, '') LIKE '%' || $1 || '%')",
        )
        .bind(secret)
        .fetch_one(common::pool(&e))
        .await
        .unwrap();
        assert_eq!(n, 0, "{secret} stored");
    }
}

// --- Task 8: drift ---------------------------------------------------------------------------------

async fn drift(e: &common::TestEnv, sub: &str, target: &str) -> Value {
    let (s, b) = member(
        &e.app,
        "GET",
        &format!("/api/targets/{target}/drift"),
        sub,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    b
}

#[tokio::test]
async fn in_sync_after_push() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    let (_, target, r) = released(&e, "alice").await;
    let (s, _) = push(&e, "alice", &r).await;
    assert_eq!(s, StatusCode::OK);
    let d = drift(&e, "alice", &target).await;
    assert_eq!(d["status"], "in_sync", "{d}");
    assert_eq!(d["carebus"]["release_id"], r["release_id"]);
    assert_eq!(d["carebus"]["hash"], r["hash"]);
    assert_eq!(d["last_pushed"]["release_id"], r["release_id"]);
    assert_eq!(d["latest_built"]["hash"], r["hash"]);
    assert_eq!(d["config_changed"], false);
    // The read-back names the tenant.
    let read = mock
        .requests()
        .into_iter()
        .find(|q| q.method == "GET")
        .unwrap();
    assert_eq!(read.path, format!("/api/jobs/{JOB}/releases/current"));
    assert!(read.tenant.is_some());
}

#[tokio::test]
async fn drift_when_carebus_holds_other_release() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    let (_, target, r) = released(&e, "alice").await;
    push(&e, "alice", &r).await;
    mock.answer(
        "current",
        200,
        json!({"release_id": "2026-10-20.1", "hash": format!("sha256:{}", "b".repeat(64)),
               "applied": true, "applied_at": "2026-10-20T00:00:00Z", "conflicts": []}),
    );
    let d = drift(&e, "alice", &target).await;
    assert_eq!(d["status"], "drift", "{d}");
    assert_eq!(d["carebus"]["release_id"], "2026-10-20.1");
    assert_eq!(d["last_pushed"]["release_id"], r["release_id"]);
}

#[tokio::test]
async fn never_pushed() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    let (_, target, r) = released(&e, "alice").await;
    let d = drift(&e, "alice", &target).await;
    assert_eq!(d["status"], "never_pushed", "{d}");
    assert_eq!(d["carebus"], Value::Null);
    assert_eq!(d["last_pushed"], Value::Null);
    assert_eq!(d["latest_built"]["release_id"], r["release_id"]);
    assert_eq!(d["config_changed"], false);
    // A tenant_user reads drift too.
    let t: Uuid = sqlx::query_scalar("SELECT id FROM tenant WHERE name = 'alice'")
        .fetch_one(common::pool(&e))
        .await
        .unwrap();
    let (s, _) = member(
        &e.app,
        "POST",
        &format!("/api/tenants/{t}/users"),
        "alice",
        json!({"sub": "carol", "role": "tenant_user"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(drift(&e, "carol", &target).await["status"], "never_pushed");
}

#[tokio::test]
async fn carebus_404_is_no_release() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    let (_, target, r) = released(&e, "alice").await;
    push(&e, "alice", &r).await;
    mock.answer(
        "current",
        404,
        json!({"error": {"code": "not_found", "message": "release not found"}}),
    );
    let d = drift(&e, "alice", &target).await;
    assert_eq!(d["status"], "carebus_has_no_release", "{d}");
    // care-bus down is a status, not a 502.
    mock.answer("current", 503, json!({}));
    let d = drift(&e, "alice", &target).await;
    assert_eq!(d["status"], "carebus_unavailable", "{d}");
    assert_eq!(d["carebus"], Value::Null);
}

#[tokio::test]
async fn config_changed_after_tenant_patch() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    let (_, target, r) = released(&e, "alice").await;
    push(&e, "alice", &r).await;
    patch_space_label(&e, "alice", "FAQ (Acme)").await;
    let d = drift(&e, "alice", &target).await;
    assert_eq!(d["config_changed"], true, "{d}");
    assert_eq!(d["status"], "in_sync", "nothing new was pushed");
}

#[tokio::test]
async fn open_conflicts_surface_in_drift() {
    let mock = CarebusMock::start().await;
    let e = seeded(&mock).await;
    let (_, target, r) = released(&e, "alice").await;
    let conflict = json!({"id": 3, "kind": "base_changed", "object": "context_space",
                          "key": {"key": "retail:faq:general"}, "tenant_row": "override",
                          "base_hash_before": null, "base_hash_after": null});
    mock.set_conflicts(vec![conflict.clone()]);
    push(&e, "alice", &r).await;
    let d = drift(&e, "alice", &target).await;
    assert_eq!(d["carebus"]["conflicts"], json!([conflict]), "{d}");
}
