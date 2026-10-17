//! care-bus release bundle v1 (care-bus plan 11, "Release bundle v1"): the typed
//! bundle, its hash and the PUT body.
//!
//! Every struct is `deny_unknown_fields`, so a connector slot cannot carry a
//! `secret_ref`. Nullable keys always serialise (as `null` when unset): care-bus
//! requires them, and an absent key and an explicit `null` hash differently.
//! Reading, an absent nullable key is taken as `null`, because job-content
//! overlay rows store unset nullables by omission (TOML has no null).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::canonical::{canonical_json, sha256_hex};
use crate::carebus::project_carebus;
use crate::compile::EffectiveConfig;
use crate::overlay::{Kind, is_kebab, is_snake63, is_space_key, is_version};

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

// --- job-content overlay rows ---------------------------------------------------

/// One job-content object (an overlay row of a job-content kind), typed.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentItem {
    ContextSpace(ContextSpaceV1),
    Procedure(ProcedureV1),
    ConnectorSlot(ConnectorSlotV1),
    KnowledgeStore(KnowledgeStoreV1),
}

/// care-bus's `Phase.gate` is nullable; a row stores an unset gate by omission.
fn normalise_plan(plan: &mut Value) {
    if let Some(phases) = plan.get_mut("phases").and_then(Value::as_array_mut) {
        for phase in phases.iter_mut().filter_map(Value::as_object_mut) {
            phase.entry("gate").or_insert(Value::Null);
        }
    }
}

/// The bundle v1 item of a job-content row: its `fields` plus what its key
/// supplies (`key`; `name` and `version` from `name@version`; `id`). The error
/// is the serde message.
pub fn content_item(
    kind: Kind,
    key: &str,
    fields: &Map<String, Value>,
) -> Result<ContentItem, String> {
    let mut obj = fields.clone();
    match kind {
        Kind::ContextSpace | Kind::KnowledgeStore => {
            obj.insert("key".into(), json!(key));
        }
        Kind::ConnectorSlot => {
            obj.insert("id".into(), json!(key));
        }
        Kind::Procedure => {
            let (name, version) = key
                .split_once('@')
                .ok_or_else(|| format!("procedure key {key:?} is not name@version"))?;
            obj.insert("name".into(), json!(name));
            obj.insert("version".into(), json!(version));
        }
        Kind::Entity | Kind::Attribute | Kind::Relation | Kind::Job => {
            return Err(format!("{kind:?} is not a job-content kind"));
        }
    }
    let v = Value::Object(obj);
    let parsed = match kind {
        Kind::ContextSpace => serde_json::from_value(v).map(ContentItem::ContextSpace),
        Kind::Procedure => serde_json::from_value::<ProcedureV1>(v).map(|mut p| {
            normalise_plan(&mut p.plan);
            ContentItem::Procedure(p)
        }),
        Kind::ConnectorSlot => serde_json::from_value(v).map(ContentItem::ConnectorSlot),
        _ => serde_json::from_value(v).map(ContentItem::KnowledgeStore),
    };
    parsed.map_err(|e| e.to_string())
}

// --- validation (care-bus plan 11 Task 3, minus the full Plan type check) -----------

/// One problem in a bundle; `path` is a JSON pointer under `/bundle`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Violation {
    pub path: String,
    pub message: String,
}

const LABEL_MAX: usize = 120;
const SETTING_MAX: usize = 4000;
const CONDITION_MAX: usize = 1000;
const PDDL_MAX: usize = 65536;
const URL_MAX: usize = 2048;

const SCOPES: &[&str] = &["generic", "industry", "industry_dept"];
const DATA_TYPES: &[&str] = &[
    "string", "text", "enum", "date", "datetime", "money", "number", "boolean",
];
const CARDINALITIES: &[&str] = &["1:1", "1:n", "n:1", "n:m"];
const STATUSES: &[&str] = &["DRAFT", "CANARY", "PUBLISHED"];
const AUTH_TYPES: &[&str] = &[
    "oauth2_client_credentials",
    "api_key",
    "mtls_api_key",
    "none",
];
const STORE_KINDS: &[&str] = &["group", "leaf"];

/// `^[A-Z][A-Za-z0-9]{0,63}$`
fn is_entity_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('A'..='Z'))
        && s.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric())
}

/// `^[a-z][a-z0-9_]{0,63}$`
fn is_attribute_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('a'..='z'))
        && s.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[derive(Default)]
struct Checker {
    out: Vec<Violation>,
}

impl Checker {
    fn add(&mut self, path: String, message: impl Into<String>) {
        self.out.push(Violation {
            path,
            message: message.into(),
        });
    }

    fn text(&mut self, path: String, s: &str, max: usize) {
        let n = s.chars().count();
        if n == 0 || n > max {
            self.add(path, format!("must be 1..{max} characters"));
        }
    }

    fn opt_text(&mut self, path: String, s: Option<&str>, max: usize) {
        if let Some(s) = s {
            self.text(path, s, max);
        }
    }

    fn texts(&mut self, path: &str, items: &[String], max: usize) {
        for (i, s) in items.iter().enumerate() {
            self.text(format!("{path}/{i}"), s, max);
        }
    }

    fn pattern(&mut self, path: String, ok: bool, pattern: &str) {
        if !ok {
            self.add(path, format!("must match {pattern}"));
        }
    }

    fn one_of(&mut self, path: String, value: &str, allowed: &[&str]) {
        if !allowed.contains(&value) {
            self.add(path, format!("must be one of {}", allowed.join(" | ")));
        }
    }
}

/// Every referential and shape problem of `b`, all listed (never first-fail).
/// Mirrors care-bus's own check, so a bundle that passes here is not refused
/// there with 400 `invalid_bundle`; the full `Plan` type check stays care-bus's.
pub fn validate(b: &BundleV1) -> Vec<Violation> {
    let mut c = Checker::default();
    for (name, value) in [
        ("preamble", &b.job_settings.preamble),
        ("actor", &b.job_settings.actor),
        ("stance", &b.job_settings.stance),
    ] {
        c.opt_text(
            format!("/bundle/job_settings/{name}"),
            value.as_deref(),
            SETTING_MAX,
        );
    }
    let attributes = check_ontology(&mut c, &b.ontology);
    let procedure_names: HashSet<&str> = b.procedures.iter().map(|p| p.name.as_str()).collect();
    let space_keys: HashSet<&str> = b.context_spaces.iter().map(|s| s.key.as_str()).collect();
    check_context_spaces(&mut c, &b.context_spaces, &attributes, &procedure_names);
    check_procedures(&mut c, &b.procedures, &space_keys);
    check_connectors(&mut c, &b.connectors, &attributes);
    check_knowledge_stores(&mut c, &b.knowledge_stores);
    c.out
}

/// Entity name -> its attribute names.
fn check_ontology<'a>(c: &mut Checker, o: &'a OntologyV1) -> HashMap<&'a str, HashSet<&'a str>> {
    let mut index: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (i, e) in o.entities.iter().enumerate() {
        let p = format!("/bundle/ontology/entities/{i}");
        c.pattern(
            format!("{p}/name"),
            is_entity_name(&e.name),
            "^[A-Z][A-Za-z0-9]{0,63}$",
        );
        if index.contains_key(e.name.as_str()) {
            c.add(format!("{p}/name"), format!("duplicate entity {}", e.name));
        }
        c.one_of(format!("{p}/scope"), &e.scope, SCOPES);
        let (needs_industry, needs_department) = match e.scope.as_str() {
            "industry" => (true, false),
            "industry_dept" => (true, true),
            _ => (false, false),
        };
        for (field, value, required) in [
            ("industry", &e.industry, needs_industry),
            ("department", &e.department, needs_department),
        ] {
            let path = format!("{p}/{field}");
            match (value.as_deref(), required) {
                (None, true) => c.add(path, format!("is required for scope {}", e.scope)),
                (Some(_), false) => c.add(path, format!("must be null for scope {}", e.scope)),
                (Some(v), true) => c.text(path, v, LABEL_MAX),
                (None, false) => {}
            }
        }
        c.text(format!("{p}/label"), &e.label, LABEL_MAX);
        c.texts(&format!("{p}/synonyms"), &e.synonyms, LABEL_MAX);
        let names = index.entry(e.name.as_str()).or_default();
        for (j, a) in e.attributes.iter().enumerate() {
            let ap = format!("{p}/attributes/{j}");
            c.pattern(
                format!("{ap}/name"),
                is_attribute_name(&a.name),
                "^[a-z][a-z0-9_]{0,63}$",
            );
            if !names.insert(a.name.as_str()) {
                c.add(
                    format!("{ap}/name"),
                    format!("duplicate attribute {}.{}", e.name, a.name),
                );
            }
            c.one_of(format!("{ap}/data_type"), &a.data_type, DATA_TYPES);
            c.texts(&format!("{ap}/synonyms"), &a.synonyms, LABEL_MAX);
        }
    }
    let mut triples: HashSet<(&str, &str, &str)> = HashSet::new();
    for (i, r) in o.relations.iter().enumerate() {
        let p = format!("/bundle/ontology/relations/{i}");
        for (field, end) in [("subject", &r.subject), ("object", &r.object)] {
            if !index.contains_key(end.as_str()) {
                c.add(
                    format!("{p}/{field}"),
                    format!("{end} is not an entity of this bundle"),
                );
            }
        }
        c.text(format!("{p}/predicate"), &r.predicate, 64);
        c.one_of(format!("{p}/cardinality"), &r.cardinality, CARDINALITIES);
        if !triples.insert((r.subject.as_str(), r.predicate.as_str(), r.object.as_str())) {
            c.add(p, "duplicate relation (subject, predicate, object)");
        }
    }
    index
}

fn check_context_spaces(
    c: &mut Checker,
    spaces: &[ContextSpaceV1],
    entities: &HashMap<&str, HashSet<&str>>,
    procedure_names: &HashSet<&str>,
) {
    let mut keys: HashSet<&str> = HashSet::new();
    let mut request_types: HashSet<&str> = HashSet::new();
    for (i, s) in spaces.iter().enumerate() {
        let p = format!("/bundle/context_spaces/{i}");
        c.pattern(
            format!("{p}/key"),
            is_space_key(&s.key),
            "^[a-z0-9-]+(:[a-z0-9-]+){1,3}$",
        );
        if !keys.insert(s.key.as_str()) {
            c.add(
                format!("{p}/key"),
                format!("duplicate context space {}", s.key),
            );
        }
        c.pattern(
            format!("{p}/area"),
            is_kebab(&s.area),
            "^[a-z][a-z0-9-]{0,62}$",
        );
        c.text(format!("{p}/label"), &s.label, LABEL_MAX);
        c.pattern(
            format!("{p}/request_type"),
            is_snake63(&s.request_type),
            "^[a-z][a-z0-9_]{0,62}$",
        );
        if !request_types.insert(s.request_type.as_str()) {
            c.add(
                format!("{p}/request_type"),
                format!("duplicate request_type {}", s.request_type),
            );
        }
        for (j, slot) in s.required.iter().enumerate() {
            c.pattern(
                format!("{p}/required/{j}"),
                is_attribute_name(slot),
                "^[a-z][a-z0-9_]{0,63}$",
            );
        }
        for (j, entity) in s.entities.iter().enumerate() {
            if !entities.contains_key(entity.as_str()) {
                c.add(
                    format!("{p}/entities/{j}"),
                    format!("{entity} is not an entity of this bundle"),
                );
            }
        }
        if !procedure_names.contains(s.procedure.as_str()) {
            c.add(
                format!("{p}/procedure"),
                format!("{} is not a procedure of this bundle", s.procedure),
            );
        }
    }
}

fn check_procedures(c: &mut Checker, procedures: &[ProcedureV1], space_keys: &HashSet<&str>) {
    let mut versions: HashSet<(&str, &str)> = HashSet::new();
    let mut published_names: HashSet<&str> = HashSet::new();
    let mut published_spaces: HashSet<&str> = HashSet::new();
    for (i, pr) in procedures.iter().enumerate() {
        let p = format!("/bundle/procedures/{i}");
        c.pattern(
            format!("{p}/name"),
            is_snake63(&pr.name),
            "^[a-z][a-z0-9_]{0,62}$",
        );
        c.pattern(
            format!("{p}/version"),
            is_version(&pr.version),
            "^\\d+\\.\\d+\\.\\d+(-[0-9A-Za-z.]+)?$",
        );
        if !versions.insert((pr.name.as_str(), pr.version.as_str())) {
            c.add(
                format!("{p}/version"),
                format!("duplicate procedure {}@{}", pr.name, pr.version),
            );
        }
        if !space_keys.contains(pr.context_space.as_str()) {
            c.add(
                format!("{p}/context_space"),
                format!("{} is not a context space of this bundle", pr.context_space),
            );
        }
        c.one_of(format!("{p}/status"), &pr.status, STATUSES);
        if pr.status == "PUBLISHED" {
            if !published_names.insert(pr.name.as_str()) {
                c.add(
                    format!("{p}/status"),
                    format!("a second PUBLISHED version of {}", pr.name),
                );
            }
            if !published_spaces.insert(pr.context_space.as_str()) {
                c.add(
                    format!("{p}/status"),
                    format!("a second PUBLISHED procedure in {}", pr.context_space),
                );
            }
        }
        c.text(format!("{p}/domain"), &pr.domain, LABEL_MAX);
        c.opt_text(
            format!("{p}/description"),
            pr.description.as_deref(),
            SETTING_MAX,
        );
        c.opt_text(
            format!("{p}/pddl_domain"),
            pr.pddl_domain.as_deref(),
            PDDL_MAX,
        );
        c.texts(
            &format!("{p}/tool_requirements"),
            &pr.tool_requirements,
            LABEL_MAX,
        );
        c.texts(
            &format!("{p}/success_criteria"),
            &pr.success_criteria,
            CONDITION_MAX,
        );
        c.texts(&format!("{p}/reviewers"), &pr.reviewers, LABEL_MAX);
        let phases = match pr.plan.get("phases").and_then(Value::as_array) {
            Some(phases) if pr.plan.is_object() => phases.len(),
            _ => {
                c.add(format!("{p}/plan"), "must be an object with a phases array");
                0
            }
        };
        check_steps(c, &p, pr, phases);
    }
}

fn check_steps(c: &mut Checker, p: &str, pr: &ProcedureV1, phases: usize) {
    let mut numbers: HashSet<i64> = HashSet::new();
    for (j, s) in pr.steps.iter().enumerate() {
        let sp = format!("{p}/steps/{j}");
        if s.step_number < 1 {
            c.add(format!("{sp}/step_number"), "must be at least 1");
        } else if !numbers.insert(s.step_number) {
            c.add(
                format!("{sp}/step_number"),
                format!("duplicate step number {}", s.step_number),
            );
        }
        if usize::try_from(s.phase).ok().is_none_or(|ph| ph >= phases) {
            c.add(
                format!("{sp}/phase"),
                format!("must index a plan phase (0..{phases})"),
            );
        }
        for (field, value) in [
            ("name", &s.name),
            ("action_type", &s.action_type),
            ("worker", &s.worker),
            ("mcp_tool", &s.mcp_tool),
            ("performed_by", &s.performed_by),
        ] {
            c.text(format!("{sp}/{field}"), value, LABEL_MAX);
        }
        c.texts(
            &format!("{sp}/preconditions"),
            &s.preconditions,
            CONDITION_MAX,
        );
        c.texts(
            &format!("{sp}/postconditions"),
            &s.postconditions,
            CONDITION_MAX,
        );
        for (k, v) in &s.parameters {
            let kp = format!("{sp}/parameters/{}", pointer_token(k));
            c.text(kp.clone(), k, LABEL_MAX);
            c.text(kp, v, LABEL_MAX);
        }
    }
}

fn check_connectors(
    c: &mut Checker,
    connectors: &[ConnectorSlotV1],
    entities: &HashMap<&str, HashSet<&str>>,
) {
    let mut ids: HashSet<&str> = HashSet::new();
    for (i, k) in connectors.iter().enumerate() {
        let p = format!("/bundle/connectors/{i}");
        c.pattern(format!("{p}/id"), is_kebab(&k.id), "^[a-z][a-z0-9-]{0,62}$");
        if !ids.insert(k.id.as_str()) {
            c.add(format!("{p}/id"), format!("duplicate connector {}", k.id));
        }
        c.text(format!("{p}/name"), &k.name, LABEL_MAX);
        c.text(format!("{p}/system"), &k.system, LABEL_MAX);
        c.one_of(format!("{p}/auth_type"), &k.auth_type, AUTH_TYPES);
        if let Some(url) = &k.base_url {
            if !url.starts_with("https://") || url.len() <= "https://".len() {
                c.add(format!("{p}/base_url"), "must be an https:// URL or null");
            } else {
                c.text(format!("{p}/base_url"), url, URL_MAX);
            }
        }
        if !(100..=600_000).contains(&k.timeout_ms) {
            c.add(format!("{p}/timeout_ms"), "must be 100..600000");
        }
        c.opt_text(
            format!("{p}/rate_limit"),
            k.rate_limit.as_deref(),
            LABEL_MAX,
        );
        let mut fields: HashSet<&str> = HashSet::new();
        for (j, m) in k.field_mappings.iter().enumerate() {
            let mp = format!("{p}/field_mappings/{j}");
            c.text(format!("{mp}/backend_field"), &m.backend_field, LABEL_MAX);
            if !fields.insert(m.backend_field.as_str()) {
                c.add(
                    format!("{mp}/backend_field"),
                    format!("duplicate backend_field {}", m.backend_field),
                );
            }
            let present = m
                .attribute
                .split_once('.')
                .is_some_and(|(e, a)| entities.get(e).is_some_and(|names| names.contains(a)));
            if !present {
                c.add(
                    format!("{mp}/attribute"),
                    format!(
                        "{} is not an Entity.attribute of this bundle's ontology",
                        m.attribute
                    ),
                );
            }
        }
    }
}

fn check_knowledge_stores(c: &mut Checker, stores: &[KnowledgeStoreV1]) {
    let mut by_key: HashMap<&str, &KnowledgeStoreV1> = HashMap::new();
    for (i, s) in stores.iter().enumerate() {
        let p = format!("/bundle/knowledge_stores/{i}");
        c.pattern(
            format!("{p}/key"),
            is_kebab(&s.key),
            "^[a-z][a-z0-9-]{0,62}$",
        );
        if by_key.contains_key(s.key.as_str()) {
            c.add(
                format!("{p}/key"),
                format!("duplicate knowledge store {}", s.key),
            );
        } else {
            by_key.insert(s.key.as_str(), s);
        }
        c.text(format!("{p}/label"), &s.label, LABEL_MAX);
        c.one_of(format!("{p}/kind"), &s.kind, STORE_KINDS);
        for (j, area) in s.areas.iter().enumerate() {
            c.pattern(
                format!("{p}/areas/{j}"),
                is_kebab(area),
                "^[a-z][a-z0-9-]{0,62}$",
            );
        }
        c.opt_text(
            format!("{p}/description"),
            s.description.as_deref(),
            SETTING_MAX,
        );
    }
    for (i, s) in stores.iter().enumerate() {
        let Some(parent) = s.parent.as_deref() else {
            continue;
        };
        let path = format!("/bundle/knowledge_stores/{i}/parent");
        let Some(ps) = by_key.get(parent) else {
            c.add(
                path,
                format!("{parent} is not a knowledge store of this bundle"),
            );
            continue;
        };
        if ps.kind != "group" {
            c.add(path.clone(), format!("{parent} is not a group"));
        }
        // Walk up at most `stores.len()` links: returning to this store, or not
        // reaching a root, is a cycle.
        let mut current = parent;
        let mut links = 0;
        loop {
            if current == s.key {
                c.add(path, "the parent chain has a cycle");
                break;
            }
            match by_key.get(current).and_then(|x| x.parent.as_deref()) {
                Some(next) if links < stores.len() => {
                    current = next;
                    links += 1;
                }
                Some(_) => {
                    c.add(path, "the parent chain has a cycle");
                    break;
                }
                None => break,
            }
        }
    }
}

// --- build -------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// The compile output cannot make a release: no job, no context space or no
    /// PUBLISHED procedure. Pushing it would delete care-bus's working base.
    Incomplete(Vec<Violation>),
    /// The built bundle fails `validate`.
    Invalid(Vec<Violation>),
}

/// The bundle v1 of a compiled `(selection, job)`. Lists are sorted by natural
/// key, so two builds of one compile are byte-identical.
///
/// The ontology slice is `job.ontology_slice` (every effective entity when it is
/// empty) plus every entity a context space names plus every entity a connector
/// field mapping names. Hidden entities are dropped; a relation is kept only when
/// both its ends are in the slice.
pub fn build_bundle(e: &EffectiveConfig) -> Result<BundleV1, BuildError> {
    let jc = &e.job_content;
    let mut missing = Vec::new();
    if e.job.is_none() {
        missing.push(Violation {
            path: "/bundle/job_settings".into(),
            message: "no job: the selection must name a job that compiles".into(),
        });
    }
    if jc.context_spaces.is_empty() {
        missing.push(Violation {
            path: "/bundle/context_spaces".into(),
            message: "the job has no context space".into(),
        });
    }
    if !jc.procedures.values().any(|p| p.status == "PUBLISHED") {
        missing.push(Violation {
            path: "/bundle/procedures".into(),
            message: "the job has no PUBLISHED procedure".into(),
        });
    }
    let Some(job) = e.job.as_ref().filter(|_| missing.is_empty()) else {
        return Err(BuildError::Incomplete(missing));
    };

    let mut slice: BTreeSet<&str> = if job.ontology_slice.is_empty() {
        e.entities.keys().map(String::as_str).collect()
    } else {
        job.ontology_slice.iter().map(String::as_str).collect()
    };
    slice.extend(
        jc.context_spaces
            .values()
            .flat_map(|s| s.entities.iter().map(String::as_str)),
    );
    slice.extend(jc.connectors.values().flat_map(|k| {
        k.field_mappings
            .iter()
            .filter_map(|m| m.attribute.split_once('.').map(|(entity, _)| entity))
    }));

    let projected = project_carebus(e);
    let entities: Vec<EntityV1> = projected
        .entities
        .iter()
        .filter(|x| !x.hidden && slice.contains(x.name.as_str()))
        .map(|x| {
            let mut attributes: Vec<AttributeV1> = e
                .attributes
                .values()
                .filter(|a| a.entity == x.name)
                .map(|a| AttributeV1 {
                    name: a.name.clone(),
                    data_type: a.data_type.clone(),
                    pii: a.pii,
                    synonyms: a.synonyms.clone(),
                })
                .collect();
            attributes.sort_by(|a, b| a.name.cmp(&b.name));
            EntityV1 {
                name: x.name.clone(),
                scope: x.scope.clone(),
                industry: x.industry.clone(),
                department: x.department.clone(),
                label: x.label.clone(),
                synonyms: x.synonyms.clone(),
                attributes,
            }
        })
        .collect();
    let kept: HashSet<&str> = entities.iter().map(|x| x.name.as_str()).collect();
    let relations: Vec<RelationV1> = projected
        .relations
        .iter()
        .filter(|r| kept.contains(r.subject.as_str()) && kept.contains(r.object.as_str()))
        .map(|r| RelationV1 {
            subject: r.subject.clone(),
            predicate: r.predicate.clone(),
            object: r.object.clone(),
            cardinality: r.cardinality.clone(),
        })
        .collect();

    let mut procedures: Vec<ProcedureV1> = jc.procedures.values().cloned().collect();
    procedures.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));

    let bundle = BundleV1 {
        job_settings: JobSettingsV1 {
            preamble: job.preamble.clone(),
            actor: job.actor_label.clone(),
            stance: job.stance.clone(),
        },
        ontology: OntologyV1 {
            entities,
            relations,
        },
        // BTreeMap values: already in key order.
        context_spaces: jc.context_spaces.values().cloned().collect(),
        procedures,
        connectors: jc.connectors.values().cloned().collect(),
        knowledge_stores: jc.knowledge_stores.values().cloned().collect(),
    };
    let violations = validate(&bundle);
    if violations.is_empty() {
        Ok(bundle)
    } else {
        Err(BuildError::Invalid(violations))
    }
}
