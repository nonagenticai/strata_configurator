//! Tenants and tenant roles (spec section 10.2). Roles live in the configurator DB, not in Keycloak.
use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Caller;
use crate::db::tenants;
use crate::error::ApiError;
use crate::state::AppState;

/// A tenant role, weakest first: each role includes the ones before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Viewer,
    Editor,
    Approver,
    Admin,
}

impl Role {
    /// The `member_role` enum label.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Editor => "editor",
            Role::Approver => "approver",
            Role::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "viewer" => Some(Role::Viewer),
            "editor" => Some(Role::Editor),
            "approver" => Some(Role::Approver),
            "admin" => Some(Role::Admin),
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

/// The caller's role in `tenant`: 404 `tenant_not_found` when it does not exist, the super-admin is
/// `admin` in every tenant, anyone else needs a membership (403 `not_a_member`).
pub async fn resolve(st: &AppState, caller: Caller, tenant: Uuid) -> Result<TenantCtx, ApiError> {
    let Some(role) = tenants::role_in(&st.db, tenant, &caller.sub).await? else {
        return Err(ApiError::Missing(
            "tenant_not_found",
            format!("tenant {tenant} does not exist"),
        ));
    };
    let role = if caller.super_admin {
        Role::Admin
    } else {
        role.ok_or_else(|| {
            ApiError::Denied(
                "not_a_member",
                format!("you are not a member of tenant {tenant}"),
            )
        })?
    };
    Ok(TenantCtx {
        caller,
        tenant,
        role,
    })
}

/// 400 `tenant_required`: a tenant-scoped route without `X-Tenant-Id`.
pub fn tenant_required() -> ApiError {
    ApiError::BadRequest(
        "tenant_required",
        "this route needs an X-Tenant-Id header".into(),
    )
}

/// The `X-Tenant-Id` header: `None` when absent; 400 `invalid_tenant` when malformed, nil or repeated.
pub fn tenant_header(headers: &HeaderMap) -> Result<Option<Uuid>, ApiError> {
    let mut values = headers.get_all("x-tenant-id").iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(invalid_tenant());
    }
    let raw = first.to_str().map_err(|_| invalid_tenant())?;
    parse_tenant_id(raw).map(Some)
}

/// Reads `X-Tenant-Id`: exactly one header holding a bare non-nil uuid. Missing → 400 `tenant_required`;
/// malformed, nil or repeated → 400 `invalid_tenant`. The token is checked first (401 before 400).
impl FromRequestParts<AppState> for TenantCtx {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, ApiError> {
        let caller = Caller::from_request_parts(parts, st).await?;
        let Some(tenant) = tenant_header(&parts.headers)? else {
            return Err(tenant_required());
        };
        resolve(st, caller, tenant).await
    }
}

/// Who a request acts as on routes where the super-admin may leave out `X-Tenant-Id`.
#[derive(Clone, Debug)]
pub enum Acting {
    /// The super-admin without `X-Tenant-Id`: platform data only, no tenant's data.
    Platform(Caller),
    /// The caller in the `X-Tenant-Id` tenant (the super-admin there is `admin`).
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

/// `X-Tenant-Id` as for [`TenantCtx`], except that the super-admin may leave it out
/// ([`Acting::Platform`]). Anyone else without it → 400 `tenant_required`.
impl FromRequestParts<AppState> for Acting {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, ApiError> {
        let caller = Caller::from_request_parts(parts, st).await?;
        match tenant_header(&parts.headers)? {
            Some(tenant) => Ok(Acting::Tenant(resolve(st, caller, tenant).await?)),
            None if caller.super_admin => Ok(Acting::Platform(caller)),
            None => Err(tenant_required()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_order_weakest_first() {
        assert!(Role::Viewer < Role::Editor);
        assert!(Role::Editor < Role::Approver);
        assert!(Role::Approver < Role::Admin);
        for r in [Role::Viewer, Role::Editor, Role::Approver, Role::Admin] {
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
