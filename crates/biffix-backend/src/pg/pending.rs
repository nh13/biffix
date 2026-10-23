//! [`PendingLaunchStore`] for [`PgStore`]: `pending_launch` and the launch claim.

use async_trait::async_trait;
use sqlx::Row;

use biffix_core::ids::RunId;

use super::PgStore;
use crate::store::pending::PendingLaunchStore;
use crate::store::StoreError;

#[async_trait]
impl PendingLaunchStore for PgStore {
    async fn put_pending_launch(
        &self,
        pending: &crate::store::pending::PendingLaunch,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO pending_launch (run_id, created_at, plan) VALUES ($1, $2, $3) \
             ON CONFLICT (run_id) DO UPDATE SET created_at = EXCLUDED.created_at, \
               plan = EXCLUDED.plan",
        )
        .bind(pending.run_id.as_str())
        .bind(pending.created_at_micros)
        .bind(&pending.plan)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_pending_launches(
        &self,
    ) -> Result<Vec<crate::store::pending::PendingLaunch>, StoreError> {
        let rows = sqlx::query(
            "SELECT run_id, created_at, plan FROM pending_launch ORDER BY created_at, run_id COLLATE \"C\"",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(pending_launch).collect())
    }

    async fn get_pending_launch(
        &self,
        run_id: &RunId,
    ) -> Result<Option<crate::store::pending::PendingLaunch>, StoreError> {
        let row =
            sqlx::query("SELECT run_id, created_at, plan FROM pending_launch WHERE run_id = $1")
                .bind(run_id.as_str())
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.as_ref().map(pending_launch))
    }

    async fn delete_pending_launch(&self, run_id: &RunId) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM pending_launch WHERE run_id = $1")
            .bind(run_id.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn mark_pending_launch_orphan(
        &self,
        run_id: &RunId,
        job_id: &str,
    ) -> Result<(), StoreError> {
        sqlx::query("UPDATE pending_launch SET plan = jsonb_set(plan, $2, to_jsonb($3::text)) WHERE run_id = $1")
            .bind(run_id.as_str())
            .bind(vec![crate::store::pending::PENDING_ORPHAN_KEY])
            .bind(job_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn claim_run_launch(
        &self,
        run_id: &RunId,
    ) -> Result<Option<crate::store::Claim>, StoreError> {
        // As `claim_output_prefix`: a transaction-scoped advisory lock the claim's open
        // transaction holds, namespaced by its own salt.
        let mut tx = self.pool.begin().await?;
        let claimed: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_xact_lock(hashtextextended('biffix:launch:' || $1, 0))",
        )
        .bind(run_id.as_str())
        .fetch_one(&mut *tx)
        .await?;
        Ok(claimed.then(|| crate::store::Claim::new(tx)))
    }
}

/// A `pending_launch` row (`run_id, created_at, plan`).
fn pending_launch(r: &sqlx::postgres::PgRow) -> crate::store::pending::PendingLaunch {
    crate::store::pending::PendingLaunch {
        run_id: RunId::new(r.get::<String, _>("run_id")),
        created_at_micros: r.get("created_at"),
        plan: r.get("plan"),
    }
}
