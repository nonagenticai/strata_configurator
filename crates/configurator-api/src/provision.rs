//! Provision-down to care-bus (W4): the tenant and its job exist in care-bus before a release is pushed
//! to it. Both calls are idempotent upserts (care-bus `PUT /api/tenants/{t}`, `PUT .../jobs/{key}`).
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::audit;
use crate::carebus_client::{CarebusClient, CarebusError, CarebusResponse};
use crate::db::tenants::Tenant;
use crate::state::AppState;

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

/// How provisioning a tenant down into care-bus went; returned as `carebus` by tenant create, signup
/// approve, import and the retry route.
#[derive(Debug, Clone, Serialize)]
pub struct CarebusProvision {
    pub ok: bool,
    /// `CAREBUS_PROVISION_ENABLED` is off: nothing was called.
    pub skipped: bool,
    pub steps: Vec<StepOutcome>,
}

async fn record(st: &AppState, actor: &str, tenant: &Tenant, action: &str, metadata: &Value) {
    let done = match st.owner.acquire().await {
        Ok(mut conn) => audit::event(
            &mut conn,
            actor,
            Some(tenant.id),
            action,
            &format!("tenant:{}", tenant.id),
            metadata,
        )
        .await
        .map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    if let Err(e) = done {
        tracing::error!(tenant = %tenant.id, action, error = %e, "could not audit care-bus provisioning");
    }
}

/// Provision `tenant` and its first admin into care-bus: `put_tenant(active)`, then
/// `put_user(first_admin_sub, tenant_admin)`. Best effort, never an error: a failure is audited as
/// `carebus.provision_failed` (`{step, status, code}`) and leaves `carebus_provisioned_at` NULL; success
/// sets it. With `CAREBUS_PROVISION_ENABLED` off nothing is called (`carebus.provision_skipped`).
pub async fn tenant_created(
    st: &AppState,
    actor: &str,
    tenant: &Tenant,
    first_admin_sub: &str,
) -> CarebusProvision {
    let client = match st.carebus.as_deref() {
        Some(c) if st.cfg.carebus_provision_enabled => c,
        _ => {
            record(st, actor, tenant, "carebus.provision_skipped", &json!({})).await;
            return CarebusProvision {
                ok: false,
                skipped: true,
                steps: Vec::new(),
            };
        }
    };
    let mut steps = Vec::new();
    let r = client.put_tenant(tenant.id, &tenant.name, "active").await;
    steps.push(StepOutcome::of("tenant", &r));
    if r.is_ok() {
        let r = client
            .put_user(tenant.id, first_admin_sub, "tenant_admin")
            .await;
        steps.push(StepOutcome::of("user", &r));
    }
    let ok = steps.iter().all(|s| s.ok);
    if ok {
        if let Err(e) =
            sqlx::query("UPDATE tenant SET carebus_provisioned_at = now() WHERE id = $1")
                .bind(tenant.id)
                .execute(&st.owner)
                .await
        {
            tracing::warn!(tenant = %tenant.id, error = %e, "could not record carebus_provisioned_at");
        }
    } else if let Some(failed) = steps.iter().find(|s| !s.ok) {
        tracing::warn!(tenant = %tenant.id, step = failed.step, "care-bus provisioning failed");
        record(
            st,
            actor,
            tenant,
            "carebus.provision_failed",
            &json!({"step": failed.step, "status": failed.status, "code": failed.error_code}),
        )
        .await;
    }
    CarebusProvision {
        ok,
        skipped: false,
        steps,
    }
}
