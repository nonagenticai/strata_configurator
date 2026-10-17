//! care-bus release bundle v1 (care-bus plan 11, "Release bundle v1"): the typed
//! bundle, its hash and the PUT body.
//!
//! Every struct is `deny_unknown_fields`, so a connector slot cannot carry a
//! `secret_ref`. Nullable keys always serialise (as `null` when unset): care-bus
//! requires them, and an absent key and an explicit `null` hash differently.
//! Reading, an absent nullable key is taken as `null`, because job-content
//! overlay rows store unset nullables by omission (TOML has no null).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::canonical::{canonical_json, sha256_hex};

pub const SCHEMA_V1: &str = "care-bus.release/1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseBodyV1 {
    pub schema: String,
    pub release_id: String,
    pub job: String,
    pub hash: String,
    pub bundle: BundleV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleV1 {
    pub job_settings: JobSettingsV1,
    pub ontology: OntologyV1,
    pub context_spaces: Vec<ContextSpaceV1>,
    pub procedures: Vec<ProcedureV1>,
    pub connectors: Vec<ConnectorSlotV1>,
    pub knowledge_stores: Vec<KnowledgeStoreV1>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSettingsV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preamble: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stance: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OntologyV1 {
    pub entities: Vec<EntityV1>,
    pub relations: Vec<RelationV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityV1 {
    pub name: String,
    /// `generic` | `industry` | `industry_dept`
    pub scope: String,
    #[serde(default)]
    pub industry: Option<String>,
    #[serde(default)]
    pub department: Option<String>,
    pub label: String,
    pub synonyms: Vec<String>,
    pub attributes: Vec<AttributeV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttributeV1 {
    pub name: String,
    pub data_type: String,
    pub pii: bool,
    pub synonyms: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationV1 {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub cardinality: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSpaceV1 {
    pub key: String,
    pub area: String,
    pub label: String,
    pub request_type: String,
    pub required: Vec<String>,
    pub entities: Vec<String>,
    pub procedure: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcedureV1 {
    pub name: String,
    pub version: String,
    /// `DRAFT` | `CANARY` | `PUBLISHED`
    pub status: String,
    pub context_space: String,
    pub domain: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub pddl_domain: Option<String>,
    pub tool_requirements: Vec<String>,
    pub success_criteria: Vec<String>,
    pub reviewers: Vec<String>,
    /// care-bus's `carebus::domain::Plan`; checked here only as `{phases: [...]}`.
    pub plan: Value,
    pub steps: Vec<StepV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepV1 {
    pub step_number: i64,
    pub phase: i64,
    pub name: String,
    pub action_type: String,
    pub worker: String,
    pub mcp_tool: String,
    pub performed_by: String,
    pub preconditions: Vec<String>,
    pub postconditions: Vec<String>,
    pub parameters: BTreeMap<String, String>,
    pub gate: bool,
    /// Optional per care-bus 3232915: absent means care-bus's own tool list decides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_effect: Option<bool>,
}

/// A connector SLOT: the shape of an integration, never its credentials.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorSlotV1 {
    pub id: String,
    pub name: String,
    pub system: String,
    /// `oauth2_client_credentials` | `api_key` | `mtls_api_key` | `none`
    pub auth_type: String,
    #[serde(default)]
    pub base_url: Option<String>,
    pub timeout_ms: i64,
    #[serde(default)]
    pub rate_limit: Option<String>,
    pub field_mappings: Vec<FieldMappingV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldMappingV1 {
    pub backend_field: String,
    /// `Entity.attribute`
    pub attribute: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeStoreV1 {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub parent: Option<String>,
    /// `group` | `leaf`
    pub kind: String,
    pub areas: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HashError {
    #[error("non-integer number at {path}")]
    Float { path: String },
}

/// One JSON-pointer reference token (RFC 6901).
fn pointer_token(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// The first non-integer number in `v`, as a JSON pointer.
fn find_float(v: &Value, path: &mut String) -> Option<String> {
    match v {
        Value::Number(n) if !(n.is_i64() || n.is_u64()) => Some(path.clone()),
        Value::Array(items) => items.iter().enumerate().find_map(|(i, item)| {
            let len = path.len();
            path.push('/');
            path.push_str(&i.to_string());
            let found = find_float(item, path);
            path.truncate(len);
            found
        }),
        Value::Object(map) => map.iter().find_map(|(k, item)| {
            let len = path.len();
            path.push('/');
            path.push_str(&pointer_token(k));
            let found = find_float(item, path);
            path.truncate(len);
            found
        }),
        _ => None,
    }
}

/// `"sha256:" + hex(SHA-256(canonical JSON of bundle))`, as care-bus computes it.
/// A float anywhere is refused: the bundle schema has integers only.
pub fn bundle_hash(bundle: &Value) -> Result<String, HashError> {
    if let Some(path) = find_float(bundle, &mut String::new()) {
        return Err(HashError::Float { path });
    }
    Ok(format!("sha256:{}", sha256_hex(&canonical_json(bundle))))
}

/// The PUT body `{schema, release_id, job, hash, bundle}` for `bundle`.
pub fn release_body(release_id: &str, job: &str, bundle: &BundleV1) -> Result<Value, HashError> {
    let bundle = serde_json::to_value(bundle).expect("a bundle serialises to JSON");
    let hash = bundle_hash(&bundle)?;
    Ok(json!({
        "schema": SCHEMA_V1,
        "release_id": release_id,
        "job": job,
        "hash": hash,
        "bundle": bundle,
    }))
}
