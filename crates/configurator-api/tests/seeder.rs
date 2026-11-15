mod common;

use axum::http::StatusCode;
use configurator_api::seeder::{self, SeedError};
use configurator_api::state::SeedStatus;
use configurator_core::seed::to_rows;
use sqlx::PgPool;

/// Every table after all migrations (0003 replaced `member` with `users`).
const TABLES: [&str; 12] = [
    "tenant",
    "users",
    "layer",
    "overlay_row",
    "axis",
    "axis_value",
    "axis_rule",
    "dimension",
    "dimension_value",
    "profile",
    "seed_version",
    "audit_event",
];

/// `SELECT count(*)` per data table, in `TABLES` order.
const COUNTS: [&str; 12] = [
    "SELECT count(*) FROM tenant",
    "SELECT count(*) FROM users",
    "SELECT count(*) FROM layer",
    "SELECT count(*) FROM overlay_row",
    "SELECT count(*) FROM axis",
    "SELECT count(*) FROM axis_value",
    "SELECT count(*) FROM axis_rule",
    "SELECT count(*) FROM dimension",
    "SELECT count(*) FROM dimension_value",
    "SELECT count(*) FROM profile",
    "SELECT count(*) FROM seed_version",
    "SELECT count(*) FROM audit_event",
];

async fn count(pool: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

async fn counts(pool: &PgPool) -> Vec<(&'static str, i64)> {
    let mut out = Vec::new();
    for (table, sql) in TABLES.iter().zip(COUNTS) {
        out.push((*table, count(pool, sql).await));
    }
    out
}

/// `counts` of every table but the identity tables, which 0003 rebuilds.
async fn data_counts(pool: &PgPool) -> Vec<(&'static str, i64)> {
    counts_of(pool, &["users"]).await
}

async fn counts_of(pool: &PgPool, skip: &[&str]) -> Vec<(&'static str, i64)> {
    let mut out = Vec::new();
    for (table, sql) in TABLES.iter().zip(COUNTS) {
        if !skip.contains(table) {
            out.push((*table, count(pool, sql).await));
        }
    }
    out
}

#[tokio::test]
async fn migration_upgrade_preserves_existing_seed_data() {
    let pool = common::empty_pool().await;
    let first = sqlx::migrate::Migrator::with_migrations(
        configurator_api::db::MIGRATOR
            .iter()
            .take(1)
            .cloned()
            .collect(),
    );
    first.run(&pool).await.unwrap();
    // A 0001-era database holds seed v1 only: v2's job-content kinds need 0008.
    let bundles = seeder::embedded().unwrap();
    let v1 = bundles
        .iter()
        .find(|b| b.version == 1)
        .expect("v1 embedded");
    seeder::apply(&pool, std::slice::from_ref(v1))
        .await
        .unwrap();
    // memberships as 0001 held them: several tenants per subject
    let (t1, t2) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    sqlx::query("INSERT INTO tenant (id, name) VALUES ($1, 'One'), ($2, 'Two')")
        .bind(t1)
        .bind(t2)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO member (tenant, sub, role, created_at) VALUES \
         ($1, 'ann', 'admin',  '2026-01-02'), ($2, 'ann', 'viewer', '2026-01-01'), \
         ($1, 'bo',  'editor', '2026-01-01'), ($2, 'bo',  'admin',  '2026-01-03'), \
         ($1, 'cy',  'viewer', '2026-01-01'), ($2, 'cy',  'editor', '2026-01-02'), \
         ($1, 'di',  'viewer', '2026-01-02'), ($2, 'di',  'viewer', '2026-01-01'), \
         ($2, 'ed',  'approver', '2026-01-01')",
    )
    .bind(t1)
    .bind(t2)
    .execute(&pool)
    .await
    .unwrap();
    let before = data_counts(&pool).await;
    configurator_api::db::migrate(&pool).await.unwrap();
    assert_eq!(data_counts(&pool).await, before);
    // one row per subject: the highest role wins, then the earliest membership
    let users: Vec<(String, uuid::Uuid, String)> =
        sqlx::query_as("SELECT sub, tenant, tenant_role::text FROM users ORDER BY sub")
            .fetch_all(&pool)
            .await
            .unwrap();
    let row = |sub: &str, t: uuid::Uuid, r: &str| (sub.to_string(), t, r.to_string());
    assert_eq!(
        users,
        [
            row("ann", t1, "tenant_admin"),
            row("bo", t2, "tenant_admin"),
            row("cy", t2, "tenant_user"),
            row("di", t2, "tenant_user"),
            row("ed", t2, "tenant_user"),
        ]
    );
    let gone: (bool, bool, bool) = sqlx::query_as(
        "SELECT to_regclass('public.member') IS NULL, \
                to_regclass('public.member_sub_idx') IS NULL, \
                NOT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'member_role')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        gone,
        (true, true, true),
        "member, its index and its type are dropped"
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM _sqlx_migrations").await,
        configurator_api::db::MIGRATOR.iter().count() as i64
    );
}

#[tokio::test]
async fn migrations_apply_on_empty_db() {
    let pool = common::empty_pool().await;
    configurator_api::db::migrate(&pool)
        .await
        .expect("migrations apply on an empty database");
    // Running them again is a no-op.
    configurator_api::db::migrate(&pool)
        .await
        .expect("migrations are idempotent");

    let names: Vec<String> = TABLES.iter().map(|t| (*t).to_owned()).collect();
    let present: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_name = ANY($1)",
    )
    .bind(names)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(present, TABLES.len() as i64);
    // Every embedded migration is recorded exactly once.
    let embedded = configurator_api::db::MIGRATOR.iter().count() as i64;
    assert!(
        embedded >= 3,
        "0001_core, 0002_member_sub_index and 0003_users_rls are embedded"
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM _sqlx_migrations").await,
        embedded
    );
    for (table, n) in counts(&pool).await {
        assert_eq!(n, 0, "{table} is empty after migrating");
    }

    // The constraints bite: a nil tenant and a base layer carrying an industry are rejected.
    let nil = sqlx::query(
        "INSERT INTO tenant (id, name) VALUES ('00000000-0000-0000-0000-000000000000', 'nil')",
    )
    .execute(&pool)
    .await;
    assert!(nil.is_err(), "nil tenant must be rejected");
    let bad_layer = sqlx::query("INSERT INTO layer (scope, industry) VALUES ('base', 'retail')")
        .execute(&pool)
        .await;
    assert!(
        bad_layer.is_err(),
        "base layer with an industry must be rejected"
    );
    let no_rule = sqlx::query("INSERT INTO layer (scope) VALUES ('profile')")
        .execute(&pool)
        .await;
    assert!(
        no_rule.is_err(),
        "profile layer without a rule must be rejected"
    );
}

#[tokio::test]
async fn seed_v1_applies_once() {
    let pool = common::migrated_pool().await;
    let bundles = seeder::embedded().expect("embedded seeds parse");
    let v1 = bundles
        .iter()
        .find(|b| b.version == 1)
        .expect("v1 embedded");
    let (rules, rows) = to_rows(v1);
    // v1 alone: v2 (job content) is covered by `v2_applies_after_v1`.
    let only_v1 = std::slice::from_ref(v1);

    let applied = seeder::apply(&pool, only_v1).await.expect("v1 applies");
    assert_eq!(applied, vec![1]);

    let after = counts(&pool).await;
    let n = |t: &str| after.iter().find(|(name, _)| *name == t).unwrap().1;
    assert_eq!(n("overlay_row"), rows.len() as i64);
    assert_eq!(n("axis"), v1.axes.len() as i64);
    assert_eq!(n("dimension"), v1.dimensions.len() as i64);
    assert_eq!(n("axis_rule"), rules.len() as i64);
    assert_eq!(n("profile"), v1.profiles.len() as i64);
    assert_eq!(n("seed_version"), 1);
    assert!(n("axis_value") > 0 && n("dimension_value") > 0);

    // Every row: rev 1, current, authored by the seed, id = the reproducible to_rows id.
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM overlay_row \
             WHERE rev = 1 AND is_current AND author = 'seed:v1'"
        )
        .await,
        rows.len() as i64
    );
    let ids: Vec<uuid::Uuid> = rows.iter().map(|r| r.id).collect();
    let matched: i64 = sqlx::query_scalar("SELECT count(*) FROM overlay_row WHERE id = ANY($1)")
        .bind(ids)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(matched, rows.len() as i64);
    // base.toml and jobs.toml share the one base layer.
    assert_eq!(
        count(&pool, "SELECT count(*) FROM layer WHERE scope = 'base'").await,
        1
    );
    // Axis and axis_value labels/descriptions come from the seed.
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM axis WHERE key = 'customer_type' \
             AND label = 'Customer type' AND description IS NOT NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM axis_value WHERE axis = 'payment_method' \
             AND key = 'digital_wallet.mobile' AND label = 'Mobile wallet'"
        )
        .await,
        1
    );
    // Every axis_value carries its seed label (or, without one, its leaf segment).
    for a in &v1.axes {
        for v in &a.values {
            let want = v.label.clone().unwrap_or_else(|| {
                v.key
                    .rsplit('.')
                    .next()
                    .unwrap_or(v.key.as_str())
                    .to_owned()
            });
            let got: String =
                sqlx::query_scalar("SELECT label FROM axis_value WHERE axis = $1 AND key = $2")
                    .bind(a.key.as_str())
                    .bind(v.key.as_str())
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(got, want, "axis_value {}={}", a.key, v.key);
        }
    }
    let sha: String = sqlx::query_scalar("SELECT sha256 FROM seed_version WHERE version = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sha, seeder::bundle_sha256(v1));

    let again = seeder::apply(&pool, only_v1).await.expect("re-apply");
    assert!(again.is_empty(), "v1 applied twice: {again:?}");
    assert_eq!(counts(&pool).await, after, "second apply changed rows");
}

#[tokio::test]
async fn v2_applies_after_v1() {
    let pool = common::migrated_pool().await;
    let bundles = seeder::embedded().expect("embedded seeds parse");
    let v1 = bundles
        .iter()
        .find(|b| b.version == 1)
        .expect("v1 embedded");
    let v2 = bundles
        .iter()
        .find(|b| b.version == 2)
        .expect("v2 embedded");
    // A database seeded before W4 holds v1 only; the next start adds v2 on top.
    let applied = seeder::apply(&pool, std::slice::from_ref(v1))
        .await
        .expect("v1 applies");
    assert_eq!(applied, vec![1]);
    let applied = seeder::apply(&pool, &bundles).await.expect("v2 applies");
    assert_eq!(applied, vec![2]);

    let versions: Vec<i32> =
        sqlx::query_scalar("SELECT version FROM seed_version ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(versions, vec![1, 2]);
    let (_, v1_rows) = to_rows(v1);
    let (_, v2_rows) = to_rows(v2);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM overlay_row").await,
        (v1_rows.len() + v2_rows.len()) as i64
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM overlay_row WHERE author = 'seed:v2' AND rev = 1 AND is_current"
        )
        .await,
        v2_rows.len() as i64
    );
    for (kind, want) in [
        ("context_space", 5_i64),
        ("procedure", 6),
        ("connector_slot", 1),
        ("knowledge_store", 2),
        ("job", 1),
    ] {
        let got: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM overlay_row r JOIN layer l ON l.id = r.layer_id \
             WHERE r.kind = $1 AND r.author = 'seed:v2' \
               AND l.scope = 'job' AND l.job = 'customer_care_agent' AND l.dimension_key IS NULL",
        )
        .bind(kind)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(got, want, "{kind}");
    }
    let sha: String = sqlx::query_scalar("SELECT sha256 FROM seed_version WHERE version = 2")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sha, seeder::bundle_sha256(v2));

    let after = counts(&pool).await;
    let again = seeder::apply(&pool, &bundles).await.expect("re-apply");
    assert!(again.is_empty(), "applied twice: {again:?}");
    assert_eq!(counts(&pool).await, after, "second apply changed rows");
}

#[tokio::test]
async fn seed_conflict_rolls_back_and_names_key() {
    let pool = common::migrated_pool().await;
    // Someone already curated a current base `entity:Order`.
    let layer: uuid::Uuid =
        sqlx::query_scalar("INSERT INTO layer (scope) VALUES ('base') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO overlay_row (id, layer_id, kind, key, op, fields, rev, is_current, author) \
         VALUES (gen_random_uuid(), $1, 'entity', 'Order', 'add', '{\"label\":\"Order\"}', 1, true, 'curator')",
    )
    .bind(layer)
    .execute(&pool)
    .await
    .unwrap();
    let before = counts(&pool).await;

    let bundles = seeder::embedded().expect("embedded seeds parse");
    let err = seeder::apply(&pool, &bundles)
        .await
        .expect_err("v1 must conflict on entity:Order");
    match &err {
        SeedError::Conflict { version, key } => {
            assert_eq!(*version, 1);
            assert_eq!(key, "entity:Order");
        }
        other => panic!("expected a seed conflict, got {other:?}"),
    }
    assert!(err.to_string().contains("entity:Order"), "{err}");

    // Nothing from v1 survived: the database is exactly as before.
    assert_eq!(counts(&pool).await, before);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM overlay_row WHERE author LIKE 'seed:%'"
        )
        .await,
        0
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM seed_version").await, 0);
    assert_eq!(count(&pool, "SELECT count(*) FROM axis").await, 0);
}

#[tokio::test]
async fn ready_reports_seed_conflict() {
    let env = common::env().await;
    let (status, body) = common::send(&env.app, common::get("/ready", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, serde_json::json!({"status": "ready"}));

    env.state.set_seed_status(SeedStatus::Conflict {
        version: 1,
        key: "entity:Order".into(),
    });
    let (status, body) = common::send(&env.app, common::get("/ready", None)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], "seed_conflict");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("entity:Order"), "{message}");

    // `/health` stays 200: liveness is not readiness.
    let (status, _) = common::send(&env.app, common::get("/health", None)).await;
    assert_eq!(status, StatusCode::OK);
}
