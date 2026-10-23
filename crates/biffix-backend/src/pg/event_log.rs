//! [`EventLogReads`] for [`PgStore`]: a run's control-plane singletons from `run_events`.

use async_trait::async_trait;
use sqlx::Row;

use biffix_core::event::RunEvent;
use biffix_core::ids::RunId;

use super::PgStore;
use crate::store::event_log::EventLogReads;
use crate::store::StoreError;

#[async_trait]
impl EventLogReads for PgStore {
    async fn run_event(
        &self,
        run_id: &RunId,
        kind: &biffix_core::event::Kind,
    ) -> Result<Option<RunEvent>, StoreError> {
        // The control plane's own: an engine's event of the same kind (its `run_failed`) never
        // stands in for it.
        let row = sqlx::query(
            "SELECT payload FROM run_events WHERE run_id = $1 AND kind = $2 AND source = $3 \
             ORDER BY ts_micros, seq LIMIT 1",
        )
        .bind(run_id.as_str())
        .bind(kind.as_str())
        .bind(biffix_core::source::EventSource::BiffixControlPlane.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            serde_json::from_value(r.get::<serde_json::Value, _>("payload")).map_err(|e| {
                StoreError::Serialization(format!("undecodable {}: {e}", kind.as_str()))
            })
        })
        .transpose()
    }
}
