//! Release targets, releases and push steps (migration 0009). Every query runs in the caller's tenant
//! transaction, so RLS confines it to that tenant.
use serde::Serialize;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::page::TimeCursor;

/// What a tenant releases: one job, compiled with one stored selection.
#[derive(Debug, Clone, Serialize)]
pub struct Target {
    pub id: Uuid,
    pub tenant: Uuid,
    pub job: String,
    pub selection: Value,
    pub created_by: String,
    /// RFC 3339.
    pub created_at: String,
}

type TargetTuple = (Uuid, Uuid, String, Value, String, String);

const TARGET_COLUMNS: &str =
    "id, tenant, job, selection, created_by, to_jsonb(created_at) #>> '{}'";

fn target((id, tenant, job, selection, created_by, created_at): TargetTuple) -> Target {
    Target {
        id,
        tenant,
        job,
        selection,
        created_by,
        created_at,
    }
}

/// One built release (its body is read separately).
#[derive(Debug, Clone, Serialize)]
pub struct Release {
    pub id: Uuid,
    pub tenant: Uuid,
    pub target: Uuid,
    pub release_id: String,
    pub hash: String,
    pub effective_hash: String,
    pub job_label: String,
    pub created_by: String,
    /// RFC 3339.
    pub created_at: String,
    /// `created_at` in microseconds since the epoch: the paging cursor, not part of the body.
    #[serde(skip)]
    pub ts_us: i64,
}

type ReleaseTuple = (
    Uuid,
    Uuid,
    Uuid,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
);

const RELEASE_COLUMNS: &str = "id, tenant, target, release_id, hash, effective_hash, job_label, \
     created_by, to_jsonb(created_at) #>> '{}', (extract(epoch FROM created_at) * 1000000)::bigint";

fn release(t: ReleaseTuple) -> Release {
    let (
        id,
        tenant,
        target,
        release_id,
        hash,
        effective_hash,
        job_label,
        created_by,
        created_at,
        ts_us,
    ) = t;
    Release {
        id,
        tenant,
        target,
        release_id,
        hash,
        effective_hash,
        job_label,
        created_by,
        created_at,
        ts_us,
    }
}

/// One care-bus call made while pushing a release.
#[derive(Debug, Clone, Serialize)]
pub struct PushStep {
    pub id: i64,
    pub step: String,
    pub request_hash: Option<String>,
    pub status: Option<i32>,
    pub ok: bool,
    pub response: Option<Value>,
    pub error_code: Option<String>,
    pub pushed_by: String,
    /// RFC 3339.
    pub pushed_at: String,
}

type StepTuple = (
    i64,
    String,
    Option<String>,
    Option<i32>,
    bool,
    Option<Value>,
    Option<String>,
    String,
    String,
);

pub async fn list_targets(conn: &mut PgConnection) -> Result<Vec<Target>, sqlx::Error> {
    let rows = sqlx::query_as::<_, TargetTuple>(sqlx::AssertSqlSafe(format!(
        "SELECT {TARGET_COLUMNS} FROM release_target ORDER BY created_at DESC, id DESC"
    )))
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(target).collect())
}

pub async fn get_target(conn: &mut PgConnection, id: Uuid) -> Result<Option<Target>, sqlx::Error> {
    let row = sqlx::query_as::<_, TargetTuple>(sqlx::AssertSqlSafe(format!(
        "SELECT {TARGET_COLUMNS} FROM release_target WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(target))
}

pub async fn insert_target(
    conn: &mut PgConnection,
    tenant: Uuid,
    job: &str,
    selection: &Value,
    by: &str,
) -> Result<Target, sqlx::Error> {
    let row = sqlx::query_as::<_, TargetTuple>(sqlx::AssertSqlSafe(format!(
        "INSERT INTO release_target (tenant, job, selection, created_by) VALUES ($1, $2, $3, $4) \
         RETURNING {TARGET_COLUMNS}"
    )))
    .bind(tenant)
    .bind(job)
    .bind(selection)
    .bind(by)
    .fetch_one(conn)
    .await?;
    Ok(target(row))
}

/// Serialise release numbering for `target` until the transaction ends.
pub async fn lock_target(conn: &mut PgConnection, target: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('release_target:' || $1::text, 0))")
        .bind(target)
        .execute(conn)
        .await?;
    Ok(())
}

/// The newest release of `target`.
pub async fn latest_release(
    conn: &mut PgConnection,
    target: Uuid,
) -> Result<Option<Release>, sqlx::Error> {
    let row = sqlx::query_as::<_, ReleaseTuple>(sqlx::AssertSqlSafe(format!(
        "SELECT {RELEASE_COLUMNS} FROM config_release WHERE target = $1 \
         ORDER BY created_at DESC, id DESC LIMIT 1"
    )))
    .bind(target)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(release))
}

/// `cfg-YYYY-MM-DD.N` for a release of `target` built now: the UTC date, and N = 1 + the target's
/// releases with that date. Call under [`lock_target`].
pub async fn next_release_id(conn: &mut PgConnection, target: Uuid) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT 'cfg-' || d || '.' || (1 + (SELECT count(*) FROM config_release \
                                             WHERE target = $1 AND release_id LIKE 'cfg-' || d || '.%')) \
         FROM (SELECT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD') AS d) today",
    )
    .bind(target)
    .fetch_one(conn)
    .await
}

pub struct NewRelease<'a> {
    pub tenant: Uuid,
    pub target: Uuid,
    pub release_id: &'a str,
    pub hash: &'a str,
    pub effective_hash: &'a str,
    pub job_label: &'a str,
    pub body: &'a Value,
    pub by: &'a str,
}

pub async fn insert_release(
    conn: &mut PgConnection,
    r: NewRelease<'_>,
) -> Result<Release, sqlx::Error> {
    let row = sqlx::query_as::<_, ReleaseTuple>(sqlx::AssertSqlSafe(format!(
        "INSERT INTO config_release \
           (tenant, target, release_id, hash, effective_hash, job_label, body, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {RELEASE_COLUMNS}"
    )))
    .bind(r.tenant)
    .bind(r.target)
    .bind(r.release_id)
    .bind(r.hash)
    .bind(r.effective_hash)
    .bind(r.job_label)
    .bind(r.body)
    .bind(r.by)
    .fetch_one(conn)
    .await?;
    Ok(release(row))
}

/// One page of `target`'s releases, newest first.
pub async fn list_releases(
    conn: &mut PgConnection,
    target: Uuid,
    before: Option<TimeCursor>,
    fetch: i64,
) -> Result<Vec<Release>, sqlx::Error> {
    let (us, id) = TimeCursor::split(before);
    let rows = sqlx::query_as::<_, ReleaseTuple>(sqlx::AssertSqlSafe(format!(
        "SELECT {RELEASE_COLUMNS} FROM config_release \
         WHERE target = $3 \
           AND ($1::bigint IS NULL \
                OR (created_at, id) < (timestamptz 'epoch' + $1::bigint * interval '1 microsecond', $2::uuid)) \
         ORDER BY created_at DESC, id DESC LIMIT $4"
    )))
    .bind(us)
    .bind(id)
    .bind(target)
    .bind(fetch)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(release).collect())
}

/// A release and its PUT body.
pub async fn get_release(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<(Release, Value)>, sqlx::Error> {
    let row = sqlx::query_as::<_, ReleaseTuple>(sqlx::AssertSqlSafe(format!(
        "SELECT {RELEASE_COLUMNS} FROM config_release WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let body: Value = sqlx::query_scalar("SELECT body FROM config_release WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    Ok(Some((release(row), body)))
}

pub async fn push_steps(
    conn: &mut PgConnection,
    release: Uuid,
) -> Result<Vec<PushStep>, sqlx::Error> {
    let rows = sqlx::query_as::<_, StepTuple>(
        "SELECT id, step, request_hash, status, ok, response, error_code, pushed_by, \
                to_jsonb(pushed_at) #>> '{}' \
         FROM push_step WHERE release = $1 ORDER BY id",
    )
    .bind(release)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, step, request_hash, status, ok, response, error_code, pushed_by, pushed_at)| {
                PushStep {
                    id,
                    step,
                    request_hash,
                    status,
                    ok,
                    response,
                    error_code,
                    pushed_by,
                    pushed_at,
                }
            },
        )
        .collect())
}

pub struct NewStep<'a> {
    pub tenant: Uuid,
    pub release: Uuid,
    pub step: &'a str,
    pub request_hash: Option<&'a str>,
    pub status: Option<i32>,
    pub ok: bool,
    pub response: Option<&'a Value>,
    pub error_code: Option<&'a str>,
    pub by: &'a str,
}

pub async fn insert_step(conn: &mut PgConnection, s: NewStep<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO push_step \
           (tenant, release, step, request_hash, status, ok, response, error_code, pushed_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(s.tenant)
    .bind(s.release)
    .bind(s.step)
    .bind(s.request_hash)
    .bind(s.status)
    .bind(s.ok)
    .bind(s.response)
    .bind(s.error_code)
    .bind(s.by)
    .execute(conn)
    .await?;
    Ok(())
}

/// The newest release of `target` that care-bus accepted (an ok `release` push step):
/// `(release_id, hash, pushed_at)`.
pub async fn last_pushed(
    conn: &mut PgConnection,
    target: Uuid,
) -> Result<Option<(String, String, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT r.release_id, r.hash, to_jsonb(p.pushed_at) #>> '{}' \
         FROM push_step p JOIN config_release r ON r.id = p.release \
         WHERE r.target = $1 AND p.step = 'release' AND p.ok \
         ORDER BY p.id DESC LIMIT 1",
    )
    .bind(target)
    .fetch_optional(conn)
    .await
}
