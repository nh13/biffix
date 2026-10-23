//! The Postgres-backed [`ReferenceStore`] (the `reference_bundles` table).

use async_trait::async_trait;
use biffix_api::ReferenceBundle;
use sqlx::{PgPool, Row};

use crate::store::references::ReferenceStore;
use crate::store::StoreError;

/// Postgres-backed [`ReferenceStore`] (the `reference_bundles` table).
pub struct PgReferenceStore {
    pool: PgPool,
}

impl PgReferenceStore {
    /// A store over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn row_to_bundle(r: &sqlx::postgres::PgRow) -> Result<ReferenceBundle, StoreError> {
    let uris: serde_json::Value = r.get("uris");
    Ok(ReferenceBundle {
        id: r.get("id"),
        kind: r.get("kind"),
        display_name: r.get("display_name"),
        uris: serde_json::from_value(uris)?,
        notes: r.get("notes"),
        igv_genome: r.get("igv_genome"),
        created_at_micros: r.get("created_at"),
        updated_at_micros: r.get("updated_at"),
        created_by: r.get("created_by"),
    })
}

fn uris_json(bundle: &ReferenceBundle) -> Result<serde_json::Value, StoreError> {
    Ok(serde_json::to_value(&bundle.uris)?)
}

#[async_trait]
impl ReferenceStore for PgReferenceStore {
    async fn list_bundles(&self) -> Result<Vec<ReferenceBundle>, StoreError> {
        sqlx::query("SELECT * FROM reference_bundles ORDER BY id COLLATE \"C\"")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(row_to_bundle)
            .collect()
    }

    async fn get_bundle(&self, id: &str) -> Result<ReferenceBundle, StoreError> {
        let row = sqlx::query("SELECT * FROM reference_bundles WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)?;
        row_to_bundle(&row)
    }

    async fn create_bundle(&self, b: &ReferenceBundle) -> Result<(), StoreError> {
        let res = sqlx::query(
            "INSERT INTO reference_bundles \
               (id, kind, display_name, uris, notes, created_at, created_by, updated_at, \
                igv_genome) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT (id) DO NOTHING",
        )
        .bind(&b.id)
        .bind(&b.kind)
        .bind(&b.display_name)
        .bind(uris_json(b)?)
        .bind(&b.notes)
        .bind(b.created_at_micros)
        .bind(&b.created_by)
        .bind(b.updated_at_micros)
        .bind(&b.igv_genome)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(StoreError::Conflict(format!("reference bundle {} already exists", b.id)));
        }
        Ok(())
    }

    async fn update_bundle(&self, b: &ReferenceBundle) -> Result<ReferenceBundle, StoreError> {
        // The creation columns are not in the SET list: the row keeps its own, and RETURNING
        // hands back the bundle as stored.
        let row = sqlx::query(
            "UPDATE reference_bundles SET kind = $2, display_name = $3, uris = $4, \
               notes = $5, updated_at = $6, igv_genome = $7 WHERE id = $1 RETURNING *",
        )
        .bind(&b.id)
        .bind(&b.kind)
        .bind(&b.display_name)
        .bind(uris_json(b)?)
        .bind(&b.notes)
        .bind(b.updated_at_micros)
        .bind(&b.igv_genome)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        row_to_bundle(&row)
    }

    async fn delete_bundle(&self, id: &str) -> Result<(), StoreError> {
        let res = sqlx::query("DELETE FROM reference_bundles WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }
}
