//! Canonical JSON and the effective hash (spec section 5.3).

use std::fmt::Write as _;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::compile::EffectiveConfig;

/// Canonical JSON: object keys sorted recursively, no whitespace. Numbers and
/// strings are printed exactly as `serde_json` prints them.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_value(v, &mut out);
    out
}

fn write_string(s: &str, out: &mut String) {
    // Serializing a `str` cannot fail.
    out.push_str(&serde_json::to_string(s).unwrap_or_default());
}

fn write_value(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            let _ = write!(out, "{n}");
        }
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write_value(&map[k], out);
            }
            out.push('}');
        }
    }
}

pub fn sha256_hex(s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// The hash of an effective config. Provenance, warnings and the hash itself
/// are excluded, so a revision that changes nothing does not change it.
pub fn effective_hash(e: &EffectiveConfig) -> String {
    let v = json!({
        "selection": e.selection,
        "entities": e.entities,
        "attributes": e.attributes,
        "relations": e.relations,
        "job": e.job,
    });
    sha256_hex(&canonical_json(&v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::{Known, Selection, compile};
    use crate::layer::{LayerKey, Scope};
    use crate::overlay::{Kind, Op, OverlayRow};
    use uuid::Uuid;

    fn layer(scope: Scope, industry: Option<&str>) -> LayerKey {
        LayerKey {
            scope,
            industry: industry.map(str::to_owned),
            department: None,
            job: None,
            rule: None,
            dimension: None,
            tenant: None,
        }
    }

    fn row(n: u128, layer: LayerKey, kind: Kind, key: &str, fields: Value) -> OverlayRow {
        OverlayRow {
            id: Uuid::from_u128(n),
            layer,
            kind,
            key: key.into(),
            op: Op::Add,
            fields: fields.as_object().cloned().unwrap_or_default(),
        }
    }

    fn sel() -> Selection {
        Selection {
            industry: Some("retail".into()),
            ..Selection::default()
        }
    }

    fn known() -> Known {
        Known {
            industries: ["retail".to_owned()].into(),
            ..Known::default()
        }
    }

    fn rows(id_offset: u128, label: &str) -> Vec<OverlayRow> {
        let base = layer(Scope::Base, None);
        let ind = layer(Scope::Industry, Some("retail"));
        vec![
            row(
                id_offset + 1,
                base.clone(),
                Kind::Entity,
                "Order",
                json!({"label": label}),
            ),
            row(
                id_offset + 2,
                base.clone(),
                Kind::Entity,
                "Payment",
                json!({"label": "Payment"}),
            ),
            row(
                id_offset + 3,
                ind.clone(),
                Kind::Attribute,
                "Order.order_id",
                json!({"data_type": "string"}),
            ),
            row(
                id_offset + 4,
                base,
                Kind::Relation,
                "Order|paidBy|Payment",
                json!({"cardinality": "1:n"}),
            ),
            row(
                id_offset + 5,
                ind,
                Kind::Entity,
                "Refund",
                json!({"label": "Refund"}),
            ),
        ]
    }

    fn hash_of(rows: &[OverlayRow]) -> String {
        compile(&sel(), &known(), &[], rows).expect("compiles").hash
    }

    #[test]
    fn hash_is_stable_under_row_permutation() {
        let original = rows(0, "Order");
        let expected = hash_of(&original);
        assert_eq!(expected.len(), 64);
        for k in 0..10 {
            let mut rotated = original.clone();
            rotated.rotate_left(k % original.len());
            if k >= original.len() {
                rotated.reverse();
            }
            assert_eq!(hash_of(&rotated), expected, "rotation {k}");
        }
    }

    #[test]
    fn hash_ignores_provenance() {
        assert_eq!(hash_of(&rows(0, "Order")), hash_of(&rows(1000, "Order")));
    }

    #[test]
    fn hash_changes_with_content() {
        assert_ne!(hash_of(&rows(0, "Order")), hash_of(&rows(0, "Purchase")));
    }

    #[test]
    fn canonical_sorts_nested_keys() {
        let a = json!({"b": {"y": 1, "x": [true, null, {"d": 1, "c": "q\"z"}]}, "a": 1.5});
        assert_eq!(
            canonical_json(&a),
            r#"{"a":1.5,"b":{"x":[true,null,{"c":"q\"z","d":1}],"y":1}}"#
        );
        let b: Value =
            serde_json::from_str(r#"{"a":1.5,"b":{"x":[true,null,{"c":"q\"z","d":1}],"y":1}}"#)
                .expect("json");
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
