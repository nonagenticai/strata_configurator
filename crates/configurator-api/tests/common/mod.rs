//! Integration test harness, copied from care-bus `crates/carebus-api/tests/common/mod.rs` @ 3b88445
//! (never a dependency on it): ONE Postgres 18 container per test binary, a fresh database per test.
#![allow(dead_code)]
pub mod client;
pub mod keys;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use configurator_api::auth::Verifier;
use configurator_api::{config::Config, db, routes, state::AppState};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};
use tokio::sync::OnceCell;
use tower::ServiceExt;

pub struct TestEnv {
    pub state: AppState,
    pub app: Router,
}

static DB_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Swap the database name (path segment) in a postgres URL, keeping any query string.
fn with_db_name(url: &str, name: &str) -> String {
    let (head, query) = match url.split_once('?') {
        Some((h, q)) => (h, Some(q)),
        None => (url, None),
    };
    let scheme_end = head.find("://").map(|i| i + 3).unwrap_or(0);
    let base = match head[scheme_end..].find('/') {
        Some(i) => &head[..scheme_end + i],
        None => head,
    };
    match query {
        Some(q) => format!("{base}/{name}?{q}"),
        None => format!("{base}/{name}"),
    }
}

/// One server per test binary: `TEST_DATABASE_URL` when set, otherwise a single throwaway Postgres 18 from the LAN
/// mirror (`TEST_PG_REGISTRY`, default `registry.internal:5000/library`), never docker.io. Every test gets its own
/// fresh database inside it. The container lives in a static, so it is removed by an `atexit` hook when the test
/// process exits.
struct Server {
    base: String,
    _container: Option<ContainerAsync<Postgres>>,
}

static SERVER: OnceCell<Server> = OnceCell::const_new();
static CONTAINER_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Statics are never dropped and the harness runs no reaper, so remove the shared container when the process exits.
extern "C" fn remove_container() {
    if let Some(id) = CONTAINER_ID.get() {
        let _ = std::process::Command::new("docker")
            .args(["rm", "-f", "-v", id])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

async fn start_container() -> (String, ContainerAsync<Postgres>) {
    let reg = match std::env::var("TEST_PG_REGISTRY") {
        Ok(r) if !r.is_empty() => r,
        _ => "registry.internal:5000/library".to_string(),
    };
    let image = format!("{}/postgres", reg.trim_end_matches('/'));
    let mut last = String::new();
    for attempt in 1..=3 {
        match Postgres::default()
            .with_name(&image)
            .with_tag("18-alpine")
            .with_cmd(["postgres", "-c", "max_connections=500", "-c", "fsync=off"])
            .start()
            .await
        {
            Ok(pg) => {
                // Docker can report the container started before the host port mapping is published
                // (`PortNotExposed` under load): poll for it instead of failing.
                let mut port = None;
                for _ in 0..40 {
                    match pg.get_host_port_ipv4(5432).await {
                        Ok(p) => {
                            port = Some(p);
                            break;
                        }
                        Err(e) => last = format!("{e:?}"),
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                if let Some(port) = port {
                    return (
                        format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
                        pg,
                    );
                }
                eprintln!("postgres container port never published (attempt {attempt}/3): {last}");
                continue;
            }
            Err(e) => {
                last = format!("{e:?}");
                eprintln!("postgres container start failed (attempt {attempt}/3): {last}");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
    panic!("could not start postgres 18 (is Docker running?): {last}");
}

async fn server() -> &'static Server {
    SERVER
        .get_or_init(|| async {
            if let Ok(base) = std::env::var("TEST_DATABASE_URL")
                && !base.is_empty()
            {
                return Server {
                    base,
                    _container: None,
                };
            }
            let (base, c) = start_container().await;
            if CONTAINER_ID.set(c.id().to_string()).is_ok() {
                // SAFETY: registering a plain `extern "C" fn()` with atexit.
                unsafe { libc::atexit(remove_container) };
            }
            Server {
                base,
                _container: Some(c),
            }
        })
        .await
}

/// A fresh, empty database on the shared server; returns its URL.
pub async fn database() -> String {
    let srv = server().await;
    let n = DB_COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("cfg_t_{}_{nanos}_{n}", std::process::id());
    let admin = PgPool::connect(&srv.base)
        .await
        .expect("connect to the test postgres server");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .expect("create test database");
    admin.close().await;
    with_db_name(&srv.base, &name)
}

/// A pool on a fresh, empty (unmigrated) database.
pub async fn empty_pool() -> PgPool {
    db::connect(&database().await)
        .await
        .expect("connect to the test database")
}

/// A pool on a fresh database with every migration applied (no seeds).
pub async fn migrated_pool() -> PgPool {
    let pool = empty_pool().await;
    db::migrate(&pool).await.expect("migrations apply");
    pool
}

/// The runtime role the migrations create, and the password the harness gives it. The role is
/// cluster-global, so every test database on the server shares it.
pub const APP_ROLE: &str = "configurator_app";
pub const APP_PASSWORD: &str = "configurator-app-test";

/// `url` with its user and password replaced.
fn with_credentials(url: &str, user: &str, password: &str) -> String {
    let scheme_end = url.find("://").map(|i| i + 3).unwrap_or(0);
    let rest = &url[scheme_end..];
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let host = match rest[..authority_end].rfind('@') {
        Some(i) => &rest[i + 1..],
        None => rest,
    };
    format!("{}{user}:{password}@{host}", &url[..scheme_end])
}

/// Let `configurator_app` log in to the database `owner` is connected to and return its URL (built
/// from `owner_url`). Concurrent tests race on the one cluster-global role row ("tuple concurrently
/// updated"), so the ALTER is retried.
pub async fn enable_app_role(owner: &PgPool, owner_url: &str) -> String {
    let mut last = String::new();
    let mut done = false;
    for _ in 0..20 {
        match sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER ROLE {APP_ROLE} LOGIN PASSWORD '{APP_PASSWORD}'"
        )))
        .execute(owner)
        .await
        {
            Ok(_) => {
                done = true;
                break;
            }
            Err(e) => {
                last = e.to_string();
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
    assert!(done, "ALTER ROLE {APP_ROLE}: {last}");
    let name: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(owner)
        .await
        .expect("current database");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "GRANT CONNECT ON DATABASE \"{name}\" TO {APP_ROLE}"
    )))
    .execute(owner)
    .await
    .expect("grant connect");
    with_credentials(owner_url, APP_ROLE, APP_PASSWORD)
}

pub fn test_config(database_url: String, migrate_database_url: String) -> Config {
    Config::from_lookup(|k| match k {
        "DATABASE_URL" => Some(database_url.clone()),
        "MIGRATE_DATABASE_URL" => Some(migrate_database_url.clone()),
        "KEYCLOAK_ISSUER" => Some(TEST_ISSUER.into()),
        "KEYCLOAK_JWKS_URL" => Some("https://issuer.test/certs".into()),
        _ => None,
    })
    .expect("test config")
}

/// A migrated fresh database (no seeds) and the router over it. As in production, requests run as
/// `configurator_app` (under RLS) and the owner pool is the superuser that migrated.
pub async fn env() -> TestEnv {
    let owner_url = database().await;
    let owner = db::connect(&owner_url)
        .await
        .expect("connect to the test database");
    db::migrate(&owner).await.expect("migrations apply");
    let app_url = enable_app_role(&owner, &owner_url).await;
    let runtime = db::connect(&app_url)
        .await
        .expect("connect as configurator_app");
    db::assert_runtime_role(&runtime)
        .await
        .expect("the runtime pool is under RLS");
    let cfg = test_config(app_url, owner_url);
    let verifier = Verifier::static_key(
        &cfg.keycloak_issuer,
        &cfg.keycloak_audience,
        keys::decoding_key(),
    );
    let state = AppState::new(cfg, runtime, owner).with_verifier(verifier);
    let app = routes::router(state.clone());
    TestEnv { state, app }
}

/// The owner pool, for assertions and fixtures that must see every tenant's rows.
pub fn pool(e: &TestEnv) -> &PgPool {
    &e.state.owner
}

pub async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(req)
        .await
        .expect("router is infallible");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()))
    };
    (status, v)
}

pub fn get(uri: &str, token: Option<&str>) -> Request<Body> {
    let mut b = Request::get(uri);
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::empty()).unwrap()
}

pub fn json(method: &str, uri: &str, token: Option<&str>, body: Value) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::from(body.to_string())).unwrap()
}

pub const TEST_ISSUER: &str = "https://issuer.test/realms/portal";
