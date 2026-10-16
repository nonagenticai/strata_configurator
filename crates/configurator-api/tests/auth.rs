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

/// Super-admin creates a tenant whose first admin is `first_admin`; returns its id.
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

async fn add_member(
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
            &format!("/api/tenants/{tenant}/members"),
            Some(as_tok),
            json!({"sub": sub, "role": role}),
        ),
    )
    .await
}

fn delete(uri: &str, tok: &str) -> Request<Body> {
    Request::delete(uri)
        .header("authorization", format!("Bearer {tok}"))
        .body(Body::empty())
        .unwrap()
}

/// A route that only extracts `TenantCtx` (the `X-Tenant-Id` path every tenant-scoped route uses).
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
        b = b.header("x-tenant-id", *h);
    }
    b.body(Body::empty()).unwrap()
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

#[tokio::test]
async fn me_lists_memberships() {
    let e = common::env().await;
    let a = create_tenant(&e.app, "Acme", "alice").await;
    let b = create_tenant(&e.app, "Beta", "bob").await;
    create_tenant(&e.app, "Gamma", "carol").await;
    let (s, _) = add_member(&e.app, b, &token("bob", &[]), "alice", "editor").await;
    assert_eq!(s, StatusCode::CREATED);

    let (s, me) = common::send(&e.app, common::get("/api/me", Some(&token("alice", &[])))).await;
    assert_eq!(s, StatusCode::OK, "{me}");
    assert_eq!(
        me,
        json!({"sub": "alice", "super_admin": false, "memberships": [
            {"tenant": a, "name": "Acme", "role": "admin"},
            {"tenant": b, "name": "Beta", "role": "editor"},
        ]})
    );
    // the super-admin role comes from the token; membership is separate
    let (_, me) = common::send(&e.app, common::get("/api/me", Some(&super_admin(ROOT)))).await;
    assert_eq!(
        me,
        json!({"sub": ROOT, "super_admin": true, "memberships": []})
    );
    // someone with no memberships
    let (_, me) = common::send(
        &e.app,
        common::get("/api/me", Some(&token("dave", &["other-role"]))),
    )
    .await;
    assert_eq!(
        me,
        json!({"sub": "dave", "super_admin": false, "memberships": []})
    );
}

#[tokio::test]
async fn only_super_admin_creates_tenants() {
    let e = common::env().await;
    let body = json!({"name": "Acme", "first_admin_sub": "alice"});
    for tok in [
        token("alice", &[]),
        token("alice", &["config-admin", "admin"]),
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
    // the first admin is a member, as admin
    let role: String =
        sqlx::query_scalar("SELECT role::text FROM member WHERE tenant = $1 AND sub = 'alice'")
            .bind(id)
            .fetch_one(common::pool(&e))
            .await
            .unwrap();
    assert_eq!(role, "admin");

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
    let stored: Vec<String> = sqlx::query_scalar("SELECT sub FROM member WHERE tenant = $1")
        .bind(tid)
        .fetch_all(common::pool(&e))
        .await
        .unwrap();
    assert_eq!(stored, ["trimmed"]);
    let (s, b) = add_member(
        &e.app,
        tid,
        &super_admin(ROOT),
        &format!(" {} ", "s".repeat(255)),
        "viewer",
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(b["sub"], "s".repeat(255));
    let (s, b) = add_member(&e.app, tid, &super_admin(ROOT), " trimmed ", "viewer").await;
    assert_eq!(
        (s, b["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("member_exists")),
        "a padded sub is the same member"
    );
    sqlx::query("DELETE FROM tenant WHERE id = $1")
        .bind(tid)
        .execute(common::pool(&e))
        .await
        .unwrap();

    // listing: the super-admin sees all, others only their own
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
    assert_eq!(names(&all), ["Acme", "Beta"]);
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
async fn tenant_header_errors() {
    let e = common::env().await;
    let t = create_tenant(&e.app, "Acme", "alice").await;
    let tid_s = t.to_string();
    let other_s = Uuid::new_v4().to_string();
    let braced_s = format!("{{{tid_s}}}");
    let urn_s = format!("urn:uuid:{tid_s}");
    let simple_s = t.simple().to_string();
    let (tid, other, braced, urn, simple) = (
        tid_s.as_str(),
        other_s.as_str(),
        braced_s.as_str(),
        urn_s.as_str(),
        simple_s.as_str(),
    );
    let alice = token("alice", &[]);
    let app = probe(&e);

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
    for (headers, status, code) in cases {
        let (s, b) = common::send(&app, probe_req(&alice, &headers)).await;
        assert_eq!(s, status, "{headers:?}: {b}");
        assert_eq!(b["error"]["code"], code, "{headers:?}");
    }
    // control: one bare uuid of a tenant the caller belongs to
    let (s, b) = common::send(&app, probe_req(&alice, &[tid])).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b, json!({"tenant": t, "role": "admin", "sub": "alice"}));
    // the path form of the member routes applies the same rules
    let (s, b) = common::send(
        &e.app,
        common::get("/api/tenants/not-a-uuid/members", Some(&alice)),
    )
    .await;
    assert_eq!(
        (s, b["error"]["code"].clone()),
        (StatusCode::BAD_REQUEST, json!("invalid_tenant"))
    );
    let (s, b) = common::send(
        &e.app,
        common::get(&format!("/api/tenants/{other}/members"), Some(&alice)),
    )
    .await;
    assert_eq!(
        (s, b["error"]["code"].clone()),
        (StatusCode::NOT_FOUND, json!("tenant_not_found"))
    );
}

#[tokio::test]
async fn non_member_403() {
    let e = common::env().await;
    let t = create_tenant(&e.app, "Acme", "alice").await;
    let tid = t.to_string();
    let app = probe(&e);
    let carol = token("carol", &[]);

    let (s, b) = common::send(&app, probe_req(&carol, &[tid.as_str()])).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
    assert_eq!(b["error"]["code"], "not_a_member");
    let members = format!("/api/tenants/{tid}/members");
    let (s, b) = common::send(&e.app, common::get(&members, Some(&carol))).await;
    assert_eq!(
        (s, b["error"]["code"].clone()),
        (StatusCode::FORBIDDEN, json!("not_a_member"))
    );
    let (s, _) = add_member(&e.app, t, &carol, "carol", "admin").await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "a non-member cannot add themselves"
    );
    let (s, _) = common::send(&e.app, delete(&format!("{members}/alice"), &carol)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // a member below admin is in the tenant but cannot manage members
    let (s, _) = add_member(&e.app, t, &token("alice", &[]), "victor", "approver").await;
    assert_eq!(s, StatusCode::CREATED);
    let victor = token("victor", &[]);
    let (s, b) = common::send(&app, probe_req(&victor, &[tid.as_str()])).await;
    assert_eq!((s, b["role"].clone()), (StatusCode::OK, json!("approver")));
    let (s, b) = common::send(&e.app, common::get(&members, Some(&victor))).await;
    assert_eq!(
        (s, b["error"]["code"].clone()),
        (StatusCode::FORBIDDEN, json!("forbidden"))
    );
    let (s, _) = add_member(&e.app, t, &victor, "mallory", "viewer").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = common::send(&e.app, delete(&format!("{members}/alice"), &victor)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM member WHERE tenant = $1")
        .bind(t)
        .fetch_one(common::pool(&e))
        .await
        .unwrap();
    assert_eq!(n, 2, "refused writes changed nothing");
}

#[tokio::test]
async fn super_admin_acts_in_any_tenant() {
    let e = common::env().await;
    let t = create_tenant(&e.app, "Acme", "alice").await;
    let tid = t.to_string();
    let root = super_admin(ROOT);

    // not a member, yet admin in the tenant
    let (s, b) = common::send(&probe(&e), probe_req(&root, &[tid.as_str()])).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["role"], "admin");
    // ... but still only in tenants that exist
    let (s, b) = common::send(
        &probe(&e),
        probe_req(&root, &[Uuid::new_v4().to_string().as_str()]),
    )
    .await;
    assert_eq!(
        (s, b["error"]["code"].clone()),
        (StatusCode::NOT_FOUND, json!("tenant_not_found"))
    );

    let (s, b) = add_member(&e.app, t, &root, "bob", "editor").await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(
        (b["sub"].clone(), b["role"].clone()),
        (json!("bob"), json!("editor"))
    );
    let (s, b) = add_member(&e.app, t, &root, "bob", "viewer").await;
    assert_eq!(
        (s, b["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("member_exists"))
    );
    let (s, b) = add_member(&e.app, t, &root, "eve", "owner").await;
    assert_eq!(
        (s, b["error"]["code"].clone()),
        (StatusCode::BAD_REQUEST, json!("invalid_body"))
    );

    let (s, b) = common::send(
        &e.app,
        common::get(&format!("/api/tenants/{tid}/members"), Some(&root)),
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
            ("alice".to_string(), "admin".to_string()),
            ("bob".into(), "editor".into())
        ]
    );

    let (s, _) = common::send(
        &e.app,
        delete(&format!("/api/tenants/{tid}/members/bob"), &root),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn last_admin_cannot_be_removed() {
    let e = common::env().await;
    let t = create_tenant(&e.app, "Acme", "alice").await;
    let base = format!("/api/tenants/{t}/members");
    let alice = token("alice", &[]);
    let root = super_admin(ROOT);

    for tok in [&alice, &root] {
        let (s, b) = common::send(&e.app, delete(&format!("{base}/alice"), tok)).await;
        assert_eq!(s, StatusCode::CONFLICT, "{b}");
        assert_eq!(b["error"]["code"], "last_admin");
    }
    let (s, b) = common::send(&e.app, delete(&format!("{base}/nobody"), &alice)).await;
    assert_eq!(
        (s, b["error"]["code"].clone()),
        (StatusCode::NOT_FOUND, json!("member_not_found"))
    );

    // with a second admin, the first may go; then the second is the last
    assert_eq!(
        add_member(&e.app, t, &alice, "bob", "admin").await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        add_member(&e.app, t, &alice, "vic", "viewer").await.0,
        StatusCode::CREATED
    );
    let (s, _) = common::send(&e.app, delete(&format!("{base}/alice"), &alice)).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, b) = common::send(&e.app, delete(&format!("{base}/bob"), &root)).await;
    assert_eq!(
        (s, b["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("last_admin"))
    );
    // a non-admin can always be removed
    let (s, _) = common::send(&e.app, delete(&format!("{base}/vic"), &root)).await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // race: two admins removed at once → exactly one succeeds, an admin remains
    assert_eq!(
        add_member(&e.app, t, &root, "cy", "admin").await.0,
        StatusCode::CREATED
    );
    for _ in 0..5 {
        let r1 = e.app.clone().oneshot(delete(&format!("{base}/bob"), &root));
        let r2 = e.app.clone().oneshot(delete(&format!("{base}/cy"), &root));
        let (r1, r2) = tokio::join!(r1, r2);
        let mut got = [r1.unwrap().status(), r2.unwrap().status()];
        got.sort();
        assert_eq!(got, [StatusCode::NO_CONTENT, StatusCode::CONFLICT]);
        let admins: Vec<String> =
            sqlx::query_scalar("SELECT sub FROM member WHERE tenant = $1 AND role = 'admin'")
                .bind(t)
                .fetch_all(common::pool(&e))
                .await
                .unwrap();
        assert_eq!(admins.len(), 1, "{admins:?}");
        // put the removed one back for the next round
        let gone = if admins[0] == "bob" { "cy" } else { "bob" };
        assert_eq!(
            add_member(&e.app, t, &root, gone, "admin").await.0,
            StatusCode::CREATED
        );
    }
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
        add_member(&e.app, t, &alice, "bob", "editor").await.0,
        StatusCode::CREATED
    );
    // refused writes record nothing
    assert_eq!(
        add_member(&e.app, t, &alice, "bob", "editor").await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        common::send(
            &e.app,
            delete(&format!("/api/tenants/{t}/members/alice"), &alice)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (s, _) = common::send(
        &e.app,
        delete(&format!("/api/tenants/{t}/members/bob"), &alice),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

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
    let bob = format!("member:{t}:bob");
    assert_eq!(
        summary,
        [
            (
                ROOT.to_string(),
                "tenant.create".to_string(),
                format!("tenant:{t}"),
                false,
                true
            ),
            (
                "alice".to_string(),
                "member.add".to_string(),
                bob.clone(),
                false,
                true
            ),
            (
                "alice".to_string(),
                "member.remove".to_string(),
                bob,
                true,
                false
            ),
        ]
    );
    // add's after and remove's before hash the same member state
    assert_eq!(rows[1].5, rows[2].4);
    assert!(rows.iter().all(|r| r.1 == Some(t)));
}
