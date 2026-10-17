//! Licence issuance (W2.2), after commonpractice `backend/src/services/licenses-admin.ts` and
//! `license-provisioning.ts` (patterns only, no shared code).
//!
//! The licences admin API sits behind the `secure-apps` realm, a different auth boundary from the
//! portal realm people sign in with: this client authenticates as the client-credentials SA
//! `strata-configurator-licenses-admin` (`LICENSES_ADMIN_*`). Issuing a tenant's licence is
//! commonpractice's sequence: register the tenant id in the licences registry (`PUT /tenants/{id}`,
//! idempotent; licences refuses to mint for an unknown tenant), resolve the app and plan ids (the
//! licence create takes ids, not codes), create the licence for the holder's `sub`, and mint its
//! opaque token, which gate 2 then introspects. Every call is fatal on failure.
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::config::LicensesAdminConfig;
use crate::service_token::{ServiceToken, UpstreamError, encode};

/// A licence issued to a tenant, and the token minted for it (returned once, never again).
#[derive(Clone)]
pub struct Issued {
    pub license_id: String,
    /// The licences service's own Subject id for the holder (not the Keycloak `sub`).
    pub subject_id: String,
    pub token: String,
}

impl std::fmt::Debug for Issued {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Issued")
            .field("license_id", &self.license_id)
            .field("subject_id", &self.subject_id)
            .field("token", &"[redacted]")
            .finish()
    }
}

#[derive(Deserialize)]
struct WireId {
    id: Option<String>,
}

#[derive(Deserialize)]
struct WirePlan {
    id: Option<String>,
    code: Option<String>,
}

#[derive(Deserialize)]
struct WireLicense {
    license_id: Option<String>,
    id: Option<String>,
    subject_id: Option<String>,
}

#[derive(Deserialize)]
struct WireToken {
    token: Option<String>,
}

#[derive(Deserialize)]
struct WireRevoke {
    revoked: Option<bool>,
}

/// The name a tenant is registered under in the licences registry, which must match
/// `^[a-z0-9][a-z0-9-]*$` and is unique per owning app: the tenant name as a slug (at most 40
/// characters) plus the last 8 hex digits of its id, so two tenants with one name do not collide.
pub fn registry_name(name: &str, id: Uuid) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    let hex = id.simple().to_string();
    let tail = &hex[hex.len() - 8..];
    if slug.is_empty() {
        format!("t-{tail}")
    } else {
        format!("{slug}-{tail}")
    }
}

pub struct LicensesAdmin {
    /// The admin API base, without a trailing slash.
    base: String,
    app_key: String,
    plan_code: String,
    /// The realm the holder lives in, stated on the licence (commonpractice `subject_realm`): without
    /// it licences would record the caller's realm, `secure-apps`.
    subject_realm: String,
    token: ServiceToken,
    http: reqwest::Client,
    /// (app id, plan id), resolved once and kept for the process (ids never change for a code).
    ids: Mutex<Option<(String, String)>>,
}

impl LicensesAdmin {
    pub fn new(
        cfg: &LicensesAdminConfig,
        app_key: &str,
        subject_realm: &str,
        http: reqwest::Client,
    ) -> Self {
        Self {
            base: cfg.url.trim_end_matches('/').into(),
            app_key: app_key.into(),
            plan_code: cfg.plan_code.clone(),
            subject_realm: subject_realm.into(),
            token: ServiceToken::new(
                &cfg.token_url,
                &cfg.client_id,
                &cfg.client_secret,
                http.clone(),
            ),
            http,
            ids: Mutex::new(None),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.base)
    }

    /// `req` with the SA's bearer token (and `X-Tenant-Id` when acting on a tenant), sent.
    async fn send(
        &self,
        req: reqwest::RequestBuilder,
        tenant: Option<Uuid>,
        what: &str,
    ) -> Result<reqwest::Response, UpstreamError> {
        let token = self.token.get().await?;
        let mut req = req.bearer_auth(token);
        if let Some(t) = tenant {
            req = req.header("x-tenant-id", t.to_string());
        }
        req.send()
            .await
            .map_err(|e| UpstreamError::transport(what, &e))
    }

    /// `resp`'s JSON body, after checking it is a 2xx.
    async fn ok_json<T: serde::de::DeserializeOwned>(
        resp: reqwest::Response,
        what: &str,
    ) -> Result<T, UpstreamError> {
        if !resp.status().is_success() {
            return Err(UpstreamError::status(what, resp.status()));
        }
        resp.json()
            .await
            .map_err(|e| UpstreamError::transport(what, &e))
    }

    /// The (app id, plan id) of `LICENSE_APP_KEY` / `LICENSE_PLAN_CODE`.
    async fn app_and_plan(&self) -> Result<(String, String), UpstreamError> {
        let mut ids = self.ids.lock().await;
        if let Some(hit) = ids.as_ref() {
            return Ok(hit.clone());
        }
        let key = encode(&self.app_key);
        let what = "licences resolve app";
        let resp = self
            .send(self.http.get(self.url(&format!("apps/{key}"))), None, what)
            .await?;
        let app: WireId = Self::ok_json(resp, what).await?;
        let app_id = app
            .id
            .ok_or_else(|| UpstreamError::new(None, "licences resolve app: no id"))?;
        let what = "licences resolve plans";
        let resp = self
            .send(
                self.http.get(self.url(&format!("apps/{key}/plans"))),
                None,
                what,
            )
            .await?;
        let plans: Vec<WirePlan> = Self::ok_json(resp, what).await?;
        let plan_id = plans
            .into_iter()
            .find(|p| p.code.as_deref() == Some(self.plan_code.as_str()))
            .and_then(|p| p.id)
            .ok_or_else(|| {
                UpstreamError::new(
                    None,
                    format!(
                        "plan {:?} not found for app {:?}",
                        self.plan_code, self.app_key
                    ),
                )
            })?;
        *ids = Some((app_id.clone(), plan_id.clone()));
        Ok((app_id, plan_id))
    }

    /// Issue `tenant`'s licence, held by `holder_sub`, and mint its token.
    pub async fn issue(
        &self,
        tenant: Uuid,
        registry_name: &str,
        holder_sub: &str,
    ) -> Result<Issued, UpstreamError> {
        let what = "licences register tenant";
        let resp = self
            .send(
                self.http
                    .put(self.url(&format!("tenants/{tenant}")))
                    .json(&json!({"name": registry_name})),
                None,
                what,
            )
            .await?;
        if !resp.status().is_success() {
            return Err(UpstreamError::status(what, resp.status()));
        }

        let (app_id, plan_id) = self.app_and_plan().await?;
        let what = "licences create licence";
        let body = json!({
            "app_id": app_id,
            "plan_id": plan_id,
            "subject_realm": self.subject_realm,
        });
        let resp = self
            .send(
                self.http
                    .post(self.url(&format!("licenses/user/{}", encode(holder_sub))))
                    .json(&body),
                Some(tenant),
                what,
            )
            .await?;
        let l: WireLicense = Self::ok_json(resp, what).await?;
        let license_id = l
            .license_id
            .or(l.id)
            .ok_or_else(|| UpstreamError::new(None, "licences create licence: no license_id"))?;
        // From here the licence exists: a failure must not leave it live without a tenant to hold it.
        // Never substitute the Keycloak `sub`: they are different identifiers.
        let Some(subject_id) = l.subject_id else {
            let err = UpstreamError::new(None, "licences create licence: no subject_id");
            return Err(self.abandon(tenant, &license_id, err).await);
        };
        let token = match self.mint(tenant, &license_id).await {
            Ok(t) => t,
            Err(err) => return Err(self.abandon(tenant, &license_id, err).await),
        };
        Ok(Issued {
            license_id,
            subject_id,
            token,
        })
    }

    /// Mint `license_id`'s opaque token: a registered PERPETUAL (`long_lived`) `api` token. Licences
    /// gives a mint without `expires_days` a bounded default (365 days), which would fail gate 2 for
    /// every tenant a year after onboarding while its licence is still active. A long-lived mint
    /// needs the SA's `license-admin` realm role (else 403 `long_lived_requires_license_admin`); a
    /// refusal keeps licences' `detail.error` in the error, and still fails the issuance.
    async fn mint(&self, tenant: Uuid, license_id: &str) -> Result<String, UpstreamError> {
        let what = "licences mint token";
        let path = format!(
            "licenses/{}/tokens?long_lived=true&long_lived_owner={}&long_lived_reason={}",
            encode(license_id),
            encode(MINT_OWNER),
            encode(MINT_REASON),
        );
        let resp = self
            .send(
                self.http.post(self.url(&path)).json(&json!({})),
                Some(tenant),
                what,
            )
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let refusal = resp
                .json::<Value>()
                .await
                .ok()
                .and_then(|b| refusal_code(&b));
            let message = match refusal {
                Some(code) => format!("{what} failed: HTTP {status}: {code}"),
                None => format!("{what} failed: HTTP {status}"),
            };
            return Err(UpstreamError::new(Some(status.as_u16()), message));
        }
        let t: WireToken = Self::ok_json(resp, what).await?;
        t.token
            .filter(|s| !s.is_empty())
            .ok_or_else(|| UpstreamError::new(None, "licences mint token: no token"))
    }

    /// Issuance failed after `license_id` was created: revoke it (best effort; a failed revoke is
    /// logged for manual cleanup) and hand back `err`, the failure that stopped the issuance.
    async fn abandon(&self, tenant: Uuid, license_id: &str, err: UpstreamError) -> UpstreamError {
        if let Err(e) = self.revoke(tenant, license_id).await {
            tracing::error!(%tenant, %license_id, error = %e,
                "could not revoke a licence whose issuance failed; revoke it by hand");
        }
        err
    }

    /// Revoke a licence by id. Licences answers 200 `{"revoked": false}` for an unknown licence or a
    /// tenant mismatch, so only `revoked: true` counts (commonpractice `revokeConfirmed`).
    pub async fn revoke(&self, tenant: Uuid, license_id: &str) -> Result<(), UpstreamError> {
        let what = "licences revoke";
        let resp = self
            .send(
                self.http
                    .post(self.url("license/revoke"))
                    .json(&json!({"license_id": license_id})),
                Some(tenant),
                what,
            )
            .await?;
        let status = resp.status();
        let r: WireRevoke = Self::ok_json(resp, what).await?;
        if r.revoked != Some(true) {
            return Err(UpstreamError::new(
                Some(status.as_u16()),
                format!("licences revoke of {license_id}: no-op, the licence is still active"),
            ));
        }
        Ok(())
    }
}

/// `long_lived_owner` of every tenant licence token: who answers for the perpetual token.
const MINT_OWNER: &str = "strata-configurator";
/// `long_lived_reason` of every tenant licence token.
const MINT_REASON: &str = "tenant licence token for gate 2; revoked on offboarding";

/// Licences' refusal code in an error body: `detail.error` (`{"detail": {"error": ...}}`) or a plain
/// string `detail`.
fn refusal_code(body: &Value) -> Option<String> {
    let detail = body.get("detail")?;
    detail
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| detail.as_str())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_names_fit_the_registry_pattern() {
        let id = Uuid::parse_str("6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b").unwrap();
        assert_eq!(
            registry_name("Acme Health B.V.", id),
            "acme-health-b-v-1e2f3a4b"
        );
        assert_eq!(registry_name("  ---  ", id), "t-1e2f3a4b");
        assert_eq!(registry_name("Zürich", id), "z-rich-1e2f3a4b");
        let long = registry_name(&"a".repeat(100), id);
        assert_eq!(long, format!("{}-1e2f3a4b", "a".repeat(40)));
    }

    #[test]
    fn refusal_codes_are_read_from_licences_error_bodies() {
        assert_eq!(
            refusal_code(&json!({"detail": {"error": "long_lived_requires_license_admin"}})),
            Some("long_lived_requires_license_admin".to_string())
        );
        assert_eq!(
            refusal_code(&json!({"detail": "License not found"})),
            Some("License not found".to_string())
        );
        assert_eq!(refusal_code(&json!({"detail": {"remedy": "x"}})), None);
        assert_eq!(refusal_code(&json!({"error": "x"})), None);
    }
}
