mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use common::keys::{self, foreign_token, super_admin, token};
use configurator_api::tenancy::TenantCtx;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const ROOT: &str = "root";

/// Super-admin creates a tenant whose first `tenant_admin` is `first_admin`; returns its id.
async fn create_tenant(app: &Router, name: &str, first_admin: &str) -> Uuid {
    let (s, b) = common::send(
        app,
        common::json(
            "POST",
            "/api/tenants",
            Some(&super_admin(ROOT)),
            json!({"name": name, "first_admin_sub": first_admin}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    Uuid::parse_str(b["id"].as_str().unwrap()).unwrap()
}

async fn add_user(
    app: &Router,
    tenant: Uuid,
    as_tok: &str,
    sub: &str,
    role: &str,
) -> (StatusCode, Value) {
    common::send(
        app,
        common::json(
            "POST",
            &format!("/api/tenants/{tenant}/users"),
            Some(as_tok),
            json!({"sub": sub, "role": role}),
        ),
    )
    .await
}

fn set_role_req(tenant: Uuid, tok: &str, sub: &str, role: &str) -> Request<Body> {
    common::json(
        "PATCH",
        &format!("/api/tenants/{tenant}/users/{sub}"),
        Some(tok),
        json!({"role": role}),
    )
}

async fn set_role(
    app: &Router,
    tenant: Uuid,
    tok: &str,
    sub: &str,
    role: &str,
) -> (StatusCode, Value) {
    common::send(app, set_role_req(tenant, tok, sub, role)).await
}

fn disable_req(tenant: Uuid, tok: &str, sub: &str) -> Request<Body> {
    Request::delete(format!("/api/tenants/{tenant}/users/{sub}"))
        .header("authorization", format!("Bearer {tok}"))
        .body(Body::empty())
        .unwrap()
}

async fn disable(app: &Router, tenant: Uuid, tok: &str, sub: &str) -> (StatusCode, Value) {
    common::send(app, disable_req(tenant, tok, sub)).await
}

/// A route that only extracts `TenantCtx` (the tenant resolution every tenant-scoped route uses).
fn probe(e: &common::TestEnv) -> Router {
    async fn whoami(ctx: TenantCtx) -> Json<Value> {
        Json(json!({"tenant": ctx.tenant, "role": ctx.role, "sub": ctx.caller.sub}))
    }
    Router::new()
        .route("/probe", get(whoami))
        .with_state(e.state.clone())
}

fn probe_req(tok: &str, tenant_headers: &[&str]) -> Request<Body> {
    let mut b = Request::get("/probe").header("authorization", format!("Bearer {tok}"));
    for h in tenant_headers {
        b = b.header("x-active-tenant-id", *h);
    }
    b.body(Body::empty()).unwrap()
}

fn code(b: &Value) -> Value {
    b["error"]["code"].clone()
}

/// actor, tenant, action, object, before_hash, after_hash
type AuditRow = (
    String,
    Option<Uuid>,
    String,
    String,
    Option<String>,
    Option<String>,
);

async fn audit_rows(db: &PgPool) -> Vec<AuditRow> {
    sqlx::query_as(
        "SELECT actor, tenant, action, object, before_hash, after_hash FROM audit_event ORDER BY id",
    )
    .fetch_all(db)
    .await
    .unwrap()
}

/// actor, tenant, metadata of every `tenant.scope_switch` event, in order.
async fn scope_switches(db: &PgPool) -> Vec<(String, Option<Uuid>, Option<Value>)> {
    sqlx::query_as(
        "SELECT actor, tenant, metadata FROM audit_event \
         WHERE action = 'tenant.scope_switch' ORDER BY id",
    )
    .fetch_all(db)
    .await
    .unwrap()
}

/// The active `tenant_admin`s of `tenant`.
async fn admins(db: &PgPool, tenant: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT sub FROM users \
         WHERE tenant = $1 AND tenant_role = 'tenant_admin' AND disabled_at IS NULL ORDER BY sub",
    )
    .bind(tenant)
    .fetch_all(db)
    .await
    .unwrap()
}

#[tokio::test]
async fn no_token_401() {
    let e = common::env().await;
    for uri in ["/api/me", "/api/tenants"] {
        let (s, b) = common::send(&e.app, common::get(uri, None)).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(b["error"]["code"], "unauthorized", "{uri}");
    }
    // a non-Bearer scheme is the same as no token
    let req = Request::get("/api/me")
        .header("authorization", format!("Basic {}", token("alice", &[])))
        .body(Body::empty())
        .unwrap();
    assert_eq!(common::send(&e.app, req).await.0, StatusCode::UNAUTHORIZED);
    // the 401 comes before the tenant header or body is looked at
    let (s, _) = common::send(
        &e.app,
        common::json(
            "POST",
            "/api/tenants",
            None,
            json!({"name": "", "first_admin_sub": ""}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = common::send(
        &probe(&e),
        Request::get("/probe").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn bad_signature_401() {
    let e = common::env().await;
    let forged = foreign_token(ROOT, &[keys::SUPER_ADMIN]);
    for tok in [forged.as_str(), "x", "a.b.c"] {
        let (s, b) = common::send(&e.app, common::get("/api/me", Some(tok))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{tok}");
        assert_eq!(b["error"]["code"], "unauthorized");
    }
    // a valid token with its signature swapped for the forged one's
    let good = super_admin(ROOT);
    let spliced = format!(
        "{}.{}",
        good.rsplit_once('.').unwrap().0,
        forged.rsplit_once('.').unwrap().1
    );
    assert_eq!(
        common::send(&e.app, common::get("/api/me", Some(&spliced)))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    // control: the same claims signed by the trusted key pass
    assert_eq!(
        common::send(&e.app, common::get("/api/me", Some(&good)))
            .await
            .0,
        StatusCode::OK
    );
}

/// Every claim check holds at the HTTP layer: each row is 401 `unauthorized`.
#[tokio::test]
async fn claim_checks_401() {
    let e = common::env().await;
    let without = |field: &str| {
        let mut c = keys::claims(ROOT, &[keys::SUPER_ADMIN]);
        c.as_object_mut().unwrap().remove(field);
        c
    };
    let with = |field: &str, v: Value| {
        let mut c = keys::claims(ROOT, &[keys::SUPER_ADMIN]);
        c[field] = v;
        c
    };
    let good = keys::claims(ROOT, &[keys::SUPER_ADMIN]);
    let cases: Vec<(&str, String)> = vec![
        ("expired", keys::sign(&keys::expiring_in(ROOT, -120))),
        ("no exp", keys::sign(&without("exp"))),
        ("no aud", keys::sign(&without("aud"))),
        ("other aud", keys::sign(&with("aud", json!("account")))),
        ("no iss", keys::sign(&without("iss"))),
        (
            "other iss",
            keys::sign(&with("iss", json!("https://issuer.test/realms/other"))),
        ),
        ("no sub", keys::sign(&without("sub"))),
        ("empty sub", keys::sign(&with("sub", json!("")))),
        ("hs256", keys::hs256(&good)),
        ("alg none", keys::alg_none(&good)),
    ];
    for (name, tok) in &cases {
        let (s, b) = common::send(&e.app, common::get("/api/me", Some(tok.as_str()))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{name}: {b}");
        assert_eq!(b["error"]["code"], "unauthorized", "{name}");
    }
    // controls: the same claims, RS256-signed by the trusted key, pass; so does a near-expiry token
    for tok in [keys::sign(&good), keys::sign(&keys::expiring_in(ROOT, 60))] {
        let (s, b) = common::send(&e.app, common::get("/api/me", Some(&tok))).await;
        assert_eq!(s, StatusCode::OK, "{b}");
    }
    // the auth scheme is case-insensitive
    for scheme in ["bearer", "BEARER", "Bearer"] {
        let req = Request::get("/api/me")
            .header("authorization", format!("{scheme} {}", keys::sign(&good)))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            common::send(&e.app, req).await.0,
            StatusCode::OK,
            "{scheme}"
        );
    }
}

/// Only this app's client role makes a super-admin: the realm role of the same name (which other apps
/// read) and another client's `super_admin` do not.
#[tokio::test]
async fn realm_role_super_admin_is_ignored() {
    let e = common::env().await;
    let mut realm = keys::claims("mallory", &[]);
    realm["realm_access"] = json!({"roles": [keys::SUPER_ADMIN]});
    let mut other_client = keys::claims("mallory", &[]);
    other_client["resource_access"] = json!({"commonpractice": {"roles": [keys::SUPER_ADMIN]}});
    for (name, claims) in [("realm role", realm), ("other client", other_client)] {
        let tok = keys::sign(&claims);
        let (s, me) = common::send(&e.app, common::get("/api/me", Some(&tok))).await;
        assert_eq!(s, StatusCode::OK, "{name}: {me}");
        assert_eq!(me["super_admin"], false, "{name}");
        let (s, b) = common::send(
            &e.app,
            common::json(
                "POST",
                "/api/tenants",
                Some(&tok),
                json!({"name": "Evil", "first_admin_sub": "mallory"}),
            ),
        )
        .await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::FORBIDDEN, json!("forbidden")),
            "{name}"
        );
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM tenant")
        .fetch_one(common::pool(&e))
        .await
        .unwrap();
    assert_eq!(n, 0);
    // control: the client role on this app's client is the super-admin
    let (_, me) = common::send(&e.app, common::get("/api/me", Some(&super_admin(ROOT)))).await;
    assert_eq!(me["super_admin"], true);
}

#[tokio::test]
async fn me_returns_home_tenant() {
    let e = common::env().await;
    let a = create_tenant(&e.app, "Acme", "alice").await;
    let b = create_tenant(&e.app, "Beta", "bob").await;
    let (s, _) = add_user(&e.app, b, &token("bob", &[]), "carol", "tenant_user").await;
    assert_eq!(s, StatusCode::CREATED);

    let me = |tok: String| {
        let app = e.app.clone();
        async move {
            let (s, me) = common::send(&app, common::get("/api/me", Some(&tok))).await;
            assert_eq!(s, StatusCode::OK, "{me}");
            me
        }
    };
    assert_eq!(
        me(token("alice", &[])).await,
        json!({"sub": "alice", "super_admin": false,
               "tenant": {"id": a, "name": "Acme", "role": "tenant_admin"}})
    );
    assert_eq!(
        me(token("carol", &[])).await,
        json!({"sub": "carol", "super_admin": false,
               "tenant": {"id": b, "name": "Beta", "role": "tenant_user"}})
    );
    // the super-admin role comes from the token; a workspace is separate
    assert_eq!(
        me(super_admin(ROOT)).await,
        json!({"sub": ROOT, "super_admin": true, "tenant": null})
    );
    let ops = create_tenant(&e.app, "Ops", "sa2").await;
    assert_eq!(
        me(super_admin("sa2")).await,
        json!({"sub": "sa2", "super_admin": true,
               "tenant": {"id": ops, "name": "Ops", "role": "tenant_admin"}})
    );
    // someone with no workspace
    assert_eq!(
        me(token("dave", &["other-role"])).await,
        json!({"sub": "dave", "super_admin": false, "tenant": null})
    );
    // a disabled user has no workspace either
    let (s, _) = disable(&e.app, b, &token("bob", &[]), "carol").await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(
        me(token("carol", &[])).await,
        json!({"sub": "carol", "super_admin": false, "tenant": null})
    );
}

#[tokio::test]
async fn only_super_admin_creates_tenants() {
    let e = common::env().await;
    let body = json!({"name": "Acme", "first_admin_sub": "alice"});
    for tok in [
        token("alice", &[]),
        token("alice", &["config-admin", "admin", "tenant_admin"]),
    ] {
        let (s, b) = common::send(
            &e.app,
            common::json("POST", "/api/tenants", Some(&tok), body.clone()),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
        assert_eq!(b["error"]["code"], "forbidden");
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM tenant")
        .fetch_one(common::pool(&e))
        .await
        .unwrap();
    assert_eq!(n, 0, "a refused create wrote nothing");

    let (s, t) = common::send(
        &e.app,
        common::json("POST", "/api/tenants", Some(&super_admin(ROOT)), body),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{t}");
    assert_eq!(t["name"], "Acme");
    let id = Uuid::parse_str(t["id"].as_str().unwrap()).unwrap();
    assert!(!id.is_nil());
    assert!(t["created_at"].as_str().unwrap().contains('T'), "{t}");
    // the first admin is a user of the tenant, as tenant_admin
    let role: String = sqlx::query_scalar(
        "SELECT tenant_role::text FROM users WHERE tenant = $1 AND sub = 'alice'",
    )
    .bind(id)
    .fetch_one(common::pool(&e))
    .await
    .unwrap();
    assert_eq!(role, "tenant_admin");

    // bad bodies are 400 invalid_body
    for bad in [
        json!({"name": "", "first_admin_sub": "x"}),
        json!({"name": "X", "first_admin_sub": "  "}),
        json!({"name": "X"}),
        json!({"name": "x".repeat(201), "first_admin_sub": "x"}),
        json!({"name": "X", "first_admin_sub": "s".repeat(256)}),
        json!({"name": "X", "first_admin_sub": "a\tb"}),
    ] {
        let (s, b) = common::send(
            &e.app,
            common::json("POST", "/api/tenants", Some(&super_admin(ROOT)), bad),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
        assert_eq!(b["error"]["code"], "invalid_body");
    }

    // values are trimmed before they are checked and stored; the caps are inclusive
    let (s, b) = common::send(
        &e.app,
        common::json(
            "POST",
            "/api/tenants",
            Some(&super_admin(ROOT)),
            json!({"name": format!("  {}  ", "n".repeat(200)), "first_admin_sub": " trimmed "}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(b["name"], "n".repeat(200));
    let tid = Uuid::parse_str(b["id"].as_str().unwrap()).unwrap();
    let stored: Vec<String> = sqlx::query_scalar("SELECT sub FROM users WHERE tenant = $1")
        .bind(tid)
        .fetch_all(common::pool(&e))
        .await
        .unwrap();
    assert_eq!(stored, ["trimmed"]);
    let (s, b) = add_user(
        &e.app,
        tid,
        &super_admin(ROOT),
        &format!(" {} ", "s".repeat(255)),
        "tenant_user",
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(b["sub"], "s".repeat(255));
    let (s, b) = add_user(&e.app, tid, &super_admin(ROOT), " trimmed ", "tenant_user").await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::CONFLICT, json!("user_exists")),
        "a padded sub is the same user"
    );
    sqlx::query("DELETE FROM tenant WHERE id = $1")
        .bind(tid)
        .execute(common::pool(&e))
        .await
        .unwrap();

    // listing: the super-admin sees all, others only their own (zero or one)
    create_tenant(&e.app, "Beta", "bob").await;
    let names = |v: &Value| -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    };
    let (_, all) = common::send(
        &e.app,
        common::get("/api/tenants", Some(&super_admin(ROOT))),
    )
    .await;
    assert_eq!(names(&all), ["Beta", "Acme"], "newest first");
    assert!(all["next"].is_null());
    let (_, own) = common::send(
        &e.app,
        common::get("/api/tenants", Some(&token("alice", &[]))),
    )
    .await;
    assert_eq!(names(&own), ["Acme"]);
    let (s, none) = common::send(
        &e.app,
        common::get("/api/tenants", Some(&token("zed", &[]))),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(names(&none), Vec::<String>::new());
}

#[tokio::test]
async fn active_tenant_header_errors() {
    let e = common::env().await;
    let t = create_tenant(&e.app, "Acme", "alice").await;
    let beta = create_tenant(&e.app, "Beta", "bob").await;
    let tid_s = t.to_string();
    let beta_s = beta.to_string();
    let other_s = Uuid::new_v4().to_string();
    let braced_s = format!("{{{tid_s}}}");
    let urn_s = format!("urn:uuid:{tid_s}");
    let simple_s = t.simple().to_string();
    let (tid, beta_h, other, braced, urn, simple) = (
        tid_s.as_str(),
        beta_s.as_str(),
        other_s.as_str(),
        braced_s.as_str(),
        urn_s.as_str(),
        simple_s.as_str(),
    );
    let root = super_admin(ROOT);
    let app = probe(&e);

    // the super-admin (no workspace of their own) names the tenant to act in
    let cases: Vec<(Vec<&str>, StatusCode, &str)> = vec![
        (vec![], StatusCode::BAD_REQUEST, "tenant_required"),
        (
            vec!["not-a-uuid"],
            StatusCode::BAD_REQUEST,
            "invalid_tenant",
        ),
        (
            vec!["00000000-0000-0000-0000-000000000000"],
            StatusCode::BAD_REQUEST,
            "invalid_tenant",
        ),
        (vec![tid, tid], StatusCode::BAD_REQUEST, "invalid_tenant"),
        (vec![tid, other], StatusCode::BAD_REQUEST, "invalid_tenant"),
        (vec!["Bearer x"], StatusCode::BAD_REQUEST, "invalid_tenant"),
        (vec![""], StatusCode::BAD_REQUEST, "invalid_tenant"),
        (vec![braced], StatusCode::BAD_REQUEST, "invalid_tenant"),
        (vec![urn], StatusCode::BAD_REQUEST, "invalid_tenant"),
        (vec![simple], StatusCode::BAD_REQUEST, "invalid_tenant"),
        (vec![other], StatusCode::NOT_FOUND, "tenant_not_found"),
    ];
    for (headers, status, want) in cases {
        let (s, b) = common::send(&app, probe_req(&root, &headers)).await;
        assert_eq!(s, status, "{headers:?}: {b}");
        assert_eq!(b["error"]["code"], want, "{headers:?}");
    }
    // control: one bare uuid of an existing tenant
    let (s, b) = common::send(&app, probe_req(&root, &[tid])).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b, json!({"tenant": t, "role": "tenant_admin", "sub": ROOT}));

    // anyone else: the header is ignored (it cannot switch tenant nor probe for tenants); the
    // tenant always comes from `users`
    let alice = token("alice", &[]);
    for headers in [
        vec![],
        vec![beta_h],
        vec![other],
        vec!["not-a-uuid"],
        vec![tid, other],
    ] {
        let (s, b) = common::send(&app, probe_req(&alice, &headers)).await;
        assert_eq!(s, StatusCode::OK, "{headers:?}: {b}");
        assert_eq!(
            b,
            json!({"tenant": t, "role": "tenant_admin", "sub": "alice"}),
            "{headers:?}"
        );
    }

    // the path form of the user routes: bad ids are 400; a tenant that is not the caller's is the
    // same 404 as one that does not exist
    let (s, b) = common::send(
        &e.app,
        common::get("/api/tenants/not-a-uuid/users", Some(&alice)),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, json!("invalid_tenant"))
    );
    for id in [other, beta_h] {
        let (s, b) = common::send(
            &e.app,
            common::get(&format!("/api/tenants/{id}/users"), Some(&alice)),
        )
        .await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::NOT_FOUND, json!("tenant_not_found")),
            "{id}"
        );
    }
    let (s, b) = common::send(
        &e.app,
        common::get(&format!("/api/tenants/{other}/users"), Some(&root)),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, json!("tenant_not_found"))
    );
}

#[tokio::test]
async fn no_users_row_403() {
    let e = common::env().await;
    let t = create_tenant(&e.app, "Acme", "alice").await;
    let tid = t.to_string();
    let app = probe(&e);
    let carol = token("carol", &[]);

    // no `users` row: no workspace, whatever header is sent
    for headers in [vec![], vec![tid.as_str()]] {
        let (s, b) = common::send(&app, probe_req(&carol, &headers)).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::FORBIDDEN, json!("no_workspace")),
            "{headers:?}"
        );
    }
    let (s, b) = common::send(&e.app, common::get("/api/layers", Some(&carol))).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, json!("no_workspace"))
    );
    let users = format!("/api/tenants/{tid}/users");
    let (s, b) = common::send(&e.app, common::get(&users, Some(&carol))).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, json!("no_workspace"))
    );
    let (s, _) = add_user(&e.app, t, &carol, "carol", "tenant_admin").await;
    assert_eq!(s, StatusCode::FORBIDDEN, "no one adds themselves");
    let (s, _) = disable(&e.app, t, &carol, "alice").await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // a tenant_user is in the tenant but cannot manage users
    let (s, _) = add_user(&e.app, t, &token("alice", &[]), "victor", "tenant_user").await;
    assert_eq!(s, StatusCode::CREATED);
    let victor = token("victor", &[]);
    let (s, b) = common::send(&app, probe_req(&victor, &[])).await;
    assert_eq!(
        (s, b["role"].clone()),
        (StatusCode::OK, json!("tenant_user"))
    );
    let (s, b) = common::send(&e.app, common::get(&users, Some(&victor))).await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, json!("forbidden")));
    let (s, _) = add_user(&e.app, t, &victor, "mallory", "tenant_user").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = set_role(&e.app, t, &victor, "victor", "tenant_admin").await;
    assert_eq!(s, StatusCode::FORBIDDEN, "no self-promotion");
    let (s, _) = disable(&e.app, t, &victor, "alice").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT sub, tenant_role::text FROM users WHERE tenant = $1 AND disabled_at IS NULL \
         ORDER BY sub",
    )
    .bind(t)
    .fetch_all(common::pool(&e))
    .await
    .unwrap();
    assert_eq!(
        rows,
        [
            ("alice".to_string(), "tenant_admin".to_string()),
            ("victor".into(), "tenant_user".into())
        ],
        "refused writes changed nothing"
    );
}

#[tokio::test]
async fn disabled_user_403() {
    let e = common::env().await;
    let t = create_tenant(&e.app, "Acme", "alice").await;
    let alice = token("alice", &[]);
    let bob = token("bob", &[]);
    let app = probe(&e);
    assert_eq!(
        add_user(&e.app, t, &alice, "bob", "tenant_admin").await.0,
        StatusCode::CREATED
    );
    let (s, b) = common::send(&app, probe_req(&bob, &[])).await;
    assert_eq!(s, StatusCode::OK, "{b}");

    let (s, b) = disable(&e.app, t, &alice, "bob").await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
    let (s, b) = common::send(&app, probe_req(&bob, &[])).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, json!("no_workspace"))
    );
    let (s, b) = common::send(&e.app, common::get("/api/tenants", Some(&bob))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["items"], json!([]));
    let (s, b) = common::send(
        &e.app,
        common::get(&format!("/api/tenants/{t}/users"), Some(&bob)),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, json!("no_workspace"))
    );

    // the identity stays bound to the tenant: not re-addable, not re-disablable, not re-roled
    let (s, b) = add_user(&e.app, t, &alice, "bob", "tenant_user").await;
    assert_eq!((s, code(&b)), (StatusCode::CONFLICT, json!("user_exists")));
    let (s, b) = disable(&e.app, t, &alice, "bob").await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, json!("user_not_found"))
    );
    let (s, b) = set_role(&e.app, t, &alice, "bob", "tenant_user").await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, json!("user_not_found"))
    );
    // the list still shows the disabled user
    let (_, list) = common::send(
        &e.app,
        common::get(&format!("/api/tenants/{t}/users"), Some(&alice)),
    )
    .await;
    let bob_row = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["sub"] == "bob")
        .cloned()
        .unwrap();
    assert!(
        bob_row["disabled_at"]
            .as_str()
            .is_some_and(|d| d.contains('T')),
        "{bob_row}"
    );
}

#[tokio::test]
async fn super_admin_acts_in_any_tenant() {
    let e = common::env().await;
    let t = create_tenant(&e.app, "Acme", "alice").await;
    let tid = t.to_string();
    let root = super_admin(ROOT);
    let db = common::pool(&e);

    // no users row, yet tenant_admin in the tenant; the switch is audited with the request
    let (s, b) = common::send(&probe(&e), probe_req(&root, &[tid.as_str()])).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["role"], "tenant_admin");
    assert_eq!(
        scope_switches(db).await,
        [(
            ROOT.to_string(),
            Some(t),
            Some(json!({"method": "GET", "path": "/probe"}))
        )]
    );
    // ... but only in tenants that exist, and a refused switch is not audited
    let (s, b) = common::send(
        &probe(&e),
        probe_req(&root, &[Uuid::new_v4().to_string().as_str()]),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, json!("tenant_not_found"))
    );
    assert_eq!(scope_switches(db).await.len(), 1);

    // the path names the tenant for user management
    let (s, b) = add_user(&e.app, t, &root, "bob", "tenant_user").await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(
        (
            b["sub"].clone(),
            b["role"].clone(),
            b["disabled_at"].clone()
        ),
        (json!("bob"), json!("tenant_user"), Value::Null)
    );
    let (s, b) = add_user(&e.app, t, &root, "bob", "tenant_admin").await;
    assert_eq!((s, code(&b)), (StatusCode::CONFLICT, json!("user_exists")));
    for bad in ["owner", "editor", "admin"] {
        let (s, b) = add_user(&e.app, t, &root, "eve", bad).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::BAD_REQUEST, json!("invalid_body")),
            "{bad}"
        );
    }
    let (s, b) = set_role(&e.app, t, &root, "bob", "tenant_admin").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["role"], "tenant_admin");

    let (s, b) = common::send(
        &e.app,
        common::get(&format!("/api/tenants/{tid}/users"), Some(&root)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let got: Vec<(String, String)> = b["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["sub"].as_str().unwrap().into(),
                m["role"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("alice".to_string(), "tenant_admin".to_string()),
            ("bob".into(), "tenant_admin".into())
        ]
    );
    let (s, _) = disable(&e.app, t, &root, "bob").await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // a super-admin with a workspace of their own: that tenant without the header (not a switch),
    // another tenant with it (a switch)
    let ops = create_tenant(&e.app, "Ops", "sa2").await;
    let sa2 = super_admin("sa2");
    let ops_s = ops.to_string();
    for headers in [vec![], vec![ops_s.as_str()]] {
        let (s, b) = common::send(&probe(&e), probe_req(&sa2, &headers)).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["tenant"], json!(ops), "{headers:?}");
    }
    assert_eq!(
        scope_switches(db).await.len(),
        1,
        "acting at home is no switch"
    );
    let (s, b) = common::send(&probe(&e), probe_req(&sa2, &[tid.as_str()])).await;
    assert_eq!((s, b["tenant"].clone()), (StatusCode::OK, json!(t)));
    let switches = scope_switches(db).await;
    assert_eq!(switches.len(), 2);
    assert_eq!(switches[1].0, "sa2");
    assert_eq!(switches[1].1, Some(t));
}

#[tokio::test]
async fn last_admin_cannot_be_removed() {
    let e = common::env().await;
    let db = common::pool(&e);
    let t = create_tenant(&e.app, "Acme", "alice").await;
    let alice = token("alice", &[]);
    let root = super_admin(ROOT);

    for tok in [&alice, &root] {
        let (s, b) = disable(&e.app, t, tok, "alice").await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::CONFLICT, json!("last_admin")),
            "{b}"
        );
        let (s, b) = set_role(&e.app, t, tok, "alice", "tenant_user").await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::CONFLICT, json!("last_admin")),
            "{b}"
        );
    }
    // a no-op "demotion" to the same role is fine
    let (s, b) = set_role(&e.app, t, &alice, "alice", "tenant_admin").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let (s, b) = disable(&e.app, t, &alice, "nobody").await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, json!("user_not_found"))
    );

    // with a second admin, the first may be demoted; then the second is the last
    assert_eq!(
        add_user(&e.app, t, &alice, "bob", "tenant_admin").await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        add_user(&e.app, t, &alice, "vic", "tenant_user").await.0,
        StatusCode::CREATED
    );
    let (s, _) = set_role(&e.app, t, &alice, "alice", "tenant_user").await;
    assert_eq!(s, StatusCode::OK);
    let (s, b) = set_role(&e.app, t, &root, "bob", "tenant_user").await;
    assert_eq!((s, code(&b)), (StatusCode::CONFLICT, json!("last_admin")));
    let (s, b) = disable(&e.app, t, &root, "bob").await;
    assert_eq!((s, code(&b)), (StatusCode::CONFLICT, json!("last_admin")));
    // a tenant_user can always be disabled
    let (s, _) = disable(&e.app, t, &root, "vic").await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(admins(db, t).await, ["bob"]);

    // race: two admins disabled at once → exactly one succeeds, an admin remains
    let mut keep = "bob".to_string();
    for i in 0..5 {
        let newcomer = format!("n{i}");
        assert_eq!(
            add_user(&e.app, t, &root, &newcomer, "tenant_admin")
                .await
                .0,
            StatusCode::CREATED
        );
        let r1 = e.app.clone().oneshot(disable_req(t, &root, &keep));
        let r2 = e.app.clone().oneshot(disable_req(t, &root, &newcomer));
        let (r1, r2) = tokio::join!(r1, r2);
        let mut got = [r1.unwrap().status(), r2.unwrap().status()];
        got.sort();
        assert_eq!(got, [StatusCode::NO_CONTENT, StatusCode::CONFLICT]);
        let left = admins(db, t).await;
        assert_eq!(left.len(), 1, "{left:?}");
        keep = left[0].clone();
    }
    // race: two admins demoted at once → exactly one succeeds
    assert_eq!(
        add_user(&e.app, t, &root, "m", "tenant_admin").await.0,
        StatusCode::CREATED
    );
    let r1 = e
        .app
        .clone()
        .oneshot(set_role_req(t, &root, &keep, "tenant_user"));
    let r2 = e
        .app
        .clone()
        .oneshot(set_role_req(t, &root, "m", "tenant_user"));
    let (r1, r2) = tokio::join!(r1, r2);
    let mut got = [r1.unwrap().status(), r2.unwrap().status()];
    got.sort();
    assert_eq!(got, [StatusCode::OK, StatusCode::CONFLICT]);
    assert_eq!(admins(db, t).await.len(), 1);
}

#[tokio::test]
async fn writes_are_audited() {
    let e = common::env().await;
    let db = common::pool(&e);
    let t = create_tenant(&e.app, "Acme", "alice").await;

    let rows = audit_rows(db).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (actor, tenant, action, object, before, after) = &rows[0];
    // the actor is the token `sub` (stable), not the display username
    assert_eq!(actor, ROOT);
    assert_eq!(*tenant, Some(t));
    assert_eq!(action, "tenant.create");
    assert_eq!(object, &format!("tenant:{t}"));
    assert!(before.is_none());
    let after = after.as_deref().unwrap();
    assert!(
        after.len() == 64 && after.bytes().all(|c| c.is_ascii_hexdigit()),
        "{after}"
    );

    let alice = token("alice", &[]);
    assert_eq!(
        add_user(&e.app, t, &alice, "bob", "tenant_user").await.0,
        StatusCode::CREATED
    );
    // refused writes record nothing
    assert_eq!(
        add_user(&e.app, t, &alice, "bob", "tenant_user").await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        disable(&e.app, t, &alice, "alice").await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        set_role(&e.app, t, &alice, "bob", "tenant_admin").await.0,
        StatusCode::OK
    );
    assert_eq!(
        disable(&e.app, t, &alice, "bob").await.0,
        StatusCode::NO_CONTENT
    );

    let rows = audit_rows(db).await;
    let summary: Vec<_> = rows
        .iter()
        .map(|(a, _, act, obj, b, af)| {
            (
                a.clone(),
                act.clone(),
                obj.clone(),
                b.is_some(),
                af.is_some(),
            )
        })
        .collect();
    let bob = format!("user:{t}:bob");
    let ev = |a: &str, act: &str, obj: &str, b: bool, af: bool| {
        (a.to_string(), act.to_string(), obj.to_string(), b, af)
    };
    assert_eq!(
        summary,
        [
            ev(ROOT, "tenant.create", &format!("tenant:{t}"), false, true),
            ev("alice", "user.add", &bob, false, true),
            ev("alice", "user.role", &bob, true, true),
            ev("alice", "user.disable", &bob, true, false),
        ]
    );
    // each event's before hashes the state the previous one left
    assert_eq!(rows[1].5, rows[2].4);
    assert_eq!(rows[2].5, rows[3].4);
    assert_ne!(rows[2].4, rows[2].5, "the role change changed the state");
    assert!(rows.iter().all(|r| r.1 == Some(t)));
}

/// One tenant per identity: a `sub` with a `users` row anywhere (even disabled) cannot be added to
/// another tenant, nor made another tenant's first admin.
#[tokio::test]
async fn user_in_one_tenant_only_409() {
    let e = common::env().await;
    let a = create_tenant(&e.app, "Acme", "alice").await;
    let b = create_tenant(&e.app, "Beta", "bob").await;
    let root = super_admin(ROOT);

    let (s, body) = add_user(&e.app, b, &token("bob", &[]), "alice", "tenant_user").await;
    assert_eq!(
        (s, code(&body)),
        (StatusCode::CONFLICT, json!("user_exists")),
        "{body}"
    );
    let (s, body) = add_user(&e.app, b, &root, "alice", "tenant_admin").await;
    assert_eq!(
        (s, code(&body)),
        (StatusCode::CONFLICT, json!("user_exists")),
        "{body}"
    );
    let (s, body) = common::send(
        &e.app,
        common::json(
            "POST",
            "/api/tenants",
            Some(&root),
            json!({"name": "Gamma", "first_admin_sub": "alice"}),
        ),
    )
    .await;
    assert_eq!(
        (s, code(&body)),
        (StatusCode::CONFLICT, json!("user_exists")),
        "{body}"
    );
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM tenant ORDER BY name")
        .fetch_all(common::pool(&e))
        .await
        .unwrap();
    assert_eq!(
        names,
        ["Acme", "Beta"],
        "the refused create left no tenant behind"
    );

    // a disabled user stays bound to their tenant
    assert_eq!(
        add_user(&e.app, a, &token("alice", &[]), "carol", "tenant_user")
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        disable(&e.app, a, &token("alice", &[]), "carol").await.0,
        StatusCode::NO_CONTENT
    );
    let (s, body) = add_user(&e.app, b, &root, "carol", "tenant_user").await;
    assert_eq!(
        (s, code(&body)),
        (StatusCode::CONFLICT, json!("user_exists"))
    );
    let home: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT tenant, sub FROM users WHERE sub IN ('alice', 'carol') ORDER BY sub",
    )
    .fetch_all(common::pool(&e))
    .await
    .unwrap();
    assert_eq!(home, [(a, "alice".to_string()), (a, "carol".to_string())]);
}
