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
//! 402 `license_unreadable` is an introspection whose `expires_at` is present but not an RFC 3339
//! timestamp: it is never read as "no expiry" (fail-closed) and never cached. (commonpractice's other
//! `license_unreadable`, a stored token that will not decrypt, cannot happen here: the token is not
//! encrypted at rest, see migration 0004.)
//!
//! Off unless `LICENSE_GATE_2_ENABLED`: then [`check`] is a no-op that touches neither the database
//! nor the network.
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{Value, json};
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
    /// `expires_at` in microseconds since the epoch; `None` only when absent or null (no expiry). An
    /// unparseable one is never cached (see [`IntrospectError::Unreadable`]).
    expires_us: Option<i64>,
}

/// Why an introspection gave no usable answer.
#[derive(Debug)]
enum IntrospectError {
    /// The licences service could not be asked, or did not answer 2xx: 402 `license_unavailable`.
    Upstream(UpstreamError),
    /// It answered, with an `expires_at` that is not an RFC 3339 timestamp: 402 `license_unreadable`.
    Unreadable(String),
}

impl From<UpstreamError> for IntrospectError {
    fn from(e: UpstreamError) -> Self {
        Self::Upstream(e)
    }
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

    /// The introspection of `token`, from the cache while it is fresh. Only a 2xx answer with a
    /// readable (or absent) `expires_at` is cached; anything else is retried on the next request.
    async fn introspect(&self, token: &str) -> Result<Cached, IntrospectError> {
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
            return Err(UpstreamError::status(what, resp.status()).into());
        }
        let v: Introspection = resp
            .json()
            .await
            .map_err(|e| UpstreamError::transport(what, &e))?;
        let expires_us = match v.expires_at.as_deref() {
            None => None,
            Some(s) => match rfc3339_epoch_us(s) {
                Some(us) => Some(us),
                None => return Err(IntrospectError::Unreadable(s.to_string())),
            },
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

/// `s` as microseconds since the epoch, strictly RFC 3339 with a mandatory offset: what licences
/// sends (Python `datetime.isoformat()` of a `timestamptz`), `YYYY-MM-DDTHH:MM:SS[.f{1,9}]` then `Z`
/// or `±HH:MM`. `None` for anything else, including a timestamp without an offset (its instant is
/// unknown). Fractions beyond microseconds are truncated. No date-time crate: this is the one shape.
fn rfc3339_epoch_us(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let d = b.get(from..from + len)?;
        let mut n = 0i64;
        for &c in d {
            if !c.is_ascii_digit() {
                return None;
            }
            n = n * 10 + i64::from(c - b'0');
        }
        Some(n)
    };
    let at = |i: usize, c: u8| b.get(i) == Some(&c);
    if !(at(4, b'-') && at(7, b'-') && matches!(b.get(10).copied(), Some(b'T' | b't' | b' '))) {
        return None;
    }
    if !(at(13, b':') && at(16, b':')) {
        return None;
    }
    let (year, month, day) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (hour, minute, second) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if !(1..=month_days).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let mut i = 19;
    let mut micros = 0i64;
    if at(i, b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            if i - start < 6 {
                micros = micros * 10 + i64::from(b[i] - b'0');
            }
            i += 1;
        }
        let digits = i - start;
        if digits == 0 || digits > 9 {
            return None;
        }
        for _ in digits..6 {
            micros *= 10;
        }
    }
    let offset_secs = match b.get(i).copied() {
        Some(b'Z' | b'z') if b.len() == i + 1 => 0,
        Some(sign @ (b'+' | b'-')) if b.len() == i + 6 && at(i + 3, b':') => {
            let (oh, om) = (num(i + 1, 2)?, num(i + 4, 2)?);
            if oh > 23 || om > 59 {
                return None;
            }
            let o = oh * 3600 + om * 60;
            if sign == b'-' { -o } else { o }
        }
        _ => return None,
    };
    // Days since 1970-01-01 of the civil date (Howard Hinnant's `days_from_civil`).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second - offset_secs;
    Some(secs * 1_000_000 + micros)
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
    let c = match gate.introspect(&token).await {
        Ok(c) => c,
        Err(IntrospectError::Upstream(e)) => {
            tracing::warn!(%tenant, error = %e, "licence introspect failed; denying");
            return Err(unavailable());
        }
        Err(IntrospectError::Unreadable(raw)) => {
            tracing::warn!(%tenant, expires_at = %raw, "licence expires_at is unreadable; denying");
            return Err(ApiError::PaymentRequired(
                "license_unreadable",
                "the tenant licence could not be read; access denied".into(),
            ));
        }
    };
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
    fn rfc3339_parses_what_licences_sends_and_nothing_else() {
        let p = rfc3339_epoch_us;
        assert_eq!(p("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(p("1970-01-01T00:00:00+00:00"), Some(0));
        assert_eq!(p("2020-01-01T00:00:00+00:00"), Some(1_577_836_800_000_000));
        // Python isoformat with microseconds, and an offset east of UTC
        assert_eq!(
            p("2020-01-01T01:00:00.000123+01:00"),
            Some(1_577_836_800_000_123)
        );
        assert_eq!(
            p("2019-12-31T23:00:00.5-01:00"),
            Some(1_577_836_800_500_000)
        );
        // nanoseconds are truncated to microseconds
        assert_eq!(p("1970-01-01T00:00:00.123456789Z"), Some(123_456));
        assert_eq!(p("2024-02-29T00:00:00Z"), Some(1_709_164_800_000_000));
        assert_eq!(p("1969-12-31T23:59:59Z"), Some(-1_000_000));
        for bad in [
            "",
            "garbage",
            "2020-01-01",
            "2020-01-01T00:00:00",
            "2020-01-01T00:00:00+0000",
            "2020-01-01T00:00:00Zjunk",
            "2020-13-01T00:00:00Z",
            "2023-02-29T00:00:00Z",
            "2020-01-01T24:00:00Z",
            "2020-01-01T00:00:00.Z",
            "2020-01-01T00:00:00.1234567890Z",
            "2020-1-01T00:00:00Z",
            "+020-01-01T00:00:00Z",
        ] {
            assert_eq!(p(bad), None, "{bad:?}");
        }
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
