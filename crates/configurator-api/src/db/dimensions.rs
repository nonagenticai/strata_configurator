//! Dimension values are standard or private to a tenant.
use serde::Serialize;
use sqlx::PgConnection;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Dimension {
    pub key: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DimensionValue {
    pub id: Uuid,
    pub dimension: String,
    pub key: String,
    pub label: String,
    pub tenant: Option<Uuid>,
}

pub async fn load(conn: &mut PgConnection) -> Result<Vec<Dimension>, sqlx::Error> {
    sqlx::query_as("SELECT key, label FROM dimension ORDER BY key")
        .fetch_all(conn)
        .await
}

pub async fn values(
    conn: &mut PgConnection,
    tenant: Option<Uuid>,
) -> Result<Vec<DimensionValue>, sqlx::Error> {
    sqlx::query_as("SELECT id, dimension, key, label, tenant FROM dimension_value WHERE tenant IS NULL OR tenant = $1 ORDER BY dimension, key, id")
        .bind(tenant).fetch_all(conn).await
}
