//! Offboarding a tenant: the licence half, after commonpractice `license-provisioning.ts`
//! `revokeTenantLicense` as `services/offboarding.ts` (`offboardTenant`, step 2a) calls it.
//!
//! There is no tenant offboard (or delete) route yet; [`revoke_tenant_license`] is the hook it must
//! call, BEFORE anything removes the `tenant` row (`tenant_license` cascades with it, and with it the
//! licence id this needs). Semantics are commonpractice's:
//! - a no-op while issuance is off (no `LICENSES_ADMIN_*`), and for a tenant that holds no licence;
//! - otherwise FAIL-LOUD: the licence is revoked at the licences service by id, through the same
//!   `secure-apps` client-credentials SA that issued it; any failure (transport, non-2xx, or licences
//!   answering `revoked: false`) is 502 `license_provisioning_failed`, audited as
//!   `license.revoke_failed`, and the stored row is KEPT so the offboard can be retried. An offboard
//!   must never report a revoke it did not do, which would leave the tenant a working licence;
//! - only after a confirmed revoke is the row deleted, in one owner transaction with its
//!   `license.revoke` audit event.
//!
//! The revoke needs only the licence id, never the token, so an unreadable token (a rotated key) does
//! not block an offboard.
use serde_json::json;
use uuid::Uuid;

use crate::audit;
use crate::db::licenses;
use crate::error::ApiError;
use crate::state::AppState;

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
