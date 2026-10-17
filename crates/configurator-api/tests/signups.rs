//! W2.3 early-access signup: the intake (client-credentials guard, validation, de-dupe, queue ceiling,
//! Turnstile, Telegram) and the super-admin queue (list, approve through the tenant-create path,
//! reject), against local mocks of Keycloak, licences, Turnstile and Telegram (an axum server on
//! 127.0.0.1:0 in the test).
mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use common::client::{member, request, root};
use common::keys::{self, super_admin};
use serde_json::{Value, json};
use uuid::Uuid;

const INTAKE: &str = "/api/signup-approvals/intake";
/// The intake client and role defaults (`SIGNUP_INTAKE_AZP`, `SIGNUP_INTAKE_ROLE`).
const INTAKE_CLIENT: &str = "configurator-signup-intake";
const BOT_TOKEN: &str = "test-bot";

#[derive(Default)]
struct Mock {
    /// Portal-realm users: (id, email).
    users: Vec<(String, String)>,
    created: Vec<String>,
    emails: Vec<String>,
    /// (tenant header, holder sub) of every licence created.
    issued: Vec<(String, String)>,
    revoked: Vec<String>,
    /// Turnstile: the tokens verified; siteverify answers 500 while `turnstile_down`.
    verified: Vec<String>,
    turnstile_down: bool,
    /// Telegram: every accepted sendMessage payload; 500 while `telegram_down`. `telegram_calls`
    /// counts every request, refused ones included.
    telegram: Vec<Value>,
    telegram_down: bool,
    telegram_calls: usize,
}

type Shared = Arc<Mutex<Mock>>;

fn lock(m: &Shared) -> MutexGuard<'_, Mock> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn authed(h: &HeaderMap) -> bool {
    h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) == Some("Bearer sa-token")
}

async fn grant(body: String) -> Response {
    if !body.contains("grant_type=client_credentials") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    Json(json!({"access_token": "sa-token", "expires_in": 300})).into_response()
}

async fn find_users(
    State(m): State<Shared>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !authed(&h) {
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

async fn action_email(State(m): State<Shared>, h: HeaderMap, Path(id): Path<String>) -> StatusCode {
    if !authed(&h) {
        return StatusCode::UNAUTHORIZED;
    }
    lock(&m).emails.push(id);
    StatusCode::NO_CONTENT
}

async fn register(Path(id): Path<String>) -> Response {
    Json(json!({"id": id})).into_response()
}

async fn get_app() -> Response {
    Json(json!({"id": "app-1", "key": "strata-configurator"})).into_response()
}

async fn plans() -> Response {
    Json(json!([{"id": "plan-1", "code": "standard"}])).into_response()
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
) -> Response {
    lock(&m).issued.push((tenant_header(&h), sub));
    Json(json!({"license_id": "lic-1", "subject_id": "subject-1"})).into_response()
}

async fn mint(h: HeaderMap) -> Response {
    Json(json!({"token": format!("tok-{}", tenant_header(&h))})).into_response()
}

async fn revoke(State(m): State<Shared>, Json(b): Json<Value>) -> Response {
    lock(&m)
        .revoked
        .push(b["license_id"].as_str().unwrap_or_default().into());
    Json(json!({"revoked": true})).into_response()
}

async fn siteverify(State(m): State<Shared>, body: String) -> Response {
    let mut g = lock(&m);
    if g.turnstile_down {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if !body.contains("secret=ts-secret") {
        return Json(json!({"success": false, "error-codes": ["invalid-input-secret"]}))
            .into_response();
    }
    let token = body
        .split('&')
        .find_map(|kv| kv.strip_prefix("response="))
        .unwrap_or_default()
        .to_string();
    let ok = token == "good-token";
    g.verified.push(token);
    Json(json!({"success": ok})).into_response()
}

async fn send_message(State(m): State<Shared>, Json(b): Json<Value>) -> Response {
    let mut g = lock(&m);
    g.telegram_calls += 1;
    if g.telegram_down {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    g.telegram.push(b);
    Json(json!({"ok": true})).into_response()
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
        .route("/license/revoke", post(revoke))
        .route("/tenants/{id}", put(register))
        .route("/apps/{key}", get(get_app))
        .route("/apps/{key}/plans", get(plans))
        .route("/licenses/user/{sub}", post(create_licence))
        .route("/licenses/{id}/tokens", post(mint))
        .route("/siteverify", post(siteverify))
        .route(&format!("/bot{BOT_TOKEN}/sendMessage"), post(send_message))
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

    fn licences(&self) -> Vec<(&'static str, String)> {
        vec![
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

    fn turnstile(&self) -> Vec<(&'static str, String)> {
        vec![
            ("TURNSTILE_SECRET", "ts-secret".into()),
            ("TURNSTILE_VERIFY_URL", format!("{}/siteverify", self.url)),
        ]
    }

    fn telegram(&self) -> Vec<(&'static str, String)> {
        vec![
            ("TELEGRAM_BOT_TOKEN", BOT_TOKEN.into()),
            ("TELEGRAM_SIGNUP_CHAT_ID", "-100123".into()),
            ("TELEGRAM_SIGNUP_THREAD_ID", "42".into()),
            ("TELEGRAM_API_BASE", self.url.clone()),
            (
                "CONFIGURATOR_PUBLIC_URL",
                "https://configurator.test/".into(),
            ),
        ]
    }
}

/// An environment with early access switched on, plus `extra`.
async fn enabled(extra: Vec<(&'static str, String)>) -> common::TestEnv {
    let mut cfg = vec![("EARLY_ACCESS_SIGNUP_ENABLED", "true".to_string())];
    cfg.extend(extra);
    common::env_with(&cfg).await
}

/// A client-credentials token for `azp` with these client roles (on this app's client).
fn sa_token(azp: &str, roles: &[&str]) -> String {
    let mut c = keys::claims("sa-intake", roles);
    c["azp"] = json!(azp);
    keys::sign(&c)
}

fn intake_token() -> String {
    sa_token(INTAKE_CLIENT, &[INTAKE_CLIENT])
}

async fn submit(e: &common::TestEnv, body: Value) -> (StatusCode, Value) {
    request(&e.app, "POST", INTAKE, &intake_token(), None, body).await
}

fn signup(email: &str) -> Value {
    json!({"email": email, "name": "Ann Early", "company": "Globex", "source": "web"})
}

fn code(b: &Value) -> &Value {
    &b["error"]["code"]
}

async fn count(e: &common::TestEnv, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
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

/// The id of the only signup for `email`.
async fn signup_id(e: &common::TestEnv, email: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM signup_approvals WHERE email = $1")
        .bind(email)
        .fetch_one(common::pool(e))
        .await
        .unwrap()
}

async fn status_of(e: &common::TestEnv, id: Uuid) -> (String, Option<Uuid>, Option<String>) {
    sqlx::query_as("SELECT status, tenant, decided_by FROM signup_approvals WHERE id = $1")
        .bind(id)
        .fetch_one(common::pool(e))
        .await
        .unwrap()
}

async fn approve(e: &common::TestEnv, id: Uuid, body: Value) -> (StatusCode, Value) {
    root(
        &e.app,
        "POST",
        &format!("/api/signup-approvals/{id}/approve"),
        body,
    )
    .await
}

async fn reject(e: &common::TestEnv, id: Uuid) -> (StatusCode, Value) {
    root(
        &e.app,
        "POST",
        &format!("/api/signup-approvals/{id}/reject"),
        Value::Null,
    )
    .await
}

/// Wait (at most 5 s) until the Telegram mock holds `n` messages, and return them.
async fn telegram_messages(up: &Upstream, n: usize) -> Vec<Value> {
    for _ in 0..100 {
        let got = lock(&up.mock).telegram.clone();
        if got.len() >= n {
            return got;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    lock(&up.mock).telegram.clone()
}

// ---- dark by default ----

#[tokio::test]
async fn dark_by_default_every_signup_route_is_the_unknown_route_404() {
    let e = common::env().await;
    let id = Uuid::new_v4();
    for (method, uri, token) in [
        ("POST", INTAKE.to_string(), intake_token()),
        (
            "GET",
            "/api/signup-approvals".to_string(),
            super_admin("root"),
        ),
        (
            "POST",
            format!("/api/signup-approvals/{id}/approve"),
            super_admin("root"),
        ),
        (
            "POST",
            format!("/api/signup-approvals/{id}/reject"),
            super_admin("root"),
        ),
    ] {
        let (s, b) = request(&e.app, method, &uri, &token, None, signup("a@globex.com")).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::NOT_FOUND, &json!("not_found")),
            "{method} {uri}"
        );
    }
    // not even a 401 without a token: the route does not exist
    let (s, _) = common::send(
        &e.app,
        common::json("POST", INTAKE, None, signup("a@globex.com")),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(count(&e, "SELECT count(*) FROM signup_approvals").await, 0);

    // control: switched on, the same intake is accepted
    let e = enabled(vec![]).await;
    let (s, b) = submit(&e, signup("a@globex.com")).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{b}");
}

// ---- intake ----

#[tokio::test]
async fn intake_takes_only_the_intake_client_with_its_role() {
    let e = enabled(vec![]).await;
    let (s, _) = common::send(
        &e.app,
        common::json("POST", INTAKE, None, signup("a@globex.com")),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    for (what, token) in [
        (
            "wrong client",
            sa_token("some-other-client", &[INTAKE_CLIENT]),
        ),
        ("no role", sa_token(INTAKE_CLIENT, &[])),
        (
            "an operator session",
            sa_token(INTAKE_CLIENT, &[INTAKE_CLIENT, keys::SUPER_ADMIN]),
        ),
        ("a person without azp", keys::token("bob", &[INTAKE_CLIENT])),
    ] {
        let (s, b) = request(&e.app, "POST", INTAKE, &token, None, signup("a@globex.com")).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::FORBIDDEN, &json!("signup_intake_forbidden")),
            "{what}"
        );
    }
    assert_eq!(count(&e, "SELECT count(*) FROM signup_approvals").await, 0);

    let (s, b) = submit(&e, signup("ann@globex.com")).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{b}");
    assert_eq!(b, json!({"status": "queued"}));
    let row: (
        String,
        String,
        Option<String>,
        String,
        String,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT email, name, company, source, trust, intake_client, status FROM signup_approvals",
    )
    .fetch_one(common::pool(&e))
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            "ann@globex.com".into(),
            "Ann Early".into(),
            Some("Globex".into()),
            "web".into(),
            "intake".into(),
            INTAKE_CLIENT.into(),
            "pending".into(),
        )
    );
    assert_eq!(audited(&e, "signup.intake").await, 1);
    // the runtime role cannot read the queue at all
    let denied = sqlx::query("SELECT email FROM signup_approvals")
        .fetch_all(&e.state.db)
        .await;
    assert!(denied.is_err(), "configurator_app read signup_approvals");
}

#[tokio::test]
async fn intake_validates_and_normalises_like_commonpractice() {
    let e = enabled(vec![]).await;
    for bad in [
        json!({"email": "a@globex.com", "name": "Ann", "source": "web", "extra": 1}),
        json!({"email": "a@globex.com", "name": "Ann", "source": "fax"}),
        json!({"email": "not-an-address", "name": "Ann", "source": "web"}),
        json!({"email": "a@globex.com", "source": "web"}),
        json!({"email": "a@globex.com", "name": "\u{200B} \n\u{202E}", "source": "web"}),
        json!({"email": "a@globex.com", "name": "x".repeat(161), "source": "web"}),
        json!({"email": "a@globex.com", "name": "Ann", "company": "c".repeat(161), "source": "chat"}),
        json!({"email": "a@globex.com", "name": "Ann", "message": "m".repeat(2001), "source": "chat"}),
        json!({"email": "a@globex.com", "name": "Ann", "source": "web", "turnstileToken": ""}),
    ] {
        let (s, b) = submit(&e, bad.clone()).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::BAD_REQUEST, &json!("invalid_body")),
            "{bad}"
        );
    }
    assert_eq!(count(&e, "SELECT count(*) FROM signup_approvals").await, 0);

    let (s, b) = submit(
        &e,
        json!({"email": "ann@globex.com", "name": " Ann\nVerified: yes\u{202E} ",
               "company": "Globex\u{200B} Ltd", "message": "Hello\r\nwe are\u{0007} keen\u{2029}",
               "source": "chat"}),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{b}");
    let row: (String, Option<String>, Option<String>, String) =
        sqlx::query_as("SELECT name, company, message, source FROM signup_approvals")
            .fetch_one(common::pool(&e))
            .await
            .unwrap();
    assert_eq!(
        row,
        (
            "Ann Verified: yes".into(),
            Some("Globex Ltd".into()),
            Some("Hello\nwe are keen".into()),
            "chat".into()
        )
    );
}

#[tokio::test]
async fn a_pending_address_is_queued_once() {
    let e = enabled(vec![]).await;
    assert_eq!(
        submit(&e, signup("ann@globex.com")).await.1,
        json!({"status": "queued"})
    );
    let (s, b) = submit(&e, signup("Ann@Globex.com")).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert_eq!(b, json!({"status": "already_pending"}));
    assert_eq!(count(&e, "SELECT count(*) FROM signup_approvals").await, 1);
    assert_eq!(
        audited(&e, "signup.intake").await,
        1,
        "a re-submit is no write"
    );

    // a decided row does not block a later signup
    let id = signup_id(&e, "ann@globex.com").await;
    assert_eq!(reject(&e, id).await.0, StatusCode::OK);
    assert_eq!(
        submit(&e, signup("ann@globex.com")).await.1,
        json!({"status": "queued"})
    );
    assert_eq!(count(&e, "SELECT count(*) FROM signup_approvals").await, 2);
}

#[tokio::test]
async fn a_full_queue_refuses_new_signups() {
    let e = enabled(vec![("SIGNUP_QUEUE_MAX_PENDING", "1".into())]).await;
    assert_eq!(
        submit(&e, signup("a@globex.com")).await.0,
        StatusCode::ACCEPTED
    );
    let (s, b) = submit(&e, signup("b@globex.com")).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::SERVICE_UNAVAILABLE, &json!("signup_queue_full"))
    );
    assert_eq!(count(&e, "SELECT count(*) FROM signup_approvals").await, 1);
    // working the queue down opens it again
    let id = signup_id(&e, "a@globex.com").await;
    assert_eq!(reject(&e, id).await.0, StatusCode::OK);
    assert_eq!(
        submit(&e, signup("b@globex.com")).await.0,
        StatusCode::ACCEPTED
    );
}

#[tokio::test]
async fn concurrent_intakes_serialise_on_the_queue_lock_and_cannot_overshoot_the_ceiling() {
    let e = enabled(vec![("SIGNUP_QUEUE_MAX_PENDING", "1".into())]).await;
    // Hold the queue lock: both intakes must wait for it, then decide one after the other.
    let mut holder = common::pool(&e).begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(configurator_api::db::signups::QUEUE_LOCK)
        .execute(&mut *holder)
        .await
        .unwrap();
    let hold = Duration::from_millis(500);
    let start = Instant::now();
    let env = &e;
    let timed = move |email: &'static str| async move {
        let r = submit(env, signup(email)).await;
        (r, start.elapsed())
    };
    let ((a, ta), (b, tb), ()) =
        tokio::join!(timed("a@globex.com"), timed("b@globex.com"), async {
            tokio::time::sleep(hold).await;
            holder.rollback().await.unwrap();
        });
    assert!(
        ta >= hold && tb >= hold,
        "an intake did not wait for the queue lock: {ta:?} {tb:?}"
    );
    let mut statuses = [a.0, b.0];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::ACCEPTED, StatusCode::SERVICE_UNAVAILABLE],
        "{a:?} {b:?}"
    );
    assert_eq!(count(&e, "SELECT count(*) FROM signup_approvals").await, 1);
}

#[tokio::test]
async fn a_relayed_turnstile_token_is_verified_when_a_secret_is_set() {
    let up = upstream().await;
    let e = enabled(up.turnstile()).await;
    let with_token = |email: &str, token: &str| {
        let mut b = signup(email);
        b["turnstileToken"] = json!(token);
        b
    };
    let (s, b) = submit(&e, with_token("bad@globex.com", "forged")).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, &json!("turnstile_failed"))
    );
    let (s, b) = submit(&e, with_token("ok@globex.com", "good-token")).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{b}");
    // the intake is a server: no token is not required
    assert_eq!(
        submit(&e, signup("none@globex.com")).await.0,
        StatusCode::ACCEPTED
    );
    assert_eq!(lock(&up.mock).verified, ["forged", "good-token"]);
    lock(&up.mock).turnstile_down = true;
    let (s, b) = submit(&e, with_token("down@globex.com", "good-token")).await;
    assert_eq!(
        (s, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            &json!("turnstile_unavailable")
        )
    );
    assert_eq!(count(&e, "SELECT count(*) FROM signup_approvals").await, 2);

    // without a secret a token is not checked at all
    let up = upstream().await;
    let e = enabled(vec![]).await;
    assert_eq!(
        submit(&e, with_token("x@globex.com", "forged")).await.0,
        StatusCode::ACCEPTED
    );
    assert!(lock(&up.mock).verified.is_empty());
}

#[tokio::test]
async fn telegram_announces_new_signups_and_decisions_without_personal_data() {
    let up = upstream().await;
    let mut cfg = up.telegram();
    cfg.extend(up.keycloak_admin());
    let e = enabled(cfg).await;
    let (s, _) = submit(&e, signup("ann.private@globex.com")).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let id = signup_id(&e, "ann.private@globex.com").await;
    let msgs = telegram_messages(&up, 1).await;
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    let m = &msgs[0];
    assert_eq!(
        (&m["chat_id"], &m["message_thread_id"]),
        (&json!("-100123"), &json!(42))
    );
    let text = m["text"].as_str().unwrap();
    assert!(text.contains(&id.to_string()), "{text}");
    assert!(text.contains("Pending signups: 1"), "{text}");
    for private in ["ann.private", "Ann Early", "Globex"] {
        assert!(!text.contains(private), "{private} leaked: {text}");
    }
    assert_eq!(
        m["reply_markup"]["inline_keyboard"][0][0]["url"],
        json!(format!(
            "https://configurator.test/signups?signup={id}&action=approve"
        ))
    );
    // a de-duplicated re-submit sends nothing
    submit(&e, signup("ann.private@globex.com")).await;
    // the decision is announced too
    let (s, b) = approve(&e, id, Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let msgs = telegram_messages(&up, 2).await;
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    let text = msgs[1]["text"].as_str().unwrap();
    assert!(text.contains("approved"), "{text}");
    assert!(text.contains(b["tenant"]["id"].as_str().unwrap()), "{text}");
    assert_eq!(b["invited"], true);
    assert!(text.contains("invited by email"), "{text}");
}

#[tokio::test]
async fn an_approval_that_links_an_existing_identity_does_not_claim_an_email() {
    let up = upstream().await;
    lock(&up.mock)
        .users
        .push(("kc-ann".into(), "ann@globex.com".into()));
    let mut cfg = up.telegram();
    cfg.extend(up.keycloak_admin());
    let e = enabled(cfg).await;
    submit(&e, signup("ann@globex.com")).await;
    let id = signup_id(&e, "ann@globex.com").await;
    let (s, b) = approve(&e, id, Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        (&b["invited"], &b["first_admin_sub"]),
        (&json!(false), &json!("kc-ann"))
    );
    assert!(lock(&up.mock).emails.is_empty(), "no invite email went");
    let msgs = telegram_messages(&up, 2).await;
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    let text = msgs[1]["text"].as_str().unwrap();
    assert!(text.contains("approved"), "{text}");
    assert!(!text.contains("invited by email"), "{text}");
    assert!(text.contains("existing portal identity"), "{text}");
    assert!(text.contains("no email was sent"), "{text}");
}

#[tokio::test]
async fn a_failed_telegram_send_never_fails_the_intake() {
    let up = upstream().await;
    lock(&up.mock).telegram_down = true;
    let e = enabled(up.telegram()).await;
    let (s, b) = submit(&e, signup("a@globex.com")).await;
    assert_eq!((s, b), (StatusCode::ACCEPTED, json!({"status": "queued"})));
    // the send is detached, so prove it was attempted (and refused) while the intake succeeded
    let mut calls = 0;
    for _ in 0..40 {
        calls = lock(&up.mock).telegram_calls;
        if calls >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(calls >= 1, "the Telegram send was never attempted");
    assert!(lock(&up.mock).telegram.is_empty(), "every send was refused");
    assert_eq!(
        count(
            &e,
            "SELECT count(*) FROM signup_approvals WHERE status = 'pending'"
        )
        .await,
        1
    );
    // nor does an unreachable Telegram
    let e = enabled(vec![
        ("TELEGRAM_BOT_TOKEN", BOT_TOKEN.into()),
        ("TELEGRAM_SIGNUP_CHAT_ID", "-1".into()),
        ("TELEGRAM_SIGNUP_THREAD_ID", "1".into()),
        ("TELEGRAM_API_BASE", "http://127.0.0.1:1".into()),
    ])
    .await;
    let (s, b) = submit(&e, signup("a@globex.com")).await;
    assert_eq!((s, b), (StatusCode::ACCEPTED, json!({"status": "queued"})));
}

// ---- the super-admin queue ----

#[tokio::test]
async fn the_queue_is_super_admin_only_and_suggests_a_name() {
    let e = enabled(vec![]).await;
    submit(&e, signup("ann@globex.com")).await;
    submit(
        &e,
        json!({"email": "bo@gmail.com", "name": "Bo Diddley", "source": "chat"}),
    )
    .await;
    let id = signup_id(&e, "ann@globex.com").await;
    for token in [keys::token("bob", &[]), intake_token()] {
        for (method, uri) in [
            ("GET", "/api/signup-approvals".to_string()),
            ("POST", format!("/api/signup-approvals/{id}/approve")),
            ("POST", format!("/api/signup-approvals/{id}/reject")),
        ] {
            let (s, b) = request(&e.app, method, &uri, &token, None, Value::Null).await;
            assert_eq!(
                (s, code(&b)),
                (StatusCode::FORBIDDEN, &json!("forbidden")),
                "{method} {uri}"
            );
        }
    }
    assert_eq!(status_of(&e, id).await.0, "pending");

    let (s, b) = root(&e.app, "GET", "/api/signup-approvals?limit=1", Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["items"].as_array().unwrap().len(), 1);
    assert_eq!(b["items"][0]["email"], "bo@gmail.com", "newest first");
    assert_eq!(
        b["items"][0]["suggested_tenant_name"], "Bo Diddley",
        "a public mailbox domain does not name the workspace"
    );
    let next = b["next"].as_str().unwrap().to_string();
    let (s, b) = root(
        &e.app,
        "GET",
        &format!("/api/signup-approvals?limit=1&before={next}"),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["items"][0]["suggested_tenant_name"], "Globex");
    assert_eq!(b["next"], Value::Null);

    reject(&e, id).await;
    let (_, b) = root(
        &e.app,
        "GET",
        "/api/signup-approvals?status=pending",
        Value::Null,
    )
    .await;
    assert_eq!(b["items"].as_array().unwrap().len(), 1);
    let (s, b) = root(&e.app, "GET", "/api/signup-approvals?status=x", Value::Null).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, &json!("invalid_query"))
    );
}

#[tokio::test]
async fn approve_creates_the_tenant_invites_the_requester_and_issues_the_licence() {
    let up = upstream().await;
    let mut cfg = up.keycloak_admin();
    cfg.extend(up.licences());
    let e = enabled(cfg).await;
    submit(&e, signup("ann@globex.com")).await;
    let id = signup_id(&e, "ann@globex.com").await;

    let (s, b) = approve(&e, id, Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let tenant = Uuid::parse_str(b["tenant"]["id"].as_str().unwrap()).unwrap();
    assert_eq!(b["tenant"]["name"], "Globex", "the suggested name");
    assert_eq!(b["approval"]["status"], "approved");
    assert_eq!(b["invited"], true);

    let (created, emails, issued) = {
        let g = lock(&up.mock);
        (g.created.clone(), g.emails.clone(), g.issued.clone())
    };
    assert_eq!(created.len(), 1, "the requester's identity was created");
    assert_eq!(emails, created, "and sent the invite email");
    assert_eq!(b["first_admin_sub"], created[0]);
    let user: (Uuid, String) =
        sqlx::query_as("SELECT tenant, tenant_role::text FROM users WHERE sub = $1")
            .bind(&created[0])
            .fetch_one(common::pool(&e))
            .await
            .unwrap();
    assert_eq!(user, (tenant, "tenant_admin".to_string()));
    assert_eq!(issued, [(tenant.to_string(), created[0].clone())]);
    let sealed: String =
        sqlx::query_scalar("SELECT token_encrypted FROM tenant_license WHERE tenant = $1")
            .bind(tenant)
            .fetch_one(common::pool(&e))
            .await
            .unwrap();
    assert!(sealed.starts_with("v1:"), "stored sealed: {sealed}");
    let cipher = e.state.token_cipher.as_deref().unwrap();
    assert_eq!(
        cipher.open(tenant, &sealed).unwrap(),
        format!("tok-{tenant}")
    );
    assert_eq!(
        status_of(&e, id).await,
        ("approved".into(), Some(tenant), Some("root".into()))
    );
    for action in ["signup.approve", "tenant.create", "license.issue"] {
        assert_eq!(audited(&e, action).await, 1, "{action}");
    }

    // the second approval is a 409 and creates nothing
    let (s, b) = approve(&e, id, json!({"tenantName": "Again"})).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::CONFLICT, &json!("approval_not_pending"))
    );
    let (s, b) = reject(&e, id).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::CONFLICT, &json!("approval_not_pending"))
    );
    assert_eq!(count(&e, "SELECT count(*) FROM tenant").await, 1);
    assert_eq!(lock(&up.mock).issued.len(), 1);
    assert_eq!(audited(&e, "signup.approve").await, 1);
}

#[tokio::test]
async fn approve_takes_a_given_name_and_refuses_bad_ones() {
    let up = upstream().await;
    let e = enabled(up.keycloak_admin()).await;
    submit(&e, signup("ann@globex.com")).await;
    let id = signup_id(&e, "ann@globex.com").await;
    for bad in [
        json!({"tenantName": "x"}),
        json!({"tenantName": "x".repeat(121)}),
        json!({"name": "Unknown field"}),
    ] {
        let (s, b) = approve(&e, id, bad.clone()).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::BAD_REQUEST, &json!("invalid_body")),
            "{bad}"
        );
    }
    assert!(lock(&up.mock).created.is_empty(), "nothing invited");
    let (s, b) = approve(&e, id, json!({"tenantName": "Globex Europe"})).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["tenant"]["name"], "Globex Europe");
}

#[tokio::test]
async fn concurrent_approvals_create_one_tenant() {
    let up = upstream().await;
    let e = enabled(up.keycloak_admin()).await;
    submit(&e, signup("ann@globex.com")).await;
    let id = signup_id(&e, "ann@globex.com").await;
    let (a, b) = tokio::join!(approve(&e, id, Value::Null), approve(&e, id, Value::Null));
    let mut statuses = [a.0, b.0];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::CONFLICT],
        "{a:?} {b:?}"
    );
    let loser = if a.0 == StatusCode::CONFLICT {
        &a.1
    } else {
        &b.1
    };
    assert_eq!(code(loser), &json!("approval_not_pending"));
    assert_eq!(count(&e, "SELECT count(*) FROM tenant").await, 1);
    assert_eq!(audited(&e, "signup.approve").await, 1);
}

#[tokio::test]
async fn approval_failures_leave_the_signup_pending() {
    // no Keycloak admin SA: the requester cannot be invited
    let e = enabled(vec![]).await;
    submit(&e, signup("ann@globex.com")).await;
    let id = signup_id(&e, "ann@globex.com").await;
    let (s, b) = approve(&e, id, Value::Null).await;
    assert_eq!(
        (s, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            &json!("invite_unavailable")
        )
    );
    assert_eq!(status_of(&e, id).await.0, "pending");

    // the requester already belongs to a tenant (one tenant per identity)
    let up = upstream().await;
    lock(&up.mock)
        .users
        .push(("kc-alice".into(), "alice@acme.com".into()));
    let e = enabled(up.keycloak_admin()).await;
    let (s, b) = root(
        &e.app,
        "POST",
        "/api/tenants",
        json!({"name": "Acme", "first_admin_sub": "kc-alice"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    submit(&e, signup("alice@acme.com")).await;
    let id = signup_id(&e, "alice@acme.com").await;
    let (s, b) = approve(&e, id, Value::Null).await;
    assert_eq!((s, code(&b)), (StatusCode::CONFLICT, &json!("user_exists")));
    assert_eq!(status_of(&e, id).await, ("pending".into(), None, None));
    assert_eq!(count(&e, "SELECT count(*) FROM tenant").await, 1);

    // unknown and malformed ids
    for uri in [
        format!("/api/signup-approvals/{}/approve", Uuid::new_v4()),
        format!("/api/signup-approvals/{}/reject", Uuid::new_v4()),
        "/api/signup-approvals/not-a-uuid/approve".to_string(),
    ] {
        let (s, b) = root(&e.app, "POST", &uri, Value::Null).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::NOT_FOUND, &json!("approval_not_found")),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn reject_marks_the_signup_and_creates_nothing() {
    let up = upstream().await;
    let e = enabled(up.keycloak_admin()).await;
    submit(&e, signup("ann@globex.com")).await;
    let id = signup_id(&e, "ann@globex.com").await;
    let (s, b) = reject(&e, id).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["approval"]["status"], "rejected");
    assert_eq!(
        status_of(&e, id).await,
        ("rejected".into(), None, Some("root".into()))
    );
    assert_eq!(audited(&e, "signup.reject").await, 1);
    // a rejected signup cannot be approved
    let (s, b) = approve(&e, id, Value::Null).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::CONFLICT, &json!("approval_not_pending"))
    );
    assert_eq!(count(&e, "SELECT count(*) FROM tenant").await, 0);
    assert!(lock(&up.mock).created.is_empty(), "no identity created");
    // a second rejection is a 409 too
    let (s, _) = reject(&e, id).await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_tenant_member_cannot_reach_the_queue() {
    let e = enabled(vec![]).await;
    let _ = common::client::tenant(&e.app, "alice").await;
    let (s, b) = member(&e.app, "GET", "/api/signup-approvals", "alice", Value::Null).await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, &json!("forbidden")));
}
