//! `signup_approvals`: the early-access signup queue (W2.3). Owner-only (the runtime role has no
//! grant): every query here takes the owner pool or an owner transaction. Writes take the caller's
//! transaction so the audit event shares it.
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::page::TimeCursor;

#[derive(Debug, Clone, Serialize)]
pub struct Approval {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub company: Option<String>,
    pub message: Option<String>,
    /// `web` or `chat`.
    pub source: String,
    pub trust: String,
    pub intake_client: String,
    /// `pending`, `approved` or `rejected`.
    pub status: String,
    /// The tenant the approval created.
    pub tenant: Option<Uuid>,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339; null while pending.
    pub decided_at: Option<String>,
    pub decided_by: Option<String>,
    /// `created_at` in microseconds since the epoch: the paging cursor, not part of the body.
    #[serde(skip)]
    pub ts_us: i64,
}

type Tuple = (
    Uuid,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    String,
    String,
    Option<Uuid>,
    String,
    Option<String>,
    Option<String>,
    i64,
);

const COLUMNS: &str = "id, email, name, company, message, source, trust, intake_client, status, \
                       tenant, to_jsonb(created_at) #>> '{}', to_jsonb(decided_at) #>> '{}', \
                       decided_by, (extract(epoch FROM created_at) * 1000000)::bigint";

fn approval(t: Tuple) -> Approval {
    let (
        id,
        email,
        name,
        company,
        message,
        source,
        trust,
        intake_client,
        status,
        tenant,
        created_at,
        decided_at,
        decided_by,
        ts_us,
    ) = t;
    Approval {
        id,
        email,
        name,
        company,
        message,
        source,
        trust,
        intake_client,
        status,
        tenant,
        created_at,
        decided_at,
        decided_by,
        ts_us,
    }
}

/// What the intake records.
pub struct NewSignup<'a> {
    pub email: &'a str,
    pub name: &'a str,
    pub company: Option<&'a str>,
    pub message: Option<&'a str>,
    pub source: &'a str,
    pub intake_client: &'a str,
}

/// How many signups are pending.
pub async fn pending_count(conn: &mut PgConnection) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM signup_approvals WHERE status = 'pending'")
        .fetch_one(conn)
        .await
}

/// Queue a pending signup; `None` when a pending row for the address (case-insensitively) already
/// exists, which is left as it is (the partial unique index; a decided row does not block).
pub async fn insert(
    conn: &mut PgConnection,
    s: &NewSignup<'_>,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO signup_approvals (email, name, company, message, source, intake_client) \
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING RETURNING id",
    )
    .bind(s.email)
    .bind(s.name)
    .bind(s.company)
    .bind(s.message)
    .bind(s.source)
    .bind(s.intake_client)
    .fetch_optional(conn)
    .await
}

/// The queue, newest first, one page; only `status` rows when given.
pub async fn list(
    owner: &PgPool,
    status: Option<&str>,
    before: Option<TimeCursor>,
    fetch: i64,
) -> Result<Vec<Approval>, sqlx::Error> {
    let (us, id) = TimeCursor::split(before);
    let rows = sqlx::query_as::<_, Tuple>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM signup_approvals \
         WHERE ($4::text IS NULL OR status = $4) \
           AND ($1::bigint IS NULL \
                OR (created_at, id) < (timestamptz 'epoch' + $1::bigint * interval '1 microsecond', $2::uuid)) \
         ORDER BY created_at DESC, id DESC LIMIT $3"
    )))
    .bind(us)
    .bind(id)
    .bind(fetch)
    .bind(status)
    .fetch_all(owner)
    .await?;
    Ok(rows.into_iter().map(approval).collect())
}

/// One signup; `None` when there is none.
pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Approval>, sqlx::Error> {
    let row = sqlx::query_as::<_, Tuple>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM signup_approvals WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(approval))
}

/// The PENDING signup `id`, locked for the rest of the transaction; `None` when it is not pending.
/// A concurrent approval or rejection blocks here until this transaction ends, then finds the row
/// decided (Postgres re-checks the condition on the row as committed).
pub async fn lock_pending(
    tx: &mut PgConnection,
    id: Uuid,
) -> Result<Option<Approval>, sqlx::Error> {
    let row = sqlx::query_as::<_, Tuple>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM signup_approvals WHERE id = $1 AND status = 'pending' FOR UPDATE"
    )))
    .bind(id)
    .fetch_optional(tx)
    .await?;
    Ok(row.map(approval))
}

/// Mark the PENDING signup `id` approved with the tenant it created; `None` when it is not pending
/// (any more). The row is locked by the update, so a concurrent approval waits for this transaction
/// and then finds it decided.
pub async fn approve(
    tx: &mut PgConnection,
    id: Uuid,
    tenant: Uuid,
    actor: &str,
) -> Result<Option<Approval>, sqlx::Error> {
    let row = sqlx::query_as::<_, Tuple>(sqlx::AssertSqlSafe(format!(
        "UPDATE signup_approvals \
         SET status = 'approved', tenant = $2, decided_at = now(), decided_by = $3 \
         WHERE id = $1 AND status = 'pending' RETURNING {COLUMNS}"
    )))
    .bind(id)
    .bind(tenant)
    .bind(actor)
    .fetch_optional(tx)
    .await?;
    Ok(row.map(approval))
}

/// Mark the PENDING signup `id` rejected; `None` when it is not pending (any more).
pub async fn reject(
    tx: &mut PgConnection,
    id: Uuid,
    actor: &str,
) -> Result<Option<Approval>, sqlx::Error> {
    let row = sqlx::query_as::<_, Tuple>(sqlx::AssertSqlSafe(format!(
        "UPDATE signup_approvals \
         SET status = 'rejected', decided_at = now(), decided_by = $2 \
         WHERE id = $1 AND status = 'pending' RETURNING {COLUMNS}"
    )))
    .bind(id)
    .bind(actor)
    .fetch_optional(tx)
    .await?;
    Ok(row.map(approval))
}
