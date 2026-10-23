-- The biffix schema.
--
-- Every table is one of four kinds, and the kind says how a row may change:
--   * the EVENT LOG (`run_events`): append-only, the source of truth;
--   * PROJECTIONS: deterministic folds of the log, rebuilt by a replay (refold) and never patched
--     by hand — a projection bug is fixed in the reducer and replayed;
--   * FACT CACHES: external facts the log does not carry (ECS hosts, S3 metadata, prices), recorded
--     as they are observed and rebuildable from their source;
--   * DOMAIN and USER STATE: rows the API writes (workflow versions, experiments, reads,
--     preferences); a replay never touches them.
-- Times are epoch microseconds (BIGINT) unless the column is a TIMESTAMPTZ.
--
-- Schema changes are new numbered migration files: an applied migration is checksum-locked, so
-- this file is never edited once it has been applied.

-- Trigram indexes make the leading-wildcard `ILIKE '%term%'` run search index-usable without
-- changing its semantics (unlike full-text search, which is word-based). pg_trgm is a standard
-- contrib extension available on Aurora Postgres; CREATE EXTENSION needs the privilege the app role
-- has on a biffix cluster (if a locked-down role lacks it, granting it is a deploy concern, not a
-- schema one).
CREATE EXTENSION IF NOT EXISTS pg_trgm;


-- ════ Event log ═══════════════════════════════════════════════════════════════════════════════════

-- The append-only log of normalized RunEvents. Every producer appends here; everything the UI shows
-- is a projection of it. `dedup_key` makes an append idempotent; `run_id` is nullable (late
-- attribution, and events that belong to no run, such as build and spot warning events).
CREATE TABLE run_events (
    seq            BIGSERIAL PRIMARY KEY,
    dedup_key      TEXT NOT NULL UNIQUE,
    ts_micros      BIGINT NOT NULL,
    source         TEXT NOT NULL,
    kind           TEXT NOT NULL,
    run_id         TEXT,
    task_key       TEXT,
    batch_job_id   TEXT,
    schema_version INT NOT NULL,
    payload        JSONB NOT NULL,
    received_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- One run's events: the run fold's per-run read.
CREATE INDEX run_events_run_ts ON run_events (run_id, ts_micros);
-- The provenance sweep asks each ended run for its newest log `seq`; with this index that is one
-- backward index probe per run rather than a scan of the run's events.
CREATE INDEX run_events_run_seq ON run_events (run_id, seq);
-- The reconciliation poller groups bound lifecycle facts by Batch job id on every schedule. Keep
-- that lookup proportional to active/bound jobs rather than the whole append-only log.
CREATE INDEX run_events_batch_lifecycle
    ON run_events (batch_job_id, ts_micros, source, kind)
    WHERE batch_job_id IS NOT NULL;
-- The Batch jobs bound to a run (a task's `task_submitted` claim, the control plane's
-- `coordinator_submitted`), by when. The reconcile backstop starts from the jobs bound in a recent
-- window and probes each one's facts, so its candidate scan is proportional to that window rather
-- than to the whole append-only log. The kinds are `biffix_core::event::Kind::TaskSubmitted` and
-- `Kind::CoordinatorSubmitted`; the backstop's queries name them the same way.
CREATE INDEX run_events_job_binding
    ON run_events (ts_micros, batch_job_id)
    WHERE batch_job_id IS NOT NULL AND kind IN ('task_submitted', 'coordinator_submitted');
-- The build fold reads one subject's events; build events carry no run_id, so key them by subject.
CREATE INDEX run_events_build_subject
    ON run_events ((payload->>'subject'), ts_micros, seq)
    WHERE kind IN ('build_requested', 'build_started', 'build_succeeded', 'build_failed');

-- How far each projection has folded the log: the `seq` of the last event it has applied.
CREATE TABLE projection_cursors (
    projection TEXT PRIMARY KEY,
    last_seq   BIGINT NOT NULL DEFAULT 0
);


-- ════ Run, task and attempt projections ═══════════════════════════════════════════════════════════

-- One row per run: the fold of its events.
CREATE TABLE run_projection (
    run_id                 TEXT PRIMARY KEY,
    state                  TEXT NOT NULL,
    completed              BIGINT,
    total                  BIGINT,
    -- When the user submitted the run, from its anchor. NULL until the anchor is folded.
    submitted_at           BIGINT,
    completed_at           BIGINT,
    submitter              TEXT,
    submitter_label        TEXT,
    coordinator_job_id     TEXT,
    workflow_version       TEXT,
    -- Resolved params at submit, already secret-redacted. Detail-only (not read by list_runs).
    params                 JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- The run's group-level dependency graph {nodes:[id], edges:[[from,to]]}, folded from run_dag.
    -- JSONB because it is opaque nested JSON, genuinely document-shaped.
    dag                    JSONB NOT NULL DEFAULT '{"nodes":[],"edges":[]}'::jsonb,
    -- The wall-clock time the row was last written, which every event and every replay moves. Write
    -- metadata only: nothing orders or filters on it, so it is unindexed.
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Per-run task-outcome counts, written by the fold so the run list reads a plain row instead of
    -- aggregating task_projection. Zero for a run with no tasks yet.
    tasks_observed         BIGINT NOT NULL DEFAULT 0,
    tasks_succeeded        BIGINT NOT NULL DEFAULT 0,
    tasks_running          BIGINT NOT NULL DEFAULT 0,
    tasks_failed           BIGINT NOT NULL DEFAULT 0,
    tasks_cached           BIGINT NOT NULL DEFAULT 0,
    -- A native text[] rather than a JSONB array re-parsed per row.
    input_uris             TEXT[] NOT NULL DEFAULT '{}',
    -- Tasks whose live Batch attempts are all still RUNNABLE (queued, waiting on capacity): they
    -- project to TaskState::Queued rather than being counted as running.
    tasks_queued           BIGINT NOT NULL DEFAULT 0,
    -- The run's effective output prefix (the engine's default storage prefix), from the anchor; a
    -- run may have been submitted with a caller-chosen `--output-prefix`. NULL until the anchor
    -- folds, or when the run had no project bucket to write to.
    output_prefix          TEXT,
    -- The coordinator's CloudWatch log stream and ECS container instance, latched from its Batch
    -- lifecycle events.
    coordinator_log_stream TEXT,
    coordinator_host_id    TEXT,
    -- Batch's account of a failed coordinator job's stop, latched from its `coordinator_failed`:
    -- the exit code (NULL when Batch reported none), the stop reason (for a coordinator that never
    -- started, e.g. `CannotPullContainerError: …`) and the termination category read from it. The
    -- run's failure summary reads them when the engine wrote no log to explain itself.
    coordinator_exit_code            INTEGER,
    coordinator_status_reason        TEXT,
    coordinator_termination_category TEXT,
    -- The engine session the run executed as (Nextflow: the cloud-cache key under
    -- <output_prefix>.nextflow/cache/<id>/) and the run it resumed, from the anchor.
    -- engine_session_id prefers the id the engine itself reported on run_started; session_mismatch
    -- records that the reported id differed from the one biffix launched it with (the NXF_UUID
    -- pre-choice did not take). NULL/false for Snakemake runs.
    engine_session_id      TEXT,
    session_mismatch       BOOLEAN NOT NULL DEFAULT false,
    resumed_from           TEXT,
    -- Output counts, materialized so lists and the run page can badge outputs without a count
    -- query. outputs_total excludes work-dir intermediates. outputs_digest gates the run_output
    -- upsert: an advance that did not change the output set does not rewrite its (possibly
    -- thousands of) rows.
    outputs_total          BIGINT NOT NULL DEFAULT 0,
    outputs_reused         BIGINT NOT NULL DEFAULT 0,
    outputs_digest         TEXT,
    -- The run's human name, given at submit (the UI pre-fills it from the workflow's run-name
    -- template) or by a later rename. NULL for a run without one.
    run_name               TEXT,
    -- Samplesheets biffix wrote for the run from rows given at submit (param → {uri, etag, rows}),
    -- from the anchor, so a re-run can start from the same samples. NULL for a run without.
    input_tables           JSONB,
    -- The reference bundle the run was launched with and the URIs it filled ({id, uris}), from the
    -- anchor. NULL for a run launched without one.
    reference_bundle       JSONB,
    -- How many samples the run was launched with (rows of the samplesheet biffix wrote for it), from
    -- the anchor; the per-sample basis of a workflow's cost estimate. NULL when unknown.
    sample_count           INTEGER,
    -- What happens to the run's intermediate files (`default`, `delete_on_success`, `keep`; NULL ⇒
    -- default), from the anchor, and when the retention sweep deleted them (from `work_deleted`;
    -- NULL while they exist).
    retention              TEXT,
    work_deleted_at        BIGINT,
    -- When the run launched (reducer `launched_at_micros`: the earliest of its anchor and
    -- coordinator lifecycle events). `submitted_at` is when the user submitted, which for a run that
    -- waited for its submit-time images is earlier; "stuck in submitted" and wall-time estimates
    -- measure from `COALESCE(launched_at, submitted_at)`. NULL until the run launches.
    launched_at            BIGINT,
    -- The run's samples (the anchor's `samples`: sample ids + the version's matching hint) and how
    -- they stand (`SampleGrid::counts` over the fold). NULL counts when no task names a sample (a
    -- run with no per-sample steps).
    samples                JSONB,
    samples_total          INTEGER,
    samples_done           INTEGER,
    samples_failed         INTEGER,
    samples_declared       BOOLEAN,
    -- Who asked to cancel the run, for a person (`RunProjection::cancelled_by`): set only on a run
    -- that ended cancelled on a recorded request.
    cancelled_by           TEXT,
    -- The experiment the run is in (`RunProjection::experiment_id`), folded from the anchor's
    -- `experiment_id` and the latest `experiment_set`.
    experiment_id          TEXT
);
-- The run list pages on a fold-derived key, not `updated_at`: `submitted_at`, with a run whose
-- anchor is not folded yet sorting first (newest), then `run_id` in byte order. The expression here
-- must match `RUN_SORT_KEY` in the backend's `pg/runs.rs`, or the list stops using the index.
CREATE INDEX run_projection_submitted_order
    ON run_projection ((COALESCE(submitted_at, 9223372036854775807)) DESC, run_id COLLATE "C" DESC);
-- The workflows page groups runs by version.
CREATE INDEX run_projection_workflow_version ON run_projection (workflow_version);
-- Experiment membership. The partial index holds every experiment-bound run; a caller's runs per
-- experiment (`experiment_runs_of`: `GET /experiments?mine=true` and each experiment's `mine` /
-- `needs_attention`, read on every experiments list, detail and update) read the
-- submitter-keyed one, which does not cost all users' experiment runs.
CREATE INDEX run_projection_experiment
    ON run_projection (experiment_id) WHERE experiment_id IS NOT NULL;
CREATE INDEX run_projection_submitter_experiment
    ON run_projection (submitter, experiment_id)
    WHERE experiment_id IS NOT NULL;
-- Run search: `ILIKE '%term%'` over these columns (and the workflow's name, project and engine).
CREATE INDEX run_projection_run_id_trgm
    ON run_projection USING gin (run_id gin_trgm_ops);
CREATE INDEX run_projection_workflow_version_trgm
    ON run_projection USING gin (workflow_version gin_trgm_ops);
CREATE INDEX run_projection_submitter_trgm
    ON run_projection USING gin (submitter gin_trgm_ops);
CREATE INDEX run_projection_submitter_label_trgm
    ON run_projection USING gin (submitter_label gin_trgm_ops);
CREATE INDEX run_projection_run_name_trgm
    ON run_projection USING gin (run_name gin_trgm_ops);

-- One row per task of a run (keyed by `task_key`, stable across retries).
CREATE TABLE task_projection (
    run_id               TEXT NOT NULL,
    task_key             TEXT NOT NULL,
    state                TEXT NOT NULL,
    attempts             INT NOT NULL DEFAULT 0,
    -- Per-task display detail, taken from the task's "display attempt" (the succeeded attempt, else
    -- the highest-numbered one). Nullable: a local rule has no Batch attempt, and a still-running
    -- task has no exit code yet.
    exit_code            INT,
    batch_job_id         TEXT,
    log_stream           TEXT,
    termination_category TEXT,
    instance_id          TEXT,
    started_at           BIGINT,
    finished_at          BIGINT,
    -- A cache-hit (engine `task_cached`): state stays 'succeeded' but the task never became a Batch
    -- job, so it is not counted as work run this time.
    cached               BOOLEAN NOT NULL DEFAULT false,
    -- The DAG node (rule/process) this task rolls up onto (from task_created.group). Quoted: `group`
    -- is a SQL reserved word.
    "group"              TEXT,
    -- The in-run tasks this task depends on (the tasks that produced its inputs), from
    -- task_created.deps. Empty with no in-run upstream. Downstream ("feeds") is the inverse,
    -- derived at read time from the run's rows.
    deps                 TEXT[] NOT NULL DEFAULT '{}',
    -- The container image the task ran in (reducer `TaskProjection::image`: its display attempt's
    -- `task_submitted.image`), for per-task image provenance on a multi-image run. NULL when the
    -- engine plugin did not report one.
    image                TEXT,
    -- Why the workflow engine failed the task although its Batch job did not fail (reducer
    -- `TaskProjection::verdict`, from an `engine_task_verdict`): the engine's reason. NULL otherwise.
    verdict_reason       TEXT,
    -- The sample the task belongs to (reducer `RunProjection::task_sample`: a pure function of the
    -- task key, its group and the run anchor's sample spec), for the Lab view's per-sample
    -- progress. NULL for a whole-study task.
    sample               TEXT,
    PRIMARY KEY (run_id, task_key)
);

-- Every Batch attempt of every task, keyed by the attempt's Batch job id. task_projection keeps
-- only the display attempt; this keeps the rest (a spot-reclaimed attempt's exit code, reason, log
-- and host) after its retry succeeds.
CREATE TABLE attempt_projection (
    run_id               TEXT NOT NULL,
    batch_job_id         TEXT NOT NULL,
    task_key             TEXT NOT NULL,
    attempt              INT,              -- engine attempt number; NULL until task_submitted binds it
    state                TEXT NOT NULL,    -- created|submitted|queued|running|succeeded|failed
    exit_code            INT,
    termination_category TEXT,
    status_reason        TEXT,
    log_stream           TEXT,
    queue                TEXT,             -- Batch job queue name
    instance_id          TEXT,             -- ECS container-instance id: the join key into host_registry
    started_at           BIGINT,
    finished_at          BIGINT,
    -- The vCPUs and memory (MiB) the attempt's Batch job requested — what the task asked for,
    -- beside the host capacity host_registry records. Folded from the Batch state-change events'
    -- resource requirements; NULL when the events do not carry them.
    requested_vcpus      DOUBLE PRECISION,
    requested_memory_mib BIGINT,
    PRIMARY KEY (run_id, batch_job_id)
);
CREATE INDEX attempt_projection_task
    ON attempt_projection (run_id, task_key, attempt, batch_job_id);


-- ════ Outputs and provenance ══════════════════════════════════════════════════════════════════════

-- The objects each run produced (or, for a Nextflow cache hit, was satisfied by), folded from the
-- engine plugins' task_outputs events. A projection: one row per (run, object), kept equal to the
-- fold's output set (`biffix_db::projection`'s `upsert_outputs` deletes a row the fold no longer
-- produces).
CREATE TABLE run_output (
    run_id   TEXT NOT NULL,
    s3_uri   TEXT NOT NULL,
    -- Opaque, deterministic id for the API, so a URL never carries a raw key
    -- (biffix_core::reducer::output_id: hex(sha256(run_id || '\n' || s3_uri))[..24]).
    id       TEXT NOT NULL,
    path     TEXT,
    task_key TEXT,
    attempt  INT,
    role     TEXT NOT NULL,           -- output | published | intermediate | log | benchmark | <other>
    reused   BOOLEAN NOT NULL DEFAULT false,
    size     BIGINT,
    etag     TEXT,
    PRIMARY KEY (run_id, s3_uri)
);
CREATE UNIQUE INDEX run_output_id ON run_output (run_id, id);
CREATE INDEX run_output_task ON run_output (run_id, task_key);
-- A run's outputs page in byte order (`COLLATE "C"`, as the in-memory store and the task pages
-- do), which the (run_id, s3_uri) primary key in the database collation cannot serve.
CREATE INDEX run_output_uri_bytes ON run_output (run_id, s3_uri COLLATE "C");

-- What S3 last said about an output object: a read-time cache the API fills with bounded HEADs so
-- the outputs list can show sizes and flag missing or since-modified files. A fact cache rebuilt
-- from S3, never from the event log, and keyed by URI alone, since two runs registering the same
-- object (a shared output prefix) are looking at the same S3 object.
CREATE TABLE s3_object_meta (
    s3_uri             TEXT PRIMARY KEY,
    present            BOOLEAN NOT NULL,     -- false: HEAD said 404 (temp() removed, or deleted)
    size               BIGINT,
    etag               TEXT,
    last_modified      BIGINT,
    checked_at         BIGINT NOT NULL
);

-- The provenance crate (ro-crate-metadata.json) written into each ended run's S3 prefix: where,
-- and up to which log `seq` it describes, so a late event rewrites it.
CREATE TABLE run_provenance (
    run_id      TEXT PRIMARY KEY,
    written_seq BIGINT NOT NULL,
    uri         TEXT NOT NULL,
    written_at  BIGINT NOT NULL
);

-- Runs whose provenance crate the sweep failed to write (built, written to S3 or marked): how many
-- times in a row, and when it may try again (epoch micros). A first failure is tried again on the
-- next sweep; each one after waits longer (up to a day), and such runs sort after runs that have not
-- failed, so a run that keeps failing never holds a fresh run's place in a sweep. Dropped once its
-- crate is written.
CREATE TABLE run_provenance_failure (
    run_id      TEXT PRIMARY KEY,
    attempts    INTEGER NOT NULL,
    retry_after BIGINT NOT NULL,
    failed_at   BIGINT NOT NULL
);


-- ════ Hosts, spot interruptions and cost ══════════════════════════════════════════════════════════

-- The EC2 hosts Batch placed work on, captured from ECS "Container Instance State Change" events.
-- Batch job events name only the ECS container-instance id (an attempt's `instance_id`), so this is
-- the join that turns it into an EC2 instance, type, AZ and size.
--
-- A fact cache, not in the event log: host facts are invariant for a container instance's lifetime
-- and gone from AWS minutes after the host terminates, so they are recorded as they are observed
-- and never deleted. Upserts fill blank columns only (an early registration event can precede the
-- agent's version report), and `raw_detail` keeps the highest-`detail_version` event, so a row is
-- independent of arrival order.
CREATE TABLE host_registry (
    container_instance_id TEXT PRIMARY KEY,   -- last segment of detail.containerInstanceArn
    ec2_instance_id       TEXT NOT NULL,
    cluster_arn           TEXT NOT NULL,
    instance_type         TEXT,               -- attribute ecs.instance-type
    availability_zone     TEXT,               -- attribute ecs.availability-zone
    cpu_architecture      TEXT,               -- attribute ecs.cpu-architecture
    os_family             TEXT,               -- attribute ecs.os-family
    ami_id                TEXT,               -- attribute ecs.ami-id
    subnet_id             TEXT,               -- attribute ecs.subnet-id
    registered_vcpus      REAL,               -- registeredResources CPU / 1024 (ECS-allocatable)
    registered_memory_mib BIGINT,             -- registeredResources MEMORY (after agent reservation)
    gpu_count             INT,                -- registeredResources GPU id count; NULL when none
    agent_version         TEXT,               -- versionInfo.agentVersion
    docker_version        TEXT,               -- versionInfo.dockerVersion
    detail_version        BIGINT NOT NULL,    -- detail.version of the event kept in raw_detail
    raw_detail            JSONB NOT NULL,     -- that event's `detail`, verbatim
    first_seen_at         BIGINT NOT NULL,    -- earliest observed source time
    received_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Whether the host is a spot or an on-demand instance. ECS container-instance events do not
    -- say, so the reconcile tick resolves it from EC2 DescribeInstances (instanceLifecycle) while
    -- EC2 still describes the instance (about an hour after it terminates). NULL until resolved,
    -- and forever if it never is; readers then fall back to the job queue's configured capacity.
    --   spot | on_demand | <other EC2 lifecycle, e.g. capacity_block>
    lifecycle             TEXT
);
-- Spot interruption warnings carry the EC2 id, not the container-instance id.
CREATE INDEX host_registry_ec2 ON host_registry (ec2_instance_id);

-- EC2 spot interruption warnings, keyed by the EC2 instance they name. A projection of the
-- `spot_reclaim_warning` events, derived by the ingest writer under the run fold's cursor: the
-- warnings carry only the EC2 instance id and no run or job, so the run fold can never attribute
-- them. Readers join a row to an attempt through its host's EC2 id (from host_registry, or the id
-- Batch names in the attempt's status_reason) and its run window, to tell a spot reclaim from any
-- other host termination.
CREATE TABLE spot_interruption (
    ec2_instance_id TEXT   NOT NULL,
    warned_at       BIGINT NOT NULL,   -- the warning's source time
    PRIMARY KEY (ec2_instance_id, warned_at)
);

-- The estimated cost of each finished Batch job — a task attempt, or a run's coordinator — computed
-- by the reconcile Lambda from its host (host_registry), its run time, EC2 prices and its queue's
-- root volume. A fact cache, not part of the event log: written once per job and rebuildable.
-- Rolled up per task / run on read.
CREATE TABLE attempt_cost (
    run_id            TEXT NOT NULL,
    batch_job_id      TEXT NOT NULL,
    role              TEXT NOT NULL,     -- task | coordinator
    instance_type     TEXT NOT NULL,
    availability_zone TEXT,
    lifecycle         TEXT,              -- spot | on_demand | NULL (unknown)
    hours             DOUBLE PRECISION NOT NULL,
    price_per_hour    DOUBLE PRECISION NOT NULL,   -- average compute USD/hour over the job
    price_source      TEXT NOT NULL,     -- on_demand | spot_history | spot_at_on_demand | unknown_at_on_demand
    compute_usd       DOUBLE PRECISION NOT NULL,
    ebs_usd           DOUBLE PRECISION NOT NULL,
    priced_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, batch_job_id)
);


-- ════ Workflow registry ═══════════════════════════════════════════════════════════════════════════

-- Frozen workflow versions: domain state written at registration, immutable afterwards. The id is
-- `{git_sha}:{content_hash}:{build}`: registering the same source again with different inputs
-- freezes the next build instead of colliding.
CREATE TABLE workflow_versions (
    id                       TEXT PRIMARY KEY,
    name                     TEXT NOT NULL,
    project                  TEXT NOT NULL,
    git_sha                  TEXT NOT NULL,
    source_content_hash      TEXT NOT NULL,
    image_digest             TEXT NOT NULL,
    image_repository         TEXT,
    image_registry_id        TEXT,
    image_region             TEXT,
    engine                   TEXT NOT NULL,
    engine_version           TEXT NOT NULL,
    entrypoint               TEXT NOT NULL,
    target                   TEXT,
    source                   JSONB NOT NULL,
    form_schema              JSONB NOT NULL,
    created_by               TEXT,
    created_by_label         TEXT,
    created_at               BIGINT NOT NULL,
    queue_classes            TEXT[] NOT NULL DEFAULT '{}',
    -- The version's About page: the Markdown the author names with `about:` in biffix.yaml, read
    -- at registration. NULL for a version without one.
    about_markdown           TEXT,
    build                    INTEGER NOT NULL DEFAULT 0,
    -- What the CLI was given beyond the source (source mode, overrides, CLI release), so the
    -- version can be re-registered.
    registration_inputs      JSONB NOT NULL,
    -- sha256 over the inputs that determine the version (and the registration pipeline version): a
    -- registration matching an existing build of the same source is refused as identical to it.
    registration_inputs_hash TEXT NOT NULL,
    -- The pinned engine runtime the version's engine release was built into — the recipe (every
    -- pin) and its key — so the same recipe is used however the pin tables move on.
    runtime_key              TEXT NOT NULL,
    runtime_recipe           JSONB NOT NULL
);
CREATE INDEX workflow_versions_project ON workflow_versions (project, name);
CREATE UNIQUE INDEX workflow_versions_source_build
    ON workflow_versions (git_sha, source_content_hash, build);
-- Run search also matches the workflow's name, project and engine.
CREATE INDEX workflow_versions_name_trgm
    ON workflow_versions USING gin (name gin_trgm_ops);
CREATE INDEX workflow_versions_project_trgm
    ON workflow_versions USING gin (project gin_trgm_ops);
CREATE INDEX workflow_versions_engine_trgm
    ON workflow_versions USING gin (engine gin_trgm_ops);

-- Which workflow versions are *released* (`PUT /workflows/{id}/release`): the rest are developer
-- versions. Released is per version, a curator's decision made after registration (the version
-- itself stays immutable), like `workflow_listing` is per workflow.
--
-- APPEND-ONLY: every release and un-release is a row, never updated or deleted, so who released
-- (or un-released) what and when can always be answered. A version's current state is its newest
-- row (absent = never released = a developer version). Only a change is recorded: releasing a
-- released version adds nothing. Not a projection of the run log: the registry is reference data,
-- not a fold over `RunEvent`s, and a refold never touches this table.
CREATE TABLE workflow_version_releases (
    seq         BIGSERIAL PRIMARY KEY,
    version_id  TEXT NOT NULL,
    released    BOOLEAN NOT NULL,
    actor       TEXT,
    actor_label TEXT,
    at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- A version's history, newest first (and so its current state: the first row).
CREATE INDEX workflow_version_releases_version
    ON workflow_version_releases (version_id, seq DESC);

-- How a workflow is listed in the Lab's pipeline catalog, per (project, name): every version of the
-- workflow shares it. `hidden` takes a deprecated pipeline off the Lab's cards (the Expert list
-- still shows it). Author-owned catalog fields (description, tags, verified) are not here: they are
-- frozen with each version's form (`catalog:` in `biffix.yaml`). Absent row = shown.
CREATE TABLE workflow_listing (
    project    TEXT NOT NULL,
    name       TEXT NOT NULL,
    hidden     BOOLEAN NOT NULL DEFAULT false,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_by TEXT,
    PRIMARY KEY (project, name)
);


-- ════ Images, builds and runtimes ═════════════════════════════════════════════════════════════════

-- A version's declared images, each pinned to a digest at registration.
CREATE TABLE workflow_version_images (
    version_id   TEXT NOT NULL,
    name         TEXT NOT NULL,     -- `default` is the image tasks without their own run on
    declared_ref TEXT NOT NULL,     -- as written in biffix.yaml / --image
    source_ref   TEXT NOT NULL,     -- pinned: <registry>/<repo>[:tag]@sha256:…
    content_key  TEXT NOT NULL,     -- biffix_workflow::images::keys::content_key(source digest)
    own_runtime  BOOLEAN NOT NULL DEFAULT false,
    -- How the image is produced: imported from `source_ref` (codebuild/image/import.sh;
    -- build_projection subject `image:<content_key>`), or built by CodeBuild from a recipe the
    -- workflow ships (one build per architecture, then an index; subjects
    -- `image:build:<content_key>[:<arch>]`). `recipe` is what the builds need (context tarball +
    -- sha256, tree hash, Dockerfile path or generated text, archs).
    kind         TEXT NOT NULL DEFAULT 'import',  -- import | dockerfile | pixi
    recipe       JSONB,                           -- NULL for an import
    PRIMARY KEY (version_id, name)
);
CREATE INDEX workflow_version_images_content_key
    ON workflow_version_images (content_key);

-- The tenant-wide result of importing or building an image, written once its build succeeded:
-- where the image now lives in the workflow repository and what its probe found. Reference data.
CREATE TABLE image_artifacts (
    content_key   TEXT PRIMARY KEY,
    source_ref    TEXT NOT NULL,
    image         TEXT NOT NULL,    -- <workflow repo>@sha256:…
    digest        TEXT NOT NULL,
    archs         JSONB NOT NULL DEFAULT '[]',
    capabilities  JSONB NOT NULL DEFAULT '{}',
    keep_tag      TEXT,
    archived      BOOLEAN NOT NULL DEFAULT false,
    archive_bytes BIGINT,
    created_at    BIGINT NOT NULL
);

-- Each build subject's latest build (images, engine runtimes): the fold of its `build_*` events
-- (biffix-core::builds). A projection like run_projection — re-derived from the log, never patched.
CREATE TABLE build_projection (
    subject    TEXT PRIMARY KEY,   -- runtime:<key> | coordinator:<…> | image:<content_key> | …
    state      TEXT NOT NULL,      -- requested | running | succeeded | failed
    build_arn  TEXT NOT NULL,      -- the latest build
    outputs    JSONB NOT NULL DEFAULT '{}',  -- the latest build's exported variables
    reason     TEXT,
    logs_url   TEXT,
    request    JSONB,              -- what biffix asked the latest build for
    builds     INTEGER NOT NULL,   -- how many builds of the subject have been seen
    updated_at BIGINT NOT NULL     -- the newest event's time
);

-- One coordinator variant per (runtime, base coordinator image): the image the runtime build pushed
-- (base + runtime layer) and the Batch job definition the backend registered for it. Reference
-- data, written once the variant's build (build_projection subject
-- `coordinator:<runtime_key>@<base_digest>`) succeeded.
CREATE TABLE coordinator_variants (
    runtime_key        TEXT NOT NULL,
    base_digest        TEXT NOT NULL,
    image              TEXT NOT NULL,   -- <repo>@sha256:…
    job_definition_arn TEXT NOT NULL,
    created_at         BIGINT NOT NULL,
    PRIMARY KEY (runtime_key, base_digest)
);

-- Runs waiting for submit-time images (launch.rs). Not part of the event log and nothing folds
-- it: the row is the launch request as given (params verbatim, including secret values, which the
-- coordinator needs and the log never carries) and is deleted once the run launches, fails or is
-- cancelled. Dropping a row abandons the launch (the run stays preparing until cancelled).
CREATE TABLE pending_launch (
    run_id     TEXT PRIMARY KEY,
    created_at BIGINT NOT NULL,
    plan       JSONB NOT NULL
);


-- ════ Reference bundles ═══════════════════════════════════════════════════════════════════════════

-- Named sets of reference-file URIs (a genome's fasta, index, annotation…) that a launch form's
-- reference dropdown fills params from. Admin-managed; uris is {key: s3 uri}.
CREATE TABLE reference_bundles (
    id           TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,
    display_name TEXT NOT NULL,
    uris         JSONB NOT NULL,
    notes        TEXT,
    created_at   BIGINT NOT NULL,
    created_by   TEXT,
    updated_at   BIGINT NOT NULL,
    -- A genome the alignment viewer hosts (`hg38`, `mm39`), shown in place of the bundle's own
    -- FASTA. NULL ⇒ none (the bundle's files).
    igv_genome   TEXT
);


-- ════ Notifications ═══════════════════════════════════════════════════════════════════════════════

-- The in-app notification bell (`/me/notifications`). A PROJECTION: one row per run that reached a
-- terminal state, addressed to the user who submitted it (`RunProjection::notification`). The run
-- fold writes it beside the run's `run_projection` row and a replay (refold) rebuilds the same
-- rows: the id is deterministic (`<run id>:<state>`). `at` is when the run first ended.
CREATE TABLE notifications (
    id       TEXT PRIMARY KEY,
    user_sub TEXT NOT NULL,
    run_id   TEXT NOT NULL,
    kind     TEXT NOT NULL,
    at       BIGINT NOT NULL
);
-- The bell's newest-first list breaks ties on the id bytewise (`ORDER BY n.at DESC, n.id COLLATE
-- "C" DESC`, as the run list does and as the test doubles sort): this index is that order exactly,
-- so the list reads the user's newest rows straight off it. Its `user_sub` prefix also serves the
-- unread counts (polled, and on every mark-read), which scan the user's notifications and probe
-- `notification_reads` by its primary key.
CREATE INDEX notifications_user_newest
    ON notifications (user_sub, at DESC, id COLLATE "C" DESC);
-- The run list's `attention` filter's per-run lookup, and the ingest writer's per-run delete.
CREATE INDEX notifications_run ON notifications (run_id);

-- Which notifications a user has read: USER STATE, not a projection (no event records a read, and
-- a replay never touches it). Keyed by the notification's deterministic id, so a read survives a
-- refold that rebuilds the notification (and can be written ahead of it).
CREATE TABLE notification_reads (
    user_sub        TEXT NOT NULL,
    notification_id TEXT NOT NULL,
    read_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_sub, notification_id)
);

-- Which failed runs a user has dismissed from *Needs attention* (`POST /me/attention/dismiss`).
--
-- USER STATE, as `notification_reads` is: no event records a dismissal and a replay never touches
-- it. Keyed by the failed notification's deterministic id (`<run id>:failed`), so a dismissal
-- survives a refold that rebuilds the notification. Apart from reads on purpose: opening a run (or
-- *Mark all read* in the bell) reads its notification but leaves it needing attention; only a
-- dismissal takes it out. Kept, never deleted, so who dismissed what, when and how (`via`:
-- `dismiss` from the UI or API, `try_again` from a re-run or resume of the run) can be answered
-- later.
CREATE TABLE notification_dismissals (
    user_sub        TEXT NOT NULL,
    notification_id TEXT NOT NULL,
    via             TEXT NOT NULL,
    dismissed_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_sub, notification_id)
);


-- ════ Experiments ═════════════════════════════════════════════════════════════════════════════════

-- The Lab view's named groups of runs. Domain state, like a workflow version: a row written by the
-- API, not a projection, and not rebuilt by a refold. It lives in a project and grants nothing
-- (viewing and changing it follow the run capabilities). A run's membership is the projection
-- column `run_projection.experiment_id`.
CREATE TABLE experiments (
    id               TEXT PRIMARY KEY,
    project          TEXT NOT NULL,
    name             TEXT NOT NULL,
    description      TEXT,
    created_by       TEXT,
    created_by_label TEXT,
    created_at       BIGINT NOT NULL,
    archived_at      BIGINT
);
-- A name is unique, ignoring case (Postgres `lower`), among a project's experiments that are not
-- archived: an archived one gives its name up, and restoring it takes the name back only if it is
-- still free.
CREATE UNIQUE INDEX experiments_live_name
    ON experiments (project, lower(name)) WHERE archived_at IS NULL;
CREATE INDEX experiments_project ON experiments (project);


-- ════ Users and site ══════════════════════════════════════════════════════════════════════════════

-- Per-user display preferences, keyed by the identity provider's `sub` (`local` when auth is off):
-- the Lab/Expert view, pinned pipelines, and when the first-visit tour was finished. biffix-domain
-- state about a user, not identity (the IdP owns who the user is). Absent row = defaults.
CREATE TABLE user_prefs (
    user_sub         TEXT PRIMARY KEY,
    view             TEXT,
    pinned_workflows JSONB NOT NULL DEFAULT '[]'::jsonb,
    tour_done_at     TIMESTAMPTZ,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Single-row site settings (UI customization). The `id` is pinned to 'default'.
CREATE TABLE site_settings (
    id            TEXT PRIMARY KEY,
    title         TEXT,
    logo_data_url TEXT,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The site's support contact (`SiteSettings.support`: `{name, email, url?}`), whom Lab users
    -- ask when an analysis needs attention. NULL = none set (no *Ask* button). A pipeline's own
    -- `catalog.contact`, frozen with its version, wins for its runs.
    support       JSONB
);


-- ════ Reconcile and status bookkeeping ════════════════════════════════════════════════════════════

-- The reconcile tick's own bookkeeping. Not part of the event log and nothing folds it: dropping a
-- row only restarts a rotation, a cursor or a pricing attempt.

-- When the reconciler last asked DescribeJobs about a stale candidate, and since when Batch has
-- left it out of the answer. The stale-candidate queries order by `last_described_micros` so a
-- bounded page rotates through every candidate instead of re-describing the same oldest ones, and
-- stop asking about a job Batch has omitted for longer than the give-up horizon.
CREATE TABLE reconcile_describe (
    batch_job_id          TEXT PRIMARY KEY,
    last_described_micros BIGINT NOT NULL,
    missing_since_micros  BIGINT
);

-- Resume points of the tick's bounded sweeps, by name. `snakemake_cache:<bucket>` holds the last
-- cache key examined, so the next tick lists from there; a completed pass removes the row.
CREATE TABLE reconcile_cursor (
    name       TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Finished jobs the pricer can never price (an instance type its region lists no price for, region
-- prices that could not be read in time, no run time), so they leave the oldest-first pricing queue
-- instead of holding its head. Deleting a row makes the job priceable again.
CREATE TABLE unpriceable_job (
    run_id       TEXT NOT NULL,
    batch_job_id TEXT NOT NULL,
    reason       TEXT NOT NULL,
    recorded_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, batch_job_id)
);

-- Service-status history: one fixed-cadence sample per component per reconcile tick (5 min). The
-- status page reads the latest row per component (live) and a daily GROUP BY (90-day uptime bars).
-- A fixed heartbeat (not transitions-only) is deliberate: a missing recent row is itself the signal
-- that the probe/reconciler is dead, which transition-only storage cannot express.
CREATE TABLE status_samples (
    sampled_at TIMESTAMPTZ NOT NULL,
    component  TEXT NOT NULL,
    status     TEXT NOT NULL,
    detail     TEXT,
    PRIMARY KEY (component, sampled_at)
);
CREATE INDEX status_samples_at ON status_samples (sampled_at);
