//! Profile visibility is enforced in the database query, including lookups by id.
use configurator_core::axis::Profile;
use serde::Serialize;
use sqlx::PgConnection;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct SavedProfile {
    pub id: Uuid,
    pub tenant: Option<Uuid>,
    pub name: String,
    pub values: Profile,
}

pub async fn load(
    conn: &mut PgConnection,
    tenant: Option<Uuid>,
) -> Result<Vec<SavedProfile>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (Uuid, Option<Uuid>, String, serde_json::Value)>(
        "SELECT id, tenant, name, \"values\" FROM profile WHERE tenant IS NULL OR tenant = $1 ORDER BY id DESC",
    ).bind(tenant).fetch_all(conn).await?;
    rows.into_iter()
        .map(|(id, tenant, name, values)| {
            Ok(SavedProfile {
                id,
                tenant,
                name,
                values: serde_json::from_value(values)
                    .map_err(|e| sqlx::Error::Decode(e.into()))?,
            })
        })
        .collect()
}

pub async fn get(
    conn: &mut PgConnection,
    tenant: Option<Uuid>,
    id: Uuid,
) -> Result<Option<SavedProfile>, sqlx::Error> {
    Ok(load(conn, tenant).await?.into_iter().find(|p| p.id == id))
}
