//! Provision-down to care-bus (W4): the tenant and its job exist in care-bus before a release is pushed
//! to it. Both calls are idempotent upserts (care-bus `PUT /api/tenants/{t}`, `PUT .../jobs/{key}`).
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;

use crate::carebus_client::{CarebusClient, CarebusError, CarebusResponse};
use crate::db::tenants::Tenant;

/// One care-bus call and how it went, as recorded in `push_step`.
#[derive(Debug, Clone, Serialize)]
pub struct StepOutcome {
    pub step: &'static str,
    /// care-bus's HTTP status; `None` when care-bus was not reached.
    pub status: Option<u16>,
    pub ok: bool,
    /// care-bus's answer; `Null` when there was none.
    pub body: Value,
    pub error_code: Option<String>,
}

impl StepOutcome {
    pub fn of(step: &'static str, r: &Result<CarebusResponse, CarebusError>) -> Self {
        match r {
            Ok(resp) => Self {
                step,
                status: Some(resp.status),
                ok: true,
                body: resp.body.clone(),
                error_code: None,
            },
            Err(CarebusError::Rejected { status, code, body }) => Self {
                step,
                status: Some(*status),
                ok: false,
                body: body.clone(),
                error_code: code.clone(),
            },
            Err(CarebusError::Unavailable(_)) => Self {
                step,
                status: None,
                ok: false,
                body: Value::Null,
                error_code: Some("carebus_unavailable".into()),
            },
        }
    }
}

/// `put_tenant(active)` then `put_job(label)`, stopping at the first failure. On success
/// `tenant.carebus_provisioned_at` is set (owner pool; a failure to record it is only logged).
pub async fn ensure_tenant_and_job(
    client: &CarebusClient,
    owner: &PgPool,
    tenant: &Tenant,
    job: &str,
    job_label: &str,
) -> (Vec<StepOutcome>, Result<(), CarebusError>) {
    let mut steps = Vec::new();
    let r = client.put_tenant(tenant.id, &tenant.name, "active").await;
    steps.push(StepOutcome::of("tenant", &r));
    if let Err(e) = r {
        return (steps, Err(e));
    }
    let r = client.put_job(tenant.id, job, job_label).await;
    steps.push(StepOutcome::of("job", &r));
    if let Err(e) = r {
        return (steps, Err(e));
    }
    if let Err(e) = sqlx::query("UPDATE tenant SET carebus_provisioned_at = now() WHERE id = $1")
        .bind(tenant.id)
        .execute(owner)
        .await
    {
        tracing::warn!(tenant = %tenant.id, error = %e, "could not record carebus_provisioned_at");
    }
    (steps, Ok(()))
}
