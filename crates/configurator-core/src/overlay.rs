//! Overlay rows, key shapes and per-kind field schemas (spec section 4.2).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::layer::LayerKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Entity,
    Attribute,
    Relation,
    Job,
    ContextSpace,
    Procedure,
    ConnectorSlot,
    KnowledgeStore,
}

impl Kind {
    /// The four job-content kinds (context spaces, procedures, connector slots,
    /// knowledge stores): allowed only in job and tenant layers.
    pub fn is_job_content(self) -> bool {
        matches!(
            self,
            Kind::ContextSpace | Kind::Procedure | Kind::ConnectorSlot | Kind::KnowledgeStore
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Add,
    Patch,
    Tombstone,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayRow {
    pub id: Uuid,
    pub layer: LayerKey,
    pub kind: Kind,
    pub key: String,
    pub op: Op,
    pub fields: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct FieldError {
    pub code: &'static str,
    pub field: Option<String>,
    pub message: String,
}

impl FieldError {
    fn new(code: &'static str, field: Option<&str>, message: impl Into<String>) -> Self {
        Self {
            code,
            field: field.map(str::to_owned),
            message: message.into(),
        }
    }
}

// --- key shapes (std only, no regex) -------------------------------------

/// `^[A-Z][A-Za-z0-9]{0,63}$`
fn is_entity_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('A'..='Z'))
        && s.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric())
}

/// `^[a-z][a-z0-9_]{0,63}$`
fn is_snake_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('a'..='z'))
        && s.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `^[a-z][A-Za-z0-9]{0,63}$`
fn is_predicate(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('a'..='z'))
        && s.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric())
}

/// `^[a-z][a-z0-9-]{0,62}$`
pub(crate) fn is_kebab(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('a'..='z'))
        && s.len() <= 63
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `^[a-z][a-z0-9_]{0,62}$` (a procedure name, a request type)
pub(crate) fn is_snake63(s: &str) -> bool {
    is_snake_name(s) && s.len() <= 63
}

/// `^[a-z0-9-]+(:[a-z0-9-]+){1,3}$`
pub(crate) fn is_space_key(s: &str) -> bool {
    let segments: Vec<&str> = s.split(':').collect();
    (2..=4).contains(&segments.len())
        && segments.iter().all(|g| {
            !g.is_empty()
                && g.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// `^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$`, each numeric part at most 9 digits (as care-bus).
pub(crate) fn is_version(s: &str) -> bool {
    let (core, pre) = match s.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (s, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 9 && p.chars().all(|c| c.is_ascii_digit()))
        && pre.is_none_or(|p| {
            !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
        })
}

fn key_matches(kind: Kind, key: &str) -> bool {
    match kind {
        Kind::Entity => is_entity_name(key),
        Kind::Attribute => key
            .split_once('.')
            .is_some_and(|(entity, name)| is_entity_name(entity) && is_snake_name(name)),
        Kind::Relation => {
            let parts: Vec<&str> = key.split('|').collect();
            matches!(parts.as_slice(), [s, p, o]
                if is_entity_name(s) && is_predicate(p) && is_entity_name(o))
        }
        Kind::Job => is_snake_name(key),
        Kind::ContextSpace => is_space_key(key),
        Kind::Procedure => key
            .split_once('@')
            .is_some_and(|(name, version)| is_snake63(name) && is_version(version)),
        Kind::ConnectorSlot | Kind::KnowledgeStore => is_kebab(key),
    }
}

/// Check that `key` has the shape required for `kind`.
pub fn validate_key(kind: Kind, key: &str) -> Result<(), FieldError> {
    if key_matches(kind, key) {
        return Ok(());
    }
    let shape = match kind {
        Kind::Entity => "Entity (e.g. Order)",
        Kind::Attribute => "Entity.attribute_name (e.g. Order.order_id)",
        Kind::Relation => "Entity|predicate|Entity (e.g. Order|paidBy|Payment)",
        Kind::Job => "snake_case name (e.g. customer_care_agent)",
        Kind::ContextSpace => "industry:area[:space] (e.g. retail:order-support:returns)",
        Kind::Procedure => "name@version (e.g. return@1.3.0)",
        Kind::ConnectorSlot => "kebab-case id (e.g. crm-core)",
        Kind::KnowledgeStore => "kebab-case key (e.g. faq-policies)",
    };
    Err(FieldError::new(
        "invalid_key",
        Some("key"),
        format!("invalid {kind:?} key {key:?}, expected {shape}"),
    ))
}

// --- field schemas ------------------------------------------------------

#[derive(Clone, Copy)]
enum Ty {
    Str,
    StrList,
    EntityList,
    Bool,
    Object,
    OneOf(&'static [&'static str]),
}

struct Spec {
    name: &'static str,
    ty: Ty,
    required_on_add: bool,
}

const fn spec(name: &'static str, ty: Ty, required_on_add: bool) -> Spec {
    Spec {
        name,
        ty,
        required_on_add,
    }
}

const DATA_TYPES: &[&str] = &[
    "string", "text", "enum", "money", "number", "boolean", "date", "datetime",
];
const CARDINALITIES: &[&str] = &["1:1", "1:n", "n:1", "n:m"];

const ENTITY_FIELDS: &[Spec] = &[
    spec("label", Ty::Str, true),
    spec("synonyms", Ty::StrList, false),
    spec("hidden", Ty::Bool, false),
];
const ATTRIBUTE_FIELDS: &[Spec] = &[
    spec("data_type", Ty::OneOf(DATA_TYPES), true),
    spec("pii", Ty::Bool, false),
    spec("synonyms", Ty::StrList, false),
];
const RELATION_FIELDS: &[Spec] = &[spec("cardinality", Ty::OneOf(CARDINALITIES), true)];
const JOB_FIELDS: &[Spec] = &[
    spec("label", Ty::Str, true),
    spec("actor", Ty::Str, true),
    spec("stance", Ty::Str, false),
    spec("preamble", Ty::Str, false),
    spec("actor_label", Ty::Str, false),
    spec("ontology_slice", Ty::EntityList, false),
    spec("channels", Ty::StrList, false),
    spec("escalation", Ty::Object, false),
    spec("use_cases", Ty::StrList, false),
    spec("sops", Ty::StrList, false),
    spec("tools", Ty::StrList, false),
    spec("stores", Ty::StrList, false),
];

fn specs(kind: Kind) -> &'static [Spec] {
    match kind {
        Kind::Entity => ENTITY_FIELDS,
        Kind::Attribute => ATTRIBUTE_FIELDS,
        Kind::Relation => RELATION_FIELDS,
        Kind::Job => JOB_FIELDS,
        // Job-content kinds are checked against their bundle v1 struct instead.
        Kind::ContextSpace | Kind::Procedure | Kind::ConnectorSlot | Kind::KnowledgeStore => &[],
    }
}

/// The fields of a job-content row: (the fields its key supplies, the others).
fn content_fields(kind: Kind) -> Option<(&'static [&'static str], &'static [&'static str])> {
    match kind {
        Kind::ContextSpace => Some((
            &["key"],
            &[
                "area",
                "label",
                "request_type",
                "required",
                "entities",
                "procedure",
            ],
        )),
        Kind::Procedure => Some((
            &["name", "version"],
            &[
                "status",
                "context_space",
                "domain",
                "description",
                "pddl_domain",
                "tool_requirements",
                "success_criteria",
                "reviewers",
                "plan",
                "steps",
            ],
        )),
        Kind::ConnectorSlot => Some((
            &["id"],
            &[
                "name",
                "system",
                "auth_type",
                "base_url",
                "timeout_ms",
                "rate_limit",
                "field_mappings",
            ],
        )),
        Kind::KnowledgeStore => Some((
            &["key"],
            &["label", "parent", "kind", "areas", "description"],
        )),
        Kind::Entity | Kind::Attribute | Kind::Relation | Kind::Job => None,
    }
}

/// A key of the right shape for `kind`, for checking an `add` row's fields
/// without its key (`validate_fields` is key-agnostic).
fn placeholder_key(kind: Kind) -> &'static str {
    match kind {
        Kind::ContextSpace => "x:y",
        Kind::Procedure => "x@0.0.0",
        _ => "x",
    }
}

fn contains_null(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Array(items) => items.iter().any(contains_null),
        Value::Object(map) => map.values().any(contains_null),
        _ => false,
    }
}

/// Job-content fields: known names only, no `null` anywhere (an unset nullable
/// is omitted, so the row renders as TOML), and on `add` the fields plus the key
/// must parse as the bundle v1 struct.
fn check_content_fields(
    kind: Kind,
    op: Op,
    fields: &Map<String, Value>,
    (from_key, allowed): (&[&str], &[&str]),
    errors: &mut Vec<FieldError>,
) {
    for (name, value) in fields {
        if from_key.contains(&name.as_str()) {
            errors.push(FieldError::new(
                "unknown_field",
                Some(name.as_str()),
                format!("field {name:?} of {kind:?} comes from the row key"),
            ));
        } else if !allowed.contains(&name.as_str()) {
            errors.push(FieldError::new(
                "unknown_field",
                Some(name.as_str()),
                format!("unknown field {name:?} for {kind:?}"),
            ));
        } else if contains_null(value) {
            errors.push(FieldError::new(
                "invalid_value",
                Some(name.as_str()),
                "null is not allowed: omit an unset field",
            ));
        }
    }
    if op == Op::Add
        && errors.is_empty()
        && let Err(message) = crate::release::content_item(kind, placeholder_key(kind), fields)
    {
        errors.push(FieldError::new("invalid_value", None, message));
    }
}

fn str_list(v: &Value) -> Option<Vec<&str>> {
    v.as_array()?.iter().map(Value::as_str).collect()
}

/// `None` when the value fits the type, otherwise a message.
fn check_value(ty: Ty, v: &Value) -> Option<String> {
    match ty {
        Ty::Str => match v.as_str() {
            Some(s) if !s.trim().is_empty() => None,
            _ => Some("must be a non-empty string".into()),
        },
        Ty::StrList => match str_list(v) {
            Some(_) => None,
            None => Some("must be an array of strings".into()),
        },
        Ty::EntityList => match str_list(v) {
            Some(items) => items
                .iter()
                .find(|s| !is_entity_name(s))
                .map(|s| format!("{s:?} is not a valid entity name")),
            None => Some("must be an array of entity names".into()),
        },
        Ty::Bool => (!v.is_boolean()).then(|| "must be a boolean".to_owned()),
        Ty::Object => (!v.is_object()).then(|| "must be an object".to_owned()),
        Ty::OneOf(allowed) => match v.as_str() {
            Some(s) if allowed.contains(&s) => None,
            _ => Some(format!("must be one of {}", allowed.join(" | "))),
        },
    }
}

/// Validate the `fields` of an overlay row for `kind` and `op`.
pub fn validate_fields(
    kind: Kind,
    op: Op,
    fields: &Map<String, Value>,
) -> Result<(), Vec<FieldError>> {
    let mut errors = Vec::new();
    match op {
        Op::Tombstone => {
            if !fields.is_empty() {
                errors.push(FieldError::new(
                    "fields_on_tombstone",
                    None,
                    "a tombstone must not carry fields",
                ));
            }
        }
        Op::Add | Op::Patch => {
            if op == Op::Patch && fields.is_empty() {
                errors.push(FieldError::new(
                    "empty_patch",
                    None,
                    "a patch must name at least one field",
                ));
            }
            if let Some(names) = content_fields(kind) {
                check_content_fields(kind, op, fields, names, &mut errors);
                return if errors.is_empty() {
                    Ok(())
                } else {
                    Err(errors)
                };
            }
            let known = specs(kind);
            for (name, value) in fields {
                match known.iter().find(|s| s.name == name.as_str()) {
                    None => errors.push(FieldError::new(
                        "unknown_field",
                        Some(name.as_str()),
                        format!("unknown field {name:?} for {kind:?}"),
                    )),
                    Some(s) => {
                        if let Some(msg) = check_value(s.ty, value) {
                            errors.push(FieldError::new("invalid_value", Some(name.as_str()), msg));
                        }
                    }
                }
            }
            if op == Op::Add {
                for s in known.iter().filter(|s| s.required_on_add) {
                    if !fields.contains_key(s.name) {
                        errors.push(FieldError::new(
                            "missing_field",
                            Some(s.name),
                            format!("field {:?} is required on add", s.name),
                        ));
                    }
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    fn codes(r: Result<(), Vec<FieldError>>) -> Vec<&'static str> {
        r.err().unwrap_or_default().iter().map(|e| e.code).collect()
    }

    #[test]
    fn key_shapes() {
        let ok = [
            (Kind::Entity, "Order"),
            (Kind::Attribute, "Order.order_id"),
            (Kind::Relation, "Order|paidBy|Payment"),
            (Kind::Job, "customer_care_agent"),
        ];
        for (kind, key) in ok {
            assert_eq!(validate_key(kind, key), Ok(()), "{key}");
        }
        let bad = [
            (Kind::Entity, "order"),
            (Kind::Attribute, "Order.Status"),
            (Kind::Relation, "Order|paid by|Payment"),
            (Kind::Attribute, "Order."),
            (Kind::Entity, ""),
            (Kind::Job, ""),
            (Kind::Relation, "Order|paidBy"),
            (Kind::Entity, "Order.id"),
        ];
        for (kind, key) in bad {
            let err = validate_key(kind, key).err();
            assert_eq!(err.map(|e| e.code), Some("invalid_key"), "{key:?}");
        }
        let long = format!("A{}", "b".repeat(64));
        assert!(validate_key(Kind::Entity, &long).is_err());
        assert!(validate_key(Kind::Entity, &long[..64]).is_ok());
    }

    #[test]
    fn add_entity_needs_label() {
        assert_eq!(
            codes(validate_fields(Kind::Entity, Op::Add, &obj(json!({})))),
            vec!["missing_field"]
        );
        let ok = obj(json!({"label": "Order", "synonyms": ["purchase"], "hidden": false}));
        assert!(validate_fields(Kind::Entity, Op::Add, &ok).is_ok());
    }

    #[test]
    fn patch_unknown_field_rejected() {
        let f = obj(json!({"colour": "red"}));
        assert_eq!(
            codes(validate_fields(Kind::Entity, Op::Patch, &f)),
            vec!["unknown_field"]
        );
        let known = obj(json!({"hidden": true}));
        assert!(validate_fields(Kind::Entity, Op::Patch, &known).is_ok());
    }

    #[test]
    fn tombstone_with_fields_rejected() {
        let f = obj(json!({"label": "x"}));
        assert_eq!(
            codes(validate_fields(Kind::Entity, Op::Tombstone, &f)),
            vec!["fields_on_tombstone"]
        );
        assert!(validate_fields(Kind::Entity, Op::Tombstone, &obj(json!({}))).is_ok());
    }

    #[test]
    fn empty_patch_rejected() {
        assert_eq!(
            codes(validate_fields(Kind::Job, Op::Patch, &obj(json!({})))),
            vec!["empty_patch"]
        );
    }

    #[test]
    fn attribute_data_type_enum() {
        for t in [
            "string", "text", "enum", "money", "number", "boolean", "date", "datetime",
        ] {
            let f = obj(json!({ "data_type": t }));
            assert!(validate_fields(Kind::Attribute, Op::Add, &f).is_ok(), "{t}");
        }
        let bad = obj(json!({"data_type": "integer"}));
        assert_eq!(
            codes(validate_fields(Kind::Attribute, Op::Add, &bad)),
            vec!["invalid_value"]
        );
        assert_eq!(
            codes(validate_fields(Kind::Attribute, Op::Add, &obj(json!({})))),
            vec!["missing_field"]
        );
    }

    #[test]
    fn relation_and_job_fields() {
        let rel = obj(json!({"cardinality": "n:m"}));
        assert!(validate_fields(Kind::Relation, Op::Add, &rel).is_ok());
        let bad = obj(json!({"cardinality": "2:3"}));
        assert!(validate_fields(Kind::Relation, Op::Add, &bad).is_err());

        let job = obj(json!({
            "label": "Care", "actor": "agent", "ontology_slice": ["Order"],
            "escalation": {"to": "human"}, "channels": ["voice"]
        }));
        assert!(validate_fields(Kind::Job, Op::Add, &job).is_ok());
        let bad_slice = obj(json!({"ontology_slice": ["order"]}));
        assert_eq!(
            codes(validate_fields(Kind::Job, Op::Patch, &bad_slice)),
            vec!["invalid_value"]
        );
    }

    #[test]
    fn context_space_key_validation() {
        for key in ["retail:order-support:returns", "retail:faq", "a:b:c:d"] {
            assert_eq!(validate_key(Kind::ContextSpace, key), Ok(()), "{key}");
        }
        for key in [
            "Retail:x",
            "retail",
            "a:b:c:d:e",
            "a::b",
            "retail:order support",
        ] {
            assert!(validate_key(Kind::ContextSpace, key).is_err(), "{key}");
        }
        assert!(validate_key(Kind::Procedure, "return@1.3.0").is_ok());
        assert!(validate_key(Kind::Procedure, "return@1.4.0-beta").is_ok());
        for key in ["return", "return@1.3", "Return@1.0.0", "return@1.0.0-"] {
            assert!(validate_key(Kind::Procedure, key).is_err(), "{key}");
        }
        assert!(validate_key(Kind::ConnectorSlot, "crm-core").is_ok());
        assert!(validate_key(Kind::ConnectorSlot, "CRM").is_err());
        assert!(validate_key(Kind::KnowledgeStore, "faq-policies").is_ok());
        assert!(validate_key(Kind::KnowledgeStore, "faq_policies").is_err());
    }

    fn procedure_fields() -> Map<String, Value> {
        obj(json!({
            "status": "PUBLISHED",
            "context_space": "retail:order-support:returns",
            "domain": "customer_care",
            "tool_requirements": [],
            "success_criteria": [],
            "reviewers": [],
            "plan": {"phases": [{"steps": []}]},
            "steps": []
        }))
    }

    #[test]
    fn procedure_fields_must_parse_as_v1() {
        assert!(validate_fields(Kind::Procedure, Op::Add, &procedure_fields()).is_ok());
        let mut missing = procedure_fields();
        missing.remove("steps");
        assert_eq!(
            codes(validate_fields(Kind::Procedure, Op::Add, &missing)),
            vec!["invalid_value"]
        );
        // A patch may name any one field; the merged object is checked in compile.
        let patch = obj(json!({"status": "CANARY"}));
        assert!(validate_fields(Kind::Procedure, Op::Patch, &patch).is_ok());
        // Key fields come from the key; unknown fields and nulls are refused.
        let named = obj(json!({"name": "return"}));
        assert_eq!(
            codes(validate_fields(Kind::Procedure, Op::Patch, &named)),
            vec!["unknown_field"]
        );
        let secret = obj(json!({"secret_ref": "k/v"}));
        assert_eq!(
            codes(validate_fields(Kind::ConnectorSlot, Op::Patch, &secret)),
            vec!["unknown_field"]
        );
        let null = obj(json!({"description": null}));
        assert_eq!(
            codes(validate_fields(Kind::Procedure, Op::Patch, &null)),
            vec!["invalid_value"]
        );
    }

    #[test]
    fn job_accepts_preamble_and_actor_label() {
        let f = obj(
            json!({"preamble": "You are the intake step.", "actor_label": "Customer care desk"}),
        );
        assert!(validate_fields(Kind::Job, Op::Patch, &f).is_ok());
        let blank = obj(json!({"preamble": " "}));
        assert_eq!(
            codes(validate_fields(Kind::Job, Op::Patch, &blank)),
            vec!["invalid_value"]
        );
    }
}
