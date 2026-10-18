//! W1.1 invites by email and W2 licence gate 2, issuance, revoke on offboarding and the token at rest,
//! against a local mock of Keycloak's token and admin endpoints and the licences service (an axum
//! server on 127.0.0.1:0 in the test).
mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use common::client::{member, request, root};
use common::keys::super_admin;
use configurator_api::license_token::{TokenCipher, TokenKey};
use configurator_api::offboarding::revoke_tenant_license;
use serde_json::{Value, json};
use uuid::Uuid;

/// What the mock upstreams hold and saw.
#[derive(Default)]
struct Mock {
    /// Portal-realm users: (id, email); the username is the email.
    users: Vec<(String, String)>,
    /// Ids of the users created through the admin API.
    created: Vec<String>,
    /// (user id, actions) of every execute-actions-email.
    emails: Vec<(String, Vec<String>)>,
    /// Client-credentials grants answered.
    grants: usize,
    /// The introspection answer; 500 while `introspect_down`.
    introspection: Value,
    introspect_down: bool,
    /// Tokens introspected, in order.
    introspected: Vec<String>,
    /// The licence create answers 503 while set.
    issue_down: bool,
    /// The token mint answers 503 while set (after the licence was created).
    mint_down: bool,
    /// The token mint answers licences' 403 `long_lived_requires_license_admin` while set.
    mint_refused: bool,
    /// The query of every token mint, in order.
    minted: Vec<HashMap<String, String>>,
    /// (tenant header, holder sub) of every licence created.
    issued: Vec<(String, String)>,
    revoked: Vec<String>,
    /// The revoke answers 503 while set.
    revoke_down: bool,
    /// The revoke answers 200 `{"revoked": false}` (licences' answer for an unknown licence or a
    /// tenant mismatch) while set.
    revoke_noop: bool,
}

type Shared = Arc<Mutex<Mock>>;

fn lock(m: &Shared) -> MutexGuard<'_, Mock> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The SA token every mock admin endpoint requires.
const SA_TOKEN: &str = "sa-token";

fn authed(h: &HeaderMap) -> bool {
    h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) == Some("Bearer sa-token")
}

async fn grant(State(m): State<Shared>, body: String) -> Response {
    if !body.contains("grant_type=client_credentials") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    lock(&m).grants += 1;
    Json(json!({"access_token": SA_TOKEN, "expires_in": 300})).into_response()
}

async fn find_users(
    State(m): State<Shared>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !authed(&h) || q.get("exact").map(String::as_str) != Some("true") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let wanted = q.get("email").or_else(|| q.get("username")).cloned();
    let hits: Vec<Value> = lock(&m)
        .users
        .iter()
        .filter(|(_, e)| wanted.as_deref().is_some_and(|w| w.eq_ignore_ascii_case(e)))
        .map(|(id, e)| json!({"id": id, "username": e, "email": e}))
        .collect();
    Json(hits).into_response()
}

async fn create_user(State(m): State<Shared>, h: HeaderMap, Json(rep): Json<Value>) -> Response {
    if !authed(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    // The configurator must never write roles or attributes into the shared realm.
    if rep.get("realmRoles").is_some() || rep.get("attributes").is_some() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let email = rep["email"].as_str().unwrap_or_default().to_string();
    let id = Uuid::new_v4().to_string();
    let mut g = lock(&m);
    g.users.push((id.clone(), email));
    g.created.push(id.clone());
    (
        StatusCode::CREATED,
        [(
            header::LOCATION,
            format!("http://kc.test/admin/realms/portal/users/{id}"),
        )],
    )
        .into_response()
}

async fn action_email(
    State(m): State<Shared>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(actions): Json<Vec<String>>,
) -> StatusCode {
    if !authed(&h) {
        return StatusCode::UNAUTHORIZED;
    }
    lock(&m).emails.push((id, actions));
    StatusCode::NO_CONTENT
}

async fn introspect(State(m): State<Shared>, Json(b): Json<Value>) -> Response {
    let mut g = lock(&m);
    g.introspected
        .push(b["token"].as_str().unwrap_or_default().into());
    if g.introspect_down {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    Json(g.introspection.clone()).into_response()
}

async fn register(h: HeaderMap, Path(id): Path<String>, Json(b): Json<Value>) -> Response {
    if !authed(&h) || b["name"].as_str().is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({"id": id, "name": b["name"]})).into_response()
}

async fn get_app(h: HeaderMap) -> Response {
    if !authed(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({"id": "app-1", "key": "strata-configurator"})).into_response()
}

async fn plans(h: HeaderMap) -> Response {
    if !authed(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!([{"id": "plan-0", "code": "trial"}, {"id": "plan-1", "code": "standard"}]))
        .into_response()
}

fn tenant_header(h: &HeaderMap) -> String {
    h.get("x-tenant-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

async fn create_licence(
    State(m): State<Shared>,
    h: HeaderMap,
    Path(sub): Path<String>,
    Json(b): Json<Value>,
) -> Response {
    if !authed(&h) || b["app_id"] != "app-1" || b["plan_id"] != "plan-1" {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }
    let mut g = lock(&m);
    if g.issue_down {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    g.issued.push((tenant_header(&h), sub));
    Json(json!({"license_id": "lic-1", "subject_id": "subject-1"})).into_response()
}

async fn mint(
    State(m): State<Shared>,
    h: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !authed(&h) || id != "lic-1" {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut g = lock(&m);
    g.minted.push(q);
    if g.mint_down {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if g.mint_refused {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"detail": {"error": "long_lived_requires_license_admin"}})),
        )
            .into_response();
    }
    Json(json!({"token": format!("tok-{}", tenant_header(&h))})).into_response()
}

async fn revoke(State(m): State<Shared>, h: HeaderMap, Json(b): Json<Value>) -> Response {
    if !authed(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut g = lock(&m);
    if g.revoke_down {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if g.revoke_noop {
        return Json(json!({"revoked": false})).into_response();
    }
    g.revoked
        .push(b["license_id"].as_str().unwrap_or_default().into());
    Json(json!({"revoked": true})).into_response()
}

struct Upstream {
    url: String,
    mock: Shared,
}

async fn upstream() -> Upstream {
    let mock: Shared = Arc::default();
    let app = Router::new()
        .route("/token", post(grant))
        .route(
            "/admin/realms/portal/users",
            get(find_users).post(create_user),
        )
        .route(
            "/admin/realms/portal/users/{id}/execute-actions-email",
            put(action_email),
        )
        .route("/license/introspect", post(introspect))
        .route("/license/revoke", post(revoke))
        .route("/tenants/{id}", put(register))
        .route("/apps/{key}", get(get_app))
        .route("/apps/{key}/plans", get(plans))
        .route("/licenses/user/{sub}", post(create_licence))
        .route("/licenses/{id}/tokens", post(mint))
        .with_state(mock.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    Upstream { url, mock }
}

impl Upstream {
    fn keycloak_admin(&self) -> Vec<(&'static str, String)> {
        vec![
            ("KEYCLOAK_ADMIN_TOKEN_URL", format!("{}/token", self.url)),
            (
                "KEYCLOAK_ADMIN_CLIENT_ID",
                "strata-configurator-admin".into(),
            ),
            ("KEYCLOAK_ADMIN_CLIENT_SECRET", "kc-secret".into()),
            (
                "KEYCLOAK_ADMIN_API_URL",
                format!("{}/admin/realms/portal", self.url),
            ),
        ]
    }

    /// Gate 2 on, licences issued on tenant create.
    fn licensing(&self) -> Vec<(&'static str, String)> {
        vec![
            ("LICENSE_GATE_2_ENABLED", "true".into()),
            (
                "LICENSE_INTROSPECT_URL",
                format!("{}/license/introspect", self.url),
            ),
            ("LICENSES_ADMIN_TOKEN_URL", format!("{}/token", self.url)),
            (
                "LICENSES_ADMIN_CLIENT_ID",
                "strata-configurator-licenses-admin".into(),
            ),
            ("LICENSES_ADMIN_CLIENT_SECRET", "lic-secret".into()),
            ("LICENSES_ADMIN_URL", self.url.clone()),
            ("LICENSE_TOKEN_ENC_KEY", common::TEST_TOKEN_KEY.into()),
        ]
    }

    fn set_introspection(&self, v: Value) {
        lock(&self.mock).introspection = v;
    }
}

fn code(b: &Value) -> &Value {
    &b["error"]["code"]
}

/// A tenant created by the super-admin with `alice` as its first admin.
async fn tenant(e: &common::TestEnv) -> Uuid {
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/tenants",
        json!({"name": "Acme", "first_admin_sub": "alice"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    Uuid::parse_str(b["id"].as_str().unwrap()).unwrap()
}

async fn add(e: &common::TestEnv, t: Uuid, body: Value) -> (StatusCode, Value) {
    member(
        &e.app,
        "POST",
        &format!("/api/tenants/{t}/users"),
        "alice",
        body,
    )
    .await
}

async fn user_row(e: &common::TestEnv, sub: &str) -> Option<(Uuid, String)> {
    sqlx::query_as("SELECT tenant, tenant_role::text FROM users WHERE sub = $1")
        .bind(sub)
        .fetch_optional(common::pool(e))
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

/// The stored (sealed) licence token of `t`, read as the owner.
async fn stored_token(e: &common::TestEnv, t: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT token_encrypted FROM tenant_license WHERE tenant = $1")
        .bind(t)
        .fetch_optional(common::pool(e))
        .await
        .unwrap()
}

// ---- W1.1: invites ----

#[tokio::test]
async fn invite_by_email_creates_the_identity_and_uses_its_id_as_sub() {
    let up = upstream().await;
    let e = common::env_with(&up.keycloak_admin()).await;
    let t = tenant(&e).await;

    let (s, b) = add(
        &e,
        t,
        json!({"email": "new.person@example.com", "role": "tenant_user"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let created = lock(&up.mock).created.clone();
    assert_eq!(created.len(), 1, "one identity created");
    assert_eq!(
        b["sub"], created[0],
        "the users row is keyed by the Keycloak id"
    );
    assert_eq!(
        user_row(&e, &created[0]).await,
        Some((t, "tenant_user".to_string()))
    );
    assert_eq!(
        lock(&up.mock).emails,
        [(
            created[0].clone(),
            vec!["UPDATE_PASSWORD".to_string(), "VERIFY_EMAIL".to_string()]
        )],
        "the new identity is sent the action email"
    );

    // a tenant's first admin can be invited the same way
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/tenants",
        json!({"name": "Beta", "first_admin_email": "first.admin@example.com"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let beta = Uuid::parse_str(b["id"].as_str().unwrap()).unwrap();
    let created = lock(&up.mock).created.clone();
    assert_eq!(created.len(), 2);
    assert_eq!(
        user_row(&e, &created[1]).await,
        Some((beta, "tenant_admin".to_string()))
    );
    assert_eq!(lock(&up.mock).emails.len(), 2);
}

#[tokio::test]
async fn an_existing_identity_is_reused_without_an_email() {
    let up = upstream().await;
    lock(&up.mock)
        .users
        .push(("kc-existing".into(), "known@example.com".into()));
    let e = common::env_with(&up.keycloak_admin()).await;
    let t = tenant(&e).await;

    let (s, b) = add(
        &e,
        t,
        json!({"email": "Known@Example.com", "role": "tenant_admin"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(b["sub"], "kc-existing");
    assert_eq!(
        user_row(&e, "kc-existing").await,
        Some((t, "tenant_admin".to_string()))
    );
    {
        let g = lock(&up.mock);
        assert!(g.created.is_empty(), "no identity created: {:?}", g.created);
        assert!(g.emails.is_empty(), "no email sent: {:?}", g.emails);
    }

    // inviting them again is the usual 409, and still sends nothing
    let (s, b) = add(
        &e,
        t,
        json!({"email": "known@example.com", "role": "tenant_user"}),
    )
    .await;
    assert_eq!((s, code(&b)), (StatusCode::CONFLICT, &json!("user_exists")));
    assert!(lock(&up.mock).emails.is_empty());
}

#[tokio::test]
async fn exactly_one_of_sub_and_email() {
    let up = upstream().await;
    let e = common::env_with(&up.keycloak_admin()).await;
    let t = tenant(&e).await;
    for bad in [
        json!({"sub": "bob", "email": "bob@example.com", "role": "tenant_user"}),
        json!({"role": "tenant_user"}),
        json!({"email": "not-an-address", "role": "tenant_user"}),
    ] {
        let (s, b) = add(&e, t, bad.clone()).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::BAD_REQUEST, &json!("invalid_body")),
            "{bad}"
        );
    }
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/tenants",
        json!({"name": "X", "first_admin_sub": "x", "first_admin_email": "x@example.com"}),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, &json!("invalid_body"))
    );
    let g = lock(&up.mock);
    assert!(
        g.created.is_empty() && g.grants == 0,
        "Keycloak never called"
    );
}

#[tokio::test]
async fn email_without_the_admin_sa_is_503() {
    let e = common::env().await;
    let t = tenant(&e).await;
    let (s, b) = add(
        &e,
        t,
        json!({"email": "someone@example.com", "role": "tenant_user"}),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            &json!("invite_unavailable")
        )
    );
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/tenants",
        json!({"name": "Gamma", "first_admin_email": "someone@example.com"}),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            &json!("invite_unavailable")
        )
    );
    let tenants: i64 = sqlx::query_scalar("SELECT count(*) FROM tenant")
        .fetch_one(common::pool(&e))
        .await
        .unwrap();
    assert_eq!(tenants, 1, "nothing was created");
    // by sub it still works, as before
    let (s, _) = add(&e, t, json!({"sub": "bob", "role": "tenant_user"})).await;
    assert_eq!(s, StatusCode::CREATED);
}

// ---- W2: licence issuance and gate 2 ----

/// A licensed tenant (issued on create) whose introspection says it is active.
async fn licensed() -> (Upstream, common::TestEnv, Uuid) {
    let up = upstream().await;
    let e = common::env_with(&up.licensing()).await;
    let t = tenant(&e).await;
    up.set_introspection(json!({
        "active": true, "tenant_id": t, "app": {"key": "strata-configurator"},
        "expires_at": "2999-01-01T00:00:00Z"
    }));
    (up, e, t)
}

#[tokio::test]
async fn tenant_create_issues_and_stores_the_licence() {
    let (up, e, t) = licensed().await;
    assert_eq!(
        lock(&up.mock).issued,
        [(t.to_string(), "alice".to_string())],
        "issued for the new tenant, held by its first admin"
    );
    let sealed = stored_token(&e, t).await.expect("a licence row");
    assert!(sealed.starts_with("v1:"), "stored sealed: {sealed}");
    assert!(
        !sealed.contains(&format!("tok-{t}")),
        "the token is not stored in clear"
    );
    let cipher = e.state.token_cipher.as_deref().unwrap();
    assert_eq!(cipher.open(t, &sealed).unwrap(), format!("tok-{t}"));
    assert_eq!(audited(&e, "license.issue").await, 1);
    let want: HashMap<String, String> = [
        ("token_type", "internal"),
        ("long_lived", "true"),
        ("long_lived_owner", "strata-configurator"),
        (
            "long_lived_reason",
            "tenant licence token for gate 2; revoked on offboarding",
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert_eq!(
        lock(&up.mock).minted,
        [want],
        "one registered perpetual internal token, no expires_days"
    );
    // the runtime role cannot read licence tokens at all
    let denied = sqlx::query("SELECT token_encrypted FROM tenant_license")
        .fetch_all(&e.state.db)
        .await;
    assert!(denied.is_err(), "configurator_app read tenant_license");
}

#[tokio::test]
async fn failed_issuance_fails_the_create() {
    let up = upstream().await;
    lock(&up.mock).issue_down = true;
    let e = common::env_with(&up.licensing()).await;
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/tenants",
        json!({"name": "Acme", "first_admin_sub": "alice"}),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_GATEWAY, &json!("licences_unavailable"))
    );
    let tenants: i64 = sqlx::query_scalar("SELECT count(*) FROM tenant")
        .fetch_one(common::pool(&e))
        .await
        .unwrap();
    assert_eq!(tenants, 0, "the tenant was not created");
    assert_eq!(user_row(&e, "alice").await, None);
    assert_eq!(audited(&e, "license.issue_failed").await, 1);
}

#[tokio::test]
async fn a_failed_mint_revokes_the_created_licence() {
    let up = upstream().await;
    lock(&up.mock).mint_down = true;
    let e = common::env_with(&up.licensing()).await;
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/tenants",
        json!({"name": "Acme", "first_admin_sub": "alice"}),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_GATEWAY, &json!("licences_unavailable"))
    );
    {
        let g = lock(&up.mock);
        assert_eq!(g.issued.len(), 1, "the licence was created: {:?}", g.issued);
        assert_eq!(
            g.revoked,
            ["lic-1"],
            "the licence whose token could not be minted is revoked, once"
        );
    }
    let tenants: i64 = sqlx::query_scalar("SELECT count(*) FROM tenant")
        .fetch_one(common::pool(&e))
        .await
        .unwrap();
    assert_eq!(tenants, 0, "the tenant was not created");
}

#[tokio::test]
async fn a_refused_long_lived_mint_fails_the_create_and_revokes_the_licence() {
    let up = upstream().await;
    lock(&up.mock).mint_refused = true;
    let e = common::env_with(&up.licensing()).await;
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/tenants",
        json!({"name": "Acme", "first_admin_sub": "alice"}),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_GATEWAY, &json!("licences_unavailable"))
    );
    assert_eq!(lock(&up.mock).revoked, ["lic-1"]);
    let tenants: i64 = sqlx::query_scalar("SELECT count(*) FROM tenant")
        .fetch_one(common::pool(&e))
        .await
        .unwrap();
    assert_eq!(tenants, 0, "the tenant was not created");
    assert_eq!(audited(&e, "license.issue_failed").await, 1);
}

#[tokio::test]
async fn an_active_licence_passes_and_is_cached() {
    let (up, e, t) = licensed().await;
    for _ in 0..3 {
        let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
        assert_eq!(s, StatusCode::OK, "{b}");
    }
    let (s, b) = member(
        &e.app,
        "GET",
        &format!("/api/tenants/{t}/users"),
        "alice",
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        lock(&up.mock).introspected,
        [format!("tok-{t}")],
        "four gated requests, one introspection"
    );
}

#[tokio::test]
async fn an_inactive_licence_is_402() {
    let (up, e, t) = licensed().await;
    up.set_introspection(
        json!({"active": false, "tenant_id": t, "app": {"key": "strata-configurator"}}),
    );
    let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, &json!("license_inactive"))
    );
    let (s, b) = member(
        &e.app,
        "GET",
        &format!("/api/tenants/{t}/users"),
        "alice",
        Value::Null,
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, &json!("license_inactive"))
    );
}

#[tokio::test]
async fn a_licence_for_another_app_or_tenant_is_403() {
    let (up, e, t) = licensed().await;
    up.set_introspection(json!({"active": true, "tenant_id": t, "app": {"key": "commonpractice"}}));
    let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, &json!("license_app_mismatch"))
    );

    let (up, e, _) = licensed().await;
    up.set_introspection(json!({
        "active": true, "tenant_id": Uuid::new_v4(), "app": {"key": "strata-configurator"}
    }));
    let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, &json!("license_tenant_mismatch"))
    );
}

#[tokio::test]
async fn an_expired_licence_is_402() {
    let (up, e, t) = licensed().await;
    up.set_introspection(json!({
        "active": true, "tenant_id": t, "app": {"key": "strata-configurator"},
        "expires_at": "2020-01-01T00:00:00+00:00"
    }));
    let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, &json!("license_expired"))
    );

    // the shape licences sends (Python isoformat of a timestamptz, with microseconds), in a fresh
    // environment so the answer above is not reused from the cache
    let (up, e, t) = licensed().await;
    up.set_introspection(json!({
        "active": true, "tenant_id": t, "app": {"key": "strata-configurator"},
        "expires_at": "2026-01-01T00:00:00.123456+00:00"
    }));
    let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, &json!("license_expired"))
    );
}

#[tokio::test]
async fn an_unreadable_expiry_is_402_and_not_cached() {
    let (up, e, t) = licensed().await;
    up.set_introspection(json!({
        "active": true, "tenant_id": t, "app": {"key": "strata-configurator"},
        "expires_at": "not-a-timestamp"
    }));
    let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, &json!("license_unreadable")),
        "a present but unparseable expiry is not \"no expiry\""
    );
    // that answer was not cached: once licences sends a readable expiry, the request passes
    up.set_introspection(json!({
        "active": true, "tenant_id": t, "app": {"key": "strata-configurator"},
        "expires_at": "2999-01-01T00:00:00+00:00"
    }));
    let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(lock(&up.mock).introspected.len(), 2, "introspected again");
}

#[tokio::test]
async fn introspect_down_fails_closed() {
    let (up, e, _) = licensed().await;
    lock(&up.mock).introspect_down = true;
    let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, &json!("license_unavailable"))
    );
    // a failure is not cached: once the service answers again, the request passes
    lock(&up.mock).introspect_down = false;
    let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{b}");
}

#[tokio::test]
async fn a_tenant_without_a_licence_is_402_license_required() {
    // gate on, issuance off: tenants are created without a licence
    let up = upstream().await;
    let mut cfg = up.licensing();
    cfg.retain(|(k, _)| !k.starts_with("LICENSES_ADMIN_"));
    let e = common::env_with(&cfg).await;
    let _ = tenant(&e).await;
    let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, &json!("license_required"))
    );
    assert!(
        lock(&up.mock).introspected.is_empty(),
        "nothing to introspect"
    );
}

/// `GET uri` as the super-admin `root`, with `X-Active-Tenant-Id: tenant` when given.
async fn as_root(e: &common::TestEnv, uri: &str, tenant: Option<Uuid>) -> (StatusCode, Value) {
    request(
        &e.app,
        "GET",
        uri,
        &super_admin("root"),
        tenant,
        Value::Null,
    )
    .await
}

#[tokio::test]
async fn a_super_admin_acting_in_a_tenant_is_gated_like_its_members() {
    let (up, e, t) = licensed().await;
    up.set_introspection(
        json!({"active": false, "tenant_id": t, "app": {"key": "strata-configurator"}}),
    );
    let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    let refused = (StatusCode::PAYMENT_REQUIRED, json!("license_inactive"));
    assert_eq!((s, code(&b).clone()), refused, "control: the member");
    // the super-admin switched into that tenant sees the same refusal
    let (s, b) = as_root(&e, "/api/layers", Some(t)).await;
    assert_eq!((s, code(&b).clone()), refused, "{b}");
    // on a route whose path names the tenant too, judged by the scope acted in
    let (s, b) = as_root(&e, &format!("/api/tenants/{t}/users"), Some(t)).await;
    assert_eq!((s, code(&b).clone()), refused, "{b}");
    // a refused switch did no work in the tenant, so it is not audited as one
    assert_eq!(audited(&e, "tenant.scope_switch").await, 0);

    // an active licence lets the switch through, and it is audited
    let (up, e, t) = licensed().await;
    let (s, b) = as_root(&e, "/api/layers", Some(t)).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(audited(&e, "tenant.scope_switch").await, 1);
    assert_eq!(
        lock(&up.mock).introspected,
        [format!("tok-{t}")],
        "the super-admin's request was gated"
    );
}

#[tokio::test]
async fn the_super_admin_without_a_tenant_in_scope_is_not_gated() {
    let (up, e, t) = licensed().await;
    // every gate check would now be 402
    lock(&up.mock).introspect_down = true;
    let (s, b) = as_root(&e, "/api/tenants", None).await;
    assert_eq!(s, StatusCode::OK, "tenant list: {b}");
    let (s, b) = as_root(&e, "/api/layers", None).await;
    assert_eq!(s, StatusCode::OK, "platform view: {b}");
    let (s, b) = as_root(&e, &format!("/api/tenants/{t}/users"), None).await;
    assert_eq!(
        s,
        StatusCode::OK,
        "platform console on a tenant's users: {b}"
    );
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/tenants",
        json!({"name": "Beta", "first_admin_sub": "bob"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "tenant create: {b}");
    let seen = lock(&up.mock).introspected.clone();
    assert!(seen.is_empty(), "nothing was gated: {seen:?}");
    // control: the same moment, a member is refused
    let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, &json!("license_unavailable"))
    );
}

#[tokio::test]
async fn with_gate_2_off_the_super_admin_is_unchanged() {
    let up = upstream().await;
    let e = common::env().await;
    let t = tenant(&e).await;
    let (s, b) = as_root(&e, "/api/layers", Some(t)).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let (s, b) = as_root(&e, &format!("/api/tenants/{t}/users"), Some(t)).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(audited(&e, "tenant.scope_switch").await, 1);
    assert!(lock(&up.mock).introspected.is_empty());
}

// ---- licence token at rest ----

#[tokio::test]
async fn a_token_that_will_not_decrypt_is_402_license_unreadable() {
    let (up, e, t) = licensed().await;
    // the row sealed under another key, as after a rotated LICENSE_TOKEN_ENC_KEY
    let other = TokenKey::from_base64("//////////////////////////////////////////8=").unwrap();
    let sealed = TokenCipher::new(&other)
        .seal(t, &format!("tok-{t}"))
        .unwrap();
    sqlx::query("UPDATE tenant_license SET token_encrypted = $2 WHERE tenant = $1")
        .bind(t)
        .bind(sealed)
        .execute(common::pool(&e))
        .await
        .unwrap();
    let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, &json!("license_unreadable")),
        "an unreadable token is an operator fault, never a pass and never \"unavailable\""
    );
    assert!(
        lock(&up.mock).introspected.is_empty(),
        "nothing readable to introspect"
    );
    // a token in clear is refused by the schema itself
    let clear =
        sqlx::query("UPDATE tenant_license SET token_encrypted = 'tok-clear' WHERE tenant = $1")
            .bind(t)
            .execute(common::pool(&e))
            .await;
    assert!(clear.is_err(), "a plaintext token was stored");
}

#[tokio::test]
async fn the_encryption_migration_refuses_a_plaintext_token() {
    let pool = common::empty_pool().await;
    // every migration before 0006, then a row as 0004 stored it: the token in clear
    let before = sqlx::migrate::Migrator::with_migrations(
        configurator_api::db::MIGRATOR
            .iter()
            .filter(|m| m.version < 6)
            .cloned()
            .collect(),
    );
    before.run(&pool).await.unwrap();
    let t = Uuid::new_v4();
    sqlx::query("INSERT INTO tenant (id, name) VALUES ($1, 'Legacy')")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenant_license (tenant, license_id, subject_id, holder_sub, token) \
         VALUES ($1, 'lic-0', 'subject-0', 'alice', 'tok-clear')",
    )
    .bind(t)
    .execute(&pool)
    .await
    .unwrap();
    let err = configurator_api::db::migrate(&pool).await.unwrap_err();
    assert!(err.to_string().contains("plaintext"), "{err}");
    // once that licence is dealt with (revoked, row deleted), the migration applies
    sqlx::query("DELETE FROM tenant_license")
        .execute(&pool)
        .await
        .unwrap();
    configurator_api::db::migrate(&pool).await.unwrap();
}

// ---- revoke on offboarding ----

#[tokio::test]
async fn offboarding_revokes_the_licence_and_forgets_it() {
    let (up, e, t) = licensed().await;
    let revoked = revoke_tenant_license(&e.state, "root", t).await.unwrap();
    assert_eq!(revoked.as_deref(), Some("lic-1"));
    assert_eq!(lock(&up.mock).revoked, ["lic-1"], "revoked by id, once");
    assert_eq!(stored_token(&e, t).await, None, "the row is gone");
    assert_eq!(audited(&e, "license.revoke").await, 1);
    // the tenant is now unlicensed
    let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, &json!("license_required"))
    );
    // a second offboard has nothing left to revoke
    assert_eq!(
        revoke_tenant_license(&e.state, "root", t).await.unwrap(),
        None
    );
    assert_eq!(lock(&up.mock).revoked.len(), 1);
}

#[tokio::test]
async fn a_failed_revoke_fails_loud_and_keeps_the_licence() {
    for (down, noop) in [(true, false), (false, true)] {
        let (up, e, t) = licensed().await;
        {
            let mut g = lock(&up.mock);
            g.revoke_down = down;
            g.revoke_noop = noop;
        }
        let err = revoke_tenant_license(&e.state, "root", t)
            .await
            .unwrap_err();
        assert_eq!(
            err.status_and_code(),
            (StatusCode::BAD_GATEWAY, "license_provisioning_failed"),
            "down={down} noop={noop}"
        );
        assert!(
            stored_token(&e, t).await.is_some(),
            "the row is kept, so the offboard can be retried"
        );
        assert_eq!(audited(&e, "license.revoke_failed").await, 1);
        assert_eq!(audited(&e, "license.revoke").await, 0);

        // once licences answers, the retry revokes
        {
            let mut g = lock(&up.mock);
            g.revoke_down = false;
            g.revoke_noop = false;
        }
        let revoked = revoke_tenant_license(&e.state, "root", t).await.unwrap();
        assert_eq!(revoked.as_deref(), Some("lic-1"));
        assert_eq!(stored_token(&e, t).await, None);
    }
}

#[tokio::test]
async fn revoke_is_a_no_op_without_issuance_or_a_licence() {
    // issuance off (gate on): nothing to call
    let up = upstream().await;
    let mut cfg = up.licensing();
    cfg.retain(|(k, _)| !k.starts_with("LICENSES_ADMIN_"));
    let e = common::env_with(&cfg).await;
    let t = tenant(&e).await;
    assert_eq!(
        revoke_tenant_license(&e.state, "root", t).await.unwrap(),
        None
    );
    // issuance on, a tenant without a licence row
    let (_, e2, _) = licensed().await;
    assert_eq!(
        revoke_tenant_license(&e2.state, "root", Uuid::new_v4())
            .await
            .unwrap(),
        None
    );
    let g = lock(&up.mock);
    assert!(
        g.revoked.is_empty() && g.grants == 0,
        "licences never called"
    );
}

#[tokio::test]
async fn license_status_is_never_402() {
    let (_up, e, t) = licensed().await;
    let (s, b) = member(&e.app, "GET", "/api/license/status", "alice", Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        (&b["state"], &b["tenant"], &b["enforced"]),
        (&json!("active"), &json!(t), &json!(true))
    );

    // a fresh environment, so the cached `active` answer is not reused
    let (up, e, t) = licensed().await;
    up.set_introspection(
        json!({"active": false, "tenant_id": t, "app": {"key": "strata-configurator"}}),
    );
    let (s, b) = member(&e.app, "GET", "/api/license/status", "alice", Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        (&b["state"], &b["code"]),
        (&json!("blocked"), &json!("license_inactive"))
    );
    // while the gated routes are 402
    let (s, _) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);

    // someone with no workspace gets a state, not an error
    let (s, b) = member(&e.app, "GET", "/api/license/status", "nobody", Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["state"], "no_workspace");

    // with the gate off nothing is enforced and nothing is introspected
    let e = common::env().await;
    let _ = tenant(&e).await;
    let (s, b) = member(&e.app, "GET", "/api/license/status", "alice", Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        (&b["state"], &b["enforced"]),
        (&json!("not_enforced"), &json!(false))
    );
}

// ---- the offboard route: POST /api/tenants/{id}/offboard ----

async fn offboard(e: &common::TestEnv, t: Uuid, confirm: &str) -> (StatusCode, Value) {
    root(
        &e.app,
        "POST",
        &format!("/api/tenants/{t}/offboard"),
        json!({"confirm_name": confirm}),
    )
    .await
}

/// `t`'s `archived_at` and how many of its users are still active, read as the owner.
async fn tenant_state(e: &common::TestEnv, t: Uuid) -> (Option<String>, i64, i64) {
    sqlx::query_as(
        "SELECT to_jsonb(t.archived_at) #>> '{}', \
                (SELECT count(*) FROM users u WHERE u.tenant = t.id), \
                (SELECT count(*) FROM users u WHERE u.tenant = t.id AND u.disabled_at IS NULL) \
         FROM tenant t WHERE t.id = $1",
    )
    .bind(t)
    .fetch_one(common::pool(e))
    .await
    .unwrap()
}

fn listed(b: &Value) -> Vec<String> {
    b["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn offboarding_revokes_the_licence_then_archives_the_tenant() {
    let (up, e, t) = licensed().await;
    let (s, _) = add(&e, t, json!({"sub": "bob", "role": "tenant_user"})).await;
    assert_eq!(s, StatusCode::CREATED);

    let (s, b) = offboard(&e, t, "Acme").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["tenant"]["id"], json!(t));
    assert!(
        b["tenant"]["archived_at"].as_str().unwrap().contains('T'),
        "{b}"
    );
    assert_eq!(
        (
            &b["already_archived"],
            &b["license_revoked"],
            &b["users_disabled"]
        ),
        (&json!(false), &json!("lic-1"), &json!(2))
    );
    assert_eq!(lock(&up.mock).revoked, ["lic-1"], "revoked by id, once");
    assert_eq!(stored_token(&e, t).await, None, "the licence row is gone");

    // soft: the tenant and its users stay, archived and disabled
    let (archived_at, users, active) = tenant_state(&e, t).await;
    assert_eq!(archived_at.as_deref(), b["tenant"]["archived_at"].as_str());
    assert_eq!((users, active), (2, 0), "users are disabled, never deleted");
    assert_eq!(audited(&e, "license.revoke").await, 1);
    assert_eq!(audited(&e, "tenant.offboard").await, 1);

    // hidden from the tenant list unless asked for
    let (s, all) = as_root(&e, "/api/tenants", None).await;
    assert_eq!(s, StatusCode::OK, "{all}");
    assert!(listed(&all).is_empty(), "{all}");
    let (s, all) = as_root(&e, "/api/tenants?include_archived=true", None).await;
    assert_eq!(s, StatusCode::OK, "{all}");
    assert_eq!(listed(&all), [t.to_string()]);
    assert_eq!(all["items"][0]["archived_at"], b["tenant"]["archived_at"]);
    let (s, bad) = as_root(&e, "/api/tenants?include_archived=yes", None).await;
    assert_eq!(
        (s, code(&bad)),
        (StatusCode::BAD_REQUEST, &json!("invalid_query"))
    );
}

#[tokio::test]
async fn a_failed_licence_revoke_keeps_the_tenant_live() {
    for (down, noop) in [(true, false), (false, true)] {
        let (up, e, t) = licensed().await;
        {
            let mut g = lock(&up.mock);
            g.revoke_down = down;
            g.revoke_noop = noop;
        }
        let (s, b) = offboard(&e, t, "Acme").await;
        assert_eq!(
            (s, code(&b)),
            (
                StatusCode::BAD_GATEWAY,
                &json!("license_provisioning_failed")
            ),
            "down={down} noop={noop}: {b}"
        );
        assert!(
            b["error"]["message"].as_str().unwrap().contains("retry"),
            "{b}"
        );
        // nothing else changed: live, licensed, its users active
        assert!(
            stored_token(&e, t).await.is_some(),
            "the licence row is kept"
        );
        assert_eq!(tenant_state(&e, t).await, (None, 1, 1));
        assert_eq!(audited(&e, "license.revoke_failed").await, 1);
        assert_eq!(audited(&e, "tenant.offboard").await, 0);
        let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
        assert_eq!(s, StatusCode::OK, "the tenant still works: {b}");

        // once licences answers, the retry offboards
        {
            let mut g = lock(&up.mock);
            g.revoke_down = false;
            g.revoke_noop = false;
        }
        let (s, b) = offboard(&e, t, "Acme").await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["license_revoked"], "lic-1");
        assert_eq!(stored_token(&e, t).await, None);
    }
}

#[tokio::test]
async fn offboarding_an_archived_tenant_is_the_same_answer_without_licences() {
    let (up, e, t) = licensed().await;
    let (s, first) = offboard(&e, t, "Acme").await;
    assert_eq!(s, StatusCode::OK, "{first}");
    let calls = {
        let g = lock(&up.mock);
        (g.revoked.len(), g.grants)
    };
    assert_eq!(calls.0, 1);

    let (s, again) = offboard(&e, t, "Acme").await;
    assert_eq!(s, StatusCode::OK, "{again}");
    assert_eq!(again["tenant"], first["tenant"], "the original archived_at");
    assert_eq!(
        (
            &again["already_archived"],
            &again["license_revoked"],
            &again["users_disabled"]
        ),
        (&json!(true), &Value::Null, &json!(0))
    );
    let seen = {
        let g = lock(&up.mock);
        (g.revoked.len(), g.grants)
    };
    assert_eq!(
        seen, calls,
        "licences not called again (no revoke, no token grant)"
    );
    assert_eq!(
        audited(&e, "tenant.offboard").await,
        1,
        "nothing new written"
    );
    assert_eq!(audited(&e, "license.revoke").await, 1);
}

#[tokio::test]
async fn only_the_super_admin_offboards_and_only_when_confirmed() {
    let (up, e, t) = licensed().await;
    let uri = format!("/api/tenants/{t}/offboard");
    // the tenant's own admin: a platform route, 403
    let (s, b) = member(
        &e.app,
        "POST",
        &uri,
        "alice",
        json!({"confirm_name": "Acme"}),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, &json!("forbidden")),
        "{b}"
    );
    // the super-admin: the wrong name, an unknown tenant, a malformed id, a bad body
    let (s, b) = offboard(&e, t, "acme").await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, &json!("confirm_mismatch")),
        "{b}"
    );
    let (s, b) = offboard(&e, Uuid::new_v4(), "Acme").await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, &json!("tenant_not_found"))
    );
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/tenants/not-a-uuid/offboard",
        json!({"confirm_name": "Acme"}),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, &json!("invalid_tenant"))
    );
    let (s, b) = root(&e.app, "POST", &uri, json!({"name": "Acme"})).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, &json!("invalid_body"))
    );

    // none of that touched anything
    assert!(lock(&up.mock).revoked.is_empty(), "licences never called");
    assert!(stored_token(&e, t).await.is_some());
    assert_eq!(tenant_state(&e, t).await, (None, 1, 1));
    assert_eq!(audited(&e, "tenant.offboard").await, 0);
}

#[tokio::test]
async fn an_archived_tenant_is_refused_on_every_tenant_scoped_route() {
    let (_up, e, t) = licensed().await;
    let (s, b) = offboard(&e, t, "Acme").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let suspended = (StatusCode::FORBIDDEN, json!("tenant_suspended"));
    // the super-admin cannot switch into it, nor reach it by path
    let (s, b) = as_root(&e, "/api/layers", Some(t)).await;
    assert_eq!((s, code(&b).clone()), suspended, "switch: {b}");
    let (s, b) = as_root(&e, &format!("/api/tenants/{t}/users"), None).await;
    assert_eq!((s, code(&b).clone()), suspended, "by path: {b}");
    let (s, b) = root(
        &e.app,
        "POST",
        &format!("/api/tenants/{t}/users"),
        json!({"sub": "carol", "role": "tenant_user"}),
    )
    .await;
    assert_eq!((s, code(&b).clone()), suspended, "no one can be added: {b}");
    // its former members have no workspace any more, and see no tenant
    let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, &json!("no_workspace"))
    );
    let (s, b) = member(&e.app, "GET", "/api/tenants", "alice", Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    assert!(listed(&b).is_empty(), "{b}");
}

#[tokio::test]
async fn an_archived_tenant_with_an_active_user_is_still_refused() {
    // Defence in depth, gate 2 off, under RLS as `configurator_app`: a user row left active on an
    // archived tenant (by hand, or a future path) is refused by the tenant, not served.
    let e = common::env().await;
    let t = tenant(&e).await;
    let (s, _) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    assert_eq!(s, StatusCode::OK, "control");
    sqlx::query("UPDATE tenant SET archived_at = now() WHERE id = $1")
        .bind(t)
        .execute(common::pool(&e))
        .await
        .unwrap();
    let (s, b) = member(&e.app, "GET", "/api/layers", "alice", Value::Null).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, &json!("tenant_suspended")),
        "{b}"
    );
    let (s, b) = member(
        &e.app,
        "GET",
        &format!("/api/tenants/{t}/users"),
        "alice",
        Value::Null,
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, &json!("tenant_suspended")),
        "{b}"
    );
    // a repeat offboard completes it: the user left active is disabled, licences is not involved
    let (s, b) = offboard(&e, t, "Acme").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        (&b["already_archived"], &b["users_disabled"]),
        (&json!(true), &json!(1))
    );
    let (archived_at, users, active) = tenant_state(&e, t).await;
    assert!(archived_at.is_some());
    assert_eq!((users, active), (1, 0));
    assert_eq!(audited(&e, "tenant.offboard").await, 1);
}

// ---- W4: import an existing tenant id ----

const FR_TEST: &str = "0195f48b-d000-7000-8000-000000000099";

async fn import(e: &common::TestEnv, name: &str) -> (StatusCode, Value) {
    root(
        &e.app,
        "POST",
        "/api/tenants/import",
        json!({"id": FR_TEST, "name": name, "first_admin_sub": "fr-admin"}),
    )
    .await
}

fn fr() -> Uuid {
    Uuid::parse_str(FR_TEST).unwrap()
}

#[tokio::test]
async fn import_creates_tenant_with_fixed_id_and_licence() {
    let up = upstream().await;
    let e = common::env_with(&up.licensing()).await;
    let (s, b) = import(&e, "fr-test-user").await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(b["imported"], true);
    assert_eq!(b["tenant"]["id"], FR_TEST);
    assert_eq!(
        lock(&up.mock).issued,
        [(FR_TEST.to_string(), "fr-admin".to_string())]
    );
    assert!(stored_token(&e, fr()).await.is_some());
    assert_eq!(
        user_row(&e, "fr-admin").await,
        Some((fr(), "tenant_admin".into()))
    );
    assert_eq!(audited(&e, "tenant.import").await, 1);
    assert_eq!(audited(&e, "tenant.create").await, 0);
}

#[tokio::test]
async fn import_is_idempotent() {
    let up = upstream().await;
    let e = common::env_with(&up.licensing()).await;
    let (s, _) = import(&e, "fr-test-user").await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, b) = import(&e, "fr-test-user").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["imported"], false);
    assert_eq!(lock(&up.mock).issued.len(), 1, "no second licence");
}

#[tokio::test]
async fn import_without_licence_row_repairs() {
    let up = upstream().await;
    let e = common::env_with(&up.licensing()).await;
    import(&e, "fr-test-user").await;
    sqlx::query("DELETE FROM tenant_license WHERE tenant = $1")
        .bind(fr())
        .execute(common::pool(&e))
        .await
        .unwrap();
    let (s, b) = import(&e, "fr-test-user").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(lock(&up.mock).issued.len(), 2);
    assert!(stored_token(&e, fr()).await.is_some());
    assert_eq!(audited(&e, "license.issue").await, 2);
}

#[tokio::test]
async fn import_name_mismatch_is_409() {
    let up = upstream().await;
    let e = common::env_with(&up.licensing()).await;
    import(&e, "fr-test-user").await;
    let (s, b) = import(&e, "someone-else").await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::CONFLICT, &json!("tenant_exists"))
    );
}

#[tokio::test]
async fn import_licence_refused_creates_nothing() {
    let up = upstream().await;
    lock(&up.mock).issue_down = true;
    let e = common::env_with(&up.licensing()).await;
    let (s, b) = import(&e, "fr-test-user").await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_GATEWAY, &json!("licences_unavailable"))
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM tenant")
        .fetch_one(common::pool(&e))
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn import_requires_super_admin() {
    let e = common::env().await;
    tenant(&e).await;
    let (s, b) = member(
        &e.app,
        "POST",
        "/api/tenants/import",
        "alice",
        json!({"id": FR_TEST, "name": "x", "first_admin_sub": "y"}),
    )
    .await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, &json!("forbidden")));
}

#[tokio::test]
async fn nil_or_braced_id_is_400() {
    let e = common::env().await;
    for id in [
        "00000000-0000-0000-0000-000000000000",
        "{0195f48b-d000-7000-8000-000000000099}",
        "not-a-uuid",
    ] {
        let (s, b) = root(
            &e.app,
            "POST",
            "/api/tenants/import",
            json!({"id": id, "name": "x", "first_admin_sub": "y"}),
        )
        .await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::BAD_REQUEST, &json!("invalid_tenant")),
            "{id}"
        );
    }
}
