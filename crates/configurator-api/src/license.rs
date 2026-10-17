//! Licence gate 2 (W2.1), after commonpractice `middleware/license-gate.ts` and
//! `services/license-client.ts` (patterns only, no shared code).
//!
//! Gate 1 is the Keycloak token (who, and through `users`, which tenant). Gate 2 asks whether that
//! tenant holds an active licence for this app: it reads the tenant's stored licence token
//! (`tenant_license`, written when the licence is issued) and introspects it at the licences service
//! (`POST LICENSE_INTROSPECT_URL {"token"}`, public, no auth; the token is opaque, not a JWT). The
//! answer is cached per token for `LICENSE_CACHE_TTL_SEC`. FAIL-CLOSED: no stored token is 402
//! `license_required`; a lookup or introspect failure is 402 `license_unavailable`, never a pass.
//! Then, in commonpractice's order: `active` must be true (402 `license_inactive`), `expires_at` not
//! passed (402 `license_expired`), `app.key` must be `LICENSE_APP_KEY` (403 `license_app_mismatch`)
//! and `tenant_id` the resolved tenant (403 `license_tenant_mismatch`), which binds the two gates.
//! commonpractice's 402 `license_unreadable` (a stored token that will not decrypt) cannot happen
//! here: the token is not encrypted at rest (see migration 0004).
//!
//! Off unless `LICENSE_GATE_2_ENABLED`: then [`check`] is a no-op that touches neither the database
//! nor the network.
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::LicenseGateConfig;
use crate::db;
use crate::error::ApiError;
use crate::service_token::UpstreamError;
use crate::state::AppState;

/// The fields gate 2 reads from an introspection. All optional: an inactive or unknown token may omit
/// them, and a missing one fails its check.
#[derive(Clone, Debug, Deserialize)]
struct Introspection {
    active: Option<bool>,
    tenant_id: Option<String>,
    app: Option<LicenseApp>,
    expires_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct LicenseApp {
    key: Option<String>,
}

#[derive(Clone, Debug)]
struct Cached {
    fetched: Instant,
    v: Introspection,
    /// `expires_at` in microseconds since the epoch; `None` when absent or unparseable (commonpractice
    /// ignores an expiry it cannot parse).
    expires_us: Option<i64>,
}

pub struct LicenseGate {
    cfg: LicenseGateConfig,
    http: reqwest::Client,
    /// Introspections by token. Bounded by the number of tenants; stale entries go on each insert.
    cache: Mutex<HashMap<String, Cached>>,
}

impl LicenseGate {
    pub fn new(cfg: LicenseGateConfig, http: reqwest::Client) -> Self {
        Self {
            cfg,
            http,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    /// The introspection of `token`, from the cache while it is fresh. Only a 2xx answer is cached; a
    /// failure is retried on the next request.
    async fn introspect(&self, owner: &PgPool, token: &str) -> Result<Cached, UpstreamError> {
        let ttl = Duration::from_secs(self.cfg.cache_ttl_sec);
        let hit = {
            let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            cache
                .get(token)
                .filter(|c| c.fetched.elapsed() < ttl)
                .cloned()
        };
        if let Some(c) = hit {
            return Ok(c);
        }
        let what = "licence introspect";
        let resp = self
            .http
            .post(self.cfg.introspect_url.as_str())
            .json(&json!({"token": token}))
            .send()
            .await
            .map_err(|e| UpstreamError::transport(what, &e))?;
        if !resp.status().is_success() {
            return Err(UpstreamError::status(what, resp.status()));
        }
        let v: Introspection = resp
            .json()
            .await
            .map_err(|e| UpstreamError::transport(what, &e))?;
        let expires_us = match v.expires_at.as_deref() {
            Some(s) => epoch_us(owner, s).await,
            None => None,
        };
        let c = Cached {
            fetched: Instant::now(),
            v,
            expires_us,
        };
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache.retain(|_, e| e.fetched.elapsed() < ttl);
        cache.insert(token.to_string(), c.clone());
        Ok(c)
    }
}

/// An ISO-8601 timestamp as microseconds since the epoch, parsed by Postgres (the crate carries no
/// date-time type); `None` when it does not parse.
async fn epoch_us(owner: &PgPool, ts: &str) -> Option<i64> {
    sqlx::query_scalar::<_, i64>("SELECT (extract(epoch FROM $1::timestamptz) * 1000000)::bigint")
        .bind(ts)
        .fetch_one(owner)
        .await
        .ok()
}

fn now_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn unavailable() -> ApiError {
    ApiError::PaymentRequired(
        "license_unavailable",
        "could not verify the tenant licence; access denied".into(),
    )
}

/// The checks after a successful introspection, in commonpractice's order.
fn verdict(app_key: &str, tenant: Uuid, c: &Cached, now_us: i64) -> Result<(), ApiError> {
    if c.v.active != Some(true) {
        return Err(ApiError::PaymentRequired(
            "license_inactive",
            "the tenant licence is not active".into(),
        ));
    }
    if c.expires_us.is_some_and(|e| e <= now_us) {
        return Err(ApiError::PaymentRequired(
            "license_expired",
            "the tenant licence has expired".into(),
        ));
    }
    if c.v.app.as_ref().and_then(|a| a.key.as_deref()) != Some(app_key) {
        return Err(ApiError::Denied(
            "license_app_mismatch",
            "the licence is not for this application".into(),
        ));
    }
    if c.v
        .tenant_id
        .as_deref()
        .and_then(|t| Uuid::parse_str(t).ok())
        != Some(tenant)
    {
        return Err(ApiError::Denied(
            "license_tenant_mismatch",
            "the licence belongs to a different tenant".into(),
        ));
    }
    Ok(())
}

/// Gate 2 for a caller acting in `tenant`. Called where the tenant is resolved (`tenancy`), for
/// everyone but the super-admin.
pub async fn check(st: &AppState, tenant: Uuid) -> Result<(), ApiError> {
    let gate = &st.license;
    if !gate.enabled() {
        return Ok(());
    }
    // The lookup is fail-closed too: a database error denies (402), never surfaces as a 500.
    let token = match db::licenses::token(&st.owner, tenant).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return Err(ApiError::PaymentRequired(
                "license_required",
                "this tenant has no licence".into(),
            ));
        }
        Err(e) => {
            tracing::warn!(%tenant, error = %e, "licence token lookup failed; denying");
            return Err(unavailable());
        }
    };
    let c = gate.introspect(&st.owner, &token).await.map_err(|e| {
        tracing::warn!(%tenant, error = %e, "licence introspect failed; denying");
        unavailable()
    })?;
    verdict(&gate.cfg.app_key, tenant, &c, now_us())
}

/// The licence state of `tenant` for `GET /api/license/status`: what gate 2 would say, as data
/// (never a 402), so a client can explain a block instead of failing on it.
pub async fn status(st: &AppState, tenant: Option<Uuid>) -> Value {
    if !st.license.enabled() {
        return json!({"enforced": false, "tenant": tenant, "state": "not_enforced", "code": null});
    }
    let Some(t) = tenant else {
        return json!({"enforced": true, "tenant": null, "state": "no_workspace", "code": null});
    };
    match check(st, t).await {
        Ok(()) => json!({"enforced": true, "tenant": t, "state": "active", "code": null}),
        Err(e) => {
            let (_, code) = e.status_and_code();
            json!({"enforced": true, "tenant": t, "state": "blocked", "code": code,
                   "message": e.to_string()})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cached(v: Value, expires_us: Option<i64>) -> Cached {
        Cached {
            fetched: Instant::now(),
            v: serde_json::from_value(v).unwrap(),
            expires_us,
        }
    }

    fn code(r: Result<(), ApiError>) -> Option<&'static str> {
        r.err().map(|e| e.status_and_code().1)
    }

    #[test]
    fn verdicts_in_commonpractice_order() {
        let t = Uuid::parse_str("6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b").unwrap();
        let good = json!({"active": true, "tenant_id": t, "app": {"key": "app"}});
        assert_eq!(
            code(verdict("app", t, &cached(good.clone(), None), 10)),
            None
        );
        // expiry in the future passes; at or before now is expired
        assert_eq!(
            code(verdict("app", t, &cached(good.clone(), Some(11)), 10)),
            None
        );
        assert_eq!(
            code(verdict("app", t, &cached(good.clone(), Some(10)), 10)),
            Some("license_expired")
        );
        let mut v = good.clone();
        v["active"] = json!(false);
        assert_eq!(
            code(verdict("app", t, &cached(v, None), 10)),
            Some("license_inactive")
        );
        // null and absent `active` are not active
        let mut v = good.clone();
        v["active"] = Value::Null;
        assert_eq!(
            code(verdict("app", t, &cached(v, None), 10)),
            Some("license_inactive")
        );
        assert_eq!(
            code(verdict("other", t, &cached(good.clone(), None), 10)),
            Some("license_app_mismatch")
        );
        let mut v = good;
        v["tenant_id"] = json!(Uuid::new_v4());
        let e = verdict("app", t, &cached(v, None), 10).unwrap_err();
        assert_eq!(e.status_and_code().0, axum::http::StatusCode::FORBIDDEN);
        assert_eq!(e.status_and_code().1, "license_tenant_mismatch");
    }
}
