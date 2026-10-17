//! Row-level security (migration 0003; commonpractice's 0028/0057 shape): the database itself keeps
//! tenants apart, whatever the API code does. Everything here is raw SQL as `configurator_app`.
mod common;

use configurator_api::db;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// Tables with a `tenant` column that deliberately stay outside RLS: identity is resolved (and audit
/// is written) before a tenant is known, as in commonpractice.
const OUTSIDE_RLS: [&str; 2] = ["audit_event", "users"];

/// `Err` with SQLSTATE 42501: "new row violates row-level security policy".
fn rls_denied<T>(r: Result<T, sqlx::Error>) -> bool {
    matches!(r, Err(sqlx::Error::Database(d)) if d.code().as_deref() == Some("42501"))
}

/// Anti-drift: every public table with a `tenant` column has RLS enabled and at least one policy,
/// except the deliberate allow-list; `overlay_row` (no tenant column, isolated through its layer) has
/// RLS and both of its policies. A new tenant table without a policy fails here.
#[tokio::test]
async fn every_tenant_table_has_rls_and_a_policy() {
    let pool = common::migrated_pool().await;
    let tables: Vec<(String, bool, i64)> = sqlx::query_as(
        "SELECT c.relname::text, c.relrowsecurity, \
                (SELECT count(*) FROM pg_policy p WHERE p.polrelid = c.oid) \
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p') \
           AND EXISTS (SELECT 1 FROM pg_attribute a \
                       WHERE a.attrelid = c.oid AND a.attname = 'tenant' \
                         AND a.attnum > 0 AND NOT a.attisdropped) \
         ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let names: Vec<&str> = tables.iter().map(|t| t.0.as_str()).collect();
    for want in ["layer", "dimension_value", "profile"] {
        assert!(names.contains(&want), "{want} not found among {names:?}");
    }
    for allowed in OUTSIDE_RLS {
        assert!(
            names.contains(&allowed),
            "allow-list entry {allowed} no longer exists; drop it from the list"
        );
    }
    for (table, rls, policies) in &tables {
        if OUTSIDE_RLS.contains(&table.as_str()) {
            continue;
        }
        assert!(*rls, "{table} has a tenant column but RLS is not enabled");
        assert!(*policies >= 1, "{table} has RLS but no policy");
    }

    let rls: bool = sqlx::query_scalar(
        "SELECT relrowsecurity FROM pg_class WHERE oid = 'overlay_row'::regclass",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(rls, "overlay_row has no RLS");
    let policies: Vec<String> = sqlx::query_scalar(
        "SELECT polname::text FROM pg_policy WHERE polrelid = 'overlay_row'::regclass ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(policies, ["overlay_row_read", "overlay_row_write"]);
}

#[tokio::test]
async fn app_role_cannot_bypass_rls() {
    let pool = common::migrated_pool().await;
    let flags: (bool, bool) = sqlx::query_as(
        "SELECT rolsuper, rolbypassrls FROM pg_roles WHERE rolname = 'configurator_app'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        flags,
        (false, false),
        "configurator_app: rolsuper, rolbypassrls"
    );
}

#[tokio::test]
async fn assert_runtime_role_refuses_a_bypassing_pool() {
    let e = common::env().await;
    let err = db::assert_runtime_role(&e.state.owner)
        .await
        .expect_err("the postgres superuser must be refused")
        .to_string();
    assert!(err.contains("superuser"), "{err}");
    assert!(err.contains("owns the database"), "{err}");
    db::assert_runtime_role(&e.state.db)
        .await
        .expect("configurator_app passes");
}

struct Fixture {
    a: Uuid,
    b: Uuid,
    /// The platform base layer, A's tenant layer and B's tenant layer.
    platform: Uuid,
    la: Uuid,
    lb: Uuid,
}

/// Two tenants, each with a layer, a row, a profile and a dimension value, beside platform ones. Written
/// by the owner (RLS does not apply to it).
async fn fixture(owner: &PgPool) -> Fixture {
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    sqlx::query("INSERT INTO tenant (id, name) VALUES ($1, 'A'), ($2, 'B')")
        .bind(a)
        .bind(b)
        .execute(owner)
        .await
        .unwrap();
    let platform: Uuid =
        sqlx::query_scalar("INSERT INTO layer (scope) VALUES ('base') RETURNING id")
            .fetch_one(owner)
            .await
            .unwrap();
    let mut tenant_layers = Vec::new();
    for t in [a, b] {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO layer (scope, tenant) VALUES ('tenant', $1) RETURNING id",
        )
        .bind(t)
        .fetch_one(owner)
        .await
        .unwrap();
        tenant_layers.push(id);
    }
    let (la, lb) = (tenant_layers[0], tenant_layers[1]);
    for (layer, key) in [(platform, "Order"), (la, "Customer"), (lb, "Secret")] {
        sqlx::query(
            "INSERT INTO overlay_row (id, layer_id, kind, key, op, rev, is_current, author) \
             VALUES ($1, $2, 'entity', $3, 'add', 1, true, 'fixture')",
        )
        .bind(Uuid::new_v4())
        .bind(layer)
        .bind(key)
        .execute(owner)
        .await
        .unwrap();
    }
    sqlx::query("INSERT INTO dimension (key, label) VALUES ('department', 'Department')")
        .execute(owner)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO dimension_value (dimension, key, label, tenant) VALUES \
         ('department', 'standard', 'Standard', NULL), \
         ('department', 'a-only', 'A only', $1), \
         ('department', 'b-only', 'B only', $2)",
    )
    .bind(a)
    .bind(b)
    .execute(owner)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO profile (tenant, name, \"values\") VALUES \
         (NULL, 'template', '{}'), ($1, 'a', '{}'), ($2, 'b', '{}')",
    )
    .bind(a)
    .bind(b)
    .execute(owner)
    .await
    .unwrap();
    Fixture {
        a,
        b,
        platform,
        la,
        lb,
    }
}

/// The `tenant` of every row of `table` the connection can see, sorted (NULL first).
async fn tenants_seen(conn: &mut PgConnection, table: &str) -> Vec<Option<Uuid>> {
    let mut v: Vec<Option<Uuid>> =
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT tenant FROM {table}")))
            .fetch_all(conn)
            .await
            .unwrap();
    v.sort();
    v
}

async fn rows_seen(conn: &mut PgConnection) -> Vec<Uuid> {
    let mut v: Vec<Uuid> = sqlx::query_scalar("SELECT layer_id FROM overlay_row")
        .fetch_all(conn)
        .await
        .unwrap();
    v.sort();
    v
}

fn sorted<T: Ord>(mut v: Vec<T>) -> Vec<T> {
    v.sort();
    v
}

#[tokio::test]
async fn app_role_sees_and_writes_only_its_tenant() {
    let e = common::env().await;
    let (owner, rt) = (&e.state.owner, &e.state.db);
    let f = fixture(owner).await;

    // reads in tenant A: platform rows and A's rows, never B's
    let mut tx = db::tenant_tx(rt, f.a).await.unwrap();
    for table in ["layer", "dimension_value", "profile"] {
        assert_eq!(
            tenants_seen(&mut tx, table).await,
            vec![None, Some(f.a)],
            "{table}"
        );
    }
    assert_eq!(rows_seen(&mut tx).await, sorted(vec![f.platform, f.la]));
    // asking for B by id finds nothing either
    let hidden: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM layer WHERE id = $1 OR tenant = $2) \
              + (SELECT count(*) FROM overlay_row WHERE layer_id = $1) \
              + (SELECT count(*) FROM profile WHERE tenant = $2) \
              + (SELECT count(*) FROM dimension_value WHERE tenant = $2)",
    )
    .bind(f.lb)
    .bind(f.b)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(hidden, 0, "B's rows are invisible in A");
    // control: A writes its own rows
    sqlx::query(
        "INSERT INTO overlay_row (id, layer_id, kind, key, op, rev, is_current, author) \
         VALUES ($1, $2, 'entity', 'Invoice', 'add', 1, true, 'a')",
    )
    .bind(Uuid::new_v4())
    .bind(f.la)
    .execute(&mut *tx)
    .await
    .expect("A writes a row in its own layer");
    sqlx::query("INSERT INTO profile (tenant, name, \"values\") VALUES ($1, 'a2', '{}')")
        .bind(f.a)
        .execute(&mut *tx)
        .await
        .expect("A writes its own profile");
    let retired = sqlx::query("UPDATE overlay_row SET is_current = false WHERE layer_id = $1")
        .bind(f.la)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(retired.rows_affected(), 2, "A updates its own rows");
    tx.commit().await.unwrap();

    // writes in tenant A that must be refused, each in its own transaction
    let new_row = "INSERT INTO overlay_row (id, layer_id, kind, key, op, rev, is_current, author) \
                   VALUES ($1, $2, 'entity', 'Hijack', 'add', 1, true, 'a')";
    for (what, layer) in [
        ("a row in B's layer", f.lb),
        ("a row in a platform layer", f.platform),
    ] {
        let mut tx = db::tenant_tx(rt, f.a).await.unwrap();
        let r = sqlx::query(new_row)
            .bind(Uuid::new_v4())
            .bind(layer)
            .execute(&mut *tx)
            .await;
        assert!(rls_denied(r), "{what}");
    }
    let refused: [(&str, &'static str); 5] = [
        (
            "a layer for B",
            "INSERT INTO layer (scope, tenant) VALUES ('tenant', $1)",
        ),
        (
            "a platform layer",
            "INSERT INTO layer (scope, industry) VALUES ('industry', 'rls') \
             RETURNING (SELECT $1::uuid)",
        ),
        (
            "a profile for B",
            "INSERT INTO profile (tenant, name, \"values\") VALUES ($1, 'x', '{}')",
        ),
        (
            "a platform profile",
            "INSERT INTO profile (tenant, name, \"values\") VALUES (NULL, 'x', '{}') \
             RETURNING (SELECT $1::uuid)",
        ),
        (
            "a dimension value for B",
            "INSERT INTO dimension_value (dimension, key, label, tenant) \
             VALUES ('department', 'x', 'X', $1)",
        ),
    ];
    for (what, sql) in refused {
        let mut tx = db::tenant_tx(rt, f.a).await.unwrap();
        let r = sqlx::query(sql).bind(f.b).execute(&mut *tx).await;
        assert!(rls_denied(r), "{what}");
    }
    // updates and deletes of rows A cannot write match nothing
    let mut tx = db::tenant_tx(rt, f.a).await.unwrap();
    for (what, sql, id) in [
        (
            "platform rows",
            "UPDATE overlay_row SET is_current = false WHERE layer_id = $1",
            f.platform,
        ),
        (
            "the platform layer",
            "UPDATE layer SET created_at = now() WHERE id = $1",
            f.platform,
        ),
        (
            "B's rows",
            "DELETE FROM overlay_row WHERE layer_id = $1",
            f.lb,
        ),
        ("B's layer", "DELETE FROM layer WHERE id = $1", f.lb),
    ] {
        let r = sqlx::query(sql).bind(id).execute(&mut *tx).await.unwrap();
        assert_eq!(r.rows_affected(), 0, "{what}");
    }
    let r = sqlx::query("UPDATE profile SET name = 'stolen' WHERE tenant IS NULL OR tenant = $1")
        .bind(f.b)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(r.rows_affected(), 0, "platform and B's profiles");
    tx.commit().await.unwrap();

    // without the GUC: platform rows only, and no tenant writes at all
    let mut tx = rt.begin().await.unwrap();
    for table in ["layer", "dimension_value", "profile"] {
        assert_eq!(tenants_seen(&mut tx, table).await, vec![None], "{table}");
    }
    assert_eq!(rows_seen(&mut tx).await, vec![f.platform]);
    let r = sqlx::query("INSERT INTO profile (tenant, name, \"values\") VALUES ($1, 'x', '{}')")
        .bind(f.a)
        .execute(&mut *tx)
        .await;
    assert!(rls_denied(r), "no GUC, no tenant write");
    drop(tx);

    // the owner saw every refused write fail: B and the platform are as the fixture left them
    let untouched: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM overlay_row WHERE layer_id IN ($1, $2) AND is_current), \
                (SELECT count(*) FROM profile WHERE name IN ('stolen', 'x')), \
                (SELECT count(*) FROM layer)",
    )
    .bind(f.platform)
    .bind(f.lb)
    .fetch_one(owner)
    .await
    .unwrap();
    assert_eq!(untouched, (2, 0, 3));
}

/// `app.current_tenant` is transaction-local: a pooled connection (or a pgbouncer server connection)
/// handed to the next request carries no tenant.
#[tokio::test]
async fn tenant_guc_does_not_leak_to_the_next_transaction() {
    let e = common::env().await;
    let f = fixture(&e.state.owner).await;
    let one = PgPoolOptions::new()
        .max_connections(1)
        .connect(&e.state.cfg.database_url)
        .await
        .expect("connect as configurator_app");
    let mut tx = db::tenant_tx(&one, f.a).await.unwrap();
    assert_eq!(tenants_seen(&mut tx, "layer").await, vec![None, Some(f.a)]);
    tx.commit().await.unwrap();
    let mut conn = one.acquire().await.unwrap();
    assert_eq!(tenants_seen(&mut conn, "layer").await, vec![None]);
    let current: Option<Uuid> = sqlx::query_scalar("SELECT app_current_tenant()")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(current, None);
}
