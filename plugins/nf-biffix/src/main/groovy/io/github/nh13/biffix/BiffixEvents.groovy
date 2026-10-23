package io.github.nh13.biffix

import groovy.transform.CompileStatic

/**
 * Pure construction of the biffix engine-wire events ({@code schemas/wire.engine.schema.json}).
 *
 * Kept free of Nextflow types so it can be unit-tested directly against the shared schema: the
 * observer extracts primitive values from a {@code TraceRecord} and hands them here, so the wire
 * shape lives in one testable place and cannot silently drift from the schema the reducer folds.
 * The envelope is {@code {schema_version, ts, run_id, kind, ...body}} — the same shape the Snakemake
 * logger emits ({@code events.py}). Only the kinds this plugin emits have a builder; the ones it
 * never sends are listed, with the reason, in {@code BiffixEventsSpec.NOT_EMITTED}.
 */
@CompileStatic
class BiffixEvents {

    /**
     * Wrap an event body in the shared envelope. {@code schemaVersion} is supplied by the caller
     * (the observer owns the single {@code SCHEMA_VERSION} literal the cross-language guard checks),
     * so this stays a pure function with no duplicated constant.
     */
    static Map<String, Object> envelope(int schemaVersion, String runId, double ts, Map<String, Object> body) {
        final Map<String, Object> ev = [schema_version: schemaVersion, ts: ts, run_id: runId] as Map<String, Object>
        ev.putAll(body)
        return ev
    }

    /**
     * The engine began executing; {@code sessionId} is Nextflow's session uuid (the resume-cache
     * key). The key is omitted when null, like {@code taskCreated}'s {@code group}.
     */
    static Map<String, Object> runStarted(String sessionId) {
        final Map<String, Object> body = [kind: 'run_started'] as Map<String, Object>
        if (sessionId != null) {
            body.session_id = sessionId
        }
        return body
    }

    static Map<String, Object> runSucceeded() {
        return [kind: 'run_succeeded'] as Map<String, Object>
    }

    static Map<String, Object> runFailed() {
        return [kind: 'run_failed'] as Map<String, Object>
    }

    /**
     * A logical task registered; {@code group} is its DAG node (process), {@code deps} the task keys
     * of the in-run tasks that produced its inputs. {@code group} is omitted when null (matching the
     * Snakemake producer's shape for the same wire kind rather than emitting {@code group: null}),
     * {@code deps} when empty; deps are sorted and de-duplicated.
     */
    static Map<String, Object> taskCreated(String taskKey, String group, Collection<String> deps) {
        final Map<String, Object> body = [kind: 'task_created', task_key: taskKey] as Map<String, Object>
        if (group != null) {
            body.group = group
        }
        if (deps) {
            body.deps = new TreeSet<String>(deps) as List<String>
        }
        return body
    }

    /**
     * An attempt submitted to Batch — binds {@code batch_job_id → (run_id, task_key)}. {@code image}
     * is the container the job runs as Nextflow resolved it (in a biffix run, the tenant copy the
     * run's image map names); omitted when null.
     */
    static Map<String, Object> taskSubmitted(String taskKey, int attempt, String batchJobId, String image) {
        final Map<String, Object> body = [kind: 'task_submitted', task_key: taskKey, attempt: attempt, batch_job_id: batchJobId] as Map<String, Object>
        if (image != null) {
            body.image = image
        }
        return body
    }

    /**
     * The longest {@code reason} an {@code engine_task_verdict} carries, in code points (as the
     * Snakemake plugin counts); a longer one is cut.
     */
    static final int REASON_MAX = 1000

    /**
     * Nextflow failed a Batch attempt, whatever Batch reported for its job: {@code verdict: failed},
     * with its {@code category} ({@code missing_output}, {@code app_error}, …) and {@code reason}
     * (Nextflow's error message, cut to {@link #REASON_MAX} characters) when known. Null fields and
     * an empty reason are omitted.
     */
    static Map<String, Object> engineTaskVerdict(String taskKey, Integer attempt, String category, String reason) {
        final Map<String, Object> body = [kind: 'engine_task_verdict', task_key: taskKey, verdict: 'failed'] as Map<String, Object>
        if (attempt != null) {
            body.attempt = attempt
        }
        if (category != null) {
            body.category = category
        }
        if (reason) {
            // Cut on a code point, never inside a surrogate pair: a lone surrogate is not valid
            // JSON text, so the backend would refuse the event.
            body.reason = reason.codePointCount(0, reason.length()) > REASON_MAX
                ? reason.substring(0, reason.offsetByCodePoints(0, REASON_MAX - 1)) + '…'
                : reason
        }
        return body
    }

    /**
     * An engine-sourced success terminal: a local task's (no Batch job). {@code attempt} and
     * {@code executionId} are omitted when null. Both keep terminals distinct in the log (the ingest
     * dedup key carries them), so a retried task's second terminal is not dropped as a redelivery
     * of the first: the attempt for a retry Nextflow numbers, the execution (the trace's
     * {@code task_id}, new for every run of a task) for an executor retry that keeps the attempt.
     */
    static Map<String, Object> taskSucceeded(String taskKey, Integer attempt, String executionId) {
        return terminal('task_succeeded', taskKey, attempt, executionId)
    }

    /**
     * An engine-sourced failure terminal: a local task's, or a Batch task's that never became a
     * Batch job (its submit failed). {@code attempt} and {@code executionId} as for {@link #taskSucceeded}.
     */
    static Map<String, Object> taskFailed(String taskKey, Integer attempt, String executionId) {
        return terminal('task_failed', taskKey, attempt, executionId)
    }

    /** A cache-hit task (e.g. Nextflow {@code -resume}); folds to succeeded-but-cached. */
    static Map<String, Object> taskCached(String taskKey, Integer attempt, String executionId) {
        return terminal('task_cached', taskKey, attempt, executionId)
    }

    private static Map<String, Object> terminal(String kind, String taskKey, Integer attempt, String executionId) {
        final Map<String, Object> body = [kind: kind, task_key: taskKey] as Map<String, Object>
        if (attempt != null) {
            body.attempt = attempt
        }
        if (executionId != null) {
            body.execution_id = executionId
        }
        return body
    }

    /** A retry: {@code prevAttempt} failed and {@code nextAttempt} is being submitted. */
    static Map<String, Object> taskRetrying(String taskKey, int prevAttempt, int nextAttempt) {
        return [
            kind        : 'task_retrying',
            task_key    : taskKey,
            prev_attempt: prevAttempt,
            next_attempt: nextAttempt,
        ] as Map<String, Object>
    }

    /**
     * A batch of output objects for one task (or, with a null {@code taskKey}, for the run itself —
     * e.g. a publish no task can be credited with). Each output is
     * {@code [s3_uri: …, path?: …, etag?: …, size?: …]}. The backend folds these as a set keyed on
     * {@code s3_uri}, so the list is de-duplicated on it (first occurrence wins) and sorted, making
     * the body independent of callback order. Null fields and a false {@code reused} are omitted
     * rather than emitted, like the other builders.
     */
    static Map<String, Object> taskOutputs(String taskKey, Integer attempt, String role, boolean reused, List<Map> outputs) {
        final Map<String, Map> byUri = new TreeMap<String, Map>()
        for (Map out : outputs) {
            final uri = out?.get('s3_uri')
            if (uri == null || byUri.containsKey(uri.toString())) {
                continue
            }
            final Map<String, Object> ref = [s3_uri: uri.toString()] as Map<String, Object>
            ['path', 'etag', 'size'].each { String k ->
                if (out.get(k) != null) {
                    ref.put(k, out.get(k))
                }
            }
            byUri.put(uri.toString(), ref)
        }
        final Map<String, Object> body = [kind: 'task_outputs'] as Map<String, Object>
        if (taskKey != null) {
            body.task_key = taskKey
        }
        if (attempt != null) {
            body.attempt = attempt
        }
        if (role != null) {
            body.role = role
        }
        if (reused) {
            body.reused = true
        }
        body.outputs = new ArrayList<Map>(byUri.values())
        return body
    }

    /** The run's group-level process graph: node ids + directed {@code [from, to]} edges. */
    static Map<String, Object> runDag(List<String> nodes, List<List<String>> edges) {
        return [
            kind: 'run_dag',
            dag : [
                nodes: nodes.collect { [id: it] as Map<String, Object> },
                edges: edges.collect { [from: it[0], to: it[1]] as Map<String, Object> },
            ],
        ] as Map<String, Object>
    }
}
