//! Layers and overlay rows API (plan Task 9; spec sections 4.1 and 10.1).
mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::keys::{super_admin, token};
use configurator_api::db::rows::load_for_layers;
use configurator_core::layer::{LayerKey, Scope};
use configurator_core::overlay::{Kind, Op};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const ROOT: &str = "root";

/// A caller: a bearer token and the `X-Tenant-Id` it sends (if any).
#[derive(Clone)]
struct Who {
    tok: String,
    tenant: Option<Uuid>,
}

impl Who {
    /// The super-admin without `X-Tenant-Id` (platform view).
    fn root() -> Who {
        Who {
            tok: super_admin(ROOT),
            tenant: None,
        }
    }

    /// `sub` (no realm roles) acting in `tenant`.
    fn member(sub: &str, tenant: Uuid) -> Who {
        Who {
            tok: token(sub, &[]),
            tenant: Some(tenant),
        }
    }

    fn req(&self, method: &str, uri: &str, body: Option<&Value>) -> Request<Body> {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {}", self.tok));
        if let Some(t) = self.tenant {
            b = b.header("x-tenant-id", t.to_string());
        }
        match body {
            Some(v) => b
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        }
    }

    async fn get(&self, app: &Router, uri: &str) -> (StatusCode, Value) {
        common::send(app, self.req("GET", uri, None)).await
    }

    async fn delete(&self, app: &Router, uri: &str) -> (StatusCode, Value) {
        common::send(app, self.req("DELETE", uri, None)).await
    }

    async fn send(
        &self,
        app: &Router,
        method: &str,
        uri: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        common::send(app, self.req(method, uri, Some(&body))).await
    }
}

/// Super-admin creates a tenant whose first admin is `admin`; `members` are `(sub, role)`.
async fn tenant(app: &Router, name: &str, admin: &str, members: &[(&str, &str)]) -> Uuid {
    let (s, b) = Who::root()
        .send(
            app,
            "POST",
            "/api/tenants",
            json!({"name": name, "first_admin_sub": admin}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let id = Uuid::parse_str(b["id"].as_str().unwrap()).unwrap();
    for (sub, role) in members {
        let (s, b) = Who::root()
            .send(
                app,
                "POST",
                &format!("/api/tenants/{id}/members"),
                json!({"sub": sub, "role": role}),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
    }
    id
}

/// `who` creates a layer; returns its id.
async fn layer(app: &Router, who: &Who, body: Value) -> Uuid {
    let (s, b) = who.send(app, "POST", "/api/layers", body).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    Uuid::parse_str(b["id"].as_str().unwrap()).unwrap()
}

fn entity(key: &str, label: &str) -> Value {
    json!({"kind": "entity", "key": key, "op": "add", "fields": {"label": label}})
}

fn patch(label: &str, expected_rev: i64) -> Value {
    json!({"op": "patch", "fields": {"label": label}, "expected_rev": expected_rev})
}

/// `who` adds `body` as a row of `layer`; asserts 201 and returns the row.
async fn add(app: &Router, who: &Who, layer: Uuid, body: Value) -> Value {
    let (s, b) = who
        .send(app, "POST", &format!("/api/layers/{layer}/rows"), body)
        .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    b
}

fn rows_uri(layer: Uuid) -> String {
    format!("/api/layers/{layer}/rows")
}

fn row_uri(layer: Uuid, kind: &str, key: &str) -> String {
    format!("/api/layers/{layer}/rows/{kind}/{key}")
}

fn items(v: &Value) -> &Vec<Value> {
    v["items"].as_array().expect("a Paged body")
}

fn ids(v: &Value) -> Vec<String> {
    items(v)
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

fn revs(v: &Value) -> Vec<(i64, bool)> {
    items(v)
        .iter()
        .map(|r| {
            (
                r["rev"].as_i64().unwrap(),
                r["is_current"].as_bool().unwrap(),
            )
        })
        .collect()
}

/// (actor, tenant, action) of every audit event, in order.
async fn audit(db: &PgPool) -> Vec<(String, Option<Uuid>, String)> {
    sqlx::query_as("SELECT actor, tenant, action FROM audit_event ORDER BY id")
        .fetch_all(db)
        .await
        .unwrap()
}

async fn row_count(db: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM overlay_row")
        .fetch_one(db)
        .await
        .unwrap()
}

fn code(b: &Value) -> &str {
    b["error"]["code"].as_str().unwrap_or("<none>")
}

#[tokio::test]
async fn editor_writes_own_tenant_layer() {
    let e = common::env().await;
    let a = tenant(&e.app, "Acme", "alice", &[("ed", "editor")]).await;
    let ed = Who::member("ed", a);

    let (s, b) = ed
        .send(&e.app, "POST", "/api/layers", json!({"scope": "tenant"}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(b["scope"], "tenant");
    assert_eq!(
        b["tenant"],
        a.to_string(),
        "the tenant comes from X-Tenant-Id"
    );
    let l = Uuid::parse_str(b["id"].as_str().unwrap()).unwrap();
    let (s, b) = ed
        .send(&e.app, "POST", "/api/layers", json!({"scope": "tenant"}))
        .await;
    assert_eq!((s, code(&b)), (StatusCode::CONFLICT, "layer_exists"));

    let row = add(&e.app, &ed, l, entity("Order", "Order")).await;
    assert_eq!(row["rev"], 1);
    assert_eq!(row["is_current"], true);
    assert_eq!(row["author"], "ed");
    assert_eq!(row["layer_id"], l.to_string());
    assert_eq!(row["fields"], json!({"label": "Order"}));
    let (s, b) = ed
        .send(&e.app, "POST", &rows_uri(l), entity("Order", "Again"))
        .await;
    assert_eq!((s, code(&b)), (StatusCode::CONFLICT, "row_exists"));

    let (s, list) = ed.get(&e.app, &rows_uri(l)).await;
    assert_eq!(s, StatusCode::OK, "{list}");
    assert_eq!(ids(&list), [row["id"].as_str().unwrap()]);
    assert!(list["next"].is_null());
    let (s, all) = ed.get(&e.app, "/api/layers").await;
    assert_eq!(s, StatusCode::OK, "{all}");
    assert_eq!(ids(&all), [l.to_string()]);
    let (s, one) = ed.get(&e.app, &format!("/api/layers/{l}")).await;
    assert_eq!(
        (s, one["id"].clone()),
        (StatusCode::OK, json!(l.to_string()))
    );

    let events: Vec<_> = audit(common::pool(&e))
        .await
        .into_iter()
        .filter(|(_, t, _)| *t == Some(a))
        .filter(|(_, _, act)| !act.starts_with("member") && !act.starts_with("tenant"))
        .collect();
    assert_eq!(
        events,
        [
            ("ed".to_string(), Some(a), "layer.create".to_string()),
            ("ed".to_string(), Some(a), "overlay_row.create".to_string()),
        ],
        "every write audited once, by sub; failed writes are not"
    );
}

#[tokio::test]
async fn viewer_cannot_write_403() {
    let e = common::env().await;
    let a = tenant(&e.app, "Acme", "alice", &[("vic", "viewer")]).await;
    let alice = Who::member("alice", a);
    let vic = Who::member("vic", a);
    let l = layer(&e.app, &alice, json!({"scope": "tenant"})).await;
    add(&e.app, &alice, l, entity("Order", "Order")).await;
    let before = row_count(common::pool(&e)).await;

    // reads are fine
    let (s, b) = vic.get(&e.app, &rows_uri(l)).await;
    assert_eq!((s, items(&b).len()), (StatusCode::OK, 1));
    let (s, _) = vic
        .get(
            &e.app,
            &format!("{}/revisions", row_uri(l, "entity", "Order")),
        )
        .await;
    assert_eq!(s, StatusCode::OK);

    // every write is 403
    let (s, b) = vic
        .send(&e.app, "POST", &rows_uri(l), entity("Customer", "Customer"))
        .await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, "forbidden"), "{b}");
    let (s, b) = vic
        .send(&e.app, "PUT", &row_uri(l, "entity", "Order"), patch("X", 1))
        .await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, "forbidden"), "{b}");
    let (s, b) = vic
        .delete(
            &e.app,
            &format!("{}?expected_rev=1", row_uri(l, "entity", "Order")),
        )
        .await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, "forbidden"), "{b}");
    let (s, b) = vic
        .send(&e.app, "POST", "/api/layers", json!({"scope": "tenant"}))
        .await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, "forbidden"), "{b}");
    assert_eq!(row_count(common::pool(&e)).await, before, "nothing written");
}

#[tokio::test]
async fn tenant_cannot_write_platform_layer_403() {
    let e = common::env().await;
    let a = tenant(&e.app, "Acme", "alice", &[]).await;
    let root = Who::root();
    let base = layer(&e.app, &root, json!({"scope": "base"})).await;
    add(&e.app, &root, base, entity("Order", "Order")).await;
    // the tenant's admin, the strongest tenant role
    let alice = Who::member("alice", a);

    let (s, b) = alice.get(&e.app, "/api/layers").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(ids(&b), [base.to_string()], "platform layers are visible");
    let (s, b) = alice.get(&e.app, &rows_uri(base)).await;
    assert_eq!((s, items(&b).len()), (StatusCode::OK, 1));

    let (s, b) = alice
        .send(
            &e.app,
            "POST",
            &rows_uri(base),
            entity("Customer", "Customer"),
        )
        .await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, "forbidden"), "{b}");
    let (s, b) = alice
        .send(
            &e.app,
            "PUT",
            &row_uri(base, "entity", "Order"),
            patch("X", 1),
        )
        .await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, "forbidden"), "{b}");
    let (s, b) = alice
        .delete(
            &e.app,
            &format!("{}?expected_rev=1", row_uri(base, "entity", "Order")),
        )
        .await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, "forbidden"), "{b}");
    for body in [
        json!({"scope": "base"}),
        json!({"scope": "industry", "industry": "retail"}),
        json!({"scope": "job", "job": "customer_care_agent"}),
    ] {
        let (s, b) = alice.send(&e.app, "POST", "/api/layers", body).await;
        assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, "forbidden"), "{b}");
    }
    // a non-super-admin must name a tenant
    let (s, b) = Who {
        tok: token("alice", &[]),
        tenant: None,
    }
    .get(&e.app, "/api/layers")
    .await;
    assert_eq!((s, code(&b)), (StatusCode::BAD_REQUEST, "tenant_required"));
    let (s, b) = Who::root().get(&e.app, &rows_uri(base)).await;
    assert_eq!(items(&b).len(), 1, "{s} {b}");
    assert_eq!(items(&b)[0]["rev"], 1, "the platform row is untouched");
}

#[tokio::test]
async fn super_admin_writes_base() {
    let e = common::env().await;
    let a = tenant(&e.app, "Acme", "alice", &[]).await;
    let root = Who::root();

    let (s, b) = root
        .send(&e.app, "POST", "/api/layers", json!({"scope": "base"}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(
        (b["scope"].clone(), b["tenant"].clone()),
        (json!("base"), Value::Null)
    );
    let base = Uuid::parse_str(b["id"].as_str().unwrap()).unwrap();
    let (s, b) = root
        .send(&e.app, "POST", "/api/layers", json!({"scope": "base"}))
        .await;
    assert_eq!((s, code(&b)), (StatusCode::CONFLICT, "layer_exists"));
    let (s, b) = root
        .send(
            &e.app,
            "POST",
            "/api/layers",
            json!({"scope": "job", "job": "customer_care_agent", "dimension": ["channel", "voice"]}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(b["dimension"], json!(["channel", "voice"]));

    // bad layer keys
    for (body, want) in [
        (json!({"scope": "industry"}), "invalid_layer"),
        (
            json!({"scope": "base", "industry": "retail"}),
            "invalid_layer",
        ),
        (json!({"scope": "base", "tenant": a}), "invalid_body"),
        (json!({"scope": "tenant"}), "tenant_required"),
    ] {
        let (s, b) = root.send(&e.app, "POST", "/api/layers", body.clone()).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::BAD_REQUEST, want),
            "{body} → {b}"
        );
    }

    let row = add(&e.app, &root, base, entity("Order", "Order")).await;
    assert_eq!(
        (row["rev"].clone(), row["author"].clone()),
        (json!(1), json!(ROOT))
    );
    let (s, b) = root
        .send(
            &e.app,
            "PUT",
            &row_uri(base, "entity", "Order"),
            patch("Purchase order", 1),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        (b["rev"].clone(), b["op"].clone()),
        (json!(2), json!("patch"))
    );

    // with X-Tenant-Id the super-admin acts as that tenant's admin
    let as_a = Who {
        tok: super_admin(ROOT),
        tenant: Some(a),
    };
    let ta = layer(&e.app, &as_a, json!({"scope": "tenant"})).await;
    let (_, platform) = root.get(&e.app, "/api/layers").await;
    assert!(
        !ids(&platform).contains(&ta.to_string()),
        "the platform view holds no tenant's layer"
    );
    assert!(ids(&platform).contains(&base.to_string()));
    let (_, in_a) = as_a.get(&e.app, "/api/layers").await;
    assert!(ids(&in_a).contains(&ta.to_string()));

    let events: Vec<_> = audit(common::pool(&e))
        .await
        .into_iter()
        .filter(|(_, _, act)| act.starts_with("layer") || act.starts_with("overlay_row"))
        .collect();
    let r = || ROOT.to_string();
    assert_eq!(
        events,
        [
            (r(), None, "layer.create".to_string()),
            (r(), None, "layer.create".to_string()),
            (r(), None, "overlay_row.create".to_string()),
            (r(), None, "overlay_row.update".to_string()),
            (r(), Some(a), "layer.create".to_string()),
        ]
    );
}

#[tokio::test]
async fn invalid_key_400() {
    let e = common::env().await;
    let root = Who::root();
    let base = layer(&e.app, &root, json!({"scope": "base"})).await;
    add(&e.app, &root, base, entity("Order", "Order")).await;
    let before = row_count(common::pool(&e)).await;

    for (kind, key) in [
        ("entity", "order"),
        ("attribute", "Order.Status"),
        ("relation", "Order|paid by|Payment"),
        ("job", "Customer-Care"),
    ] {
        let body = json!({"kind": kind, "key": key, "op": "tombstone"});
        let (s, b) = root.send(&e.app, "POST", &rows_uri(base), body).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::BAD_REQUEST, "invalid_key"),
            "{key}: {b}"
        );
        assert_eq!(b["errors"][0]["code"], "invalid_key", "{b}");
        assert_eq!(b["errors"][0]["field"], "key", "{b}");
    }
    // `order` and `Order` can never coexist: the lowercase spelling is refused on PUT too
    let (s, b) = root
        .send(
            &e.app,
            "PUT",
            &row_uri(base, "entity", "order"),
            patch("x", 1),
        )
        .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, "invalid_key"),
        "{b}"
    );
    let (s, b) = root
        .send(
            &e.app,
            "PUT",
            &row_uri(base, "widget", "Order"),
            patch("x", 1),
        )
        .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, "invalid_kind"),
        "{b}"
    );
    assert_eq!(row_count(common::pool(&e)).await, before, "nothing written");
}

#[tokio::test]
async fn invalid_fields_400_lists_all() {
    let e = common::env().await;
    let root = Who::root();
    let base = layer(&e.app, &root, json!({"scope": "base"})).await;

    let body = json!({"kind": "entity", "key": "Order", "op": "add",
                      "fields": {"bogus": true, "hidden": "yes"}});
    let (s, b) = root.send(&e.app, "POST", &rows_uri(base), body).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, "invalid_fields"),
        "{b}"
    );
    let mut got: Vec<(String, String)> = b["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            assert!(e["message"].as_str().is_some_and(|m| !m.is_empty()), "{e}");
            (
                e["code"].as_str().unwrap().to_string(),
                e["field"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    got.sort();
    assert_eq!(
        got,
        [
            ("invalid_value".to_string(), "hidden".to_string()),
            ("missing_field".to_string(), "label".to_string()),
            ("unknown_field".to_string(), "bogus".to_string()),
        ],
        "every field error, not just the first"
    );
    assert_eq!(row_count(common::pool(&e)).await, 0);

    // PUT validates the same way
    add(&e.app, &root, base, entity("Order", "Order")).await;
    let (s, b) = root
        .send(
            &e.app,
            "PUT",
            &row_uri(base, "entity", "Order"),
            json!({"op": "patch", "fields": {"label": 7, "synonyms": "x"}, "expected_rev": 1}),
        )
        .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, "invalid_fields"),
        "{b}"
    );
    assert_eq!(b["errors"].as_array().unwrap().len(), 2, "{b}");
    let (s, b) = root
        .send(
            &e.app,
            "PUT",
            &row_uri(base, "entity", "Order"),
            json!({"op": "tombstone", "fields": {"label": "x"}, "expected_rev": 1}),
        )
        .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, "invalid_fields"),
        "{b}"
    );
    assert_eq!(b["errors"][0]["code"], "fields_on_tombstone");
    assert_eq!(row_count(common::pool(&e)).await, 1);
}

#[tokio::test]
async fn revision_history_kept() {
    let e = common::env().await;
    let a = tenant(&e.app, "Acme", "alice", &[]).await;
    let alice = Who::member("alice", a);
    let l = layer(&e.app, &alice, json!({"scope": "tenant"})).await;
    let order = row_uri(l, "entity", "Order");

    add(&e.app, &alice, l, entity("Order", "Order")).await;
    let (s, b) = alice
        .send(&e.app, "PUT", &order, patch("Order v2", 1))
        .await;
    assert_eq!((s, b["rev"].clone()), (StatusCode::OK, json!(2)), "{b}");
    let (s, b) = alice
        .send(
            &e.app,
            "PUT",
            &order,
            json!({"op": "tombstone", "expected_rev": 2}),
        )
        .await;
    assert_eq!((s, b["rev"].clone()), (StatusCode::OK, json!(3)), "{b}");
    assert_eq!(b["op"], "tombstone");

    // stale and unknown
    let (s, b) = alice.send(&e.app, "PUT", &order, patch("stale", 1)).await;
    assert_eq!((s, code(&b)), (StatusCode::CONFLICT, "revision_conflict"));
    let (s, b) = alice
        .send(
            &e.app,
            "PUT",
            &row_uri(l, "entity", "Customer"),
            patch("x", 1),
        )
        .await;
    assert_eq!((s, code(&b)), (StatusCode::NOT_FOUND, "row_not_found"));

    let (s, h) = alice.get(&e.app, &format!("{order}/revisions")).await;
    assert_eq!(s, StatusCode::OK, "{h}");
    assert_eq!(
        revs(&h),
        [(3, true), (2, false), (1, false)],
        "newest first"
    );
    let labels: Vec<Value> = items(&h)
        .iter()
        .map(|r| r["fields"]["label"].clone())
        .collect();
    assert_eq!(labels, [Value::Null, json!("Order v2"), json!("Order")]);
    let (_, cur) = alice.get(&e.app, &rows_uri(l)).await;
    assert_eq!(
        revs(&cur),
        [(3, true)],
        "only the current revision is a row of the layer"
    );

    // revisions page
    let (_, p1) = alice
        .get(&e.app, &format!("{order}/revisions?limit=2"))
        .await;
    assert_eq!(revs(&p1), [(3, true), (2, false)]);
    let next = p1["next"].as_str().expect("a next cursor").to_string();
    let (_, p2) = alice
        .get(&e.app, &format!("{order}/revisions?limit=2&before={next}"))
        .await;
    assert_eq!(revs(&p2), [(1, false)]);
    assert!(p2["next"].is_null());
    let (s, b) = alice
        .get(
            &e.app,
            &format!("{}/revisions", row_uri(l, "entity", "Customer")),
        )
        .await;
    assert_eq!((s, code(&b)), (StatusCode::NOT_FOUND, "row_not_found"));
}

#[tokio::test]
async fn concurrent_put_one_wins() {
    let e = common::env().await;
    let a = tenant(&e.app, "Acme", "alice", &[("ed", "editor")]).await;
    let alice = Who::member("alice", a);
    let ed = Who::member("ed", a);
    let l = layer(&e.app, &alice, json!({"scope": "tenant"})).await;
    let order = row_uri(l, "entity", "Order");
    add(&e.app, &alice, l, entity("Order", "Order")).await;

    // several rounds: two PUTs with the same expected_rev, sent concurrently
    let rounds = 5;
    for round in 1..=rounds {
        let (r1, r2) = tokio::join!(
            alice.send(&e.app, "PUT", &order, patch("by alice", round)),
            ed.send(&e.app, "PUT", &order, patch("by ed", round)),
        );
        let mut got = [r1.clone(), r2.clone()];
        got.sort_by_key(|(s, _)| s.as_u16());
        assert_eq!(
            (got[0].0, got[1].0),
            (StatusCode::OK, StatusCode::CONFLICT),
            "round {round}: {r1:?} / {r2:?}"
        );
        assert_eq!(code(&got[1].1), "revision_conflict");
        assert_eq!(got[0].1["rev"], round + 1);
    }
    let (_, h) = alice.get(&e.app, &format!("{order}/revisions")).await;
    assert_eq!(
        items(&h).len() as i64,
        rounds + 1,
        "one revision per winning PUT"
    );
    assert_eq!(
        revs(&h).iter().filter(|(_, cur)| *cur).count(),
        1,
        "exactly one current revision"
    );

    // concurrent POSTs of one new object: one 201, one 409 `row_exists`
    let (r1, r2) = tokio::join!(
        alice.send(&e.app, "POST", &rows_uri(l), entity("Customer", "Customer")),
        ed.send(&e.app, "POST", &rows_uri(l), entity("Customer", "Client")),
    );
    let mut got = [r1.0.as_u16(), r2.0.as_u16()];
    got.sort();
    assert_eq!(got, [201, 409], "{r1:?} / {r2:?}");

    // a unique violation from the revision constraint is a 409, never a 500: plant a stale
    // non-current revision where the next PUT will try to write
    let current = rounds as i32 + 1;
    sqlx::query(
        "INSERT INTO overlay_row (id, layer_id, kind, key, op, fields, rev, is_current, author) \
         VALUES ($1, $2, 'entity', 'Order', 'patch', '{\"label\":\"planted\"}', $3, false, 'planted')",
    )
    .bind(Uuid::new_v4())
    .bind(l)
    .bind(current + 1)
    .execute(common::pool(&e))
    .await
    .unwrap();
    let (s, b) = alice
        .send(&e.app, "PUT", &order, patch("collides", i64::from(current)))
        .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::CONFLICT, "revision_conflict"),
        "{b}"
    );
    let (_, cur) = alice
        .get(&e.app, &format!("{}?kind=entity", rows_uri(l)))
        .await;
    let order_now: Vec<_> = items(&cur)
        .iter()
        .filter(|r| r["key"] == "Order")
        .map(|r| r["rev"].clone())
        .collect();
    assert_eq!(
        order_now,
        [json!(current)],
        "the failed PUT rolled back whole"
    );
}

#[tokio::test]
async fn other_tenants_layer_is_404() {
    let e = common::env().await;
    let a = tenant(&e.app, "Acme", "alice", &[]).await;
    let b_t = tenant(&e.app, "Beta", "bob", &[]).await;
    let bob = Who::member("bob", b_t);
    let lb = layer(&e.app, &bob, json!({"scope": "tenant"})).await;
    add(&e.app, &bob, lb, entity("Order", "Bob's order")).await;
    let alice = Who::member("alice", a);
    let order = row_uri(lb, "entity", "Order");

    let probes: Vec<(&str, String, Option<Value>)> = vec![
        ("GET", format!("/api/layers/{lb}"), None),
        ("GET", rows_uri(lb), None),
        ("POST", rows_uri(lb), Some(entity("Customer", "Customer"))),
        ("PUT", order.clone(), Some(patch("hijacked", 1))),
        ("DELETE", format!("{order}?expected_rev=1"), None),
        ("GET", format!("{order}/revisions"), None),
    ];
    for (method, uri, body) in &probes {
        let (s, b) = common::send(&e.app, alice.req(method, uri, body.as_ref())).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::NOT_FOUND, "layer_not_found"),
            "{method} {uri}: {b}"
        );
        assert!(!b.to_string().contains("Bob's order"), "no B data: {b}");
    }
    // unknown and malformed ids look the same
    for id in [Uuid::new_v4().to_string(), "not-a-uuid".to_string()] {
        let (s, b) = alice.get(&e.app, &format!("/api/layers/{id}")).await;
        assert_eq!((s, code(&b)), (StatusCode::NOT_FOUND, "layer_not_found"));
    }
    let (_, list) = alice.get(&e.app, "/api/layers").await;
    assert!(
        !ids(&list).contains(&lb.to_string()),
        "B's layer is not listed for A"
    );
    // naming B's tenant without membership is a 403 on the header, before the layer is looked at
    let (s, b) = Who::member("alice", b_t)
        .get(&e.app, &format!("/api/layers/{lb}"))
        .await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, "not_a_member"));
    // the super-admin's platform view holds no tenant layer; in B it sees B's
    let (s, _) = Who::root().get(&e.app, &format!("/api/layers/{lb}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let in_b = Who {
        tok: super_admin(ROOT),
        tenant: Some(b_t),
    };
    let (s, _) = in_b.get(&e.app, &format!("/api/layers/{lb}")).await;
    assert_eq!(s, StatusCode::OK);

    // B's data is untouched
    let (_, h) = bob.get(&e.app, &format!("{order}/revisions")).await;
    assert_eq!(revs(&h), [(1, true)]);
    assert_eq!(items(&h)[0]["fields"]["label"], "Bob's order");
    let (_, rows) = bob.get(&e.app, &rows_uri(lb)).await;
    assert_eq!(
        items(&rows).len(),
        1,
        "A's POST wrote nothing into B's layer"
    );
}

#[tokio::test]
async fn retire_then_re_add() {
    let e = common::env().await;
    let a = tenant(&e.app, "Acme", "alice", &[]).await;
    let alice = Who::member("alice", a);
    let l = layer(&e.app, &alice, json!({"scope": "tenant"})).await;
    let order = row_uri(l, "entity", "Order");
    add(&e.app, &alice, l, entity("Order", "Order")).await;

    let (s, b) = alice.delete(&e.app, &order).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, "invalid_query"),
        "expected_rev is required"
    );
    let (s, b) = alice
        .delete(&e.app, &format!("{order}?expected_rev=2"))
        .await;
    assert_eq!((s, code(&b)), (StatusCode::CONFLICT, "revision_conflict"));
    let (s, b) = alice
        .delete(&e.app, &format!("{order}?expected_rev=1"))
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
    let (_, cur) = alice.get(&e.app, &rows_uri(l)).await;
    assert!(
        items(&cur).is_empty(),
        "a retired object is no longer in the layer"
    );
    let (s, b) = alice
        .delete(&e.app, &format!("{order}?expected_rev=1"))
        .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::CONFLICT, "revision_conflict"),
        "already retired"
    );
    let (s, b) = alice.send(&e.app, "PUT", &order, patch("x", 1)).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::CONFLICT, "revision_conflict"),
        "no current to revise"
    );
    let (s, b) = alice
        .delete(
            &e.app,
            &format!("{}?expected_rev=1", row_uri(l, "entity", "Nothing")),
        )
        .await;
    assert_eq!((s, code(&b)), (StatusCode::NOT_FOUND, "row_not_found"));

    let again = add(&e.app, &alice, l, entity("Order", "Order again")).await;
    assert_eq!(again["rev"], 2, "re-add continues the revision numbers");
    let (_, h) = alice.get(&e.app, &format!("{order}/revisions")).await;
    assert_eq!(
        revs(&h),
        [(2, true), (1, false)],
        "the retired revision stays as history"
    );

    let actions: Vec<String> = audit(common::pool(&e))
        .await
        .into_iter()
        .map(|(_, _, act)| act)
        .filter(|act| act.starts_with("overlay_row"))
        .collect();
    assert_eq!(
        actions,
        [
            "overlay_row.create",
            "overlay_row.retire",
            "overlay_row.create"
        ]
    );
}

#[tokio::test]
async fn paging_limit_and_cursor() {
    let e = common::env().await;
    let root = Who::root();
    let mut made: Vec<String> = vec![
        layer(&e.app, &root, json!({"scope": "base"}))
            .await
            .to_string(),
        layer(
            &e.app,
            &root,
            json!({"scope": "industry", "industry": "retail"}),
        )
        .await
        .to_string(),
        layer(
            &e.app,
            &root,
            json!({"scope": "industry", "industry": "energy"}),
        )
        .await
        .to_string(),
    ];

    for (q, want) in [
        ("limit=501", "invalid_limit"),
        ("limit=0", "invalid_limit"),
        ("limit=ten", "invalid_limit"),
        ("before=zzz", "invalid_cursor"),
        ("scope=bogus", "invalid_scope"),
    ] {
        let (s, b) = root.get(&e.app, &format!("/api/layers?{q}")).await;
        assert_eq!((s, code(&b)), (StatusCode::BAD_REQUEST, want), "{q}");
    }
    let (s, b) = root.get(&e.app, "/api/layers?limit=500").await;
    assert_eq!((s, items(&b).len()), (StatusCode::OK, 3));

    let (_, p1) = root.get(&e.app, "/api/layers?limit=2").await;
    assert_eq!(items(&p1).len(), 2);
    let next = p1["next"].as_str().expect("a next cursor");
    let (_, p2) = root
        .get(&e.app, &format!("/api/layers?limit=2&before={next}"))
        .await;
    assert_eq!(items(&p2).len(), 1);
    assert!(p2["next"].is_null(), "{p2}");
    let mut seen = [ids(&p1), ids(&p2)].concat();
    seen.sort();
    made.sort();
    assert_eq!(seen, made, "the pages hold every layer exactly once");
    assert_eq!(ids(&p1)[0], made_newest(&e).await, "newest first");

    let (_, retail) = root
        .get(&e.app, "/api/layers?scope=industry&industry=retail")
        .await;
    assert_eq!(items(&retail).len(), 1);
    assert_eq!(items(&retail)[0]["industry"], "retail");

    // rows and tenants page the same way
    let base = Uuid::parse_str(&made_base(&e).await).unwrap();
    for k in ["Order", "Customer", "Payment"] {
        add(&e.app, &root, base, entity(k, k)).await;
    }
    let (_, r1) = root
        .get(&e.app, &format!("{}?limit=2", rows_uri(base)))
        .await;
    let next = r1["next"].as_str().expect("a rows cursor");
    let (_, r2) = root
        .get(&e.app, &format!("{}?limit=2&before={next}", rows_uri(base)))
        .await;
    let mut keys: Vec<String> = [items(&r1).clone(), items(&r2).clone()]
        .concat()
        .iter()
        .map(|r| r["key"].as_str().unwrap().to_string())
        .collect();
    keys.sort();
    assert_eq!(keys, ["Customer", "Order", "Payment"]);
    let (s, b) = root
        .get(&e.app, &format!("{}?kind=widget", rows_uri(base)))
        .await;
    assert_eq!((s, code(&b)), (StatusCode::BAD_REQUEST, "invalid_kind"));

    tenant(&e.app, "Acme", "alice", &[]).await;
    tenant(&e.app, "Beta", "bob", &[]).await;
    let (_, t1) = root.get(&e.app, "/api/tenants?limit=1").await;
    assert_eq!(items(&t1)[0]["name"], "Beta");
    let next = t1["next"].as_str().expect("a tenants cursor");
    let (_, t2) = root
        .get(&e.app, &format!("/api/tenants?limit=1&before={next}"))
        .await;
    assert_eq!(items(&t2)[0]["name"], "Acme");
    assert!(t2["next"].is_null());
    let (s, b) = root.get(&e.app, "/api/tenants?limit=501").await;
    assert_eq!((s, code(&b)), (StatusCode::BAD_REQUEST, "invalid_limit"));
}

/// The id of the newest layer, from the database.
async fn made_newest(e: &common::TestEnv) -> String {
    let id: Uuid =
        sqlx::query_scalar("SELECT id FROM layer ORDER BY created_at DESC, id DESC LIMIT 1")
            .fetch_one(common::pool(e))
            .await
            .unwrap();
    id.to_string()
}

async fn made_base(e: &common::TestEnv) -> String {
    let id: Uuid = sqlx::query_scalar("SELECT id FROM layer WHERE scope = 'base'")
        .fetch_one(common::pool(e))
        .await
        .unwrap();
    id.to_string()
}

#[tokio::test]
async fn load_for_layers_returns_current_rows_of_exactly_those_layers() {
    let e = common::env().await;
    let a = tenant(&e.app, "Acme", "alice", &[]).await;
    let b_t = tenant(&e.app, "Beta", "bob", &[]).await;
    let root = Who::root();
    let alice = Who::member("alice", a);
    let base = layer(&e.app, &root, json!({"scope": "base"})).await;
    let la = layer(&e.app, &alice, json!({"scope": "tenant"})).await;
    let lb = layer(&e.app, &Who::member("bob", b_t), json!({"scope": "tenant"})).await;
    add(&e.app, &root, base, entity("Order", "Order")).await;
    add(&e.app, &root, base, entity("Customer", "Customer")).await;
    let (s, _) = root
        .send(
            &e.app,
            "PUT",
            &row_uri(base, "entity", "Order"),
            patch("Order v2", 1),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = root
        .delete(
            &e.app,
            &format!("{}?expected_rev=1", row_uri(base, "entity", "Customer")),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    add(
        &e.app,
        &alice,
        la,
        json!({"kind": "entity", "key": "Order", "op": "patch", "fields": {"label": "Acme order"}}),
    )
    .await;
    add(
        &e.app,
        &Who::member("bob", b_t),
        lb,
        entity("Order", "Beta order"),
    )
    .await;

    let key = |scope: Scope, tenant: Option<Uuid>, industry: Option<&str>| LayerKey {
        scope,
        industry: industry.map(str::to_string),
        department: None,
        job: None,
        rule: None,
        dimension: None,
        tenant,
    };
    let base_key = key(Scope::Base, None, None);
    let a_key = key(Scope::Tenant, Some(a), None);
    let missing = key(Scope::Industry, None, Some("retail"));
    let got = load_for_layers(
        common::pool(&e),
        &[base_key.clone(), missing, a_key.clone()],
    )
    .await
    .unwrap();
    let summary: Vec<(LayerKey, Kind, String, Op, Value)> = got
        .into_iter()
        .map(|r| (r.layer, r.kind, r.key, r.op, Value::Object(r.fields)))
        .collect();
    assert_eq!(
        summary,
        [
            (
                base_key,
                Kind::Entity,
                "Order".to_string(),
                Op::Patch,
                json!({"label": "Order v2"})
            ),
            (
                a_key,
                Kind::Entity,
                "Order".to_string(),
                Op::Patch,
                json!({"label": "Acme order"})
            ),
        ],
        "current rows only (no retired Customer, no old Order rev), none of B's"
    );
    assert!(
        load_for_layers(common::pool(&e), &[])
            .await
            .unwrap()
            .is_empty()
    );
}
