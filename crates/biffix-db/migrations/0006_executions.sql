-- One row per execution of a Batch job: each time AWS ran the job on a host (one ECS task), under
-- the run that bound the job. A job Batch retries itself (a reclaimed spot host) has several, and
-- is still one `attempt_projection` row. A projection, kept equal to the fold of the run's
-- `batch_execution_started` and `batch_execution_ended` events: the fold upserts the rows it
-- produces and deletes the ones it no longer does. A run has rows once its log holds execution
-- events; a run recorded before those events existed has none, and a replay alone gives it none.
CREATE TABLE execution_projection (
    run_id               TEXT NOT NULL,
    execution_id         TEXT NOT NULL,     -- ECS task ARN
    batch_job_id         TEXT NOT NULL,
    role                 TEXT NOT NULL,     -- task | coordinator, by the job's binding
    task_key             TEXT,              -- NULL for a coordinator
    -- 1-based display position within the job's binding (task or coordinator role)
    ordinal              INT  NOT NULL,
    -- the last execution of the job's binding (task or coordinator role) in that order
    is_final             BOOLEAN NOT NULL,
    ended                BOOLEAN NOT NULL,
    instance_id          TEXT,              -- container-instance id (host_registry key)
    queue                TEXT,
    log_stream           TEXT,
    started_at           BIGINT,
    stopped_at           BIGINT,
    exit_code            INT,
    status_reason        TEXT,
    termination_category TEXT,
    PRIMARY KEY (run_id, execution_id)
);

-- A job's executions in display order, for the reads that list them under their attempt.
CREATE INDEX execution_projection_job ON execution_projection (run_id, batch_job_id, ordinal);

-- What an attempt's span (`started_at` to `finished_at`, unchanged) was made of, in microseconds,
-- from its executions: the time its containers ran, the part of that spent in executions lost with
-- their host, and the time the job waited for a host between executions. A projection of the same
-- fold. All three are NULL for an attempt with no execution facts (which is not zero): they are
-- set once the job's execution events are in the log, and a replay alone sets none.
ALTER TABLE attempt_projection
    ADD COLUMN run_micros BIGINT, ADD COLUMN lost_micros BIGINT, ADD COLUMN wait_micros BIGINT;

-- When the reconcile tick last described a finished job to complete its execution events, in
-- epoch microseconds. Not a projection: the tick's own bookkeeping, like the row's other columns.
-- NULL until that has happened.
ALTER TABLE reconcile_describe ADD COLUMN executions_described_micros BIGINT;
