//! The overlay compiler (spec section 5): a pure function from a selection and
//! already-loaded overlay rows to the effective configuration. No IO.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::axis::{Axis, Cardinality, Condition, Profile};
use crate::layer::{LayerKey, Scope};
use crate::overlay::{Kind, Op, OverlayRow, validate_fields, validate_key};
use crate::release::{
    ConnectorSlotV1, ContentItem, ContextSpaceV1, KnowledgeStoreV1, ProcedureV1, content_item,
};

/// What a compile is asked for (spec 5.1).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Selection {
    pub industry: Option<String>,
    pub departments: BTreeSet<String>,
    pub job: Option<String>,
    pub profile: Profile,
    pub dimensions: BTreeMap<String, String>,
    pub tenant: Option<Uuid>,
}

/// An axis rule: owns one profile-scope layer, applied when `condition` holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxisRule {
    pub key: String,
    pub priority: i32,
    pub condition: Condition,
}

/// What exists, for selection validation (filled by the API from the DB).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Known {
    pub industries: BTreeSet<String>,
    pub departments: BTreeSet<String>,
    pub jobs: BTreeSet<String>,
    pub dimensions: BTreeMap<String, BTreeSet<String>>,
    pub axes: Vec<Axis>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidSelection,
    DuplicateAdd,
    PatchMissing,
    TombstoneMissing,
    /// `add` of a key a lower layer tombstoned (spec 4.2: hidden at and above that layer).
    AddTombstoned,
    DanglingReference,
    InvalidRow,
    /// A job-content kind (context space, procedure, connector slot, knowledge
    /// store) in a layer other than job or tenant.
    KindNotAllowedInLayer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompileError {
    pub code: ErrorCode,
    pub row_id: Option<Uuid>,
    pub kind: Option<Kind>,
    pub key: Option<String>,
    pub message: String,
}

impl CompileError {
    fn selection(key: &str, message: String) -> Self {
        Self {
            code: ErrorCode::InvalidSelection,
            row_id: None,
            kind: None,
            key: Some(key.to_owned()),
            message,
        }
    }

    fn row(code: ErrorCode, row_id: Uuid, kind: Kind, key: &str, message: String) -> Self {
        Self {
            code,
            row_id: Some(row_id),
            kind: Some(kind),
            key: Some(key.to_owned()),
            message,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub row_id: Uuid,
    pub layer: LayerKey,
    pub op: Op,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entity {
    pub label: String,
    pub synonyms: Vec<String>,
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attribute {
    pub entity: String,
    pub name: String,
    pub data_type: String,
    pub pii: bool,
    pub synonyms: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relation {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub cardinality: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub key: String,
    pub label: String,
    pub actor: String,
    pub stance: Option<String>,
    /// The prompt preamble care-bus uses (bundle `job_settings.preamble`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preamble: Option<String>,
    /// The actor as care-bus prompts name it, e.g. `Customer care desk`
    /// (bundle `job_settings.actor`); `actor` stays the role word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_label: Option<String>,
    pub ontology_slice: Vec<String>,
    pub channels: Vec<String>,
    pub escalation: Option<Value>,
    pub use_cases: Vec<String>,
    pub sops: Vec<String>,
    pub tools: Vec<String>,
    pub stores: Vec<String>,
}

/// The job's content beyond the ontology, keyed by row key (`name@version` for
/// procedures). Built from the four job-content overlay kinds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JobContent {
    pub context_spaces: BTreeMap<String, ContextSpaceV1>,
    pub procedures: BTreeMap<String, ProcedureV1>,
    pub connectors: BTreeMap<String, ConnectorSlotV1>,
    pub knowledge_stores: BTreeMap<String, KnowledgeStoreV1>,
}

impl JobContent {
    pub fn is_empty(&self) -> bool {
        self.context_spaces.is_empty()
            && self.procedures.is_empty()
            && self.connectors.is_empty()
            && self.knowledge_stores.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Warning {
    /// e.g. `slice_entity_missing`
    pub code: &'static str,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EffectiveConfig {
    pub selection: Selection,
    pub entities: BTreeMap<String, Entity>,
    pub attributes: BTreeMap<String, Attribute>,
    pub relations: BTreeMap<String, Relation>,
    pub job: Option<Job>,
    #[serde(skip_serializing_if = "JobContent::is_empty")]
    pub job_content: JobContent,
    /// `"entity:Order"` -> the rows that built the object, in apply order.
    pub provenance: BTreeMap<String, Vec<Origin>>,
    pub warnings: Vec<Warning>,
    /// sha256 of the canonical JSON of selection, entities, attributes, relations and job.
    pub hash: String,
}

// --- layer order ----------------------------------------------------------

fn layer(scope: Scope) -> LayerKey {
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

/// The ordered layer list for a selection (spec 5.1 / 5.2).
pub fn layer_order(sel: &Selection, rules: &[AxisRule]) -> Vec<LayerKey> {
    let mut out = vec![layer(Scope::Base)];

    let mut active: Vec<&AxisRule> = rules
        .iter()
        .filter(|r| r.condition.eval(&sel.profile))
        .collect();
    active.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.key.cmp(&b.key)));
    for r in active {
        out.push(LayerKey {
            rule: Some(r.key.clone()),
            ..layer(Scope::Profile)
        });
    }

    if let Some(industry) = &sel.industry {
        out.push(LayerKey {
            industry: Some(industry.clone()),
            ..layer(Scope::Industry)
        });
        for dept in &sel.departments {
            out.push(LayerKey {
                industry: Some(industry.clone()),
                department: Some(dept.clone()),
                ..layer(Scope::IndustryDept)
            });
        }
    }

    if let Some(job) = &sel.job {
        out.push(LayerKey {
            job: Some(job.clone()),
            ..layer(Scope::Job)
        });
        for (k, v) in &sel.dimensions {
            out.push(LayerKey {
                job: Some(job.clone()),
                dimension: Some((k.clone(), v.clone())),
                ..layer(Scope::Job)
            });
        }
    }

    if let Some(tenant) = sel.tenant {
        out.push(LayerKey {
            tenant: Some(tenant),
            ..layer(Scope::Tenant)
        });
    }
    out
}

// --- selection validation ---------------------------------------------------

fn validate_selection(sel: &Selection, known: &Known) -> Vec<CompileError> {
    let mut errors = Vec::new();
    if let Some(industry) = sel
        .industry
        .as_ref()
        .filter(|i| !known.industries.contains(*i))
    {
        errors.push(CompileError::selection(
            industry,
            format!("unknown industry {industry:?}"),
        ));
    }
    for dept in &sel.departments {
        if !known.departments.contains(dept) {
            errors.push(CompileError::selection(
                dept,
                format!("unknown department {dept:?}"),
            ));
        }
    }
    if !sel.departments.is_empty() && sel.industry.is_none() {
        errors.push(CompileError::selection(
            "departments",
            "departments require an industry (industry x department cells)".to_owned(),
        ));
    }
    if sel.job.is_none() {
        for dim in sel.dimensions.keys() {
            errors.push(CompileError::selection(
                dim,
                format!("dimension {dim:?} requires a job (dimensions qualify job overlays)"),
            ));
        }
    }
    if let Some(job) = sel.job.as_ref().filter(|j| !known.jobs.contains(*j)) {
        errors.push(CompileError::selection(job, format!("unknown job {job:?}")));
    }
    for (dim, value) in &sel.dimensions {
        match known.dimensions.get(dim) {
            None => errors.push(CompileError::selection(
                dim,
                format!("unknown dimension {dim:?}"),
            )),
            Some(values) if !values.contains(value) => errors.push(CompileError::selection(
                &format!("{dim}={value}"),
                format!("unknown value {value:?} for dimension {dim:?}"),
            )),
            Some(_) => {}
        }
    }
    for (axis_key, values) in &sel.profile.0 {
        let Some(axis) = known.axes.iter().find(|a| a.key == *axis_key) else {
            errors.push(CompileError::selection(
                axis_key,
                format!("unknown axis {axis_key:?}"),
            ));
            continue;
        };
        for value in values {
            if !axis.values.iter().any(|v| v.key == *value) {
                errors.push(CompileError::selection(
                    &format!("{axis_key}={value}"),
                    format!("unknown value {value:?} on axis {axis_key:?}"),
                ));
            }
        }
        if axis.cardinality == Cardinality::One && values.len() > 1 {
            errors.push(CompileError::selection(
                axis_key,
                format!(
                    "axis {axis_key:?} has cardinality one but the profile selects {} values",
                    values.len()
                ),
            ));
        }
    }
    if sel.tenant.is_some_and(|t| t.is_nil()) {
        errors.push(CompileError::selection(
            "tenant",
            "tenant must be a non-nil uuid".to_owned(),
        ));
    }
    errors
}

// --- row helpers ------------------------------------------------------------

fn kind_index(kind: Kind) -> usize {
    match kind {
        Kind::Entity => 0,
        Kind::Attribute => 1,
        Kind::Relation => 2,
        Kind::Job => 3,
        Kind::ContextSpace => 4,
        Kind::Procedure => 5,
        Kind::ConnectorSlot => 6,
        Kind::KnowledgeStore => 7,
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
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

fn provenance_key(kind: Kind, key: &str) -> String {
    format!("{}:{key}", kind_name(kind))
}

/// `Order.order_id` -> `Order`.
fn attribute_entity(key: &str) -> &str {
    key.split_once('.').map_or(key, |(entity, _)| entity)
}

/// `Order|paidBy|Payment` -> `(Order, paidBy, Payment)`.
fn relation_parts(key: &str) -> (&str, &str, &str) {
    let mut it = key.splitn(3, '|');
    let subject = it.next().unwrap_or_default();
    let predicate = it.next().unwrap_or_default();
    let object = it.next().unwrap_or_default();
    (subject, predicate, object)
}

/// The entities an object's key refers to.
fn referenced_entities(kind: Kind, key: &str) -> Vec<&str> {
    match kind {
        Kind::Attribute => vec![attribute_entity(key)],
        Kind::Relation => {
            let (subject, _, object) = relation_parts(key);
            if subject == object {
                vec![subject]
            } else {
                vec![subject, object]
            }
        }
        Kind::Entity
        | Kind::Job
        | Kind::ContextSpace
        | Kind::Procedure
        | Kind::ConnectorSlot
        | Kind::KnowledgeStore => Vec::new(),
    }
}

/// Task 2 validation of one row; empty when the row is well formed.
fn row_problems(row: &OverlayRow) -> Vec<String> {
    let mut problems = Vec::new();
    if let Err(e) = row.layer.validate() {
        problems.push(e.to_string());
    }
    if let Err(e) = validate_key(row.kind, &row.key) {
        problems.push(e.to_string());
    }
    if let Err(es) = validate_fields(row.kind, row.op, &row.fields) {
        problems.extend(es.iter().map(ToString::to_string));
    }
    problems
}

fn origin(row: &OverlayRow) -> Origin {
    Origin {
        row_id: row.id,
        layer: row.layer.clone(),
        op: row.op,
    }
}

// --- apply ------------------------------------------------------------------

struct Obj {
    fields: Map<String, Value>,
    add_row: Uuid,
}

#[derive(Default)]
struct State {
    /// Present objects, indexed by `kind_index`.
    objects: [BTreeMap<String, Obj>; 8],
    /// Objects removed by a tombstone (directly or by cascade) -> index of the
    /// layer (in `layer_order`) that removed them.
    removed: [BTreeMap<String, usize>; 8],
    provenance: BTreeMap<String, Vec<Origin>>,
    errors: Vec<CompileError>,
    warnings: Vec<Warning>,
}

impl State {
    fn exists(&self, kind: Kind, key: &str) -> bool {
        self.objects[kind_index(kind)].contains_key(key)
    }

    /// The layer index that tombstoned `key`, if any.
    fn removed_at(&self, kind: Kind, key: &str) -> Option<usize> {
        self.removed[kind_index(kind)].get(key).copied()
    }

    fn error(&mut self, code: ErrorCode, row: &OverlayRow, message: String) {
        self.errors
            .push(CompileError::row(code, row.id, row.kind, &row.key, message));
    }

    fn remove(&mut self, kind: Kind, key: &str, layer: usize) {
        let idx = kind_index(kind);
        self.objects[idx].remove(key);
        self.removed[idx].insert(key.to_owned(), layer);
        self.provenance.remove(&provenance_key(kind, key));
    }

    /// Apply one row of the layer at index `layer` in `layer_order`.
    fn apply(&mut self, row: &OverlayRow, layer: usize) {
        let kind = row.kind;
        let idx = kind_index(kind);
        // A patch/tombstone of an attribute/relation whose entity this same layer
        // tombstoned is superseded by that tombstone: skip it with a warning.
        if row.op != Op::Add
            && !self.exists(kind, &row.key)
            && referenced_entities(kind, &row.key)
                .into_iter()
                .any(|e| self.removed_at(Kind::Entity, e) == Some(layer))
        {
            self.warnings.push(Warning {
                code: "superseded_by_tombstone",
                key: row.key.clone(),
            });
            return;
        }
        match row.op {
            Op::Add => {
                if self.exists(kind, &row.key) {
                    let msg = format!("{} {:?} already exists", kind_name(kind), row.key);
                    self.error(ErrorCode::DuplicateAdd, row, msg);
                    return;
                }
                if self.removed_at(kind, &row.key).is_some_and(|at| at < layer) {
                    let msg = format!(
                        "{} {:?} was tombstoned in a lower layer and stays hidden above it",
                        kind_name(kind),
                        row.key
                    );
                    self.error(ErrorCode::AddTombstoned, row, msg);
                    return;
                }
                let dead: Vec<&str> = referenced_entities(kind, &row.key)
                    .into_iter()
                    .filter(|e| {
                        !self.exists(Kind::Entity, e) && self.removed_at(Kind::Entity, e).is_some()
                    })
                    .collect();
                if !dead.is_empty() {
                    let msg = format!("entity {} was tombstoned", dead.join(", "));
                    self.error(ErrorCode::DanglingReference, row, msg);
                    return;
                }
                self.objects[idx].insert(
                    row.key.clone(),
                    Obj {
                        fields: row.fields.clone(),
                        add_row: row.id,
                    },
                );
                self.provenance
                    .insert(provenance_key(kind, &row.key), vec![origin(row)]);
            }
            Op::Patch => match self.objects[idx].get_mut(&row.key) {
                Some(obj) => {
                    for (name, value) in &row.fields {
                        obj.fields.insert(name.clone(), value.clone());
                    }
                    self.provenance
                        .entry(provenance_key(kind, &row.key))
                        .or_default()
                        .push(origin(row));
                }
                None => {
                    let msg = self.missing_message("patch", row, layer);
                    self.error(ErrorCode::PatchMissing, row, msg);
                }
            },
            Op::Tombstone => {
                if !self.exists(kind, &row.key) {
                    let msg = self.missing_message("tombstone", row, layer);
                    self.error(ErrorCode::TombstoneMissing, row, msg);
                    return;
                }
                self.remove(kind, &row.key, layer);
                if kind == Kind::Entity {
                    self.cascade(&row.key, layer);
                }
            }
        }
    }

    fn missing_message(&self, verb: &str, row: &OverlayRow, layer: usize) -> String {
        let why = match self.removed_at(row.kind, &row.key) {
            Some(at) if at < layer => "it was tombstoned in a lower layer",
            Some(_) => "it was tombstoned in this layer",
            None => "it does not exist",
        };
        format!("cannot {verb} {} {:?}: {why}", kind_name(row.kind), row.key)
    }

    /// Remove the attributes of `entity` and every relation touching it.
    fn cascade(&mut self, entity: &str, layer: usize) {
        let attributes: Vec<String> = self.objects[kind_index(Kind::Attribute)]
            .keys()
            .filter(|k| attribute_entity(k) == entity)
            .cloned()
            .collect();
        let relations: Vec<String> = self.objects[kind_index(Kind::Relation)]
            .keys()
            .filter(|k| {
                let (subject, _, object) = relation_parts(k);
                subject == entity || object == entity
            })
            .cloned()
            .collect();
        for key in attributes {
            self.remove(Kind::Attribute, &key, layer);
        }
        for key in relations {
            self.remove(Kind::Relation, &key, layer);
        }
    }

    /// After all layers: attributes/relations whose entity is absent.
    fn check_dangling(&mut self) {
        let mut found = Vec::new();
        for kind in [Kind::Attribute, Kind::Relation] {
            for (key, obj) in &self.objects[kind_index(kind)] {
                let missing: Vec<&str> = referenced_entities(kind, key)
                    .into_iter()
                    .filter(|e| !self.exists(Kind::Entity, e))
                    .collect();
                if !missing.is_empty() {
                    found.push(CompileError::row(
                        ErrorCode::DanglingReference,
                        obj.add_row,
                        kind,
                        key,
                        format!("entity {} is not in the effective view", missing.join(", ")),
                    ));
                }
            }
        }
        self.errors.extend(found);
    }
}

// --- typed output -----------------------------------------------------------

fn text(fields: &Map<String, Value>, name: &str) -> Option<String> {
    fields.get(name).and_then(Value::as_str).map(str::to_owned)
}

fn list(fields: &Map<String, Value>, name: &str) -> Vec<String> {
    fields
        .get(name)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn flag(fields: &Map<String, Value>, name: &str) -> bool {
    fields.get(name).and_then(Value::as_bool).unwrap_or(false)
}

fn to_entity(fields: &Map<String, Value>) -> Entity {
    Entity {
        label: text(fields, "label").unwrap_or_default(),
        synonyms: list(fields, "synonyms"),
        hidden: flag(fields, "hidden"),
    }
}

fn to_attribute(key: &str, fields: &Map<String, Value>) -> Attribute {
    let (entity, name) = key.split_once('.').unwrap_or((key, ""));
    Attribute {
        entity: entity.to_owned(),
        name: name.to_owned(),
        data_type: text(fields, "data_type").unwrap_or_default(),
        pii: flag(fields, "pii"),
        synonyms: list(fields, "synonyms"),
    }
}

fn to_relation(key: &str, fields: &Map<String, Value>) -> Relation {
    let (subject, predicate, object) = relation_parts(key);
    Relation {
        subject: subject.to_owned(),
        predicate: predicate.to_owned(),
        object: object.to_owned(),
        cardinality: text(fields, "cardinality").unwrap_or_default(),
    }
}

fn to_job(key: &str, fields: &Map<String, Value>) -> Job {
    Job {
        key: key.to_owned(),
        label: text(fields, "label").unwrap_or_default(),
        actor: text(fields, "actor").unwrap_or_default(),
        stance: text(fields, "stance"),
        preamble: text(fields, "preamble"),
        actor_label: text(fields, "actor_label"),
        ontology_slice: list(fields, "ontology_slice"),
        channels: list(fields, "channels"),
        escalation: fields.get("escalation").cloned(),
        use_cases: list(fields, "use_cases"),
        sops: list(fields, "sops"),
        tools: list(fields, "tools"),
        stores: list(fields, "stores"),
    }
}

/// Type the job-content objects. Patches merge shallowly, so the merged fields
/// are parsed again here; a failure is `invalid_row` on the object's add row.
fn build_job_content(state: &mut State) -> JobContent {
    let mut out = JobContent::default();
    let mut errors = Vec::new();
    for kind in [
        Kind::ContextSpace,
        Kind::Procedure,
        Kind::ConnectorSlot,
        Kind::KnowledgeStore,
    ] {
        for (key, obj) in &state.objects[kind_index(kind)] {
            match content_item(kind, key, &obj.fields) {
                Ok(ContentItem::ContextSpace(x)) => {
                    out.context_spaces.insert(key.clone(), x);
                }
                Ok(ContentItem::Procedure(x)) => {
                    out.procedures.insert(key.clone(), x);
                }
                Ok(ContentItem::ConnectorSlot(x)) => {
                    out.connectors.insert(key.clone(), x);
                }
                Ok(ContentItem::KnowledgeStore(x)) => {
                    out.knowledge_stores.insert(key.clone(), x);
                }
                Err(message) => errors.push(CompileError::row(
                    ErrorCode::InvalidRow,
                    obj.add_row,
                    kind,
                    key,
                    message,
                )),
            }
        }
    }
    state.errors.extend(errors);
    out
}

// --- compile ----------------------------------------------------------------

/// Compile the effective view (spec section 5). Rows whose layer is not in
/// `layer_order(sel, rules)` are ignored: the loader decides what to pass.
/// Errors are collected, never first-fail.
pub fn compile(
    sel: &Selection,
    known: &Known,
    rules: &[AxisRule],
    rows: &[OverlayRow],
) -> Result<EffectiveConfig, Vec<CompileError>> {
    let selection_errors = validate_selection(sel, known);
    if !selection_errors.is_empty() {
        return Err(selection_errors);
    }

    let order = layer_order(sel, rules);
    let mut state = State::default();

    // Bucket well-formed rows by layer; malformed rows become `invalid_row`.
    let mut by_layer: Vec<Vec<&OverlayRow>> = vec![Vec::new(); order.len()];
    for row in rows {
        let Some(pos) = order.iter().position(|l| *l == row.layer) else {
            // Not a selected layer: the loader's business, unless the layer itself is
            // malformed, which is never silently skipped.
            if let Err(e) = row.layer.validate() {
                state.error(ErrorCode::InvalidRow, row, e.to_string());
            }
            continue;
        };
        if row.kind.is_job_content() && !matches!(row.layer.scope, Scope::Job | Scope::Tenant) {
            let msg = format!(
                "{} rows are allowed only in job and tenant layers",
                kind_name(row.kind)
            );
            state.error(ErrorCode::KindNotAllowedInLayer, row, msg);
            continue;
        }
        let problems = row_problems(row);
        if problems.is_empty() {
            by_layer[pos].push(row);
        } else {
            state.error(ErrorCode::InvalidRow, row, problems.join("; "));
        }
    }

    for (pos, bucket) in by_layer.iter_mut().enumerate() {
        // Kind order entity -> attribute -> relation -> job; key order within a kind.
        bucket.sort_by(|a, b| {
            kind_index(a.kind)
                .cmp(&kind_index(b.kind))
                .then_with(|| a.key.cmp(&b.key))
                .then_with(|| a.id.cmp(&b.id))
        });
        let mut previous: Option<(Kind, &str)> = None;
        for row in bucket.iter() {
            let current = (row.kind, row.key.as_str());
            if previous == Some(current) {
                let msg = format!(
                    "more than one current row for {} {:?} in one layer",
                    kind_name(row.kind),
                    row.key
                );
                state.error(ErrorCode::InvalidRow, row, msg);
                continue;
            }
            previous = Some(current);
            state.apply(row, pos);
        }
    }

    state.check_dangling();
    let job_content = build_job_content(&mut state);
    if !state.errors.is_empty() {
        return Err(state.errors);
    }

    let entities: BTreeMap<String, Entity> = state.objects[kind_index(Kind::Entity)]
        .iter()
        .map(|(k, o)| (k.clone(), to_entity(&o.fields)))
        .collect();
    let attributes = state.objects[kind_index(Kind::Attribute)]
        .iter()
        .map(|(k, o)| (k.clone(), to_attribute(k, &o.fields)))
        .collect();
    let relations = state.objects[kind_index(Kind::Relation)]
        .iter()
        .map(|(k, o)| (k.clone(), to_relation(k, &o.fields)))
        .collect();

    let mut warnings = std::mem::take(&mut state.warnings);
    let job = sel.job.as_ref().and_then(|key| {
        let job = state.objects[kind_index(Kind::Job)]
            .get(key)
            .map(|o| to_job(key, &o.fields));
        if job.is_none() {
            warnings.push(Warning {
                code: "selected_job_missing",
                key: key.clone(),
            });
        }
        job
    });
    if let Some(job) = &job {
        let slice: BTreeSet<&String> = job.ontology_slice.iter().collect();
        for name in slice {
            if !entities.contains_key(name) {
                warnings.push(Warning {
                    code: "slice_entity_missing",
                    key: name.clone(),
                });
            }
        }
    }

    // Tombstones drop provenance entries, so every entry is for a present object.
    let provenance = state.provenance;

    let mut cfg = EffectiveConfig {
        selection: sel.clone(),
        entities,
        attributes,
        relations,
        job,
        job_content,
        provenance,
        warnings,
        hash: String::new(),
    };
    cfg.hash = crate::canonical::effective_hash(&cfg);
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::axis::AxisValue;
    use serde_json::json;

    const TENANT: Uuid = Uuid::from_u128(0x7e7e);

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn base() -> LayerKey {
        layer(Scope::Base)
    }

    fn industry(name: &str) -> LayerKey {
        LayerKey {
            industry: Some(name.into()),
            ..layer(Scope::Industry)
        }
    }

    fn dept(ind: &str, d: &str) -> LayerKey {
        LayerKey {
            industry: Some(ind.into()),
            department: Some(d.into()),
            ..layer(Scope::IndustryDept)
        }
    }

    fn rule_layer(rule: &str) -> LayerKey {
        LayerKey {
            rule: Some(rule.into()),
            ..layer(Scope::Profile)
        }
    }

    fn tenant() -> LayerKey {
        LayerKey {
            tenant: Some(TENANT),
            ..layer(Scope::Tenant)
        }
    }

    fn row(n: u128, layer: LayerKey, kind: Kind, key: &str, op: Op, fields: Value) -> OverlayRow {
        OverlayRow {
            id: id(n),
            layer,
            kind,
            key: key.into(),
            op,
            fields: fields.as_object().cloned().unwrap_or_default(),
        }
    }

    fn entity(n: u128, layer: LayerKey, key: &str) -> OverlayRow {
        row(n, layer, Kind::Entity, key, Op::Add, json!({"label": key}))
    }

    fn known() -> Known {
        let val = |k: &str| AxisValue {
            key: k.into(),
            parent: None,
            label: None,
            description: None,
        };
        Known {
            industries: ["retail".to_owned()].into(),
            departments: ["a".to_owned(), "b".to_owned()].into(),
            jobs: ["customer_care_agent".to_owned()].into(),
            dimensions: [(
                "channel".to_owned(),
                ["voice".to_owned(), "chat".to_owned()].into(),
            )]
            .into(),
            axes: vec![Axis {
                key: "order_type".into(),
                cardinality: Cardinality::One,
                values: vec![val("continuous"), val("one_off")],
                label: None,
                description: None,
            }],
        }
    }

    fn profile(axis: &str, value: &str) -> Profile {
        Profile([(axis.to_owned(), [value.to_owned()].into())].into())
    }

    fn rules() -> Vec<AxisRule> {
        vec![
            AxisRule {
                key: "metered".into(),
                priority: 10,
                condition: Condition::Eq("order_type".into(), "continuous".into()),
            },
            AxisRule {
                key: "one_off_extras".into(),
                priority: 5,
                condition: Condition::Eq("order_type".into(), "one_off".into()),
            },
        ]
    }

    fn sel_retail() -> Selection {
        Selection {
            industry: Some("retail".into()),
            ..Selection::default()
        }
    }

    fn codes(errors: &[CompileError]) -> Vec<ErrorCode> {
        errors.iter().map(|e| e.code).collect()
    }

    fn compile_err(sel: &Selection, rows: &[OverlayRow]) -> Vec<CompileError> {
        compile(sel, &known(), &rules(), rows).expect_err("compile should fail")
    }

    fn compile_ok(sel: &Selection, rows: &[OverlayRow]) -> EffectiveConfig {
        match compile(sel, &known(), &rules(), rows) {
            Ok(cfg) => cfg,
            Err(errors) => panic!("unexpected compile errors: {errors:?}"),
        }
    }

    fn attr(n: u128, layer: LayerKey, key: &str) -> OverlayRow {
        row(
            n,
            layer,
            Kind::Attribute,
            key,
            Op::Add,
            json!({"data_type": "string"}),
        )
    }

    fn rel(n: u128, layer: LayerKey, key: &str) -> OverlayRow {
        row(
            n,
            layer,
            Kind::Relation,
            key,
            Op::Add,
            json!({"cardinality": "1:n"}),
        )
    }

    fn tomb(n: u128, layer: LayerKey, kind: Kind, key: &str) -> OverlayRow {
        row(n, layer, kind, key, Op::Tombstone, json!({}))
    }

    fn sel_retail_tenant() -> Selection {
        Selection {
            tenant: Some(TENANT),
            ..sel_retail()
        }
    }

    /// Base adds Order, Payment, Order.status, Order|paidBy|Payment; industry tombstones Order.
    fn platform_tombstoned_order() -> Vec<OverlayRow> {
        vec![
            entity(1, base(), "Order"),
            entity(2, base(), "Payment"),
            attr(3, base(), "Order.status"),
            rel(4, base(), "Order|paidBy|Payment"),
            tomb(5, industry("retail"), Kind::Entity, "Order"),
        ]
    }

    #[test]
    fn tenant_patch_of_attribute_after_platform_tombstone_is_patch_missing() {
        let mut rows = platform_tombstoned_order();
        rows.push(row(
            10,
            tenant(),
            Kind::Attribute,
            "Order.status",
            Op::Patch,
            json!({"pii": true}),
        ));
        let errors = compile_err(&sel_retail_tenant(), &rows);
        assert_eq!(codes(&errors), vec![ErrorCode::PatchMissing]);
        assert_eq!(errors[0].row_id, Some(id(10)));
        assert_eq!(errors[0].kind, Some(Kind::Attribute));
        assert_eq!(errors[0].key.as_deref(), Some("Order.status"));
        assert!(
            errors[0].message.contains("lower layer"),
            "{}",
            errors[0].message
        );
    }

    #[test]
    fn tenant_tombstone_of_cascaded_relation_is_tombstone_missing() {
        let mut rows = platform_tombstoned_order();
        rows.push(tomb(10, tenant(), Kind::Relation, "Order|paidBy|Payment"));
        let errors = compile_err(&sel_retail_tenant(), &rows);
        assert_eq!(codes(&errors), vec![ErrorCode::TombstoneMissing]);
        assert_eq!(errors[0].row_id, Some(id(10)));
        assert_eq!(errors[0].kind, Some(Kind::Relation));
    }

    #[test]
    fn dimensions_without_job_rejected() {
        let sel = Selection {
            dimensions: [("channel".to_owned(), "voice".to_owned())].into(),
            ..Selection::default()
        };
        let errors = compile_err(&sel, &[]);
        assert_eq!(codes(&errors), vec![ErrorCode::InvalidSelection]);
        assert_eq!(errors[0].key.as_deref(), Some("channel"));
    }

    #[test]
    fn same_layer_dependent_patch_and_tombstone_superseded_by_entity_tombstone() {
        let rows = [
            entity(1, base(), "Order"),
            entity(2, base(), "Payment"),
            attr(3, base(), "Order.status"),
            attr(4, base(), "Order.total"),
            rel(5, base(), "Order|paidBy|Payment"),
            tomb(10, tenant(), Kind::Entity, "Order"),
            tomb(11, tenant(), Kind::Attribute, "Order.status"),
            row(
                12,
                tenant(),
                Kind::Attribute,
                "Order.total",
                Op::Patch,
                json!({"pii": true}),
            ),
            tomb(13, tenant(), Kind::Relation, "Order|paidBy|Payment"),
        ];
        let sel = Selection {
            tenant: Some(TENANT),
            ..Selection::default()
        };
        let cfg = compile_ok(&sel, &rows);
        assert_eq!(cfg.entities.keys().collect::<Vec<_>>(), vec!["Payment"]);
        assert!(cfg.attributes.is_empty());
        assert!(cfg.relations.is_empty());
        let superseded: Vec<&str> = cfg
            .warnings
            .iter()
            .filter(|w| w.code == "superseded_by_tombstone")
            .map(|w| w.key.as_str())
            .collect();
        assert_eq!(
            superseded,
            vec!["Order.status", "Order.total", "Order|paidBy|Payment"]
        );
    }

    #[test]
    fn same_layer_dependent_add_after_entity_tombstone_is_dangling_reference() {
        let rows = [
            entity(1, base(), "Order"),
            tomb(10, tenant(), Kind::Entity, "Order"),
            attr(11, tenant(), "Order.note"),
        ];
        let sel = Selection {
            tenant: Some(TENANT),
            ..Selection::default()
        };
        let errors = compile_err(&sel, &rows);
        assert_eq!(codes(&errors), vec![ErrorCode::DanglingReference]);
        assert_eq!(errors[0].row_id, Some(id(11)));
    }

    #[test]
    fn readd_of_tombstoned_entity_is_add_tombstoned() {
        let mut rows = platform_tombstoned_order();
        rows.push(entity(10, tenant(), "Order"));
        let errors = compile_err(&sel_retail_tenant(), &rows);
        assert_eq!(codes(&errors), vec![ErrorCode::AddTombstoned]);
        assert_eq!(errors[0].row_id, Some(id(10)));
        assert_eq!(errors[0].kind, Some(Kind::Entity));
        assert_eq!(
            serde_json::to_value(ErrorCode::AddTombstoned).ok(),
            Some(json!("add_tombstoned"))
        );
    }

    #[test]
    fn readd_of_cascaded_attribute_is_add_tombstoned() {
        let mut rows = platform_tombstoned_order();
        rows.push(attr(10, tenant(), "Order.status"));
        let errors = compile_err(&sel_retail_tenant(), &rows);
        assert_eq!(codes(&errors), vec![ErrorCode::AddTombstoned]);
        assert_eq!(errors[0].row_id, Some(id(10)));
        assert_eq!(errors[0].kind, Some(Kind::Attribute));
    }

    #[test]
    fn row_with_malformed_layer_is_invalid_row() {
        let bad_layer = LayerKey {
            tenant: Some(Uuid::nil()),
            ..layer(Scope::Tenant)
        };
        let rows = [entity(1, base(), "Order"), entity(2, bad_layer, "Refund")];
        let errors = compile_err(&Selection::default(), &rows);
        assert_eq!(codes(&errors), vec![ErrorCode::InvalidRow]);
        assert_eq!(errors[0].row_id, Some(id(2)));
    }

    #[test]
    fn layer_order_full_selection() {
        let sel = Selection {
            industry: Some("retail".into()),
            departments: ["b".to_owned(), "a".to_owned()].into(),
            job: Some("customer_care_agent".into()),
            profile: profile("order_type", "continuous"),
            dimensions: [("channel".to_owned(), "voice".to_owned())].into(),
            tenant: Some(TENANT),
        };
        let job = |dimension: Option<(String, String)>| LayerKey {
            job: Some("customer_care_agent".into()),
            dimension,
            ..layer(Scope::Job)
        };
        let expected = vec![
            base(),
            rule_layer("metered"),
            industry("retail"),
            dept("retail", "a"),
            dept("retail", "b"),
            job(None),
            job(Some(("channel".into(), "voice".into()))),
            tenant(),
        ];
        assert_eq!(layer_order(&sel, &rules()), expected);
    }

    #[test]
    fn layer_order_rules_sorted_by_priority_then_key() {
        let always = Condition::And(vec![]);
        let rules = vec![
            AxisRule {
                key: "z".into(),
                priority: 1,
                condition: always.clone(),
            },
            AxisRule {
                key: "b".into(),
                priority: 2,
                condition: always.clone(),
            },
            AxisRule {
                key: "a".into(),
                priority: 2,
                condition: always,
            },
        ];
        let order = layer_order(&Selection::default(), &rules);
        assert_eq!(
            order,
            vec![base(), rule_layer("z"), rule_layer("a"), rule_layer("b")]
        );
    }

    #[test]
    fn patch_replaces_field_whole() {
        let rows = [
            row(
                1,
                base(),
                Kind::Entity,
                "Order",
                Op::Add,
                json!({"label": "Order", "synonyms": ["x", "y"]}),
            ),
            row(
                2,
                industry("retail"),
                Kind::Entity,
                "Order",
                Op::Patch,
                json!({"synonyms": ["z"]}),
            ),
        ];
        let cfg = compile_ok(&sel_retail(), &rows);
        let order = &cfg.entities["Order"];
        assert_eq!(order.synonyms, vec!["z".to_owned()]);
        assert_eq!(order.label, "Order");
        assert_eq!(cfg.hash.len(), 64);
    }

    #[test]
    fn add_existing_is_duplicate_add() {
        let rows = [
            entity(1, base(), "Order"),
            entity(2, industry("retail"), "Order"),
        ];
        let errors = compile_err(&sel_retail(), &rows);
        assert_eq!(codes(&errors), vec![ErrorCode::DuplicateAdd]);
        assert_eq!(errors[0].row_id, Some(id(2)));
        assert_eq!(errors[0].key.as_deref(), Some("Order"));
    }

    #[test]
    fn two_departments_adding_same_entity_is_duplicate_add() {
        let sel = Selection {
            departments: ["a".to_owned(), "b".to_owned()].into(),
            ..sel_retail()
        };
        let rows = [
            entity(1, dept("retail", "b"), "Refund"),
            entity(2, dept("retail", "a"), "Refund"),
        ];
        let errors = compile_err(&sel, &rows);
        assert_eq!(codes(&errors), vec![ErrorCode::DuplicateAdd]);
        // department `a` applies first, so the `b` row is the duplicate.
        assert_eq!(errors[0].row_id, Some(id(1)));
    }

    #[test]
    fn tombstone_cascades_attributes_and_relations() {
        let rows = [
            entity(1, base(), "Order"),
            entity(2, base(), "Payment"),
            entity(3, base(), "Customer"),
            row(
                4,
                base(),
                Kind::Attribute,
                "Order.order_id",
                Op::Add,
                json!({"data_type": "string"}),
            ),
            row(
                5,
                base(),
                Kind::Attribute,
                "Payment.amount",
                Op::Add,
                json!({"data_type": "money"}),
            ),
            row(
                6,
                base(),
                Kind::Relation,
                "Order|paidBy|Payment",
                Op::Add,
                json!({"cardinality": "1:n"}),
            ),
            row(
                7,
                base(),
                Kind::Relation,
                "Customer|places|Order",
                Op::Add,
                json!({"cardinality": "1:n"}),
            ),
            row(
                8,
                base(),
                Kind::Relation,
                "Customer|pays|Payment",
                Op::Add,
                json!({"cardinality": "1:n"}),
            ),
            row(
                9,
                industry("retail"),
                Kind::Entity,
                "Order",
                Op::Tombstone,
                json!({}),
            ),
        ];
        let cfg = compile_ok(&sel_retail(), &rows);
        assert_eq!(
            cfg.entities.keys().collect::<Vec<_>>(),
            vec!["Customer", "Payment"]
        );
        assert_eq!(
            cfg.attributes.keys().collect::<Vec<_>>(),
            vec!["Payment.amount"]
        );
        assert_eq!(
            cfg.relations.keys().collect::<Vec<_>>(),
            vec!["Customer|pays|Payment"]
        );
        assert!(!cfg.provenance.contains_key("entity:Order"));
        assert!(!cfg.provenance.contains_key("attribute:Order.order_id"));
        assert!(
            cfg.provenance
                .contains_key("relation:Customer|pays|Payment")
        );
    }

    #[test]
    fn add_attribute_on_tombstoned_entity_is_dangling_reference() {
        let rows = [
            entity(1, base(), "Order"),
            row(
                2,
                industry("retail"),
                Kind::Entity,
                "Order",
                Op::Tombstone,
                json!({}),
            ),
            row(
                3,
                tenant(),
                Kind::Attribute,
                "Order.note",
                Op::Add,
                json!({"data_type": "text"}),
            ),
        ];
        let sel = Selection {
            tenant: Some(TENANT),
            ..sel_retail()
        };
        let errors = compile_err(&sel, &rows);
        assert_eq!(codes(&errors), vec![ErrorCode::DanglingReference]);
        assert_eq!(errors[0].row_id, Some(id(3)));
    }

    #[test]
    fn attribute_without_entity_is_dangling_reference() {
        let rows = [row(
            1,
            base(),
            Kind::Attribute,
            "Ghost.id",
            Op::Add,
            json!({"data_type": "string"}),
        )];
        let errors = compile_err(&Selection::default(), &rows);
        assert_eq!(codes(&errors), vec![ErrorCode::DanglingReference]);
        assert_eq!(errors[0].row_id, Some(id(1)));
    }

    #[test]
    fn tenant_patch_after_platform_tombstone_is_patch_missing() {
        let rows = [
            entity(1, base(), "Order"),
            row(
                2,
                industry("retail"),
                Kind::Entity,
                "Order",
                Op::Tombstone,
                json!({}),
            ),
            row(
                3,
                tenant(),
                Kind::Entity,
                "Order",
                Op::Patch,
                json!({"hidden": true}),
            ),
        ];
        let sel = Selection {
            tenant: Some(TENANT),
            ..sel_retail()
        };
        let errors = compile_err(&sel, &rows);
        assert_eq!(codes(&errors), vec![ErrorCode::PatchMissing]);
        assert_eq!(errors[0].row_id, Some(id(3)));
        assert_eq!(errors[0].kind, Some(Kind::Entity));
        assert_eq!(errors[0].key.as_deref(), Some("Order"));
    }

    #[test]
    fn errors_are_collected() {
        let rows = [
            entity(1, base(), "Order"),
            // 1: duplicate add
            entity(2, industry("retail"), "Order"),
            // 2: patch of something that never existed
            row(
                3,
                industry("retail"),
                Kind::Entity,
                "Refund",
                Op::Patch,
                json!({"hidden": true}),
            ),
            // 3: tombstone of something that never existed
            row(
                4,
                industry("retail"),
                Kind::Relation,
                "Order|paidBy|Payment",
                Op::Tombstone,
                json!({}),
            ),
        ];
        let errors = compile_err(&sel_retail(), &rows);
        let mut got: Vec<(ErrorCode, Option<Uuid>)> =
            errors.iter().map(|e| (e.code, e.row_id)).collect();
        got.sort_by_key(|(_, row_id)| *row_id);
        assert_eq!(
            got,
            vec![
                (ErrorCode::DuplicateAdd, Some(id(2))),
                (ErrorCode::PatchMissing, Some(id(3))),
                (ErrorCode::TombstoneMissing, Some(id(4))),
            ]
        );
    }

    #[test]
    fn invalid_row_is_collected_not_applied() {
        let rows = [
            row(
                1,
                base(),
                Kind::Entity,
                "order",
                Op::Add,
                json!({"label": "x"}),
            ),
            row(2, base(), Kind::Entity, "Order", Op::Add, json!({})),
            entity(3, base(), "Payment"),
        ];
        let errors = compile_err(&Selection::default(), &rows);
        assert_eq!(
            codes(&errors),
            vec![ErrorCode::InvalidRow, ErrorCode::InvalidRow]
        );
        let ids: BTreeSet<Option<Uuid>> = errors.iter().map(|e| e.row_id).collect();
        let want: BTreeSet<Option<Uuid>> = [Some(id(1)), Some(id(2))].into();
        assert_eq!(ids, want);
    }

    #[test]
    fn unknown_selection_keys_rejected() {
        let sel = Selection {
            industry: Some("bogus_industry".into()),
            departments: ["bogus_dept".to_owned()].into(),
            job: Some("bogus_job".into()),
            profile: profile("order_type", "bogus_value"),
            dimensions: [("channel".to_owned(), "fax".to_owned())].into(),
            tenant: None,
        };
        let errors = compile_err(&sel, &[entity(1, base(), "Order")]);
        assert!(errors.iter().all(|e| e.code == ErrorCode::InvalidSelection));
        let keys: BTreeSet<&str> = errors.iter().filter_map(|e| e.key.as_deref()).collect();
        let expected: BTreeSet<&str> = [
            "bogus_industry",
            "bogus_dept",
            "bogus_job",
            "channel=fax",
            "order_type=bogus_value",
        ]
        .into();
        assert_eq!(keys, expected);
        assert_eq!(errors.len(), 5);

        // unknown axis and unknown dimension key are named too
        let sel2 = Selection {
            profile: profile("no_such_axis", "v"),
            dimensions: [("no_such_dim".to_owned(), "v".to_owned())].into(),
            ..Selection::default()
        };
        let errors2 = compile_err(&sel2, &[]);
        let keys2: BTreeSet<&str> = errors2.iter().filter_map(|e| e.key.as_deref()).collect();
        let want2: BTreeSet<&str> = ["no_such_axis", "no_such_dim"].into();
        assert_eq!(keys2, want2);

        // nil tenant is rejected
        let sel3 = Selection {
            tenant: Some(Uuid::nil()),
            ..Selection::default()
        };
        assert_eq!(
            codes(&compile_err(&sel3, &[])),
            vec![ErrorCode::InvalidSelection]
        );
    }

    #[test]
    fn rule_false_layer_not_applied() {
        let sel = Selection {
            profile: profile("order_type", "continuous"),
            ..Selection::default()
        };
        let rows = [
            entity(1, rule_layer("metered"), "MeterPoint"),
            entity(2, rule_layer("one_off_extras"), "Voucher"),
        ];
        let cfg = compile_ok(&sel, &rows);
        assert!(cfg.entities.contains_key("MeterPoint"));
        assert!(!cfg.entities.contains_key("Voucher"));
    }

    #[test]
    fn provenance_lists_add_then_patch() {
        let rows = [
            entity(1, base(), "Order"),
            row(
                2,
                tenant(),
                Kind::Entity,
                "Order",
                Op::Patch,
                json!({"label": "Purchase"}),
            ),
        ];
        let sel = Selection {
            tenant: Some(TENANT),
            ..Selection::default()
        };
        let cfg = compile_ok(&sel, &rows);
        assert_eq!(
            cfg.provenance["entity:Order"],
            vec![
                Origin {
                    row_id: id(1),
                    layer: base(),
                    op: Op::Add
                },
                Origin {
                    row_id: id(2),
                    layer: tenant(),
                    op: Op::Patch
                },
            ]
        );
        assert_eq!(cfg.entities["Order"].label, "Purchase");
    }

    #[test]
    fn slice_entity_missing_is_warning_not_error() {
        let rows = [
            entity(1, base(), "Order"),
            row(
                2,
                base(),
                Kind::Job,
                "customer_care_agent",
                Op::Add,
                json!({
                    "label": "Customer care",
                    "actor": "customer",
                    "ontology_slice": ["Order", "Refund"]
                }),
            ),
        ];
        let sel = Selection {
            job: Some("customer_care_agent".into()),
            ..Selection::default()
        };
        let cfg = compile_ok(&sel, &rows);
        assert_eq!(
            cfg.warnings,
            vec![Warning {
                code: "slice_entity_missing",
                key: "Refund".into()
            }]
        );
        let job = cfg.job.expect("job compiled");
        assert_eq!(
            job.ontology_slice,
            vec!["Order".to_owned(), "Refund".to_owned()]
        );
        assert_eq!(job.actor, "customer");
    }

    fn job_layer() -> LayerKey {
        LayerKey {
            job: Some("customer_care_agent".into()),
            ..layer(Scope::Job)
        }
    }

    fn sel_job() -> Selection {
        Selection {
            job: Some("customer_care_agent".into()),
            ..Selection::default()
        }
    }

    fn sel_job_tenant() -> Selection {
        Selection {
            tenant: Some(TENANT),
            ..sel_job()
        }
    }

    fn space(n: u128, layer: LayerKey) -> OverlayRow {
        row(
            n,
            layer,
            Kind::ContextSpace,
            "retail:order-support:returns",
            Op::Add,
            json!({
                "area": "order-support",
                "label": "Returns",
                "request_type": "return",
                "required": ["order_id"],
                "entities": ["Order"],
                "procedure": "return"
            }),
        )
    }

    fn procedure(n: u128, layer: LayerKey, fields: Value) -> OverlayRow {
        row(n, layer, Kind::Procedure, "return@1.3.0", Op::Add, fields)
    }

    fn procedure_fields() -> Value {
        json!({
            "status": "PUBLISHED",
            "context_space": "retail:order-support:returns",
            "domain": "customer_care",
            "tool_requirements": ["order_create_rma"],
            "success_criteria": [],
            "reviewers": [],
            "plan": {"phases": [{"steps": []}]},
            "steps": []
        })
    }

    #[test]
    fn job_content_compiles_in_job_layer() {
        let rows = [
            space(1, job_layer()),
            procedure(2, job_layer(), procedure_fields()),
        ];
        let cfg = compile_ok(&sel_job(), &rows);
        let s = &cfg.job_content.context_spaces["retail:order-support:returns"];
        assert_eq!(s.key, "retail:order-support:returns");
        assert_eq!(s.label, "Returns");
        let p = &cfg.job_content.procedures["return@1.3.0"];
        assert_eq!((p.name.as_str(), p.version.as_str()), ("return", "1.3.0"));
        assert_eq!(p.description, None);
        // An absent gate is restored as null (care-bus Phase.gate is nullable).
        assert_eq!(p.plan, json!({"phases": [{"steps": [], "gate": null}]}));
        assert!(
            cfg.provenance
                .contains_key("context_space:retail:order-support:returns")
        );
        // Job content is hashed: a label change moves the hash.
        let mut changed = rows.to_vec();
        changed[0]
            .fields
            .insert("label".into(), json!("Returns and refunds"));
        assert_ne!(compile_ok(&sel_job(), &changed).hash, cfg.hash);
    }

    #[test]
    fn procedure_fields_must_parse_as_v1() {
        let mut fields = procedure_fields();
        if let Some(o) = fields.as_object_mut() {
            o.remove("steps");
        }
        let errors = compile_err(&sel_job(), &[procedure(1, job_layer(), fields)]);
        assert_eq!(codes(&errors), vec![ErrorCode::InvalidRow]);
        assert!(errors[0].message.contains("steps"), "{}", errors[0].message);
    }

    #[test]
    fn tenant_patch_overrides_context_space_label() {
        let rows = [
            space(1, job_layer()),
            row(
                2,
                tenant(),
                Kind::ContextSpace,
                "retail:order-support:returns",
                Op::Patch,
                json!({"label": "Mine"}),
            ),
        ];
        let cfg = compile_ok(&sel_job_tenant(), &rows);
        let s = &cfg.job_content.context_spaces["retail:order-support:returns"];
        assert_eq!(s.label, "Mine");
        assert_eq!(s.request_type, "return");
        // A patch that breaks the item is invalid_row after the merge.
        let bad = [
            space(1, job_layer()),
            row(
                2,
                tenant(),
                Kind::ContextSpace,
                "retail:order-support:returns",
                Op::Patch,
                json!({"required": "order_id"}),
            ),
        ];
        let errors = compile_err(&sel_job_tenant(), &bad);
        assert_eq!(codes(&errors), vec![ErrorCode::InvalidRow]);
    }

    #[test]
    fn tombstone_removes_procedure() {
        let rows = [
            procedure(1, job_layer(), procedure_fields()),
            tomb(2, tenant(), Kind::Procedure, "return@1.3.0"),
        ];
        let cfg = compile_ok(&sel_job_tenant(), &rows);
        assert!(cfg.job_content.procedures.is_empty());
        assert!(cfg.job_content.is_empty());
    }

    #[test]
    fn job_content_in_industry_layer_is_refused() {
        for l in [industry("retail"), base()] {
            let errors = compile_err(&sel_retail(), &[space(1, l)]);
            assert_eq!(codes(&errors), vec![ErrorCode::KindNotAllowedInLayer]);
        }
    }

    #[test]
    fn phase1_hash_unchanged() {
        // Without job content and without the new job fields, neither key is
        // serialised or hashed: the hash is the phase-1 formula.
        let rows = [
            entity(1, base(), "Order"),
            row(
                2,
                base(),
                Kind::Job,
                "customer_care_agent",
                Op::Add,
                json!({"label": "Customer care", "actor": "customer"}),
            ),
        ];
        let cfg = compile_ok(&sel_job(), &rows);
        let v = serde_json::to_value(&cfg).expect("serialises");
        assert!(v.get("job_content").is_none());
        assert!(v["job"].get("preamble").is_none());
        assert!(v["job"].get("actor_label").is_none());
        let phase1 = json!({
            "selection": cfg.selection,
            "entities": cfg.entities,
            "attributes": cfg.attributes,
            "relations": cfg.relations,
            "job": cfg.job,
        });
        assert_eq!(
            cfg.hash,
            crate::canonical::sha256_hex(&crate::canonical::canonical_json(&phase1))
        );
    }

    #[test]
    fn job_preamble_and_actor_label_are_optional() {
        let job = |n: u128, fields: Value| {
            row(n, base(), Kind::Job, "customer_care_agent", Op::Add, fields)
        };
        let cfg = compile_ok(
            &sel_job(),
            &[job(1, json!({"label": "Care", "actor": "customer"}))],
        );
        let j = cfg.job.expect("job");
        assert_eq!((j.preamble, j.actor_label), (None, None));
        let rows = [
            job(1, json!({"label": "Care", "actor": "customer"})),
            row(
                2,
                job_layer(),
                Kind::Job,
                "customer_care_agent",
                Op::Patch,
                json!({"preamble": "You are the intake step.", "actor_label": "Customer care desk"}),
            ),
        ];
        let j = compile_ok(&sel_job(), &rows).job.expect("job");
        assert_eq!(j.preamble.as_deref(), Some("You are the intake step."));
        assert_eq!(j.actor_label.as_deref(), Some("Customer care desk"));
        assert_eq!(j.actor, "customer");
    }
}
