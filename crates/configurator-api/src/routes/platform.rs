//! Export current platform data, excluding all tenant content, as a fresh seed bundle.
use crate::{
    auth::{Caller, require_super_admin},
    db::{axes, dimensions, profiles, rows},
    error::ApiError,
    state::AppState,
};
use axum::{Json, Router, extract::State, routing::get};
use configurator_core::{
    layer::Scope,
    seed::{self, Dimension, RowSpec, SeedBundle, SeedLayer, SeedProfile, SeedRule},
};
use serde_json::{Value, json};

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/platform/export", get(export))
}

async fn export(State(st): State<AppState>, caller: Caller) -> Result<Json<Value>, ApiError> {
    require_super_admin(&caller)?;
    // Platform data only: the owner pool, one read-only snapshot.
    let mut tx = st.snapshot(None).await?;
    let version: i32 = sqlx::query_scalar("SELECT coalesce(max(version), 0) + 1 FROM seed_version")
        .fetch_one(&mut *tx)
        .await?;
    let axes = axes::load(&mut tx).await?;
    let dims = dimensions::load(&mut tx).await?;
    let values = dimensions::values(&mut tx, None).await?;
    let dimensions = dims
        .into_iter()
        .map(|d| Dimension {
            values: values
                .iter()
                .filter(|v| v.dimension == d.key)
                .map(|v| (v.key.clone(), v.label.clone()))
                .collect(),
            key: d.key,
            label: d.label,
        })
        .collect();
    let profiles = profiles::load(&mut tx, None)
        .await?
        .into_iter()
        .map(|p| SeedProfile {
            source: "profiles.toml".into(),
            name: p.name,
            values: p.values,
        })
        .collect();
    let mut rules: Vec<_> = axes::rules(&mut tx)
        .await?
        .into_iter()
        .map(|rule| SeedRule {
            source: "rules.toml".into(),
            rule,
            rows: vec![],
        })
        .collect();
    let mut layers = Vec::new();
    for layer in super::compile::platform_layers(&mut tx).await? {
        let specs = rows::load_for_layers_conn(&mut tx, std::slice::from_ref(&layer))
            .await?
            .into_iter()
            .map(|r| RowSpec {
                kind: r.kind,
                key: r.key,
                op: r.op,
                fields: r.fields,
            })
            .collect();
        if layer.scope == Scope::Profile {
            let rule = rules
                .iter_mut()
                .find(|r| Some(&r.rule.key) == layer.rule.as_ref())
                .ok_or_else(|| {
                    ApiError::Conflict(
                        "orphan_profile_layer",
                        "a profile layer has no axis rule; create its rule before exporting".into(),
                    )
                })?;
            rule.rows = specs;
        } else {
            layers.push(SeedLayer {
                source: "layers.toml".into(),
                layer,
                rows: specs,
            });
        }
    }
    let bundle = SeedBundle {
        version: version as u32,
        axes,
        dimensions,
        profiles,
        rules,
        layers,
    };
    let files = seed::render(&bundle);
    // Never return a success-shaped export which cannot be parsed back into a seed.
    let borrowed: Vec<_> = files
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    seed::parse(bundle.version, &borrowed)
        .map_err(|e| ApiError::Conflict("invalid_platform_export", e.to_string()))?;
    tx.commit().await?;
    Ok(Json(
        json!({"version":version,"files": files.into_iter().collect::<std::collections::BTreeMap<_,_>>()}),
    ))
}
