-- How many of a run's tasks its cancel ended: open when the run was cancelled, or failed only once
-- the cancel had been requested (`RunProjection::effective_task_state`). Materialized beside the
-- other per-outcome counts so a run-list read stays a plain row fetch. 0 on the rows written
-- before this column until their runs are replayed.
ALTER TABLE run_projection ADD COLUMN tasks_cancelled BIGINT NOT NULL DEFAULT 0;
