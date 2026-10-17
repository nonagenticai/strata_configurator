//! Tenants and tenant roles (spec section 10.2, rev 3: commonpractice's identity model). Keycloak only
//! authenticates; the tenant and tenant role of a person come from this service's `users` table, one
//! tenant per identity. Only the super-admin can act elsewhere, by naming a tenant in
//! `X-Active-Tenant-Id`; every such switch is audited.
use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit;
use crate::auth::Caller;
use crate::db::{tenants, users};
use crate::error::ApiError;
use crate::state::AppState;

/// The super-admin's tenant switch. Ignored for everyone else.
pub const ACTIVE_TENANT_HEADER: &str = "x-active-tenant-id";

/// A tenant role, weakest first: each role includes the ones before it. `tenant_user` reads;
/// `tenant_admin` also edits and approves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    TenantUser,
    TenantAdmin,
}

impl Role {
    /// The `tenant_role` enum label.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::TenantUser => "tenant_user",
            Role::TenantAdmin => "tenant_admin",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "tenant_user" => Some(Role::TenantUser),
            "tenant_admin" => Some(Role::TenantAdmin),
            _ => None,
        }
    }
}

/// The caller acting in one tenant, with their role there.
#[derive(Clone, Debug)]
pub struct TenantCtx {
    pub caller: Caller,
    pub tenant: Uuid,
    pub role: Role,
}

impl TenantCtx {
    /// 403 `forbidden` unless the caller's role here is at least `min`.
    pub fn require(&self, min: Role) -> Result<(), ApiError> {
        if self.role >= min {
            Ok(())
        } else {
            Err(ApiError::Forbidden(min.as_str()))
        }
    }
}

fn invalid_tenant() -> ApiError {
    ApiError::BadRequest(
        "invalid_tenant",
        "the tenant must be one bare, non-nil uuid".into(),
    )
}

/// A tenant id as the caller wrote it: exactly the 36-character hyphenated uuid form, not nil.
/// `Uuid::parse_str` also takes braced, `urn:uuid:` and unhyphenated forms; those are refused here.
pub fn parse_tenant_id(s: &str) -> Result<Uuid, ApiError> {
    let b = s.as_bytes();
    let shaped = b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        });
    if !shaped {
        return Err(invalid_tenant());
    }
    let id = Uuid::parse_str(s).map_err(|_| invalid_tenant())?;
    if id.is_nil() {
        return Err(invalid_tenant());
    }
    Ok(id)
}

fn tenant_not_found(tenant: Uuid) -> ApiError {
    ApiError::Missing(
        "tenant_not_found",
        format!("tenant {tenant} does not exist"),
    )
}

/// 403 `no_workspace`: the caller has no active `users` row.
fn no_workspace() -> ApiError {
    ApiError::Denied(
        "no_workspace",
        "you do not belong to a workspace (or your access was disabled)".into(),
    )
}

/// 400 `tenant_required`: the super-admin, with no workspace of their own, on a tenant-scoped route
/// without `X-Active-Tenant-Id`.
pub fn tenant_required() -> ApiError {
    ApiError::BadRequest(
        "tenant_required",
        "this route needs an X-Active-Tenant-Id header".into(),
    )
}

/// The `X-Active-Tenant-Id` header: `None` when absent; 400 `invalid_tenant` when malformed, nil or
/// repeated.
pub fn active_tenant_header(headers: &HeaderMap) -> Result<Option<Uuid>, ApiError> {
    let mut values = headers.get_all(ACTIVE_TENANT_HEADER).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(invalid_tenant());
    }
    let raw = first.to_str().map_err(|_| invalid_tenant())?;
    parse_tenant_id(raw).map(Some)
}

/// Who the request acts as.
///
/// - Anyone but the super-admin: their own tenant and role from `users`; no row, or a disabled one, is
///   403 `no_workspace`. `X-Active-Tenant-Id` is ignored, so it cannot be used to probe tenants.
/// - The super-admin: the header's tenant (400 `invalid_tenant` when malformed, 404 `tenant_not_found`
///   when unknown) as `tenant_admin`, audited as `tenant.scope_switch` unless it is their own tenant;
///   without the header their own tenant if they have one, else the platform view.
pub async fn acting(st: &AppState, caller: Caller, parts: &Parts) -> Result<Acting, ApiError> {
    if !caller.super_admin {
        let (tenant, role) = users::home(&st.db, &caller.sub)
            .await?
            .ok_or_else(no_workspace)?;
        return Ok(Acting::Tenant(TenantCtx {
            caller,
            tenant,
            role,
        }));
    }
    let header = active_tenant_header(&parts.headers)?;
    let home = users::home(&st.db, &caller.sub).await?.map(|(t, _)| t);
    let tenant = match (header, home) {
        (Some(t), Some(h)) if t == h => t,
        (Some(t), _) => {
            if !tenants::exists(&st.db, t).await? {
                return Err(tenant_not_found(t));
            }
            audit::scope_switch(
                &mut *st.db.acquire().await?,
                &caller.sub,
                t,
                parts.method.as_str(),
                parts.uri.path(),
            )
            .await?;
            t
        }
        (None, Some(h)) => h,
        (None, None) => return Ok(Acting::Platform(caller)),
    };
    Ok(Acting::Tenant(TenantCtx {
        caller,
        tenant,
        role: Role::TenantAdmin,
    }))
}

/// The caller in the tenant a route's path names (`/api/tenants/{id}/…`): the path wins over the
/// active scope. The super-admin is `tenant_admin` in any tenant that exists (404 `tenant_not_found`
/// otherwise); anyone else must belong to it, and a tenant that is not theirs is the same 404 as one
/// that does not exist (no workspace at all is 403 `no_workspace`).
pub async fn in_tenant(st: &AppState, caller: Caller, tenant: Uuid) -> Result<TenantCtx, ApiError> {
    if caller.super_admin {
        if !tenants::exists(&st.db, tenant).await? {
            return Err(tenant_not_found(tenant));
        }
        return Ok(TenantCtx {
            caller,
            tenant,
            role: Role::TenantAdmin,
        });
    }
    let (home, role) = users::home(&st.db, &caller.sub)
        .await?
        .ok_or_else(no_workspace)?;
    if home != tenant {
        return Err(tenant_not_found(tenant));
    }
    Ok(TenantCtx {
        caller,
        tenant,
        role,
    })
}

/// The caller in a tenant, as [`acting`]; the super-admin in the platform view → 400
/// `tenant_required`. The token is checked first (401 before 400/403).
impl FromRequestParts<AppState> for TenantCtx {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, ApiError> {
        let caller = Caller::from_request_parts(parts, st).await?;
        match acting(st, caller, parts).await? {
            Acting::Tenant(ctx) => Ok(ctx),
            Acting::Platform(_) => Err(tenant_required()),
        }
    }
}

/// Who a request acts as on routes where the super-admin may work on platform data.
#[derive(Clone, Debug)]
pub enum Acting {
    /// The super-admin with no tenant in scope: platform data only, no tenant's data.
    Platform(Caller),
    /// The caller in a tenant (the super-admin there is `tenant_admin`).
    Tenant(TenantCtx),
}

impl Acting {
    pub fn caller(&self) -> &Caller {
        match self {
            Acting::Platform(c) => c,
            Acting::Tenant(t) => &t.caller,
        }
    }

    /// The tenant acted in; `None` for the platform view.
    pub fn tenant(&self) -> Option<Uuid> {
        match self {
            Acting::Platform(_) => None,
            Acting::Tenant(t) => Some(t.tenant),
        }
    }
}

/// See [`acting`].
impl FromRequestParts<AppState> for Acting {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, ApiError> {
        let caller = Caller::from_request_parts(parts, st).await?;
        acting(st, caller, parts).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_order_weakest_first() {
        assert!(Role::TenantUser < Role::TenantAdmin);
        assert_eq!(Role::parse("admin"), None, "the old member roles are gone");
        for r in [Role::TenantUser, Role::TenantAdmin] {
            assert_eq!(Role::parse(r.as_str()), Some(r));
            assert_eq!(serde_json::to_value(r).unwrap(), r.as_str());
        }
    }

    #[test]
    fn only_the_bare_hyphenated_form_parses() {
        let ok = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
        assert!(parse_tenant_id(ok).is_ok());
        assert!(parse_tenant_id(&ok.to_uppercase()).is_ok());
        for bad in [
            "",
            "not-a-uuid",
            "00000000-0000-0000-0000-000000000000",
            "{6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b}",
            "urn:uuid:6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b",
            "6f1c2a3b4d5e4f608a7b9c0d1e2f3a4b",
            " 6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b",
            "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4g",
        ] {
            let e = parse_tenant_id(bad).unwrap_err();
            assert_eq!(e.status_and_code().1, "invalid_tenant", "{bad:?}");
        }
    }
}
