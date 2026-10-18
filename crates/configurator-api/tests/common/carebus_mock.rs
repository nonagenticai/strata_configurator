//! A local care-bus (and the Keycloak token endpoint its SA client uses) on 127.0.0.1:0, built from the
//! release-bundle-v1 contract (care-bus plan 11; `routes/{tenancy,releases}.rs` on care-bus-agents main
//! fc7f394). It records every request, validates a release body as care-bus does (schema, path, hash,
//! typed parse, referential checks), keeps releases per (tenant, job), and can be told to answer any
//! route with a given status and body.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{post, put};
use axum::{Json, Router};
use configurator_core::release::{ReleaseBodyV1, SCHEMA_V1, bundle_hash, validate};
use serde_json::{Value, json};

/// The access token the mock's token endpoint hands out and every care-bus route requires.
pub const CB_TOKEN: &str = "cb-sa-token";
/// The SA's client secret in the mock config.
pub const CB_SECRET: &str = "cb-release-writer-secret";

/// One request the mock received.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub auth: Option<String>,
    pub tenant: Option<String>,
    pub body: Value,
}

/// One job's releases in one tenant.
#[derive(Default)]
struct JobReleases {
    /// release_id → hash
    known: HashMap<String, String>,
    /// The current release's report.
    current: Option<Value>,
}

#[derive(Default)]
pub struct CbState {
    /// Client-credentials grants answered.
    pub grants: usize,
    pub requests: Vec<Recorded>,
    /// Route → a forced `(status, body)`: `tenant`, `user`, `job`, `release`, `current`.
    pub answers: HashMap<&'static str, (u16, Value)>,
    /// The next this-many care-bus calls answer 401 (a revoked token).
    pub reject_tokens: usize,
    /// The conflicts every release report carries.
    pub conflicts: Vec<Value>,
    tenants: Vec<String>,
    jobs: Vec<(String, String)>,
    releases: HashMap<(String, String), JobReleases>,
}

pub type Shared = Arc<Mutex<CbState>>;

pub fn lock(m: &Shared) -> MutexGuard<'_, CbState> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn reply(status: u16, body: Value) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(body),
    )
        .into_response()
}

fn error(status: u16, code: &str, message: &str) -> Response {
    reply(status, json!({"error": {"code": code, "message": message}}))
}

fn parse(body: &Bytes) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

/// Record the request, then check the bearer token and any forced answer for `route`.
fn pre(
    m: &Shared,
    method: &str,
    path: String,
    h: &HeaderMap,
    body: &Value,
    route: &'static str,
) -> Option<Response> {
    let mut s = lock(m);
    let auth = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let tenant = h
        .get("x-active-tenant-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    s.requests.push(Recorded {
        method: method.into(),
        path,
        auth: auth.clone(),
        tenant,
        body: body.clone(),
    });
    if auth.as_deref() != Some(format!("Bearer {CB_TOKEN}").as_str()) {
        return Some(error(401, "unauthorized", "missing or invalid token"));
    }
    if s.reject_tokens > 0 {
        s.reject_tokens -= 1;
        return Some(error(401, "unauthorized", "missing or invalid token"));
    }
    s.answers
        .get(route)
        .map(|(status, body)| reply(*status, body.clone()))
}

async fn grant(State(m): State<Shared>, body: String) -> Response {
    if !body.contains("grant_type=client_credentials") || !body.contains("client_secret=cb-release")
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    lock(&m).grants += 1;
    Json(json!({"access_token": CB_TOKEN, "expires_in": 300})).into_response()
}

async fn put_tenant(
    State(m): State<Shared>,
    Path(t): Path<String>,
    h: HeaderMap,
    body: Bytes,
) -> Response {
    let v = parse(&body);
    if let Some(r) = pre(&m, "PUT", format!("/api/tenants/{t}"), &h, &v, "tenant") {
        return r;
    }
    let mut s = lock(&m);
    let created = !s.tenants.contains(&t);
    if created {
        s.tenants.push(t.clone());
    }
    reply(
        if created { 201 } else { 200 },
        json!({"id": t, "name": v["name"], "status": v["status"]}),
    )
}

/// care-bus's tenant scope for the release-writer (`tenancy.rs`): the header must name a bare, non-nil
/// tenant uuid (400 `active_tenant_required` / `invalid_active_tenant`), the tenant must exist (404
/// `not_found` "tenant not found"), then the job must exist in it (404 "job not found", `job_ctx`).
fn job_scope(s: &CbState, h: &HeaderMap, job: &str) -> Result<String, Box<Response>> {
    let Some(raw) = h.get("x-active-tenant-id").and_then(|v| v.to_str().ok()) else {
        return Err(Box::new(error(
            400,
            "active_tenant_required",
            "Name the tenant to act in with the X-Active-Tenant-Id header.",
        )));
    };
    match uuid::Uuid::parse_str(raw) {
        Ok(u) if !u.is_nil() && raw.len() == 36 => {}
        _ => {
            return Err(Box::new(error(
                400,
                "invalid_active_tenant",
                "X-Active-Tenant-Id must be a bare, non-nil tenant uuid",
            )));
        }
    }
    let tenant = raw.to_owned();
    if !s.tenants.contains(&tenant) {
        return Err(Box::new(error(404, "not_found", "tenant not found")));
    }
    if !s.jobs.contains(&(tenant.clone(), job.to_owned())) {
        return Err(Box::new(error(404, "not_found", "job not found")));
    }
    Ok(tenant)
}

async fn put_user(
    State(m): State<Shared>,
    Path((t, sub)): Path<(String, String)>,
    h: HeaderMap,
    body: Bytes,
) -> Response {
    let v = parse(&body);
    if let Some(r) = pre(
        &m,
        "PUT",
        format!("/api/tenants/{t}/users/{sub}"),
        &h,
        &v,
        "user",
    ) {
        return r;
    }
    if !lock(&m).tenants.contains(&t) {
        return error(404, "not_found", "tenant not found");
    }
    reply(200, json!({"sub": sub, "tenant_role": v["tenant_role"]}))
}

async fn put_job(
    State(m): State<Shared>,
    Path((t, key)): Path<(String, String)>,
    h: HeaderMap,
    body: Bytes,
) -> Response {
    let v = parse(&body);
    if let Some(r) = pre(
        &m,
        "PUT",
        format!("/api/tenants/{t}/jobs/{key}"),
        &h,
        &v,
        "job",
    ) {
        return r;
    }
    let mut s = lock(&m);
    if !s.tenants.contains(&t) {
        return error(404, "not_found", "tenant not found");
    }
    let id = (t, key.clone());
    let created = !s.jobs.contains(&id);
    if created {
        s.jobs.push(id);
    }
    reply(
        if created { 201 } else { 200 },
        json!({"key": key, "label": v["label"]}),
    )
}

fn counts(b: &ReleaseBodyV1) -> Value {
    let bundle = &b.bundle;
    json!({
        "context_spaces": bundle.context_spaces.len(),
        "ontology_entities": bundle.ontology.entities.len(),
        "ontology_attributes": bundle.ontology.entities.iter().map(|e| e.attributes.len()).sum::<usize>(),
        "ontology_relations": bundle.ontology.relations.len(),
        "procedures": bundle.procedures.len(),
        "procedure_steps": bundle.procedures.iter().map(|p| p.steps.len()).sum::<usize>(),
        "connectors": bundle.connectors.len(),
        "connector_field_mappings": bundle.connectors.iter().map(|c| c.field_mappings.len()).sum::<usize>(),
        "knowledge_stores": bundle.knowledge_stores.len(),
    })
}

async fn put_release(
    State(m): State<Shared>,
    Path((job, rid)): Path<(String, String)>,
    h: HeaderMap,
    body: Bytes,
) -> Response {
    let v = parse(&body);
    let path = format!("/api/jobs/{job}/releases/{rid}");
    if let Some(r) = pre(&m, "PUT", path, &h, &v, "release") {
        return r;
    }
    let tenant = match job_scope(&lock(&m), &h, &job) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    if v["schema"] != SCHEMA_V1 {
        return error(400, "invalid_bundle", "schema");
    }
    if v["release_id"] != rid.as_str() || v["job"] != job.as_str() {
        return error(400, "release_path_mismatch", "body and path disagree");
    }
    match bundle_hash(&v["bundle"]) {
        Ok(h) if v["hash"] == h.as_str() => {}
        Ok(_) => return error(400, "bundle_hash_mismatch", "hash"),
        Err(_) => return error(400, "invalid_bundle", "numbers must be integers"),
    }
    let parsed: ReleaseBodyV1 = match serde_json::from_value(v.clone()) {
        Ok(p) => p,
        Err(e) => return error(400, "invalid_bundle", &e.to_string()),
    };
    let violations = validate(&parsed.bundle);
    if !violations.is_empty() {
        return reply(
            400,
            json!({"error": {"code": "invalid_bundle", "message": "invalid", "details": violations}}),
        );
    }
    let mut s = lock(&m);
    let conflicts = s.conflicts.clone();
    let jr = s.releases.entry((tenant, job)).or_default();
    let previous = jr
        .current
        .as_ref()
        .and_then(|c| c["release_id"].as_str().map(str::to_owned));
    let report = |applied: bool| {
        json!({
            "release_id": rid,
            "hash": parsed.hash,
            "applied": applied,
            "previous_release_id": previous,
            "applied_at": "2026-10-18T00:00:00Z",
            "applied_by": "service-account-strata-configurator-release-writer",
            "counts": counts(&parsed),
            "conflicts": conflicts,
        })
    };
    let (status, out) = match jr.known.get(&rid) {
        Some(h) if *h != parsed.hash => {
            return error(409, "release_hash_mismatch", "known release, other hash");
        }
        Some(_) if previous.as_deref() == Some(rid.as_str()) => (200, report(false)),
        Some(_) => (200, report(true)),
        None => (201, report(true)),
    };
    jr.known.insert(rid.clone(), parsed.hash.clone());
    if out["applied"] == true {
        jr.current = Some(out.clone());
    }
    reply(status, out)
}

async fn get_release(
    State(m): State<Shared>,
    Path((job, rid)): Path<(String, String)>,
    h: HeaderMap,
) -> Response {
    let path = format!("/api/jobs/{job}/releases/{rid}");
    if let Some(r) = pre(&m, "GET", path, &h, &Value::Null, "current") {
        return r;
    }
    let s = lock(&m);
    let tenant = match job_scope(&s, &h, &job) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    if rid != "current" {
        return error(404, "not_found", "release not found");
    }
    let conflicts = s.conflicts.clone();
    match s
        .releases
        .get(&(tenant, job))
        .and_then(|j| j.current.clone())
    {
        Some(mut r) => {
            r["applied"] = json!(true);
            r["conflicts"] = json!(conflicts);
            reply(200, r)
        }
        None => error(404, "not_found", "release not found"),
    }
}

pub struct CarebusMock {
    pub url: String,
    pub state: Shared,
}

impl CarebusMock {
    pub async fn start() -> Self {
        let state: Shared = Arc::default();
        let app = Router::new()
            .route("/token", post(grant))
            .route("/api/tenants/{t}", put(put_tenant))
            .route("/api/tenants/{t}/users/{sub}", put(put_user))
            .route("/api/tenants/{t}/jobs/{key}", put(put_job))
            .route(
                "/api/jobs/{job}/releases/{rid}",
                put(put_release).get(get_release),
            )
            .with_state(state.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        Self { url, state }
    }

    /// The four `CAREBUS_*` settings pointing at this mock.
    pub fn config(&self) -> Vec<(&'static str, String)> {
        vec![
            ("CAREBUS_API_URL", self.url.clone()),
            ("CAREBUS_TOKEN_URL", format!("{}/token", self.url)),
            (
                "CAREBUS_CLIENT_ID",
                "strata-configurator-release-writer".into(),
            ),
            ("CAREBUS_CLIENT_SECRET", CB_SECRET.into()),
        ]
    }

    pub fn requests(&self) -> Vec<Recorded> {
        lock(&self.state).requests.clone()
    }

    pub fn grants(&self) -> usize {
        lock(&self.state).grants
    }

    /// Answer `route` with `status` and `body` from now on.
    pub fn answer(&self, route: &'static str, status: u16, body: Value) {
        lock(&self.state).answers.insert(route, (status, body));
    }

    pub fn clear_answer(&self, route: &'static str) {
        lock(&self.state).answers.remove(route);
    }

    pub fn set_conflicts(&self, conflicts: Vec<Value>) {
        lock(&self.state).conflicts = conflicts;
    }

    pub fn reject_tokens(&self, n: usize) {
        lock(&self.state).reject_tokens = n;
    }
}
