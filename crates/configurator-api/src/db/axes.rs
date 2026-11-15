//! Platform axes and conditional rules.
use configurator_core::axis::{Axis, AxisValue};
use configurator_core::compile::AxisRule;
use sqlx::PgConnection;

pub async fn load(conn: &mut PgConnection) -> Result<Vec<Axis>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (String, String, Option<String>, Option<String>)>(
        "SELECT key, cardinality, label, description FROM axis ORDER BY key",
    )
    .fetch_all(&mut *conn)
    .await?;
    let values = sqlx::query_as::<_, (String, String, Option<String>, String, Option<String>)>(
        "SELECT axis, key, parent, label, description FROM axis_value ORDER BY axis, key",
    )
    .fetch_all(&mut *conn)
    .await?;
    rows.into_iter()
        .map(|(key, cardinality, label, description)| {
            let axis_values = values
                .iter()
                .filter(|v| v.0 == key)
                .map(|v| AxisValue {
                    key: v.1.clone(),
                    parent: v.2.clone(),
                    label: Some(v.3.clone()),
                    description: v.4.clone(),
                })
                .collect();
            Ok(Axis {
                key,
                cardinality: super::from_text(&cardinality)?,
                label,
                description,
                values: axis_values,
            })
        })
        .collect()
}

pub async fn rules(conn: &mut PgConnection) -> Result<Vec<AxisRule>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (String, i32, serde_json::Value)>(
        "SELECT key, priority, condition FROM axis_rule ORDER BY priority, key",
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|(key, priority, condition)| {
            Ok(AxisRule {
                key,
                priority,
                condition: serde_json::from_value(condition)
                    .map_err(|e| sqlx::Error::Decode(e.into()))?,
            })
        })
        .collect()
}
