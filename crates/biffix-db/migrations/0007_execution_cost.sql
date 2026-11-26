-- The estimated cost of each execution of a Batch job (one ECS task, an `execution_projection`
-- row), priced on its own host from its own start to its own stop, so a job Batch re-ran after a
-- spot reclaim costs every run it made. Written by the reconcile tick's pricer, for an execution
-- that ended and has a start, with the execution's host, queue, start and stop it was priced from:
-- the fold can still change those (a later fact, a replay after a reducer fix), and a line whose
-- inputs no longer match its execution's is owed again (`biffix_db::rules::priced_from`), so the
-- pricer re-prices it in place. Until then it stays the job's estimate, and the job counts as
-- pending; one that has left the pricing window keeps its last estimate.
-- `attempt_cost` keeps its meaning, one job-level line priced over the job's span, for a job the
-- pricer cannot price by execution (it has no ended execution with a start), and its own writer:
-- an older pricer's `ON CONFLICT (run_id, batch_job_id)` keeps working during a rolling deploy.
CREATE TABLE execution_cost (
    run_id            TEXT NOT NULL,
    execution_id      TEXT NOT NULL,     -- ECS task ARN
    batch_job_id      TEXT NOT NULL,
    role              TEXT NOT NULL,     -- task | coordinator
    instance_type     TEXT NOT NULL,
    availability_zone TEXT,
    lifecycle         TEXT,              -- spot | on_demand | NULL (unknown)
    hours             DOUBLE PRECISION NOT NULL,
    price_per_hour    DOUBLE PRECISION NOT NULL,   -- average compute USD/hour over the execution
    price_source      TEXT NOT NULL,     -- as attempt_cost.price_source
    compute_usd       DOUBLE PRECISION NOT NULL,
    ebs_usd           DOUBLE PRECISION NOT NULL,
    -- the execution_projection inputs it was priced from
    instance_id       TEXT NOT NULL,     -- container-instance id (host_registry key)
    queue             TEXT,
    started_at        BIGINT NOT NULL,   -- epoch micros
    stopped_at        BIGINT NOT NULL,   -- epoch micros
    priced_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, execution_id)
);

-- A job's execution lines, for the view's anti-join and the per-job reads.
CREATE INDEX execution_cost_job ON execution_cost (run_id, batch_job_id);

-- An execution the pricer can never price, and why, so it is neither priced nor counted as pending
-- again while its inputs (as `execution_cost`'s, NULL where the execution had none) are the ones it
-- was recorded from; once the fold changes them, the record no longer counts and is dropped.
-- `start_unknown` (it ended with no start recorded, so it has no run time) only records why it has
-- no cost: it is dropped, and the execution priced, if the start is filled in later.
CREATE TABLE unpriceable_execution (
    run_id       TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    reason       TEXT NOT NULL,
    instance_id  TEXT,
    queue        TEXT,
    started_at   BIGINT,
    stopped_at   BIGINT,
    recorded_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, execution_id)
);

-- Every cost line: a job's execution lines, else its legacy job-level line, never both, so no
-- reader can count a job twice. A legacy line written before the job's executions were priced
-- (or by an older pricer mid-deploy) is hidden once the job has an execution line.
-- `execution_id` is NULL on a legacy line.
CREATE VIEW cost_line AS
    SELECT run_id, batch_job_id, execution_id, role, instance_type, availability_zone, lifecycle,
           hours, price_per_hour, price_source, compute_usd, ebs_usd
      FROM execution_cost
    UNION ALL
    SELECT c.run_id, c.batch_job_id, NULL::text, c.role, c.instance_type, c.availability_zone,
           c.lifecycle, c.hours, c.price_per_hour, c.price_source, c.compute_usd, c.ebs_usd
      FROM attempt_cost c
     WHERE NOT EXISTS (SELECT 1 FROM execution_cost x
                        WHERE x.run_id = c.run_id AND x.batch_job_id = c.batch_job_id);
