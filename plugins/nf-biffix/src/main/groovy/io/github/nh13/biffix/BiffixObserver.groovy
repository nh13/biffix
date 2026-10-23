package io.github.nh13.biffix

import groovy.transform.CompileDynamic
import groovy.transform.CompileStatic
import groovy.transform.PackageScope
import groovy.util.logging.Slf4j
import java.nio.file.Path
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.TimeUnit
import java.util.function.LongSupplier
import nextflow.Session
import nextflow.extension.FilesEx
import nextflow.processor.TaskHandler
import nextflow.script.params.FileOutParam
import nextflow.trace.TraceObserverV2
import nextflow.trace.TraceRecord
import nextflow.trace.event.FilePublishEvent
import nextflow.trace.event.TaskEvent
import nextflow.trace.event.WorkflowOutputEvent

/**
 * A Nextflow {@link TraceObserverV2} that emits biffix RunEvents — the structural analog of
 * {@code snakemake-logger-plugin-biffix}. It reports the run/task lifecycle to the biffix control-plane
 * ingest route so the projection tracks a Nextflow run exactly as it tracks a Snakemake one.
 *
 * <h3>The two-stream join</h3>
 * biffix folds two independent streams into a run's state:
 * <ul>
 *   <li>this engine stream, which owns the {@code task_submitted} <em>binding</em>
 *       ({@code batch_job_id → (run_id, task_key)}); and</li>
 *   <li>AWS EventBridge, which owns the terminal ground truth for Batch jobs.</li>
 * </ul>
 * For a task that ran on AWS Batch we emit {@code task_submitted} carrying the Batch job id
 * ({@code trace.native_id}) and let EventBridge supply the terminal. For a task that ran on the
 * <em>local</em> executor (on the coordinator, never submitted to Batch, so no EventBridge
 * terminal) we emit an engine-sourced {@code task_succeeded}/{@code task_failed} the reducer honors
 * directly — mirroring the Snakemake plugin's local-rule handling. So does a Batch task whose
 * submit failed: it never became a Batch job, so EventBridge never reports it either. A Batch job
 * Nextflow fails although Batch may report it succeeded (missing outputs, an exit status outside
 * {@code validExitStatus}) gets the engine's {@code engine_task_verdict}, which outranks that
 * success for the task.
 *
 * A retry is announced ({@code task_retrying}) when the failed attempt completes and Nextflow has
 * decided to retry it ({@code error_action == RETRY}), and again, idempotently, when the next
 * attempt is submitted.
 *
 * Config comes from the environment the coordinator injects (docker/coordinator/entrypoint.sh,
 * the same contract the Snakemake logger reads): {@code BIFFIX_RUN_ID}, {@code BIFFIX_INGEST_URL}
 * and {@code BIFFIX_INGEST_TOKEN}. Absent all of them the observer is inert (the plugin can load in
 * a non-biffix run without effect); with only some of them it warns, naming the missing ones.
 * Every callback swallows its own errors: a plugin bug never fails the run.
 */
@Slf4j
@CompileStatic
class BiffixObserver implements TraceObserverV2 {

    /** Matches SCHEMA_VERSION in the Snakemake plugin's events.py — one shared wire contract. */
    private static final int SCHEMA_VERSION = 1

    /** The coordinator env the observer needs, all or none. */
    @PackageScope
    static final List<String> CONFIG_ENV = ['BIFFIX_RUN_ID', 'BIFFIX_INGEST_URL', 'BIFFIX_INGEST_TOKEN']

    private final String runId
    private final IngestPoster poster
    /** The durable events file the coordinator uploads and replays (null: none, e.g. in tests). */
    private final EventWriter eventsFile
    private Session session
    /**
     * Work-dir key ({@code ab/cdef…}) → the task key that ran there. A downstream task's staged
     * inputs live in its producer's work dir, so this maps an input back to the task that made it.
     */
    private final Map<String, String> taskByWorkKey = new ConcurrentHashMap<String, String>()

    /**
     * Refs per {@code task_outputs} event (about 120 KB of URIs), so one event stays bounded: biffix-core's
     * {@code ingest_limits::poster::MAX_OUTPUTS_PER_EVENT} (IngestPosterSpec pins it).
     */
    static final int TASK_OUTPUTS_CHUNK = 1000

    /**
     * A task instance ({@code process} + its {@code index}) → its task key, fixed the first time the
     * instance is seen. The key is a function of the instance alone ({@link #taskKey}), so this only
     * pins it for a tag that changes between attempts ({@code tag "${task.attempt}"}): every attempt
     * of an instance, and every event about it, then still shares one key.
     */
    private final Map<String, String> keyByInstance = new ConcurrentHashMap<String, String>()

    /** The ingest token, scrubbed from any engine text an event carries (null: none). */
    private final String token

    /**
     * Keys of the tasks satisfied from the resume cache. A publish credited to one of them is a
     * re-publish of a prior run's file, so it is flagged {@code reused} like the task's outputs.
     */
    private final Set<String> cachedTaskKeys = ConcurrentHashMap.<String>newKeySet()
    private final PublishBuffer publishes = new PublishBuffer()
    /** Flushes a quiet publish buffer on time; started by {@link #onFlowCreate}, stopped by {@link #onFlowComplete}. */
    private ScheduledExecutorService publishTimer
    /** Test seam: the wall clock the publish flush interval is measured on. */
    @PackageScope
    LongSupplier clock = { System.currentTimeMillis() } as LongSupplier
    /** Test seam: whether {@link #onFlowCreate} starts the publish timer (off for clock-driven tests). */
    @PackageScope
    boolean publishTimerEnabled = true
    /**
     * This coordinator's own AWS Batch attempt ({@code AWS_BATCH_JOB_ATTEMPT}), or null outside
     * Batch. Batch restarts a coordinator whose host went away under the same run, and Nextflow's
     * task ids restart at 1 in the new process, so an execution id from a later attempt is prefixed
     * with it ({@link #executionOf}): otherwise a new execution could reuse an earlier attempt's id
     * and be dropped as a redelivery of that one's terminal. Test seam.
     */
    @PackageScope
    Integer coordinatorAttempt = batchAttempt(System.getenv('AWS_BATCH_JOB_ATTEMPT'))

    BiffixObserver() {
        this(
            System.getenv('BIFFIX_RUN_ID'),
            new IngestPoster(System.getenv('BIFFIX_INGEST_URL'), System.getenv('BIFFIX_INGEST_TOKEN')),
            EventWriter.fromEnv(System.getenv()),
            System.getenv('BIFFIX_INGEST_TOKEN'),
        )
    }

    /**
     * Test seam: inject the run id + poster, and optionally the events file and the ingest token (the
     * default ctor reads the coordinator env).
     */
    BiffixObserver(String runId, IngestPoster poster, EventWriter eventsFile = null, String token = null) {
        this.runId = runId
        this.poster = poster
        this.eventsFile = eventsFile
        this.token = token
    }

    private boolean isActive() {
        return runId && poster.enabled
    }

    /**
     * The warning for a partly configured coordinator — some but not all of {@link #CONFIG_ENV}
     * set — naming the missing variables (never a value), or null when all or none are set. Pure.
     */
    @PackageScope
    static String configWarning(Map<String, String> env) {
        final missing = CONFIG_ENV.findAll { !env.get(it) }
        if (missing.isEmpty() || missing.size() == CONFIG_ENV.size()) {
            return null
        }
        return "nf-biffix: ${missing.join(', ')} not set; this run is not tracked by biffix".toString()
    }

    private static double now() {
        return System.currentTimeMillis() / 1000.0d
    }

    /**
     * Stamp the shared envelope onto a pure event body and POST it (best-effort). The envelope is
     * {schema_version, ts, run_id, kind, ...body} (shared with events.py); the bodies are built by
     * the pure {@link BiffixEvents} (unit-tested against the wire schema). `now()` is evaluated once
     * per emission here, so per-event timestamps and ordering match the call order.
     */
    private void emit(Map<String, Object> body) {
        final event = BiffixEvents.envelope(SCHEMA_VERSION, runId, now(), body)
        // The durable copy first, so an event the live POST drops is still replayed at exit.
        eventsFile?.write(event)
        poster.post(event)
    }

    /**
     * The fields of the task behind {@code handler} the key, group and attempt fall back on:
     * {@code name} (the current run's), {@code index}, {@code process} and {@code attempt}, each
     * null when absent. Best-effort and dynamically typed like {@link #taskDeps}.
     */
    @CompileDynamic
    private static Map<String, Object> taskFields(TaskHandler handler) {
        final Map<String, Object> out = [:]
        try {
            final task = handler?.task
            if (task == null) {
                return out
            }
            out.name = task.name ? task.name.toString() : null
            final index = task.index
            out.index = index instanceof Number && ((Number) index).intValue() > 0 ? ((Number) index).intValue() : null
            out.process = task.processor?.name ? task.processor.name.toString() : null
            final attempt = task.config?.attempt
            out.attempt = attempt instanceof Number && ((Number) attempt).intValue() > 0 ? ((Number) attempt).intValue() : null
        } catch (Throwable e) {
            log.debug("nf-biffix: could not read the task's fields: ${e.message}")
        }
        return out
    }

    private static String str(Object value) {
        return value != null && value.toString() ? value.toString() : null
    }

    /**
     * Stable logical-task identity, unique per task instance and shared by all its attempts.
     *
     * The base is the task's name as the current run spells it ({@code handler.task.name}, which is
     * {@code "process (tag)"} with a tag and {@code "process (index)"} without; the trace's name only
     * when the task's is unavailable — a cached task's trace is the prior run's, whose index-based
     * name can belong to a different task now). A tag need not be unique, so a tagged name alone is
     * not: a task with a tag — its name is neither {@code "process"} nor {@code "process (index)"} —
     * is keyed {@code "<name> #<index>"} ({@code index}, which a retry keeps, is unique within the
     * process). That is a function of the task alone, never of which tasks this coordinator saw first,
     * so a restarted coordinator (a Batch retry of the coordinator job) keys every task as the first
     * one did. An untagged task's name is already unique. Every form keeps the process before the
     * first {@code " ("}, which is how a consumer without the task's group recovers it
     * (crates/biffix-backend/src/runs/provenance.rs {@code task_group}).
     */
    @PackageScope
    String taskKey(TraceRecord trace, TaskHandler handler) {
        final fields = taskFields(handler)
        final process = str(trace?.get('process')) ?: str(fields.process)
        final Integer index = fields.index as Integer
        String name = str(fields.name) ?: str(trace?.get('name'))
        if (!name) {
            name = index != null ? "${process} (${index})".toString() : "${process}:${trace?.get('task_id')}".toString()
        }
        if (index == null || process == null) {
            return name
        }
        final String base = name
        final boolean tagged = base != process && base != "${process} (${index})".toString()
        return keyByInstance.computeIfAbsent("${process}\u0000${index}".toString()) {
            tagged ? "${base} #${index}".toString() : base
        }
    }

    /**
     * This execution's id: Nextflow's task id, prefixed {@code <attempt>/} on a coordinator's second
     * or later Batch attempt ({@link #coordinatorAttempt}), so ids stay unique across a restart
     * while a first attempt's are exactly the task id. {@code /}, never {@code :}, which the
     * ingest's dedup key separates its parts with.
     */
    private String executionOf(TraceRecord trace, TaskHandler handler) {
        final id = taskIdOf(trace, handler)
        return id != null && coordinatorAttempt != null && coordinatorAttempt > 1 ? "${coordinatorAttempt}/${id}".toString() : id
    }

    /**
     * The id of this one execution of the task: the trace's {@code task_id}, else the task's own id.
     * Nextflow gives every execution (each retry's task copy) a new one, so it tells apart two runs
     * of a task at the same attempt (the executor retry path does not advance the attempt). Null
     * when neither is known.
     */
    @CompileDynamic
    private static String taskIdOf(TraceRecord trace, TaskHandler handler) {
        final fromTrace = str(trace?.get('task_id'))
        if (fromTrace) {
            return fromTrace
        }
        try {
            return str(handler?.task?.id)
        } catch (Throwable e) {
            log.debug("nf-biffix: could not read the task's id: ${e.message}")
            return null
        }
    }

    /** {@code AWS_BATCH_JOB_ATTEMPT} as a number, or null when unset or not one. */
    @PackageScope
    static Integer batchAttempt(String value) {
        return value?.trim()?.isInteger() ? value.trim().toInteger() : null
    }

    /**
     * The container the task runs, as Nextflow resolved it (in a biffix run the tenant copy
     * {@link BiffixContainerResolver} mapped it to): the trace's {@code container}, else the task's.
     * Null when neither is known.
     */
    @CompileDynamic
    private static String imageOf(TraceRecord trace, TaskHandler handler) {
        final fromTrace = str(trace?.get('container'))
        if (fromTrace) {
            return fromTrace
        }
        try {
            return str(handler?.task?.container)
        } catch (Throwable e) {
            log.debug("nf-biffix: could not read the task's container: ${e.message}")
            return null
        }
    }

    /** Exceptions Nextflow raises for an infrastructure failure it re-runs at the same attempt. */
    private static final Set<String> RETRYABLE_ERRORS = ['ProcessRetryableException', 'CloudSpotTerminationException'] as Set<String>

    /** Exceptions Nextflow raises when a task's declared outputs are missing. */
    private static final Set<String> MISSING_OUTPUT_ERRORS = ['MissingFileException', 'MissingValueException'] as Set<String>

    /**
     * Why Nextflow failed the task behind {@code handler}: {@code [category: …, reason: …]} (either
     * may be null), or null when the failure is not the engine's own verdict — an infrastructure
     * failure Nextflow re-runs at the same attempt (a spot reclaim), which AWS reports itself. A
     * verdict names an attempt number, so one for such a failure would also fail the re-run.
     *
     * The error is the task's own ({@code task.error}, set by the executor: fresh per execution),
     * else the one Nextflow recorded while finalizing it ({@code previousException}: missing
     * outputs, a bad exit status), which it stores before notifying observers. The category is
     * {@code missing_output} for missing outputs, {@code app_error} for a known exit status outside
     * {@code validExitStatus}. Best-effort and dynamically typed like {@link #taskDeps}.
     */
    @CompileDynamic
    @PackageScope
    static Map<String, String> failureOf(TaskHandler handler) {
        final Map<String, String> out = [category: null, reason: null]
        try {
            final task = handler?.task
            if (task == null) {
                return out
            }
            Throwable error = task.error instanceof Throwable ? (Throwable) task.error : null
            if (error == null) {
                final prev = task.config?.get('previousException')
                error = prev instanceof Throwable ? (Throwable) prev : null
            }
            final names = [] as Set<String>
            for (Throwable e = error; e != null && names.size() < 16; e = e.cause == e ? null : e.cause) {
                names << e.class.simpleName
            }
            if (names.any { it in RETRYABLE_ERRORS }) {
                return null
            }
            final exit = task.exitStatus
            if (names.any { it in MISSING_OUTPUT_ERRORS }) {
                out.category = 'missing_output'
            } else if (exit instanceof Integer && exit != Integer.MAX_VALUE && !task.isSuccess(exit)) {
                out.category = 'app_error'
            }
            out.reason = error?.message ? error.message.toString() : null
        } catch (Throwable e) {
            log.debug("nf-biffix: could not read the task's failure: ${e.message}")
        }
        return out
    }

    /** {@code text} with the ingest token replaced, so engine text can never leak it. */
    private String scrub(String text) {
        return text && token ? text.replace(token, '<token>') : text
    }

    /** The DAG node (process) a task rolls up onto, for the run's graph view. */
    private static String processGroup(TraceRecord trace, TaskHandler handler) {
        return str(trace?.get('process')) ?: str(taskFields(handler).process)
    }

    private static int attemptOf(TraceRecord trace, TaskHandler handler) {
        final a = trace?.get('attempt')
        if (a != null) {
            return a as Integer
        }
        final Integer fromTask = taskFields(handler).attempt as Integer
        return fromTask != null ? fromTask : 1
    }

    /**
     * The work-dir key of {@code path} — its first two directory levels under {@code workDir}
     * ({@code ab/cdef0123…}, one task's directory) — or null when {@code path} is not inside a task
     * directory of {@code workDir}. Pure.
     */
    @PackageScope
    static String workKey(String workDir, String path) {
        if (!workDir || !path) {
            return null
        }
        final base = workDir.endsWith('/') ? workDir : workDir + '/'
        if (!path.startsWith(base)) {
            return null
        }
        final parts = path.substring(base.length()).split('/')
        return parts.length >= 2 && parts[0] && parts[1] ? "${parts[0]}/${parts[1]}".toString() : null
    }

    private static String uriOf(Object path) {
        return path instanceof Path ? FilesEx.toUriString((Path) path) : path?.toString()
    }

    /**
     * Record task {@code tkey}'s work dir, and return the keys of the in-run tasks that produced
     * its inputs (the inputs staged from another task's work dir). Best-effort: the task API is
     * version-sensitive, so any mismatch yields no deps rather than a failed event.
     */
    @CompileDynamic
    private List<String> taskDeps(TaskHandler handler, String tkey) {
        try {
            final task = handler?.task
            if (task == null) {
                return []
            }
            final base = uriOf(session?.workDir)
            final own = workKey(base, uriOf(task.workDir))
            if (own) {
                taskByWorkKey.put(own, tkey)
            }
            final deps = new TreeSet<String>()
            task.inputFilesMap?.values()?.each { input ->
                final key = workKey(base, uriOf(input))
                final dep = key ? taskByWorkKey.get(key) : null
                if (dep && dep != tkey) {
                    deps.add(dep)
                }
            }
            return deps as List<String>
        } catch (Throwable e) {
            log.debug("nf-biffix: could not derive task deps for ${tkey}: ${e.message}")
            return []
        }
    }

    /**
     * The S3 URIs of the task's declared file outputs ({@code path}/{@code file} outputs), for the
     * {@code task_outputs} inventory. Only {@code s3://} objects are kept — a local-executor task's
     * files never reach S3 — and Nextflow's own {@code .command.*}/{@code .exitcode} bookkeeping is
     * dropped (it is never a declared output, but a glob like {@code '*'} could match it).
     *
     * Nextflow collects outputs ({@code TaskProcessor.collectOutputs}) before it notifies
     * {@code onTaskComplete}/{@code onTaskCached}, so {@code task.outputs} is populated here.
     * Best-effort and dynamically typed like {@link #taskDeps}: the task API is version-sensitive,
     * so any mismatch yields no outputs (and so no event) rather than a failed callback.
     */
    @CompileDynamic
    private List<String> taskFileOutputs(TaskHandler handler, String tkey) {
        try {
            final outputs = handler?.task?.outputs
            if (!(outputs instanceof Map)) {
                return []
            }
            final uris = new TreeSet<String>()
            // Values are a Path, or a (possibly nested) collection of them for a multi-file glob.
            Closure collect
            collect = { value ->
                if (value instanceof Path) {
                    final uri = uriOf(value)
                    final name = ((Path) value).fileName?.toString() ?: ''
                    if (uri?.startsWith('s3://') && !name.startsWith('.command.') && name != '.exitcode') {
                        uris.add(uri)
                    }
                } else if (value instanceof Collection) {
                    value.each { collect(it) }
                }
            }
            outputs.each { param, value ->
                if (param instanceof FileOutParam) {
                    collect(value)
                }
            }
            return uris as List<String>
        } catch (Throwable e) {
            log.debug("nf-biffix: could not collect task outputs for ${tkey}: ${e.message}")
            return []
        }
    }

    /** Emit the task's declared S3 file outputs as one {@code intermediate} batch (none ⇒ no event). */
    private void emitTaskOutputs(TaskHandler handler, String tkey, int attempt, boolean reused) {
        try {
            final uris = taskFileOutputs(handler, tkey)
            if (uris.isEmpty()) {
                return
            }
            // In chunks, so a task with a huge glob output never makes one event over the ingest
            // route's body limit (which would be dropped whole). Each chunk is its own fact (the
            // dedup disc hashes the URI set) and the fold unions them.
            uris.collate(TASK_OUTPUTS_CHUNK).each { List<String> chunk ->
                final List<Map> refs = chunk.collect { [s3_uri: it] as Map }
                emit(BiffixEvents.taskOutputs(tkey, attempt, 'intermediate', reused, refs))
            }
        } catch (Throwable t) {
            log.warn("nf-biffix: task_outputs emission failed for ${tkey} (swallowed): ${t.message}")
        }
    }

    /** Buffer one publish ref under its task key, emitting the buffer when a flush is due. */
    private void bufferPublish(String tkey, Map ref) {
        final drained = publishes.add(tkey, ref, clock.asLong)
        // Emitted outside the buffer's lock, so a slow emission never stalls the other publish threads.
        if (drained != null) {
            emitPublishes(drained)
        }
    }

    /** The timer's tick: emit the buffer when it has waited a full interval. Never throws. */
    private void flushDuePublishes() {
        try {
            final drained = publishes.drainIfDue(clock.asLong)
            if (drained != null) {
                emitPublishes(drained)
            }
        } catch (Throwable t) {
            log.warn("nf-biffix: timed publish flush failed (swallowed): ${t.message}")
        }
    }

    private void flushPublishes() {
        emitPublishes(publishes.drain(clock.asLong))
    }

    private void startPublishTimer() {
        if (!publishTimerEnabled || publishTimer != null) {
            return
        }
        publishTimer = Executors.newSingleThreadScheduledExecutor { Runnable r ->
            final t = new Thread(r, 'biffix-publish-flush')
            t.daemon = true
            return t
        }
        final long every = PublishBuffer.FLUSH_MILLIS
        publishTimer.scheduleWithFixedDelay({ flushDuePublishes() } as Runnable, every, every, TimeUnit.MILLISECONDS)
    }

    private void stopPublishTimer() {
        final timer = publishTimer
        publishTimer = null
        if (timer != null) {
            timer.shutdownNow()
            try {
                // A tick in flight finishes its emission before the final drain runs.
                timer.awaitTermination(PublishBuffer.FLUSH_MILLIS, TimeUnit.MILLISECONDS)
            } catch (InterruptedException ignored) {
                Thread.currentThread().interrupt()
            }
        }
    }

    /** One {@code published} batch per task key; a cached task's publishes are flagged reused. */
    private void emitPublishes(Map<String, List<Map>> byTask) {
        byTask.each { String tkey, List<Map> refs ->
            final reused = tkey != null && cachedTaskKeys.contains(tkey)
            emit(BiffixEvents.taskOutputs(tkey, null, 'published', reused, refs))
        }
    }

    /**
     * Emit the run's group-level process graph from Nextflow's {@code session.dag}, collapsing the
     * channel/operator vertices so only process → process edges remain. Best-effort and dynamically
     * typed: the DAG API is version-sensitive, so any shape mismatch is swallowed (the run-level
     * node rollups still come from {@code task_created.group}). Emitted at completion, when the DAG
     * is fully known; the projection re-folds from zero, so the graph backfills onto the run.
     */
    @CompileDynamic
    private void emitRunDag() {
        try {
            final dag = session?.dag
            if (dag == null) return
            final isProc = { v -> v?.type?.toString() == 'PROCESS' && v?.label }
            final adj = [:].withDefault { [] }
            dag.edges.each { e -> if (e.from != null && e.to != null) adj[e.from] << e.to }
            final nodes = [] as Set
            final edges = [] as Set
            dag.vertices.findAll { isProc(it) }.each { pv ->
                nodes << pv.label.toString()
                final seen = [] as Set
                final queue = new ArrayDeque(adj[pv] as Collection)
                while (!queue.isEmpty()) {
                    final n = queue.poll()
                    if (!seen.add(n)) continue
                    if (isProc(n)) {
                        edges << [pv.label.toString(), n.label.toString()]
                    } else {
                        adj[n].each { queue.add(it) }
                    }
                }
            }
            if (nodes) {
                emit(BiffixEvents.runDag(nodes.toList(), edges.collect { it as List<String> }))
            }
        }
        catch (Throwable t) {
            log.warn("nf-biffix: run_dag emission failed (swallowed): ${t.message}")
        }
    }

    /**
     * True when the task runs on AWS Batch (its terminal is EventBridge's to report).
     *
     * Keys off the task's executor name (nf-amazon registers AWS Batch as {@code 'awsbatch'}) — a
     * stable, version-independent identifier — read via the handler, since {@code 'executor'} is not
     * a valid {@link TraceRecord} field. Falls back to the handler class-simple-name substring only
     * when the accessor chain is unavailable, so classification never silently breaks.
     */
    private static boolean isBatchTask(TaskHandler handler) {
        if (handler == null) {
            return false
        }
        final execName = handler.task?.processor?.executor?.name
        if (execName) {
            return execName == 'awsbatch'
        }
        return handler.getClass().getSimpleName().toLowerCase().contains('awsbatch')
    }

    private static String statusOf(TraceRecord trace) {
        final status = trace?.get('status')
        return status != null ? status.toString().toUpperCase() : null
    }

    /**
     * Whether the task failed, by Nextflow's own verdict, the trace's status: COMPLETED is a success
     * whatever the exit status (one in the process's {@code validExitStatus}, or a native
     * {@code exec:} task's, which has none: {@code Integer.MAX_VALUE}). Only a trace without a
     * known status is judged by its exit status, {@code Integer.MAX_VALUE} again meaning none.
     */
    @PackageScope
    static boolean isFailed(TraceRecord trace) {
        final status = statusOf(trace)
        if (status in ['FAILED', 'ABORTED']) {
            return true
        }
        if (status == 'COMPLETED') {
            return false
        }
        final exit = trace?.get('exit')
        if (exit == null) {
            return false
        }
        final code = exit as Integer
        return code != 0 && code != Integer.MAX_VALUE
    }

    private static String nativeIdOf(TraceRecord trace) {
        return str(trace?.get('native_id'))
    }

    @Override
    void onFlowCreate(Session session) {
        try {
            this.session = session
            if (!active) {
                final warning = configWarning(System.getenv())
                if (warning) {
                    log.warn(warning)
                } else {
                    log.debug('nf-biffix: inactive (BIFFIX_RUN_ID / ingest env not set); no events emitted')
                }
                return
            }
            log.info("nf-biffix: tracking run=${runId} -> ${System.getenv('BIFFIX_INGEST_URL')}")
            startPublishTimer()
            // The session uuid the engine actually runs as: biffix cross-checks it against the one it
            // launched the coordinator with (NXF_UUID / -resume) — it keys the run's resume cache.
            emit(BiffixEvents.runStarted(session?.uniqueId?.toString()))
        } catch (Throwable t) {
            log.warn("nf-biffix: onFlowCreate failed (swallowed): ${t.message}")
        }
    }

    @Override
    void onTaskSubmit(TaskEvent event) {
        if (!active) return
        String tkey = null
        try {
            final trace = event.trace
            final handler = event.handler
            tkey = taskKey(trace, handler)
            final attempt = attemptOf(trace, handler)
            if (isBatchTask(handler)) {
                final nativeId = nativeIdOf(trace)
                if (nativeId) {
                    // The binding first: it binds this Batch job id to (run_id, task_key) so the
                    // EventBridge terminal for that job folds into this task. The
                    // terminal comes from EventBridge, not here. It names the image the job runs.
                    emit(BiffixEvents.taskSubmitted(tkey, attempt, nativeId, imageOf(trace, handler)))
                }
            }
            // Always record the task exists + its DAG group (process), for EVERY task — Batch or
            // local. The group is what rolls task states up onto the graph node; task_submitted
            // carries no group, so a Batch task would otherwise never appear under its process.
            // Mirrors the Snakemake plugin, which emits task_created (with the rule) for every job.
            emit(BiffixEvents.taskCreated(tkey, processGroup(trace, handler), taskDeps(handler, tkey)))
            // A retry (attempt > 1) is a distinct lifecycle event, for any executor. Usually already
            // announced when the failed attempt completed (onTaskComplete); this repeats it (it
            // dedupes on next_attempt) for a retry that announcement missed.
            if (attempt > 1) {
                emit(BiffixEvents.taskRetrying(tkey, attempt - 1, attempt))
            }
            // A local task has no Batch job; its terminal is emitted from onTaskComplete.
        } catch (Throwable t) {
            log.warn("nf-biffix: onTaskSubmit failed for ${tkey} (swallowed): ${t.message}")
        }
    }

    @Override
    void onTaskComplete(TaskEvent event) {
        if (!active) return
        String tkey = null
        try {
            final trace = event.trace
            final handler = event.handler
            final failed = isFailed(trace)
            tkey = taskKey(trace, handler)
            final attempt = attemptOf(trace, handler)
            final execution = executionOf(trace, handler)
            if (!isBatchTask(handler)) {
                // A LOCAL task: EventBridge never sees it, so the engine is its only terminal source.
                emit(failed ? BiffixEvents.taskFailed(tkey, attempt, execution) : BiffixEvents.taskSucceeded(tkey, attempt, execution))
            } else if (failed && statusOf(trace) != 'ABORTED') {
                // (An ABORTED Batch task was still pending, or was stopped, when the run stopped: it
                // was not failed by Nextflow, so it is not reported as a failure; Batch reports the
                // end of one that ran itself.)
                if (!nativeIdOf(trace)) {
                    // A Batch task whose submit failed (a job-definition 403, SubmitJob
                    // AccessDenied): onTaskSubmit never ran and there is no Batch job, so
                    // EventBridge never reports it. Register it and report the failure from the
                    // engine.
                    emit(BiffixEvents.taskCreated(tkey, processGroup(trace, handler), taskDeps(handler, tkey)))
                    emit(BiffixEvents.taskFailed(tkey, attempt, execution))
                } else {
                    // A Batch job Nextflow failed. EventBridge still owns the job's terminal, but
                    // Batch reports SUCCEEDED for a job that exited 0 whose outputs are missing, or
                    // whose exit status is outside validExitStatus: the engine's verdict fails the
                    // task anyway (the attempt keeps its AWS facts). Before the retry announcement
                    // below, which the verdict's failure precedes.
                    final failure = failureOf(handler)
                    if (failure != null) {
                        emit(BiffixEvents.engineTaskVerdict(tkey, attempt, failure.category, scrub(failure.reason)))
                    }
                }
            }
            // Otherwise a Batch task gets its terminal from EventBridge (the authoritative source).
            if (failed && trace?.get('error_action')?.toString() == 'RETRY') {
                // Nextflow has decided to retry: announce it now, so the task reads as between
                // attempts (not FAILED) until the retry is submitted. After the terminal above, so
                // that failure predates the announcement the retry's own terminal must answer.
                emit(BiffixEvents.taskRetrying(tkey, attempt, attempt + 1))
            }
            // A successful attempt's declared outputs, in its work dir (they are what downstream
            // tasks consume). A failed attempt's partial files are not outputs, so it reports none.
            if (!failed) {
                emitTaskOutputs(handler, tkey, attempt, false)
            }
        } catch (Throwable t) {
            log.warn("nf-biffix: onTaskComplete failed for ${tkey} (swallowed): ${t.message}")
        }
    }

    /**
     * A task satisfied without running: from the resume cache ({@code CachedTaskHandler}, whose
     * trace is the prior run's) or from its {@code storeDir} ({@code StoredTaskHandler}, whose trace
     * is null), so the key, group and attempt come from the current run's task where they can.
     */
    @Override
    void onTaskCached(TaskEvent event) {
        if (!active) return
        String tkey = null
        try {
            final trace = event.trace
            final handler = event.handler
            tkey = taskKey(trace, handler)
            final attempt = attemptOf(trace, handler)
            // Recorded before anything is emitted: Nextflow publishes a cached task's outputs right
            // after this callback, and those publishes must see the task as reused.
            cachedTaskKeys.add(tkey)
            // Register the task + its DAG group first (like onTaskSubmit): task_cached carries no
            // group, so without this a cached task would never roll up onto its process node.
            emit(BiffixEvents.taskCreated(tkey, processGroup(trace, handler), taskDeps(handler, tkey)))
            // A cached task produced its outputs on a prior run and never became a Batch job this
            // time. Emit a distinct task_cached (not task_succeeded): the reducer folds it to a
            // succeeded terminal but flags it cached, so cost/runtime views don't count it as work run.
            emit(BiffixEvents.taskCached(tkey, attempt, executionOf(trace, handler)))
            // The prior run's outputs this task was satisfied by, flagged reused (not produced now).
            emitTaskOutputs(handler, tkey, attempt, true)
        } catch (Throwable t) {
            log.warn("nf-biffix: onTaskCached failed for ${tkey} (swallowed): ${t.message}")
        }
    }

    /**
     * A file copied to a publish location ({@code publishDir}, or a workflow {@code output} block).
     * Only S3 targets are inventoried — a publish to the coordinator's local disk dies with the
     * container. The publish is credited to the task whose work dir holds its source (recorded by
     * {@link #taskDeps} at submit/cache time), else it is run-level. The backend derives each ref's
     * display path from the run's output prefix, so only the URI is sent. Buffered
     * ({@link PublishBuffer}), so the log gets one event per task per flush rather than one per file.
     */
    @Override
    void onFilePublish(FilePublishEvent event) {
        if (!active) return
        try {
            final target = uriOf(event.target)
            if (!target?.startsWith('s3://')) {
                return
            }
            final key = workKey(uriOf(session?.workDir), uriOf(event.source))
            final tkey = key ? taskByWorkKey.get(key) : null
            bufferPublish(tkey, [s3_uri: target] as Map)
        } catch (Throwable t) {
            log.warn("nf-biffix: file publish tracking failed (swallowed): ${t.message}")
        }
    }

    /**
     * Deliberately a no-op. A workflow {@code output} block publishes each of its files through
     * {@code PublishDir}, which already fires {@link #onFilePublish} per file (with the work-dir
     * source that lets us credit the producing task), and its index file the same way. This event's
     * value is those same files, already rewritten to their publish targets — so inventorying it
     * would only add unattributable run-level duplicates of refs we already hold.
     */
    @Override
    void onWorkflowOutput(WorkflowOutputEvent event) {
    }

    @Override
    void onFlowComplete() {
        if (!active) return
        // session.isSuccess() is Nextflow's authoritative whole-run verdict — it already accounts
        // for failures that never surface as a task error (a job-def 403, a post-start config/compile
        // error, an abort). Defer to it directly. A per-task failure latch must NOT be AND-ed in: a
        // task can fail while the run still succeeds (errorStrategy 'ignore', or a later retry attempt
        // succeeding), and AND-ing a failure latch could only turn such a true success into a false
        // run_failed — it can never fix a false success (which isSuccess() already prevents).
        // Drain the buffered publishes first, so the run's published inventory is complete before
        // its terminal (best-effort; never fatal).
        try {
            stopPublishTimer()
            flushPublishes()
        } catch (Throwable t) {
            log.warn("nf-biffix: publish flush failed (swallowed): ${t.message}")
        }
        // Record the process graph now the DAG is fully known (best-effort; never fatal).
        emitRunDag()
        try {
            final ok = session != null && session.isSuccess() && !session.isAborted() && !session.isCancelled()
            emit(ok ? BiffixEvents.runSucceeded() : BiffixEvents.runFailed())
        } catch (Throwable t) {
            log.warn("nf-biffix: run terminal emission failed (swallowed): ${t.message}")
        }
        // The poster batches in the background; drain it so the terminal is sent before Nextflow
        // exits (its flusher is a daemon thread).
        poster.close()
        eventsFile?.close()
    }
}
