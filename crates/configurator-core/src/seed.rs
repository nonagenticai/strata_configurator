//! Versioned seed files (spec section 4.6): the TOML format, `parse`, its
//! inverse `render`, and the reproducible overlay rows a seed version yields.
//!
//! A seed file may hold any of the top-level sections `[[axis]]`, `[[dimension]]`,
//! `[[profile]]`, `[[rule]]` (with `[[rule.row]]`) and `[[layer]]` (with
//! `[[layer.row]]`). `render` writes axes to `axes.toml`, dimensions to
//! `dimensions.toml`, and every profile, rule and layer back to the file it was
//! parsed from (`source`).

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::axis::{Axis, AxisValue, Cardinality, Condition, Profile, validate_axis};
use crate::compile::{AxisRule, Known};
use crate::layer::{LayerKey, Scope};
use crate::overlay::{Kind, Op, OverlayRow, validate_fields, validate_key};

pub const AXES_FILE: &str = "axes.toml";
pub const DIMENSIONS_FILE: &str = "dimensions.toml";

/// A seed problem, naming the file and the key (or name) it is about.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{file}: {key}: {message}")]
pub struct SeedError {
    pub file: String,
    pub key: String,
    pub message: String,
}

impl SeedError {
    fn new(file: &str, key: &str, message: impl Into<String>) -> Self {
        Self {
            file: file.to_owned(),
            key: key.to_owned(),
            message: message.into(),
        }
    }
}

/// One overlay row as written in a seed file (its layer is the enclosing section).
#[derive(Debug, Clone, PartialEq)]
pub struct RowSpec {
    pub kind: Kind,
    pub key: String,
    pub op: Op,
    pub fields: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dimension {
    pub key: String,
    pub label: String,
    /// `(value key, value label)`
    pub values: Vec<(String, String)>,
}

/// An axis rule and the rows of the profile-scope layer it owns.
#[derive(Debug, Clone, PartialEq)]
pub struct SeedRule {
    /// File the rule was parsed from.
    pub source: String,
    pub rule: AxisRule,
    pub rows: Vec<RowSpec>,
}

/// A profile template, e.g. `electricity`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedProfile {
    /// File the profile was parsed from.
    pub source: String,
    pub name: String,
    pub values: Profile,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeedLayer {
    /// File the layer was parsed from.
    pub source: String,
    pub layer: LayerKey,
    pub rows: Vec<RowSpec>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeedBundle {
    pub version: u32,
    pub axes: Vec<Axis>,
    pub dimensions: Vec<Dimension>,
    pub rules: Vec<SeedRule>,
    pub profiles: Vec<SeedProfile>,
    pub layers: Vec<SeedLayer>,
}

impl SeedBundle {
    /// What the bundle defines, for selection validation without a DB:
    /// industries of industry layers, `department` dimension values, base-layer
    /// jobs, all dimension values and the axes.
    pub fn known(&self) -> Known {
        let industries = self
            .layers
            .iter()
            .filter(|l| matches!(l.layer.scope, Scope::Industry | Scope::IndustryDept))
            .filter_map(|l| l.layer.industry.clone())
            .collect();
        let departments = self
            .dimensions
            .iter()
            .filter(|d| d.key == "department")
            .flat_map(|d| d.values.iter().map(|(k, _)| k.clone()))
            .collect();
        let jobs = self
            .layers
            .iter()
            .filter(|l| l.layer.scope == Scope::Base)
            .flat_map(|l| l.rows.iter())
            .filter(|r| r.kind == Kind::Job && r.op == Op::Add)
            .map(|r| r.key.clone())
            .collect();
        let dimensions = self
            .dimensions
            .iter()
            .map(|d| {
                (
                    d.key.clone(),
                    d.values.iter().map(|(k, _)| k.clone()).collect(),
                )
            })
            .collect();
        Known {
            industries,
            departments,
            jobs,
            dimensions,
            axes: self.axes.clone(),
        }
    }

    /// The profile template called `name`.
    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles
            .iter()
            .find(|p| p.name == name)
            .map(|p| &p.values)
    }
}

// --- names ------------------------------------------------------------------

fn scope_name(s: Scope) -> &'static str {
    match s {
        Scope::Base => "base",
        Scope::Profile => "profile",
        Scope::Industry => "industry",
        Scope::IndustryDept => "industry_dept",
        Scope::Job => "job",
        Scope::Tenant => "tenant",
    }
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Entity => "entity",
        Kind::Attribute => "attribute",
        Kind::Relation => "relation",
        Kind::Job => "job",
        Kind::ContextSpace => "context_space",
        Kind::Procedure => "procedure",
        Kind::ConnectorSlot => "connector_slot",
        Kind::KnowledgeStore => "knowledge_store",
    }
}

fn op_name(op: Op) -> &'static str {
    match op {
        Op::Add => "add",
        Op::Patch => "patch",
        Op::Tombstone => "tombstone",
    }
}

fn cardinality_name(c: Cardinality) -> &'static str {
    match c {
        Cardinality::One => "one",
        Cardinality::Many => "many",
    }
}

/// A stable text form of a layer key, e.g. `industry_dept:retail:logistics`,
/// `profile:metered_supply`, `job:customer_care_agent:channel=voice`.
pub fn layer_path(l: &LayerKey) -> String {
    let mut parts = vec![scope_name(l.scope).to_owned()];
    for v in [&l.industry, &l.department, &l.job, &l.rule]
        .into_iter()
        .flatten()
    {
        parts.push(v.clone());
    }
    if let Some((k, v)) = &l.dimension {
        parts.push(format!("{k}={v}"));
    }
    if let Some(t) = l.tenant {
        parts.push(t.to_string());
    }
    parts.join(":")
}

/// The reproducible id of a seed row: uuid v5 of `v{version}/{layer}/{kind}/{key}`.
pub fn row_id(version: u32, layer: &LayerKey, kind: Kind, key: &str) -> Uuid {
    let name = format!("v{version}/{}/{}/{key}", layer_path(layer), kind_name(kind));
    Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes())
}

fn empty_layer(scope: Scope) -> LayerKey {
    LayerKey {
        scope,
        industry: None,
        department: None,
        job: None,
        rule: None,
        dimension: None,
        tenant: None,
    }
}

/// The profile-scope layer an axis rule owns.
pub fn rule_layer(rule_key: &str) -> LayerKey {
    LayerKey {
        rule: Some(rule_key.to_owned()),
        ..empty_layer(Scope::Profile)
    }
}

// --- raw (file) shapes --------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    axis: Vec<Value>,
    #[serde(default)]
    dimension: Vec<Value>,
    #[serde(default)]
    profile: Vec<Value>,
    #[serde(default)]
    rule: Vec<Value>,
    #[serde(default)]
    layer: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAxisValue {
    key: String,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAxis {
    key: String,
    cardinality: Cardinality,
    values: Vec<RawAxisValue>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLabelled {
    key: String,
    label: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDimension {
    key: String,
    label: String,
    values: Vec<RawLabelled>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    name: String,
    values: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    key: String,
    priority: i32,
    condition: Condition,
    #[serde(default)]
    row: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLayer {
    scope: Scope,
    #[serde(default)]
    industry: Option<String>,
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    job: Option<String>,
    #[serde(default)]
    dimension: Option<(String, String)>,
    #[serde(default)]
    row: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRow {
    kind: Kind,
    key: String,
    op: Op,
    #[serde(default)]
    fields: Map<String, Value>,
}

/// The string at `field` of a raw item, for naming it in an error.
fn name_of(v: &Value, field: &str) -> String {
    v.get(field)
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_owned()
}

/// A label or description, when given, must not be blank.
fn check_text(
    file: &str,
    key: &str,
    label: &Option<String>,
    description: &Option<String>,
) -> Result<(), SeedError> {
    for (name, text) in [("label", label), ("description", description)] {
        if text.as_deref().is_some_and(|t| t.trim().is_empty()) {
            return Err(SeedError::new(
                file,
                key,
                format!("{name} must not be blank"),
            ));
        }
    }
    Ok(())
}

fn typed<T: DeserializeOwned>(file: &str, key: &str, v: Value) -> Result<T, SeedError> {
    serde_json::from_value(v).map_err(|e| SeedError::new(file, key, e.to_string()))
}

fn parse_rows(file: &str, layer: &str, raw: Vec<Value>) -> Result<Vec<RowSpec>, SeedError> {
    let mut rows = Vec::with_capacity(raw.len());
    for v in raw {
        let ctx = format!("{layer}/{}", name_of(&v, "key"));
        let r: RawRow = typed(file, &ctx, v)?;
        validate_key(r.kind, &r.key).map_err(|e| SeedError::new(file, &ctx, e.to_string()))?;
        validate_fields(r.kind, r.op, &r.fields).map_err(|es| {
            let msg = es
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            SeedError::new(file, &ctx, msg)
        })?;
        rows.push(RowSpec {
            kind: r.kind,
            key: r.key,
            op: r.op,
            fields: r.fields,
        });
    }
    Ok(rows)
}

// --- parse --------------------------------------------------------------------

/// Parse the files of seed version `version`. Every key and field (spec 4.2),
/// axis, profile and rule condition (spec 4.3) is validated; the first problem
/// is returned, naming its file and key.
pub fn parse(version: u32, files: &[(&str, &str)]) -> Result<SeedBundle, SeedError> {
    let mut b = SeedBundle {
        version,
        axes: Vec::new(),
        dimensions: Vec::new(),
        rules: Vec::new(),
        profiles: Vec::new(),
        layers: Vec::new(),
    };
    let mut names = BTreeSet::new();
    for &(file, text) in files {
        if !names.insert(file) {
            return Err(SeedError::new(file, "-", "file given twice"));
        }
        let table: toml::Table =
            toml::from_str(text).map_err(|e| SeedError::new(file, "-", e.to_string()))?;
        let doc =
            serde_json::to_value(&table).map_err(|e| SeedError::new(file, "-", e.to_string()))?;
        let raw: RawFile = typed(file, "-", doc)?;

        for v in raw.axis {
            let key = name_of(&v, "key");
            let a: RawAxis = typed(file, &key, v)?;
            if b.axes.iter().any(|x| x.key == a.key) {
                return Err(SeedError::new(file, &key, "duplicate axis"));
            }
            let axis = Axis {
                key: a.key,
                cardinality: a.cardinality,
                values: a
                    .values
                    .into_iter()
                    .map(|v| AxisValue {
                        key: v.key,
                        parent: v.parent,
                        label: v.label,
                        description: v.description,
                    })
                    .collect(),
                label: a.label,
                description: a.description,
            };
            check_text(file, &key, &axis.label, &axis.description)?;
            for v in &axis.values {
                check_text(file, &format!("{key}={}", v.key), &v.label, &v.description)?;
            }
            validate_axis(&axis).map_err(|e| SeedError::new(file, &key, e.to_string()))?;
            b.axes.push(axis);
        }

        for v in raw.dimension {
            let key = name_of(&v, "key");
            let d: RawDimension = typed(file, &key, v)?;
            if d.key.is_empty() || b.dimensions.iter().any(|x| x.key == d.key) {
                return Err(SeedError::new(file, &key, "empty or duplicate dimension"));
            }
            let mut seen = BTreeSet::new();
            for value in &d.values {
                if value.key.is_empty() || !seen.insert(value.key.as_str()) {
                    return Err(SeedError::new(
                        file,
                        &format!("{key}={}", value.key),
                        "empty or duplicate dimension value",
                    ));
                }
            }
            b.dimensions.push(Dimension {
                key: d.key,
                label: d.label,
                values: d.values.into_iter().map(|v| (v.key, v.label)).collect(),
            });
        }

        for v in raw.profile {
            let name = name_of(&v, "name");
            let p: RawProfile = typed(file, &name, v)?;
            if p.name.is_empty() || b.profiles.iter().any(|x| x.name == p.name) {
                return Err(SeedError::new(file, &name, "empty or duplicate profile"));
            }
            b.profiles.push(SeedProfile {
                source: file.to_owned(),
                name: p.name,
                values: Profile(
                    p.values
                        .into_iter()
                        .map(|(axis, vs)| (axis, vs.into_iter().collect()))
                        .collect(),
                ),
            });
        }

        for v in raw.rule {
            let key = name_of(&v, "key");
            let r: RawRule = typed(file, &key, v)?;
            if b.rules.iter().any(|x| x.rule.key == r.key) {
                return Err(SeedError::new(file, &key, "duplicate rule"));
            }
            let layer = rule_layer(&r.key);
            layer
                .validate()
                .map_err(|e| SeedError::new(file, &key, e.to_string()))?;
            let rows = parse_rows(file, &layer_path(&layer), r.row)?;
            b.rules.push(SeedRule {
                source: file.to_owned(),
                rule: AxisRule {
                    key: r.key,
                    priority: r.priority,
                    condition: r.condition,
                },
                rows,
            });
        }

        for v in raw.layer {
            let ctx = ["scope", "industry", "department", "job"]
                .iter()
                .filter_map(|f| v.get(*f).and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(":");
            let l: RawLayer = typed(file, &format!("layer {ctx}"), v)?;
            let layer = LayerKey {
                scope: l.scope,
                industry: l.industry,
                department: l.department,
                job: l.job,
                rule: None,
                dimension: l.dimension,
                tenant: None,
            };
            let path = layer_path(&layer);
            if matches!(layer.scope, Scope::Profile | Scope::Tenant) {
                return Err(SeedError::new(
                    file,
                    &path,
                    "seed layers may not be profile (use [[rule]]) or tenant scoped",
                ));
            }
            layer
                .validate()
                .map_err(|e| SeedError::new(file, &path, e.to_string()))?;
            let rows = parse_rows(file, &path, l.row)?;
            b.layers.push(SeedLayer {
                source: file.to_owned(),
                layer,
                rows,
            });
        }
    }
    check_bundle(&b)?;
    Ok(b)
}

/// Cross-file checks, once every file is read.
fn check_bundle(b: &SeedBundle) -> Result<(), SeedError> {
    for p in &b.profiles {
        p.values
            .validate(&b.axes)
            .map_err(|e| SeedError::new(&p.source, &p.name, e.to_string()))?;
    }
    for r in &b.rules {
        r.rule
            .condition
            .validate(&b.axes)
            .map_err(|e| SeedError::new(&r.source, &r.rule.key, e.to_string()))?;
    }

    let known = b.known();
    for l in &b.layers {
        if let Some(dept) = l
            .layer
            .department
            .as_ref()
            .filter(|d| !known.departments.contains(*d))
        {
            return Err(SeedError::new(
                &l.source,
                &layer_path(&l.layer),
                format!("department {dept:?} is not a value of the department dimension"),
            ));
        }
        if let Some((dim, value)) = &l.layer.dimension {
            let ok = known
                .dimensions
                .get(dim)
                .is_some_and(|values| values.contains(value));
            if !ok {
                return Err(SeedError::new(
                    &l.source,
                    &layer_path(&l.layer),
                    format!("unknown dimension value {dim}={value}"),
                ));
            }
        }
    }

    // One row per (layer, kind, key) across the bundle: row ids must be unique.
    let mut seen = BTreeSet::new();
    let owned_layers = b
        .rules
        .iter()
        .map(|r| (r.source.as_str(), rule_layer(&r.rule.key), &r.rows));
    let plain_layers = b
        .layers
        .iter()
        .map(|l| (l.source.as_str(), l.layer.clone(), &l.rows));
    for (source, layer, rows) in owned_layers.chain(plain_layers) {
        let path = layer_path(&layer);
        for row in rows {
            if !seen.insert((path.clone(), kind_name(row.kind), row.key.clone())) {
                return Err(SeedError::new(
                    source,
                    &format!("{path}/{}", row.key),
                    format!(
                        "more than one {} row for this key in one layer",
                        kind_name(row.kind)
                    ),
                ));
            }
        }
    }
    Ok(())
}

// --- render -------------------------------------------------------------------

type Sections = BTreeMap<&'static str, Vec<Value>>;

fn push(files: &mut Vec<(String, Sections)>, file: &str, section: &'static str, item: Value) {
    let pos = match files.iter().position(|(name, _)| name == file) {
        Some(pos) => pos,
        None => {
            files.push((file.to_owned(), Sections::new()));
            files.len() - 1
        }
    };
    files[pos].1.entry(section).or_default().push(item);
}

fn row_json(r: &RowSpec) -> Value {
    let mut m = Map::new();
    m.insert("kind".into(), json!(kind_name(r.kind)));
    m.insert("key".into(), json!(r.key));
    m.insert("op".into(), json!(op_name(r.op)));
    if !r.fields.is_empty() {
        m.insert("fields".into(), Value::Object(r.fields.clone()));
    }
    Value::Object(m)
}

fn with_rows(mut m: Map<String, Value>, rows: &[RowSpec]) -> Value {
    if !rows.is_empty() {
        m.insert("row".into(), rows.iter().map(row_json).collect());
    }
    Value::Object(m)
}

/// Insert `name` when `value` is set.
fn put(m: &mut Map<String, Value>, name: &str, value: &Option<String>) {
    if let Some(v) = value {
        m.insert(name.into(), json!(v));
    }
}

fn axis_json(a: &Axis) -> Value {
    let values: Vec<Value> = a
        .values
        .iter()
        .map(|v| {
            let mut m = Map::new();
            m.insert("key".into(), json!(v.key));
            put(&mut m, "parent", &v.parent);
            put(&mut m, "label", &v.label);
            put(&mut m, "description", &v.description);
            Value::Object(m)
        })
        .collect();
    let mut m = Map::new();
    m.insert("key".into(), json!(a.key));
    m.insert("cardinality".into(), json!(cardinality_name(a.cardinality)));
    put(&mut m, "label", &a.label);
    put(&mut m, "description", &a.description);
    m.insert("values".into(), Value::Array(values));
    Value::Object(m)
}

fn dimension_json(d: &Dimension) -> Value {
    let values: Vec<Value> = d
        .values
        .iter()
        .map(|(k, l)| json!({"key": k, "label": l}))
        .collect();
    json!({"key": d.key, "label": d.label, "values": values})
}

fn profile_json(p: &SeedProfile) -> Value {
    let values: Map<String, Value> = p
        .values
        .0
        .iter()
        .map(|(axis, vs)| (axis.clone(), vs.iter().cloned().collect()))
        .collect();
    json!({"name": p.name, "values": values})
}

fn rule_json(r: &SeedRule) -> Value {
    let mut m = Map::new();
    m.insert("key".into(), json!(r.rule.key));
    m.insert("priority".into(), json!(r.rule.priority));
    m.insert(
        "condition".into(),
        serde_json::to_value(&r.rule.condition).expect("a condition serializes to JSON"),
    );
    with_rows(m, &r.rows)
}

fn layer_json(l: &SeedLayer) -> Value {
    let mut m = Map::new();
    m.insert("scope".into(), json!(scope_name(l.layer.scope)));
    for (name, value) in [
        ("industry", &l.layer.industry),
        ("department", &l.layer.department),
        ("job", &l.layer.job),
    ] {
        if let Some(v) = value {
            m.insert(name.into(), json!(v));
        }
    }
    if let Some((k, v)) = &l.layer.dimension {
        m.insert("dimension".into(), json!([k, v]));
    }
    with_rows(m, &l.rows)
}

/// The inverse of `parse`: `(file name, toml)` per file. Axes go to `axes.toml`,
/// dimensions to `dimensions.toml`, profiles/rules/layers to their `source`.
pub fn render(b: &SeedBundle) -> Vec<(String, String)> {
    let mut files: Vec<(String, Sections)> = Vec::new();
    for a in &b.axes {
        push(&mut files, AXES_FILE, "axis", axis_json(a));
    }
    for d in &b.dimensions {
        push(&mut files, DIMENSIONS_FILE, "dimension", dimension_json(d));
    }
    for p in &b.profiles {
        push(&mut files, &p.source, "profile", profile_json(p));
    }
    for r in &b.rules {
        push(&mut files, &r.source, "rule", rule_json(r));
    }
    for l in &b.layers {
        push(&mut files, &l.source, "layer", layer_json(l));
    }
    files
        .into_iter()
        .map(|(name, sections)| {
            let doc: Map<String, Value> = sections
                .into_iter()
                .map(|(section, items)| (section.to_owned(), Value::Array(items)))
                .collect();
            let text = toml::to_string(&Value::Object(doc)).expect("a seed bundle renders as TOML");
            (name, text)
        })
        .collect()
}

// --- rows -----------------------------------------------------------------------

fn append_rows(version: u32, layer: &LayerKey, specs: &[RowSpec], out: &mut Vec<OverlayRow>) {
    for s in specs {
        out.push(OverlayRow {
            id: row_id(version, layer, s.kind, &s.key),
            layer: layer.clone(),
            kind: s.kind,
            key: s.key.clone(),
            op: s.op,
            fields: s.fields.clone(),
        });
    }
}

/// The axis rules and overlay rows of a bundle. Row ids are uuid v5 of
/// `v{version}/{layer}/{kind}/{key}`, so applying a seed version is reproducible.
pub fn to_rows(b: &SeedBundle) -> (Vec<AxisRule>, Vec<OverlayRow>) {
    let mut rows = Vec::new();
    for r in &b.rules {
        append_rows(b.version, &rule_layer(&r.rule.key), &r.rows, &mut rows);
    }
    for l in &b.layers {
        append_rows(b.version, &l.layer, &l.rows, &mut rows);
    }
    let rules = b.rules.iter().map(|r| r.rule.clone()).collect();
    (rules, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AXES: &str = r#"
[[axis]]
key = "order_type"
cardinality = "one"
values = [{ key = "discrete" }, { key = "continuous" }]
"#;

    const BASE: &str = r#"
[[layer]]
scope = "base"
  [[layer.row]]
  kind = "entity"
  key = "Order"
  op = "add"
  fields = { label = "Order", synonyms = ["purchase"] }
"#;

    #[test]
    fn seed_error_names_file_and_key() {
        let bad = r#"
[[layer]]
scope = "base"
  [[layer.row]]
  kind = "entity"
  key = "order"
  op = "add"
  fields = { label = "Order" }
"#;
        let err = parse(1, &[("base.toml", bad)]).err();
        let err = err.as_ref().map(|e| (e.file.as_str(), e.key.as_str()));
        assert_eq!(err, Some(("base.toml", "base/order")));

        let rule = r#"
[[rule]]
key = "r"
priority = 1
condition = { eq = ["order_type", "weekly"] }
"#;
        let err = parse(1, &[("axes.toml", AXES), ("rules.toml", rule)]).err();
        let err = err.as_ref().map(|e| (e.file.as_str(), e.key.as_str()));
        assert_eq!(err, Some(("rules.toml", "r")));
    }

    #[test]
    fn seed_rejects_duplicate_row_and_tenant_layer() {
        let err = parse(1, &[("a.toml", BASE), ("b.toml", BASE)]).err();
        assert_eq!(err.map(|e| e.file), Some("b.toml".to_owned()));
        let tenant = "[[layer]]\nscope = \"tenant\"\n";
        assert!(parse(1, &[("t.toml", tenant)]).is_err());
        let unknown = "[[layer]]\nscope = \"base\"\ncolour = \"red\"\n";
        assert!(parse(1, &[("u.toml", unknown)]).is_err());
    }

    #[test]
    fn seed_axis_labels_round_trip() {
        let labelled = r#"
[[axis]]
key = "payment_method"
label = "Payment method"
description = "How a customer pays."
cardinality = "many"
values = [
  { key = "digital_wallet", label = "Digital wallet" },
  { key = "digital_wallet.mobile", parent = "digital_wallet", label = "Mobile wallet", description = "Google Pay, Apple Pay" },
  { key = "cash" },
]
"#;
        let a = parse(1, &[("axes.toml", labelled)]).expect("parses");
        let axis = &a.axes[0];
        assert_eq!(axis.label.as_deref(), Some("Payment method"));
        assert_eq!(
            axis.values[1].description.as_deref(),
            Some("Google Pay, Apple Pay")
        );
        assert_eq!(axis.values[2].label, None);
        let files = render(&a);
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(n, t)| (n.as_str(), t.as_str()))
            .collect();
        assert_eq!(parse(1, &refs).expect("rendered parses"), a);

        let blank = labelled.replace("\"Mobile wallet\"", "\"  \"");
        let err = parse(1, &[("axes.toml", blank.as_str())]).err();
        assert_eq!(
            err.map(|e| e.key),
            Some("payment_method=digital_wallet.mobile".to_owned())
        );
    }

    #[test]
    fn seed_row_ids_are_reproducible() {
        let a = parse(1, &[("base.toml", BASE)]).expect("parses");
        let (_, rows) = to_rows(&a);
        let (_, again) = to_rows(&a);
        assert_eq!(rows, again);
        let base = empty_layer(Scope::Base);
        assert_eq!(rows[0].id, row_id(1, &base, Kind::Entity, "Order"));
        assert_ne!(rows[0].id, row_id(2, &base, Kind::Entity, "Order"));
        assert_eq!(
            layer_path(&rule_layer("metered_supply")),
            "profile:metered_supply"
        );
    }
}
