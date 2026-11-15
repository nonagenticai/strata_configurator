//! Projection of the effective config to care-bus's ontology tables
//! (spec section 5.4; `carebus-api/migrations/0001_schema.sql`).

use serde::Serialize;

use crate::compile::{EffectiveConfig, Origin};
use crate::layer::Scope;
use crate::overlay::Op;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CbEntity {
    pub name: String,
    /// `generic` | `industry` | `industry_dept`
    pub scope: String,
    pub industry: Option<String>,
    pub department: Option<String>,
    pub label: String,
    pub synonyms: Vec<String>,
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CbAttribute {
    pub entity: String,
    pub name: String,
    pub data_type: String,
    pub pii: bool,
    pub synonyms: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CbRelation {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub cardinality: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CarebusOntology {
    pub entities: Vec<CbEntity>,
    pub attributes: Vec<CbAttribute>,
    pub relations: Vec<CbRelation>,
}

/// (scope, industry, department) of an entity, from the layer of its `add` row.
fn scope_of(
    e: &EffectiveConfig,
    origins: Option<&Vec<Origin>>,
) -> (String, Option<String>, Option<String>) {
    let add = origins.and_then(|o| o.iter().find(|o| o.op == Op::Add));
    let generic = || ("generic".to_owned(), None, None);
    let from_selection = || {
        let sel = &e.selection;
        match (&sel.industry, sel.departments.len()) {
            (Some(i), 1) => (
                "industry_dept".to_owned(),
                Some(i.clone()),
                sel.departments.iter().next().cloned(),
            ),
            (Some(i), _) => ("industry".to_owned(), Some(i.clone()), None),
            (None, _) => generic(),
        }
    };
    let Some(add) = add else {
        return generic();
    };
    match add.layer.scope {
        Scope::Base | Scope::Profile => generic(),
        Scope::Industry => ("industry".to_owned(), add.layer.industry.clone(), None),
        Scope::IndustryDept => (
            "industry_dept".to_owned(),
            add.layer.industry.clone(),
            add.layer.department.clone(),
        ),
        Scope::Job | Scope::Tenant => from_selection(),
    }
}

pub fn project_carebus(e: &EffectiveConfig) -> CarebusOntology {
    let mut entities: Vec<CbEntity> = e
        .entities
        .iter()
        .map(|(name, ent)| {
            let (scope, industry, department) =
                scope_of(e, e.provenance.get(&format!("entity:{name}")));
            CbEntity {
                name: name.clone(),
                scope,
                industry,
                department,
                label: ent.label.clone(),
                synonyms: ent.synonyms.clone(),
                hidden: ent.hidden,
            }
        })
        .collect();
    entities.sort_by(|a, b| a.name.cmp(&b.name));

    let mut attributes: Vec<CbAttribute> = e
        .attributes
        .values()
        .map(|a| CbAttribute {
            entity: a.entity.clone(),
            name: a.name.clone(),
            data_type: a.data_type.clone(),
            pii: a.pii,
            synonyms: a.synonyms.clone(),
        })
        .collect();
    attributes.sort_by(|a, b| (&a.entity, &a.name).cmp(&(&b.entity, &b.name)));

    let mut relations: Vec<CbRelation> = e
        .relations
        .values()
        .map(|r| CbRelation {
            subject: r.subject.clone(),
            predicate: r.predicate.clone(),
            object: r.object.clone(),
            cardinality: r.cardinality.clone(),
        })
        .collect();
    relations.sort_by(|a, b| {
        (&a.subject, &a.predicate, &a.object).cmp(&(&b.subject, &b.predicate, &b.object))
    });

    CarebusOntology {
        entities,
        attributes,
        relations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::{Known, Selection, compile};
    use crate::layer::LayerKey;
    use crate::overlay::{Kind, OverlayRow};
    use serde_json::json;
    use uuid::Uuid;

    const TENANT: Uuid = Uuid::from_u128(0x7e7e);

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

    fn ent(n: u128, layer: LayerKey, key: &str) -> OverlayRow {
        OverlayRow {
            id: Uuid::from_u128(n),
            layer,
            kind: Kind::Entity,
            key: key.into(),
            op: Op::Add,
            fields: json!({"label": key})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        }
    }

    fn known() -> Known {
        Known {
            industries: ["retail".to_owned()].into(),
            departments: ["a".to_owned(), "b".to_owned()].into(),
            ..Known::default()
        }
    }

    fn project(depts: &[&str], with_industry: bool) -> CarebusOntology {
        let sel = Selection {
            industry: with_industry.then(|| "retail".to_owned()),
            departments: depts.iter().map(|d| (*d).to_owned()).collect(),
            tenant: Some(TENANT),
            ..Selection::default()
        };
        let industry = LayerKey {
            industry: Some("retail".into()),
            ..layer(Scope::Industry)
        };
        let dept = LayerKey {
            industry: Some("retail".into()),
            department: Some("a".into()),
            ..layer(Scope::IndustryDept)
        };
        let tenant = LayerKey {
            tenant: Some(TENANT),
            ..layer(Scope::Tenant)
        };
        let mut rows = vec![
            ent(1, layer(Scope::Base), "Base"),
            ent(3, industry, "Ind"),
            ent(5, tenant, "Ten"),
        ];
        if !depts.is_empty() {
            rows.push(ent(4, dept, "Dept"));
        }
        let cfg = compile(&sel, &known(), &[], &rows).expect("compiles");
        project_carebus(&cfg)
    }

    fn find<'a>(o: &'a CarebusOntology, name: &str) -> &'a CbEntity {
        o.entities.iter().find(|e| e.name == name).expect("entity")
    }

    fn triple(e: &CbEntity) -> (&str, Option<&str>, Option<&str>) {
        (
            e.scope.as_str(),
            e.industry.as_deref(),
            e.department.as_deref(),
        )
    }

    #[test]
    fn projection_scope_mapping() {
        let one = project(&["a"], true);
        assert_eq!(triple(find(&one, "Base")), ("generic", None, None));
        assert_eq!(
            triple(find(&one, "Ind")),
            ("industry", Some("retail"), None)
        );
        assert_eq!(
            triple(find(&one, "Dept")),
            ("industry_dept", Some("retail"), Some("a"))
        );
        // tenant add with exactly one department -> industry_dept
        assert_eq!(
            triple(find(&one, "Ten")),
            ("industry_dept", Some("retail"), Some("a"))
        );

        // tenant add with two departments -> industry
        let two = project(&["a", "b"], true);
        assert_eq!(
            triple(find(&two, "Ten")),
            ("industry", Some("retail"), None)
        );

        // tenant add with no industry -> generic
        let none = project(&[], false);
        assert_eq!(none.entities.len(), 2);
        assert_eq!(triple(find(&none, "Ten")), ("generic", None, None));
    }
}
