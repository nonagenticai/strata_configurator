//! The care-bus API as the release-writer SA (W4): provision a tenant, its first admin and a job
//! (`PUT /api/tenants/...`, idempotent upserts), push a release (`PUT /api/jobs/{job}/releases/{id}`)
//! and read the current one back. Contract: care-bus plan 11 "Release bundle v1" and
//! `crates/carebus-api/src/routes/{tenancy,releases}.rs` (care-bus-agents main, fc7f394).
//!
//! The tenant goes in the path for the provisioning routes and in `X-Active-Tenant-Id` for the release
//! routes. The bearer token is never logged, stored or returned.
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::config::CarebusConfig;
use crate::service_token::{ServiceToken, encode};

/// The header naming the tenant on care-bus's release routes.
pub const ACTIVE_TENANT_HEADER: &str = "x-active-tenant-id";

/// A care-bus call that did not succeed. Never carries a secret.
#[derive(Debug, thiserror::Error)]
pub enum CarebusError {
    /// Transport failure, a 5xx, or no SA token: worth retrying later.
    #[error("care-bus unavailable: {0}")]
    Unavailable(String),
    /// care-bus answered 4xx; `code` is its `error.code`.
    #[error("care-bus refused the call: HTTP {status} {code:?}")]
    Rejected {
        status: u16,
        code: Option<String>,
        body: Value,
    },
}

/// A 2xx answer.
#[derive(Debug, Clone, PartialEq)]
pub struct CarebusResponse {
    pub status: u16,
    pub body: Value,
}

pub struct CarebusClient {
    base: String,
    token: ServiceToken,
    http: reqwest::Client,
}

impl CarebusClient {
    pub fn new(cfg: &CarebusConfig, http: reqwest::Client) -> Self {
        Self {
            base: cfg.api_url.trim_end_matches('/').to_string(),
            token: ServiceToken::new(
                &cfg.token_url,
                &cfg.client_id,
                &cfg.client_secret,
                http.clone(),
            ),
            http,
        }
    }

    /// One call with the SA's bearer token. A 401 drops the cached token and retries once.
    async fn call(
        &self,
        method: Method,
        path: &str,
        tenant_header: Option<Uuid>,
        body: Option<&Value>,
    ) -> Result<CarebusResponse, CarebusError> {
        let mut retried = false;
        loop {
            let token = self
                .token
                .get()
                .await
                .map_err(|e| CarebusError::Unavailable(e.message))?;
            let mut req = self
                .http
                .request(method.clone(), format!("{}{path}", self.base))
                .bearer_auth(token);
            if let Some(t) = tenant_header {
                req = req.header(ACTIVE_TENANT_HEADER, t.to_string());
            }
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| CarebusError::Unavailable(format!("{method} {path}: {e}")))?;
            let status = resp.status();
            if status == StatusCode::UNAUTHORIZED && !retried {
                retried = true;
                self.token.clear().await;
                continue;
            }
            let text = resp
                .text()
                .await
                .map_err(|e| CarebusError::Unavailable(format!("{method} {path}: {e}")))?;
            let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            if status.is_success() {
                return Ok(CarebusResponse {
                    status: status.as_u16(),
                    body,
                });
            }
            if status.is_client_error() {
                let code = body["error"]["code"].as_str().map(str::to_owned);
                return Err(CarebusError::Rejected {
                    status: status.as_u16(),
                    code,
                    body,
                });
            }
            return Err(CarebusError::Unavailable(format!(
                "{method} {path}: HTTP {status}"
            )));
        }
    }

    /// `PUT /api/tenants/{tenant}` `{name, status}` (care-bus caps the name at 120 characters).
    pub async fn put_tenant(
        &self,
        tenant: Uuid,
        name: &str,
        status: &str,
    ) -> Result<CarebusResponse, CarebusError> {
        let name: String = name.chars().take(120).collect();
        self.call(
            Method::PUT,
            &format!("/api/tenants/{tenant}"),
            None,
            Some(&json!({"name": name, "status": status})),
        )
        .await
    }

    /// `PUT /api/tenants/{tenant}/users/{sub}` `{tenant_role}`.
    pub async fn put_user(
        &self,
        tenant: Uuid,
        sub: &str,
        role: &str,
    ) -> Result<CarebusResponse, CarebusError> {
        self.call(
            Method::PUT,
            &format!("/api/tenants/{tenant}/users/{}", encode(sub)),
            None,
            Some(&json!({"tenant_role": role})),
        )
        .await
    }

    /// `PUT /api/tenants/{tenant}/jobs/{job}` `{label}`. Never sends `settings`: on an existing job a
    /// different `settings` is 409 `job_settings_read_only`; the base settings come with the release.
    pub async fn put_job(
        &self,
        tenant: Uuid,
        job: &str,
        label: &str,
    ) -> Result<CarebusResponse, CarebusError> {
        let label: String = label.chars().take(120).collect();
        self.call(
            Method::PUT,
            &format!("/api/tenants/{tenant}/jobs/{}", encode(job)),
            None,
            Some(&json!({"label": label})),
        )
        .await
    }

    /// `PUT /api/jobs/{job}/releases/{release_id}` with the release body, in `tenant`.
    pub async fn put_release(
        &self,
        tenant: Uuid,
        job: &str,
        release_id: &str,
        body: &Value,
    ) -> Result<CarebusResponse, CarebusError> {
        self.call(
            Method::PUT,
            &format!("/api/jobs/{}/releases/{}", encode(job), encode(release_id)),
            Some(tenant),
            Some(body),
        )
        .await
    }

    /// `GET /api/jobs/{job}/releases/current` in `tenant`: the release report with its open
    /// conflicts; `None` when care-bus has no release for the job. care-bus resolves the tenant, then
    /// the job, then the release (`tenancy.rs` `job_ctx`), all as 404 `not_found` told apart by the
    /// message: "job not found" (the job was never provisioned) and "release not found" (it was, but
    /// never released) both mean no release. "tenant not found" and any other 404 stay errors.
    pub async fn current_release(
        &self,
        tenant: Uuid,
        job: &str,
    ) -> Result<Option<Value>, CarebusError> {
        match self
            .call(
                Method::GET,
                &format!("/api/jobs/{}/releases/current", encode(job)),
                Some(tenant),
                None,
            )
            .await
        {
            Ok(r) => Ok(Some(r.body)),
            Err(CarebusError::Rejected {
                status: 404, body, ..
            }) if matches!(
                body["error"]["message"].as_str(),
                Some("release not found" | "job not found")
            ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}
