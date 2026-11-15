//! Offboarding a tenant (`POST /api/tenants/{id}/offboard`), after commonpractice
//! `services/offboarding.ts` (`offboardTenantFully`) and `license-provisioning.ts`
//! `revokeTenantLicense`.
//!
//! Two halves, in this order:
//! 1. [`revoke_tenant_license`], BEFORE anything changes the `tenant` row, and outside any transaction
//!    (no network inside a transaction). Semantics are commonpractice's:
//!    - a no-op while issuance is off (no `LICENSES_ADMIN_*`), and for a tenant that holds no licence;
//!    - otherwise FAIL-LOUD: the licence is revoked at the licences service by id, through the same
//!      `secure-apps` client-credentials SA that issued it; any failure (transport, non-2xx, or
//!      licences answering `revoked: false`) is 502 `license_provisioning_failed`, audited as
//!      `license.revoke_failed`, and the stored row is KEPT so the offboard can be retried. An offboard
//!      must never report a revoke it did not do, which would leave the tenant a working licence;
//!    - only after a confirmed revoke is the row deleted, in one owner transaction with its
//!      `license.revoke` audit event.
//!
//!    The revoke needs only the licence id, never the token, so an unreadable token (a rotated key)
//!    does not block an offboard.
//! 2. [`archive_tenant`]: the tenant is soft-archived (`archived_at`, write-once) and every active user
//!    disabled, in one owner transaction with the `tenant.offboard` audit event. Nothing is deleted:
//!    users never are, and a deleted tenant would cascade its users and licence row away.
//!
//! Where this differs from commonpractice, deliberately: commonpractice suspends the workspace first
//! and revokes after (so a failed revoke leaves it suspended, and re-running is the repair); here the
//! licence goes first, so a failed revoke leaves the tenant exactly as it was, live and still licensed,
//! and the retry starts from the top. As in commonpractice, Keycloak is never touched: the portal realm
//! is shared, so a person leaving this tenant keeps their login for other apps.
use serde_json::json;
use uuid::Uuid;

use crate::audit;
use crate::db::{licenses, tenants, users};
use crate::error::ApiError;
use crate::state::AppState;
use crate::tenancy;

/// Revoke `tenant`'s licence and forget it; `actor` is the `sub` audit events name. The licence id
/// revoked, or `None` when there was nothing to revoke (issuance off, or no licence held).
pub async fn revoke_tenant_license(
    st: &AppState,
    actor: &str,
    tenant: Uuid,
) -> Result<Option<String>, ApiError> {
    let Some(la) = st.licenses_admin.as_deref() else {
        return Ok(None);
    };
    let Some(license_id) = licenses::license_id(&st.owner, tenant).await? else {
        return Ok(None);
    };
    let object = format!("license:{license_id}");
    let held = json!({"tenant": tenant, "license_id": license_id});
    if let Err(e) = la.revoke(tenant, &license_id).await {
        tracing::error!(%tenant, %license_id, error = %e,
            "licence revoke on offboarding failed; the licence is still active, retry the offboard");
        audit::record(
            &mut *st.owner.acquire().await?,
            actor,
            Some(tenant),
            "license.revoke_failed",
            &object,
            Some(&held),
            None,
        )
        .await?;
        return Err(ApiError::BadGateway(
            "license_provisioning_failed",
            "revoking the tenant licence failed, so the tenant still holds it; retry".into(),
        ));
    }
    let mut tx = st.owner.begin().await?;
    licenses::remove(&mut tx, tenant, &license_id).await?;
    audit::record(
        &mut tx,
        actor,
        Some(tenant),
        "license.revoke",
        &object,
        Some(&held),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(Some(license_id))
}

/// What [`archive_tenant`] did.
pub struct Archived {
    /// The tenant as it now is (`archived_at` set).
    pub tenant: tenants::Tenant,
    /// It was archived before this call (a repeat offboard, or a concurrent one that won the race);
    /// its `archived_at` is the original one.
    pub already_archived: bool,
    /// Users this call disabled (0 on a plain repeat).
    pub users_disabled: u64,
}

/// Archive `tenant` and disable its active users, in one owner transaction; `license_id` is what
/// [`revoke_tenant_license`] revoked, for the audit event. Idempotent by construction, as
/// commonpractice's `offboardTenantWrites`: `archived_at` is write-once and only active users are
/// disabled, so a repeat changes nothing (and a user somehow left active is disabled then). Audited as
/// `tenant.offboard` when it wrote anything. The tenant row is locked first, so two concurrent
/// offboards serialise and the second sees the first's archive. 404 `tenant_not_found` when the
/// tenant does not exist.
pub async fn archive_tenant(
    st: &AppState,
    actor: &str,
    tenant: Uuid,
    license_id: Option<&str>,
) -> Result<Archived, ApiError> {
    let mut tx = st.owner.begin().await?;
    let Some(before) = tenants::lock(&mut tx, tenant).await? else {
        return Err(tenancy::tenant_not_found(tenant));
    };
    let already_archived = before.archived_at.is_some();
    let after = tenants::archive(&mut tx, tenant).await?;
    let users_disabled = users::disable_all(&mut tx, tenant).await?;
    if !already_archived || users_disabled > 0 {
        audit::record(
            &mut tx,
            actor,
            Some(tenant),
            "tenant.offboard",
            &format!("tenant:{tenant}"),
            Some(&json!({"tenant": before})),
            Some(&json!({"tenant": after, "license_id": license_id,
                         "users_disabled": users_disabled})),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Archived {
        tenant: after,
        already_archived,
        users_disabled,
    })
}
