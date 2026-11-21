-- How a run executed as one Batch job on one host (the anchor's `single_job`: its queue-class,
-- instance type, vCPUs and memory), as JSON. NULL for a run that executed one Batch job per task,
-- and on the rows written before this column until their runs are replayed.
ALTER TABLE run_projection ADD COLUMN single_job JSONB;
