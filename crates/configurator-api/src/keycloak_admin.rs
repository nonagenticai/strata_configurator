//! Keycloak admin API for the portal realm (W1.1), after commonpractice
//! `backend/src/services/keycloak-admin.ts` (patterns only, no shared code).
//!
//! The portal realm is shared with every other app, so this client's writes are deliberately narrow:
//! create an identity for someone the configurator invites, send them Keycloak's action email, and
//! disable. It NEVER writes realm roles or a `tenant_id` user attribute (keycloak-admin.ts:1-7):
//! tenancy and tenant roles live in this service's `users` table.
//!
//! Auth: client credentials for the confidential portal-realm client `strata-configurator-admin`
//! (`KEYCLOAK_ADMIN_*`). Every call is fatal on failure, the lookup included: "could not ask" must
//! never read as "no such user", because the answer decides whether an identity gets created.
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::KeycloakAdminConfig;
use crate::service_token::{ServiceToken, UpstreamError, encode};

/// The actions an invite email asks for: set a password, confirm the address.
pub const INVITE_ACTIONS: [&str; 2] = ["UPDATE_PASSWORD", "VERIFY_EMAIL"];

/// The identity an invite resolved to. `created` is true only when this call created it (and sent the
/// action email).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invited {
    /// The Keycloak user id, which is the `sub` of that person's tokens.
    pub sub: String,
    pub created: bool,
}

#[derive(Deserialize)]
struct RealmUser {
    id: String,
    username: Option<String>,
    email: Option<String>,
}

pub struct KeycloakAdmin {
    /// `.../admin/realms/portal`, without a trailing slash.
    api: String,
    token: ServiceToken,
    http: reqwest::Client,
}

impl KeycloakAdmin {
    pub fn new(cfg: &KeycloakAdminConfig, http: reqwest::Client) -> Self {
        Self {
            api: cfg.api_url.trim_end_matches('/').into(),
            token: ServiceToken::new(
                &cfg.token_url,
                &cfg.client_id,
                &cfg.client_secret,
                http.clone(),
            ),
            http,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.api)
    }

    /// `req` with the service account's bearer token, sent.
    async fn send(
        &self,
        req: reqwest::RequestBuilder,
        what: &str,
    ) -> Result<reqwest::Response, UpstreamError> {
        let token = self.token.get().await?;
        req.bearer_auth(token)
            .send()
            .await
            .map_err(|e| UpstreamError::transport(what, &e))
    }

    /// The id of the realm user whose email, or else username, is exactly `email` (Keycloak creates
    /// users with `username == email`, and its 409 on create can come from either field). Keycloak's
    /// `exact=true` is still case-insensitive, so the match is re-checked here.
    pub async fn find_by_email(&self, email: &str) -> Result<Option<String>, UpstreamError> {
        for field in ["email", "username"] {
            let url = self.url(&format!("users?{field}={}&exact=true&max=2", encode(email)));
            let what = format!("keycloak user lookup by {field}");
            let resp = self.send(self.http.get(url), &what).await?;
            if !resp.status().is_success() {
                return Err(UpstreamError::status(&what, resp.status()));
            }
            let hits: Vec<RealmUser> = resp
                .json()
                .await
                .map_err(|e| UpstreamError::transport(&what, &e))?;
            let hit = hits.into_iter().find(|u| {
                let v = if field == "email" {
                    &u.email
                } else {
                    &u.username
                };
                v.as_deref().is_some_and(|v| v.eq_ignore_ascii_case(email))
            });
            if let Some(u) = hit {
                return Ok(Some(u.id));
            }
        }
        Ok(None)
    }

    /// Create an enabled realm user for `email` and return its id (from the `Location` header);
    /// `None` when Keycloak answers 409 (the user exists).
    pub async fn create_user(&self, email: &str) -> Result<Option<String>, UpstreamError> {
        // No roles and no attributes: the identity only authenticates (see the module doc).
        let rep =
            json!({"username": email, "email": email, "enabled": true, "emailVerified": false});
        let what = "keycloak create user";
        let resp = self
            .send(self.http.post(self.url("users")).json(&rep), what)
            .await?;
        if resp.status() == StatusCode::CONFLICT {
            return Ok(None);
        }
        if resp.status() != StatusCode::CREATED {
            return Err(UpstreamError::status(what, resp.status()));
        }
        resp.headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|l| l.trim_end_matches('/').rsplit('/').next())
            .filter(|id| !id.is_empty())
            .map(|id| Some(id.to_string()))
            .ok_or_else(|| UpstreamError::new(None, "keycloak create user: no id in Location"))
    }

    /// Send Keycloak's action email ([`INVITE_ACTIONS`]); no password is ever emailed.
    pub async fn send_invite_email(&self, id: &str) -> Result<(), UpstreamError> {
        let what = "keycloak execute-actions-email";
        let url = self.url(&format!("users/{}/execute-actions-email", encode(id)));
        let resp = self
            .send(self.http.put(url).json(&INVITE_ACTIONS), what)
            .await?;
        if !resp.status().is_success() {
            return Err(UpstreamError::status(what, resp.status()));
        }
        Ok(())
    }

    /// Disable a realm user (GET-merge-PUT: Keycloak's `PUT /users/{id}` rejects a partial
    /// representation). Not used when a configurator user is disabled: the identity is shared with
    /// every other app in the portal realm.
    pub async fn disable_user(&self, id: &str) -> Result<(), UpstreamError> {
        let url = self.url(&format!("users/{}", encode(id)));
        let what = "keycloak read user";
        let resp = self.send(self.http.get(url.as_str()), what).await?;
        if !resp.status().is_success() {
            return Err(UpstreamError::status(what, resp.status()));
        }
        let mut rep: Value = resp
            .json()
            .await
            .map_err(|e| UpstreamError::transport(what, &e))?;
        rep["enabled"] = Value::Bool(false);
        let what = "keycloak disable user";
        let resp = self.send(self.http.put(url).json(&rep), what).await?;
        if !resp.status().is_success() {
            return Err(UpstreamError::status(what, resp.status()));
        }
        Ok(())
    }

    /// Delete a realm user; 404 counts as done. ONLY to undo an identity created earlier in the same
    /// call (commonpractice `deleteRealmUser`): removing someone from a tenant never deletes them.
    async fn delete_user(&self, id: &str) -> Result<(), UpstreamError> {
        let what = "keycloak delete user";
        let url = self.url(&format!("users/{}", encode(id)));
        let resp = self.send(self.http.delete(url), what).await?;
        if resp.status() == StatusCode::NOT_FOUND || resp.status().is_success() {
            return Ok(());
        }
        Err(UpstreamError::status(what, resp.status()))
    }

    /// The identity for `email`: the existing realm user (no email is sent), else a new one that is
    /// sent the action email. If that email cannot be sent the new identity is deleted again, so a
    /// retry creates it afresh and sends the email then.
    pub async fn find_or_invite(&self, email: &str) -> Result<Invited, UpstreamError> {
        if let Some(sub) = self.find_by_email(email).await? {
            return Ok(Invited {
                sub,
                created: false,
            });
        }
        let Some(sub) = self.create_user(email).await? else {
            // Created concurrently: it exists now, and whoever created it sent the email.
            let sub = self.find_by_email(email).await?.ok_or_else(|| {
                UpstreamError::new(Some(409), "keycloak create user: 409, yet no such user")
            })?;
            return Ok(Invited {
                sub,
                created: false,
            });
        };
        if let Err(e) = self.send_invite_email(&sub).await {
            if let Err(undo) = self.delete_user(&sub).await {
                tracing::error!(%sub, error = %undo, "could not delete the identity whose invite email failed");
            }
            return Err(e);
        }
        Ok(Invited { sub, created: true })
    }
}
