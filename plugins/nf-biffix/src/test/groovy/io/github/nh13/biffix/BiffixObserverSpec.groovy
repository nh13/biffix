package io.github.nh13.biffix

import java.nio.file.FileSystem
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.spi.FileSystemProvider
import java.util.function.LongSupplier
import com.networknt.schema.InputFormat
import com.networknt.schema.Schema
import com.networknt.schema.SchemaRegistry
import com.networknt.schema.SpecificationVersion
import groovy.json.JsonOutput
import nextflow.Session
import nextflow.cloud.CloudSpotTerminationException
import nextflow.dag.DAG
import nextflow.exception.MissingFileException
import nextflow.exception.ProcessFailedException
import nextflow.executor.Executor
import nextflow.processor.TaskConfig
import nextflow.processor.TaskHandler
import nextflow.processor.TaskId
import nextflow.processor.TaskProcessor
import nextflow.processor.TaskRun
import nextflow.script.params.FileOutParam
import nextflow.script.params.ValueOutParam
import nextflow.trace.TraceRecord
import nextflow.trace.event.FilePublishEvent
import nextflow.trace.event.TaskEvent
import spock.lang.Shared
import spock.lang.Specification
import spock.util.concurrent.PollingConditions

/**
 * The observer's dispatch/decision logic — the run-success verdict, batch-vs-local terminal routing,
 * the task key, the cached-task registration, and the retry signal — driven through the public
 * TraceObserverV2 callbacks with a captured poster (the pure event builders are covered by
 * {@link BiffixEventsSpec}).
 */
class BiffixObserverSpec extends Specification {

    IngestPoster poster
    List<Map<String, Object>> posted
    BiffixObserver observer

    /** The engine wire contract ({@code schemas/wire.engine.schema.json}), as BiffixEventsSpec reads it. */
    @Shared
    Schema wire

    def setupSpec() {
        final candidates = ['../../schemas/wire.engine.schema.json', 'schemas/wire.engine.schema.json']
        final file = candidates.collect { new File(it) }.find { it.exists() }
        assert file != null: "wire.engine.schema.json not found from ${System.getProperty('user.dir')}"
        wire = SchemaRegistry.withDefaultDialect(SpecificationVersion.DRAFT_2020_12).getSchema(file.text)
    }

    /**
     * What the observer posted in this feature's scenario conforms to the wire contract: BiffixEventsSpec
     * checks one builder sample per kind, this every event the callbacks actually emit (as the Snakemake
     * plugin's test_schema_conformance does for its handler).
     */
    def cleanup() {
        posted.each { event ->
            final errors = wire.validate(JsonOutput.toJson(event), InputFormat.JSON)
            assert errors.isEmpty(): "${event.kind} does not conform: ${errors}"
        }
    }

    def setup() {
        posted = []
        poster = Stub(IngestPoster)
        poster.isEnabled() >> true
        poster.post(_) >> { args -> posted << (args[0] as Map<String, Object>) }
        observer = observer('run-1', poster)
    }

    /** An observer without the publish timer (a real-time thread the clock-driven tests must not race). */
    private static BiffixObserver observer(String runId, IngestPoster p) {
        final o = new BiffixObserver(runId, p)
        o.publishTimerEnabled = false
        return o
    }

    private List<String> kinds() {
        return posted.collect { it.kind }
    }

    private List<Map<String, Object>> ofKind(String kind) {
        return posted.findAll { it.kind == kind }
    }

    /** A trace holding {@code fields}; like a real TraceRecord, any other field reads null. */
    private TraceRecord trace(Map<String, Object> fields) {
        final t = Stub(TraceRecord)
        t.get(_) >> { args -> fields.get(args[0]) }
        return t
    }

    /** A local task: getTask() is a dummy → executor-name chain empty → class-name fallback (never 'awsbatch'). */
    private TaskHandler localHandler() {
        return Stub(TaskHandler)
    }

    /** A task run by the executor named {@code executor}, as Nextflow names it. */
    private TaskHandler handlerWithExecutor(String executor) {
        return task(executor: executor)
    }

    /** A Batch task: the executor accessor chain reports name 'awsbatch'. */
    private TaskHandler batchHandler() {
        return handlerWithExecutor('awsbatch')
    }

    /**
     * A handler whose task has the given {@code name} (the current run's), {@code index},
     * {@code process} (its processor's name) and {@code executor} name; any may be omitted. For a
     * failed task: its executor {@code error}, the {@code previousException} Nextflow recorded while
     * finalizing it, its {@code exitStatus} and the {@code validExitStatus} list {@code isSuccess}
     * checks it against (default {@code [0]}). Also its resolved {@code container}.
     */
    private TaskHandler task(Map opts) {
        final exec = Stub(Executor) { getName() >> (opts.executor ?: '') }
        final proc = Stub(TaskProcessor) {
            getExecutor() >> exec
            getName() >> (opts.process ?: '')
        }
        final conf = Stub(TaskConfig) { get('previousException') >> opts.previousException }
        final valid = (opts.validExitStatus ?: [0]) as List
        final run = Stub(TaskRun) {
            getProcessor() >> proc
            getName() >> (opts.name ?: '')
            getIndex() >> (opts.index as Integer)
            getWorkDir() >> (opts.workDir ? Paths.get(opts.workDir as String) : null)
            getInputFilesMap() >> ((opts.inputs ?: [:]) as Map).collectEntries { k, v -> [(k): Paths.get(v as String)] }
            getError() >> (opts.error as Throwable)
            getConfig() >> conf
            getExitStatus() >> (opts.exitStatus as Integer)
            isSuccess(_) >> { args -> args[0] in valid }
            getContainer() >> (opts.container as String)
        }
        return Stub(TaskHandler) { getTask() >> run }
    }

    private TaskEvent event(TraceRecord t, TaskHandler h) {
        return Stub(TaskEvent) { getTrace() >> t; getHandler() >> h }
    }

    private Session session(boolean success, boolean aborted = false, boolean cancelled = false, Object dag = null) {
        return Stub(Session) {
            isSuccess() >> success
            isAborted() >> aborted
            isCancelled() >> cancelled
            getDag() >> dag
            getUniqueId() >> UUID.fromString('3f6c2a1e-0000-4000-8000-000000000000')
        }
    }

    def "run_started carries the session's uniqueId"() {
        when:
        observer.onFlowCreate(session(true))

        then:
        with(posted.find { it.kind == 'run_started' }) {
            session_id == '3f6c2a1e-0000-4000-8000-000000000000'
        }
    }

    def "workKey is the task directory under the work dir, or null outside it"() {
        expect:
        BiffixObserver.workKey('s3://b/work', 's3://b/work/ab/cdef01/out.txt') == 'ab/cdef01'
        BiffixObserver.workKey('s3://b/work/', 's3://b/work/ab/cdef01/sub/dir/x') == 'ab/cdef01'
        BiffixObserver.workKey('s3://b/work', 's3://b/inputs/reads.fq') == null
        BiffixObserver.workKey('s3://b/work', 's3://b/work/ab') == null
        BiffixObserver.workKey(null, 's3://b/work/ab/cd/x') == null
    }

    /** A handler whose task ran in work dir `dir` with the given staged inputs. */
    private TaskHandler taskIn(String dir, Map<String, String> inputs) {
        return task(workDir: dir, inputs: inputs)
    }

    def "task_created names the in-run tasks whose outputs it consumes, never itself"() {
        given: 'a session whose work dir is /w'
        final s = session(true)
        s.getWorkDir() >> Paths.get('/w')
        observer.onFlowCreate(s)

        when: 'SAY runs in /w/ab/cdef01, then REVERSE consumes its output plus an external input'
        observer.onTaskSubmit(event(trace([name: 'SAY (1)', process: 'SAY', attempt: 1]), taskIn('/w/ab/cdef01', [:])))
        observer.onTaskSubmit(event(
            trace([name: 'REVERSE (1)', process: 'REVERSE', attempt: 1]),
            taskIn('/w/12/3456ab', ['greeting.txt': '/w/ab/cdef01/greeting.txt', 'ref.fa': '/data/ref.fa'])))
        and: 'a task whose staged input lives in its own work dir'
        observer.onTaskSubmit(event(
            trace([name: 'SELF (1)', process: 'SELF', attempt: 1]),
            taskIn('/w/99/aaaaaa', ['own.txt': '/w/99/aaaaaa/own.txt'])))

        then:
        final created = ofKind('task_created').collectEntries { [(it.task_key): it] }
        !created['SAY (1)'].containsKey('deps')
        created['REVERSE (1)'].deps == ['SAY (1)']
        !created['SELF (1)'].containsKey('deps')
    }

    def "a task failure with an overall-successful session still reports run_succeeded"() {
        // errorStrategy 'ignore' (or a later retry succeeding): a task fails but Nextflow's
        // authoritative session verdict is success. The run must NOT be reported as failed.
        given:
        observer.onFlowCreate(session(true))
        observer.onTaskComplete(event(trace([name: 'x (1)', process: 'x', status: 'FAILED', exit: 1]), localHandler()))

        when:
        observer.onFlowComplete()

        then:
        'run_succeeded' in kinds()
        !('run_failed' in kinds())
        'task_failed' in kinds()  // the task terminal is still reported truthfully
    }

    def "the run terminal is run_succeeded only for a successful, unaborted, uncancelled session"() {
        given:
        if (created) {
            observer.onFlowCreate(session(success, aborted, cancelled))
        }

        when:
        observer.onFlowComplete()

        then:
        kinds().findAll { it in ['run_succeeded', 'run_failed'] } == [want]

        where:
        created | success | aborted | cancelled || want
        true    | true    | false   | false     || 'run_succeeded'
        true    | false   | false   | false     || 'run_failed'
        true    | true    | true    | false     || 'run_failed'
        true    | true    | false   | true      || 'run_failed'
        false   | true    | false   | false     || 'run_failed'  // no session: never a success
    }

    def "the poster is closed after the run terminal is posted, so the terminal is delivered"() {
        given:
        final p = Mock(IngestPoster) { isEnabled() >> true }
        final obs = observer('run-1', p)
        obs.onFlowCreate(session(true))

        when:
        obs.onFlowComplete()

        then:
        1 * p.post({ it.kind == 'run_succeeded' })

        then:
        1 * p.close()
    }

    def "a cached task registers task_created (with its group) before task_cached"() {
        when:
        observer.onTaskCached(event(trace([name: 'align (s1)', process: 'align']), localHandler()))

        then:
        kinds() == ['task_created', 'task_cached']
        posted[0].group == 'align'
        posted[0].task_key == 'align (s1)'
        posted[1].task_key == 'align (s1)'
        posted[1].attempt == 1
    }

    def "a cached task is keyed by the current run's name, not its prior-run trace's"() {
        // CachedTaskHandler's trace is the prior run's: its index-based name can be another task's now.
        when:
        observer.onTaskCached(event(trace([name: 'FOO (1)', process: 'FOO']), task(name: 'FOO (2)', index: 2, process: 'FOO')))

        then:
        ofKind('task_cached')*.task_key == ['FOO (2)']
    }

    def "a task satisfied from its storeDir (no trace) is registered, cached and inventoried from the task"() {
        given:
        observer.onFlowCreate(s3Session(true))
        posted.clear()
        final run = Stub(TaskRun) {
            getName() >> 'STORE (s1)'
            getIndex() >> 1
            getProcessor() >> Stub(TaskProcessor) { getName() >> 'STORE' }
            getWorkDir() >> s3Path('s3://b/work/ab/cd')
            getInputFilesMap() >> [:]
            getOutputs() >> [(Stub(FileOutParam)): s3Path('s3://b/store/o.bam')]
            getId() >> new TaskId(7)
        }

        when: 'StoredTaskHandler reports a null trace'
        observer.onTaskCached(event(null, Stub(TaskHandler) { getTask() >> run }))

        then:
        kinds() == ['task_created', 'task_cached', 'task_outputs']
        posted[0].task_key == 'STORE (s1) #1'
        posted[0].group == 'STORE'
        posted[1].attempt == 1
        posted[1].execution_id == '7'
        with(posted[2]) {
            reused == true
            attempt == 1
            outputs == [[s3_uri: 's3://b/store/o.bam']]
        }
    }

    def "a retried submit (attempt > 1) emits task_retrying; a first attempt does not"() {
        when: 'the second attempt is submitted'
        observer.onTaskSubmit(event(trace([name: 'align (s1)', process: 'align', attempt: 2]), localHandler()))

        then:
        'task_retrying' in kinds()
        with(posted.find { it.kind == 'task_retrying' }) {
            prev_attempt == 1
            next_attempt == 2
        }

        when: 'a fresh first attempt is submitted'
        posted.clear()
        observer.onTaskSubmit(event(trace([name: 'align (s2)', process: 'align', attempt: 1]), localHandler()))

        then:
        !('task_retrying' in kinds())
    }

    def "a local task retried to exhaustion reports each attempt's failure, and each retry when it fails"() {
        given: 'a local task that fails attempts 1 and 2 (Nextflow retries) and then 3 (it gives up)'
        final local = handlerWithExecutor('local')

        when:
        (1..3).each { n ->
            observer.onTaskSubmit(event(trace([name: 'FLAKY (1)', process: 'FLAKY', attempt: n, task_id: 10 + n]), local))
            observer.onTaskComplete(event(trace([name: 'FLAKY (1)', process: 'FLAKY', attempt: n, task_id: 10 + n,
                                                 status: 'FAILED', exit: 1, error_action: n < 3 ? 'RETRY' : 'TERMINATE']), local))
        }

        then: 'every failure carries its attempt, so none dedupes against another'
        ofKind('task_failed')*.attempt == [1, 2, 3]
        ofKind('task_failed')*.task_key.unique() == ['FLAKY (1)']

        and: 'each retry is announced right after the failure it follows (and again, idempotently, at submit)'
        final seq = posted.findAll { it.kind in ['task_failed', 'task_retrying'] }.collect { [it.kind, it.attempt ?: it.next_attempt] }
        seq == [['task_failed', 1], ['task_retrying', 2], ['task_retrying', 2],
                ['task_failed', 2], ['task_retrying', 3], ['task_retrying', 3],
                ['task_failed', 3]]
    }

    def "a local success carries its attempt"() {
        when:
        observer.onTaskComplete(event(trace([name: 'b (1)', process: 'b', status: 'COMPLETED', exit: 0, attempt: 2]), handlerWithExecutor('local')))

        then:
        ofKind('task_succeeded')*.attempt == [2]
    }

    def "a failed Batch attempt Nextflow will retry announces the retry at once, after its verdict, with no engine terminal"() {
        when:
        observer.onTaskComplete(event(trace([name: 'a (1)', process: 'a', status: 'FAILED', exit: 1, attempt: 1,
                                             native_id: 'job-1', error_action: 'RETRY']),
            task(name: 'a (1)', index: 1, process: 'a', executor: 'awsbatch', exitStatus: 1,
                previousException: new ProcessFailedException('Process `a (1)` terminated with an error exit status (1)'))))

        then:
        kinds() == ['engine_task_verdict', 'task_retrying']
        posted[0].attempt == 1
        posted[1].prev_attempt == 1
        posted[1].next_attempt == 2
    }

    // ---- engine_task_verdict ---------------------------------------------------------------------

    /** A Batch task {@code a (1)} that ran as job-1 and completed with {@code status}. */
    private TaskEvent batchCompletion(Map taskOpts, String status = 'FAILED', int attempt = 1, String errorAction = 'TERMINATE') {
        return event(
            trace([name: 'a (1)', process: 'a', status: status, exit: taskOpts.exitStatus, attempt: attempt,
                   native_id: 'job-1', error_action: errorAction]),
            task([name: 'a (1)', index: 1, process: 'a', executor: 'awsbatch'] + taskOpts))
    }

    def "a Batch job that exited 0 but whose outputs are missing gets a missing_output verdict"() {
        when: 'Batch reports SUCCEEDED; Nextflow found no output'
        observer.onTaskComplete(batchCompletion(exitStatus: 0,
            previousException: new MissingFileException('Missing output file(s) `o.bam` expected by process `a (1)`')))

        then:
        kinds() == ['engine_task_verdict']
        with(posted[0]) {
            task_key == 'a (1)'
            attempt == 1
            verdict == 'failed'
            category == 'missing_output'
            reason == 'Missing output file(s) `o.bam` expected by process `a (1)`'
        }
    }

    def "an exit status outside validExitStatus is an app_error verdict, on the attempt that ran"() {
        when: 'exit 0 is not a valid status for this process'
        observer.onTaskComplete(batchCompletion([exitStatus: 0, validExitStatus: [3],
            previousException: new ProcessFailedException('Process `a (1)` terminated with an error exit status (0)')], 'FAILED', 2))

        then:
        with(ofKind('engine_task_verdict')[0]) {
            attempt == 2
            category == 'app_error'
            reason == 'Process `a (1)` terminated with an error exit status (0)'
        }
    }

    def "a verdict with no known cause still fails the attempt, without a category"() {
        when:
        observer.onTaskComplete(batchCompletion(exitStatus: Integer.MAX_VALUE))

        then:
        ofKind('engine_task_verdict') == [[schema_version: 1, ts: posted[0].ts, run_id: 'run-1',
                                           kind: 'engine_task_verdict', task_key: 'a (1)', attempt: 1, verdict: 'failed']]
    }

    def "no verdict for a spot reclaim Nextflow re-runs at the same attempt, an aborted job, or a success"() {
        when: 'the executor reports a spot termination (Nextflow resubmits it without a new attempt)'
        observer.onTaskComplete(batchCompletion([exitStatus: Integer.MAX_VALUE,
            error: new CloudSpotTerminationException('Host EC2 (instance i-1) terminated.')], 'FAILED', 1, 'RETRY'))
        and: 'a job stopped with the run'
        observer.onTaskComplete(batchCompletion([exitStatus: Integer.MAX_VALUE], 'ABORTED'))
        and: 'a job that succeeded'
        observer.onTaskComplete(batchCompletion([exitStatus: 0], 'COMPLETED'))

        then:
        ofKind('engine_task_verdict').isEmpty()
    }

    def "no verdict for a Batch task Nextflow completed with an exit status in validExitStatus"() {
        when:
        observer.onTaskComplete(batchCompletion([exitStatus: 1, validExitStatus: [0, 1]], 'COMPLETED'))

        then:
        ofKind('engine_task_verdict').isEmpty()
    }

    def "a task fails by its trace's status, else by an exit status other than none"() {
        expect:
        BiffixObserver.isFailed(trace(fields)) == failed

        where:
        fields                                         | failed
        [status: 'FAILED', exit: 0]                    | true
        [status: 'aborted']                            | true
        [status: 'COMPLETED', exit: 1]                 | false
        [status: 'COMPLETED', exit: Integer.MAX_VALUE] | false
        [exit: 1]                                      | true
        [exit: 0]                                      | false
        [exit: Integer.MAX_VALUE]                      | false
        [:]                                            | false
    }

    def "a verdict's reason never carries the ingest token"() {
        given:
        final withToken = new BiffixObserver('run-1', poster, null, 'sekrit-token')
        withToken.publishTimerEnabled = false

        when:
        withToken.onTaskComplete(batchCompletion(exitStatus: 0,
            previousException: new MissingFileException('Missing output file(s) `sekrit-token.txt`')))

        then:
        ofKind('engine_task_verdict')[0].reason == 'Missing output file(s) `<token>.txt`'
    }

    // ---- task_submitted image --------------------------------------------------------------------

    def "task_submitted names the image the job runs: the trace's container, else the task's"() {
        given:
        final mapped = '123456789012.dkr.ecr.us-east-1.amazonaws.com/biffix/wf/align@sha256:' + 'a' * 64

        when:
        observer.onTaskSubmit(event(trace([name: 'a (1)', process: 'a', attempt: 1, native_id: 'j1', container: mapped]),
            task(name: 'a (1)', index: 1, process: 'a', executor: 'awsbatch', container: 'ignored')))
        observer.onTaskSubmit(event(trace([name: 'a (2)', process: 'a', attempt: 1, native_id: 'j2']),
            task(name: 'a (2)', index: 2, process: 'a', executor: 'awsbatch', container: mapped)))
        observer.onTaskSubmit(event(trace([name: 'a (3)', process: 'a', attempt: 1, native_id: 'j3']),
            task(name: 'a (3)', index: 3, process: 'a', executor: 'awsbatch')))

        then: 'omitted when neither knows it'
        ofKind('task_submitted').collect { it.image } == [mapped, mapped, null]
        !ofKind('task_submitted')[2].containsKey('image')
    }

    // ---- execution ids ---------------------------------------------------------------------------

    def "a local task Nextflow re-runs at the same attempt reports each execution's failure as its own fact"() {
        given: 'an executor retry: attempt stays 1, each execution has a new task_id'
        final local = handlerWithExecutor('local')

        when:
        [21, 22].each { id ->
            observer.onTaskSubmit(event(trace([name: 'FLAKY (1)', process: 'FLAKY', attempt: 1, task_id: id]), local))
            observer.onTaskComplete(event(trace([name: 'FLAKY (1)', process: 'FLAKY', attempt: 1, task_id: id,
                                                 status: 'FAILED', exit: 1, error_action: id == 21 ? 'RETRY' : 'TERMINATE']), local))
        }
        and: 'a success and a cache hit carry their execution too'
        observer.onTaskComplete(event(trace([name: 'OK (1)', process: 'OK', attempt: 1, task_id: 30, status: 'COMPLETED', exit: 0]), local))
        observer.onTaskCached(event(trace([name: 'C (1)', process: 'C', attempt: 1, task_id: 31]), localHandler()))

        then:
        ofKind('task_failed').collect { [it.attempt, it.execution_id] } == [[1, '21'], [1, '22']]
        ofKind('task_succeeded')*.execution_id == ['30']
        ofKind('task_cached')*.execution_id == ['31']
    }

    def "a restarted coordinator's execution ids carry its Batch attempt, so they never reuse the first's"() {
        given: 'the coordinator is Batch attempt #coordinatorAttempt; Nextflow task ids restart at 1'
        observer.coordinatorAttempt = coordinatorAttempt
        final local = handlerWithExecutor('local')

        when:
        observer.onTaskComplete(event(trace([name: 'OK (1)', process: 'OK', attempt: 1, task_id: 5, status: 'COMPLETED', exit: 0]), local))
        observer.onTaskComplete(event(trace([name: 'BAD (1)', process: 'BAD', attempt: 1, task_id: 6, status: 'FAILED', exit: 1,
                                             error_action: 'TERMINATE']), local))
        observer.onTaskCached(event(trace([name: 'C (1)', process: 'C', attempt: 1, task_id: 7]), localHandler()))

        then:
        ofKind('task_succeeded')*.execution_id == [ids[0]]
        ofKind('task_failed')*.execution_id == [ids[1]]
        ofKind('task_cached')*.execution_id == [ids[2]]

        where:
        coordinatorAttempt || ids
        null               || ['5', '6', '7']
        1                  || ['5', '6', '7']
        2                  || ['2/5', '2/6', '2/7']
    }

    def "the coordinator's Batch attempt is read only from a number"() {
        expect:
        BiffixObserver.batchAttempt(value) == attempt

        where:
        value  || attempt
        null   || null
        ''     || null
        'x'    || null
        '1'    || 1
        ' 2 '  || 2
    }

    def "a batch task gets no engine terminal (EventBridge owns it); a local task does"() {
        when: 'a batch task completes'
        observer.onTaskComplete(event(trace([name: 'a (1)', process: 'a', status: 'COMPLETED', exit: 0, native_id: 'job-1']), batchHandler()))

        then: 'no engine terminal — its terminal comes from EventBridge'
        kinds() == []

        when: 'a local task completes'
        observer.onTaskComplete(event(trace([name: 'b', process: 'b', status: 'COMPLETED', exit: 0]), localHandler()))

        then: 'the engine is its only terminal source'
        kinds() == ['task_succeeded']
    }

    def "a task on a named non-Batch executor is local: an engine terminal, no binding"() {
        when:
        observer.onTaskSubmit(event(trace([name: 'b (1)', process: 'b', attempt: 1, native_id: '4242']), handlerWithExecutor('local')))
        observer.onTaskComplete(event(trace([name: 'b (1)', process: 'b', status: 'FAILED', exit: 2, native_id: '4242']), handlerWithExecutor('local')))

        then:
        !('task_submitted' in kinds())
        'task_failed' in kinds()
    }

    def "a Batch task whose submit failed is registered and failed from the engine; an aborted pending one is not"() {
        when: 'SubmitJob threw: no Batch job id, and onTaskSubmit never ran'
        observer.onTaskComplete(event(trace([name: 'a (1)', process: 'a', status: 'FAILED', attempt: 1]), batchHandler()))

        then:
        kinds() == ['task_created', 'task_failed']
        posted[0].group == 'a'
        posted[1].task_key == 'a (1)'
        posted[1].attempt == 1

        when: 'a task still pending when the run stopped'
        posted.clear()
        observer.onTaskComplete(event(trace([name: 'a (2)', process: 'a', status: 'ABORTED', attempt: 1]), batchHandler()))

        then:
        kinds() == []
    }

    def "a batch submit emits the task_submitted binding, then task_created"() {
        when:
        observer.onTaskSubmit(event(trace([name: 'a (1)', process: 'a', attempt: 1, native_id: 'batch-77']), batchHandler()))

        then:
        kinds() == ['task_submitted', 'task_created']
        posted[0].batch_job_id == 'batch-77'
        posted[0].attempt == 1
    }

    // ---- task_key -------------------------------------------------------------------------------

    def "tasks of one process that share a tag get distinct keys, and a retry keeps its key"() {
        given: 'two chunks of sample s1, both tagged s1 (index 1 and 2)'
        final chunk1 = task(name: 'ALIGN (s1)', index: 1, process: 'ALIGN', executor: 'awsbatch')
        final chunk2 = task(name: 'ALIGN (s1)', index: 2, process: 'ALIGN', executor: 'awsbatch')

        when:
        observer.onTaskSubmit(event(trace([name: 'ALIGN (s1)', process: 'ALIGN', attempt: 1, task_id: 3, native_id: 'j1']), chunk1))
        observer.onTaskSubmit(event(trace([name: 'ALIGN (s1)', process: 'ALIGN', attempt: 1, task_id: 4, native_id: 'j2']), chunk2))
        and: "chunk 2's retry: a new task_id, the same index"
        observer.onTaskSubmit(event(trace([name: 'ALIGN (s1)', process: 'ALIGN', attempt: 2, task_id: 7, native_id: 'j3']), chunk2))

        then: 'every tagged task carries its index'
        ofKind('task_submitted').collect { [it.batch_job_id, it.task_key] } ==
            [['j1', 'ALIGN (s1) #1'], ['j2', 'ALIGN (s1) #2'], ['j3', 'ALIGN (s1) #2']]
        ofKind('task_retrying')*.task_key == ['ALIGN (s1) #2']
    }

    def "a task's key does not depend on which tasks the coordinator saw first"() {
        // A restarted coordinator (the coordinator job's Batch retry) sees the tasks in another order.
        given:
        final chunk1 = task(name: 'ALIGN (s1)', index: 1, process: 'ALIGN', executor: 'awsbatch')
        final chunk2 = task(name: 'ALIGN (s1)', index: 2, process: 'ALIGN', executor: 'awsbatch')
        final restarted = observer('run-1', poster)

        when: 'the first coordinator sees chunk 1 first, the restarted one chunk 2'
        observer.onTaskSubmit(event(trace([name: 'ALIGN (s1)', process: 'ALIGN', attempt: 1, native_id: 'j1']), chunk1))
        observer.onTaskSubmit(event(trace([name: 'ALIGN (s1)', process: 'ALIGN', attempt: 1, native_id: 'j2']), chunk2))
        restarted.onTaskSubmit(event(trace([name: 'ALIGN (s1)', process: 'ALIGN', attempt: 1, native_id: 'k2']), chunk2))
        restarted.onTaskSubmit(event(trace([name: 'ALIGN (s1)', process: 'ALIGN', attempt: 1, native_id: 'k1']), chunk1))

        then: 'each chunk has one key, whichever coordinator reported it'
        ofKind('task_submitted').collectEntries { [(it.batch_job_id): it.task_key] } ==
            [j1: 'ALIGN (s1) #1', j2: 'ALIGN (s1) #2', k2: 'ALIGN (s1) #2', k1: 'ALIGN (s1) #1']
    }

    def "an untagged or single task keeps its name as its key"() {
        when:
        observer.onTaskSubmit(event(trace([name: 'FOO (3)', process: 'FOO', attempt: 1]), task(name: 'FOO (3)', index: 3, process: 'FOO')))
        observer.onTaskSubmit(event(trace([name: 'ONE', process: 'ONE', attempt: 1]), task(name: 'ONE', index: 1, process: 'ONE')))
        observer.onTaskSubmit(event(trace([name: 'TWO (t)', process: 'TWO', attempt: 1]), task(name: 'TWO (t)', index: 1, process: 'TWO')))

        then: 'a tagged single task still carries its index'
        ofKind('task_created')*.task_key == ['FOO (3)', 'ONE', 'TWO (t) #1']
    }

    def "a tag that changes between attempts keeps the task's first key"() {
        when:
        observer.onTaskSubmit(event(trace([name: 'A (try1)', process: 'A', attempt: 1]), task(name: 'A (try1)', index: 4, process: 'A')))
        observer.onTaskSubmit(event(trace([name: 'A (try2)', process: 'A', attempt: 2]), task(name: 'A (try2)', index: 4, process: 'A')))

        then:
        ofKind('task_created')*.task_key == ['A (try1) #4', 'A (try1) #4']
    }

    def "an untagged task's key is its index-based name, stable across retries"() {
        when:
        observer.onTaskSubmit(event(trace([name: 'FOO (3)', process: 'FOO', attempt: 1, task_id: 5]), task(name: 'FOO (3)', index: 3, process: 'FOO')))
        observer.onTaskSubmit(event(trace([name: 'FOO (3)', process: 'FOO', attempt: 2, task_id: 9]), task(name: 'FOO (3)', index: 3, process: 'FOO')))

        then:
        ofKind('task_created')*.task_key == ['FOO (3)', 'FOO (3)']
    }

    def "a task with no name is keyed by its process and index, else its process and task id"() {
        when:
        observer.onTaskComplete(event(trace([process: 'proc', task_id: 42, status: 'COMPLETED', exit: 0]), task(index: 7, process: 'proc')))
        observer.onTaskComplete(event(trace([process: 'proc', task_id: 42, status: 'COMPLETED', exit: 0]), localHandler()))

        then:
        ofKind('task_succeeded')*.task_key == ['proc (7)', 'proc:42']
    }

    // ---- robustness -----------------------------------------------------------------------------

    def "a callback never throws into Nextflow"() {
        given: 'a trace whose attempt is not a number, and a poster that throws'
        final bad = trace([name: 'x (1)', process: 'x', attempt: 'not-a-number', status: 'FAILED', exit: 1])
        final throwing = Stub(IngestPoster) {
            isEnabled() >> true
            post(_) >> { throw new IllegalStateException('boom') }
        }
        final obs = observer('run-1', throwing)

        when:
        observer.onTaskSubmit(event(bad, localHandler()))
        observer.onTaskComplete(event(bad, localHandler()))
        observer.onTaskCached(event(bad, localHandler()))
        obs.onFlowCreate(session(true))
        obs.onTaskSubmit(event(trace([name: 'y (1)', process: 'y', attempt: 1]), localHandler()))
        obs.onFlowComplete()

        then:
        noExceptionThrown()
    }

    def "the observer is inert when the run id or poster is not configured"() {
        given: 'no run id ⇒ inactive, whatever the poster; a run id with a disabled poster ⇒ inactive too'
        final disabled = Stub(IngestPoster) { isEnabled() >> false }
        disabled.post(_) >> { args -> posted << (args[0] as Map<String, Object>) }

        when:
        [observer('', poster), observer('run-1', disabled)].each { inert ->
            inert.onFlowCreate(session(true))
            inert.onTaskSubmit(event(trace([name: 'b', process: 'b', attempt: 1]), localHandler()))
            inert.onTaskComplete(event(trace([name: 'b', process: 'b', status: 'COMPLETED', exit: 0]), localHandler()))
            inert.onFlowComplete()
        }

        then:
        posted.isEmpty()
    }

    def "a partly configured coordinator is named in a warning, without any value"() {
        expect:
        BiffixObserver.configWarning([:]) == null
        BiffixObserver.configWarning([BIFFIX_RUN_ID: 'r', BIFFIX_INGEST_URL: 'u', BIFFIX_INGEST_TOKEN: 't']) == null
        BiffixObserver.configWarning([BIFFIX_RUN_ID: 'r', BIFFIX_INGEST_TOKEN: 'secret-token']) ==
            'nf-biffix: BIFFIX_INGEST_URL not set; this run is not tracked by biffix'
        !BiffixObserver.configWarning([BIFFIX_INGEST_TOKEN: 'secret-token']).contains('secret-token')
    }

    // ---- run_dag --------------------------------------------------------------------------------

    def "run_dag collapses operator vertices into process → process edges"() {
        given: 'origin → a → (map operator) → b'
        final dag = new DAG()
        final origin = new DAG.Vertex(dag, DAG.Type.ORIGIN)
        final a = new DAG.Vertex(dag, DAG.Type.PROCESS, 'a')
        final op = new DAG.Vertex(dag, DAG.Type.OPERATOR, 'map')
        final b = new DAG.Vertex(dag, DAG.Type.PROCESS, 'b')
        final graph = Stub(DAG) {
            getVertices() >> [origin, a, op, b]
            getEdges() >> [[from: origin, to: a], [from: a, to: op], [from: op, to: b]]
        }
        observer.onFlowCreate(session(true, false, false, graph))

        when:
        observer.onFlowComplete()

        then:
        with(ofKind('run_dag')[0].dag) {
            nodes as Set == [[id: 'a'], [id: 'b']] as Set
            edges == [[from: 'a', to: 'b']]
        }
        kinds().indexOf('run_dag') < kinds().indexOf('run_succeeded')
    }

    // ---- task_outputs ---------------------------------------------------------------------------

    /**
     * A Path in an S3 filesystem, as {@code FilesEx.toUriString} sees it (scheme {@code s3}, URI
     * string = {@code uri}) — without pulling nf-amazon into the tests.
     */
    private Path s3Path(String uri) {
        final provider = Stub(FileSystemProvider) { getScheme() >> 's3' }
        final fs = Stub(FileSystem) { provider() >> provider }
        final name = uri.substring(uri.lastIndexOf('/') + 1)
        return Stub(Path) {
            getFileSystem() >> fs
            toUri() >> new URI(uri)
            getFileName() >> Paths.get(name)
            toString() >> uri
        }
    }

    /** A session whose work dir is the S3 prefix {@code s3://b/work}. */
    private Session s3Session(boolean success) {
        final s = session(success)
        s.getWorkDir() >> s3Path('s3://b/work')
        return s
    }

    /** A handler whose task ran in {@code dir} and declared {@code outputs} (param → value). */
    private TaskHandler taskWithOutputs(Object dir, Map outputs) {
        final run = Stub(TaskRun) {
            getWorkDir() >> (dir instanceof String ? s3Path(dir as String) : dir)
            getInputFilesMap() >> [:]
            getOutputs() >> outputs
        }
        return Stub(TaskHandler) { getTask() >> run }
    }

    private List<Map<String, Object>> outputsEvents() {
        return ofKind('task_outputs')
    }

    def "a successful task reports its declared S3 file outputs, skipping non-S3, non-file and bookkeeping files"() {
        given:
        observer.onFlowCreate(s3Session(true))
        final declared = [
            (Stub(FileOutParam)) : [s3Path('s3://b/work/ab/cd/b.bam'), [s3Path('s3://b/work/ab/cd/a.bai'), s3Path('s3://b/work/ab/cd/.command.sh'), s3Path('s3://b/work/ab/cd/.exitcode')]],
            (Stub(FileOutParam)) : Paths.get('/local/x.txt'),
            (Stub(ValueOutParam)): s3Path('s3://b/work/ab/cd/value.txt'),
        ]
        posted.clear()

        when:
        observer.onTaskComplete(event(trace([name: 'a (1)', process: 'a', status: 'COMPLETED', exit: 0, attempt: 2]),
            taskWithOutputs('s3://b/work/ab/cd', declared)))

        then:
        kinds() == ['task_succeeded', 'task_outputs']
        with(outputsEvents()[0]) {
            task_key == 'a (1)'
            attempt == 2
            role == 'intermediate'
            !containsKey('reused')
            outputs == [[s3_uri: 's3://b/work/ab/cd/a.bai'], [s3_uri: 's3://b/work/ab/cd/b.bam']]
        }
    }

    def "a task Nextflow completed succeeds whatever its exit status, and reports its outputs"() {
        when: 'a native exec: task (no exit status) and one whose exit 1 is in validExitStatus'
        observer.onTaskComplete(event(trace([name: 'a (1)', process: 'a', status: 'COMPLETED', exit: exit, attempt: 1]),
            taskWithOutputs('s3://b/work/ab/cd', [(Stub(FileOutParam)): s3Path('s3://b/work/ab/cd/o.txt')])))

        then:
        kinds() == ['task_succeeded', 'task_outputs']
        outputsEvents()[0].outputs == [[s3_uri: 's3://b/work/ab/cd/o.txt']]

        where:
        exit << [Integer.MAX_VALUE, 1]
    }

    def "a failed task reports no outputs"() {
        when:
        observer.onTaskComplete(event(trace([name: 'a (1)', process: 'a', status: 'FAILED', exit: 1, attempt: 1]),
            taskWithOutputs('s3://b/work/ab/cd', [(Stub(FileOutParam)): s3Path('s3://b/work/ab/cd/o.txt')])))

        then:
        kinds() == ['task_failed']
    }

    def "a task with no S3 outputs emits no task_outputs"() {
        when:
        observer.onTaskComplete(event(trace([name: 'a (1)', process: 'a', status: 'COMPLETED', exit: 0, attempt: 1]),
            taskWithOutputs(Paths.get('/w/ab/cd'), [(Stub(FileOutParam)): Paths.get('/w/ab/cd/o.txt')])))

        then:
        kinds() == ['task_succeeded']
    }

    def "a cached task reports its outputs as reused, after task_created and task_cached"() {
        given:
        observer.onFlowCreate(s3Session(true))
        posted.clear()

        when:
        observer.onTaskCached(event(trace([name: 'align (s1)', process: 'align', attempt: 1]),
            taskWithOutputs('s3://b/work/ab/cd', [(Stub(FileOutParam)): s3Path('s3://b/work/ab/cd/o.bam')])))

        then:
        kinds() == ['task_created', 'task_cached', 'task_outputs']
        with(posted[2]) {
            task_key == 'align (s1)'
            reused == true
            role == 'intermediate'
            outputs == [[s3_uri: 's3://b/work/ab/cd/o.bam']]
        }
    }

    def "a publish is credited to the task whose work dir holds its source, and flushed before the run terminal"() {
        given:
        observer.onFlowCreate(s3Session(true))
        observer.onTaskSubmit(event(trace([name: 'SAY (1)', process: 'SAY', attempt: 1]), taskWithOutputs('s3://b/work/ab/cdef01', [:])))

        when: 'SAY publishes its output, and an unrelated file is published to local disk'
        observer.onFilePublish(new FilePublishEvent(s3Path('s3://b/work/ab/cdef01/hi.txt'), s3Path('s3://b/runs/r1/results/hi.txt')))
        observer.onFilePublish(new FilePublishEvent(s3Path('s3://b/work/ab/cdef01/hi.txt'), Paths.get('/local/results/hi.txt')))

        then: 'buffered, not yet emitted'
        outputsEvents().isEmpty()

        when:
        observer.onFlowComplete()

        then: 'one published batch for SAY (the URI only: the backend derives the display path), before run_succeeded'
        outputsEvents().size() == 1
        with(outputsEvents()[0]) {
            task_key == 'SAY (1)'
            role == 'published'
            !containsKey('attempt')
            !containsKey('reused')
            outputs == [[s3_uri: 's3://b/runs/r1/results/hi.txt']]
        }
        kinds().indexOf('task_outputs') < kinds().indexOf('run_succeeded')
    }

    def "a publish from outside any task work dir is run-level, and precedes run_failed too"() {
        given:
        observer.onFlowCreate(s3Session(false))

        when:
        observer.onFilePublish(new FilePublishEvent(s3Path('s3://b/inputs/ref.fa'), s3Path('s3://b/results/ref.fa')))
        observer.onFlowComplete()

        then: 'no task_key'
        with(outputsEvents()[0]) {
            !containsKey('task_key')
            outputs == [[s3_uri: 's3://b/results/ref.fa']]
        }
        kinds().indexOf('task_outputs') < kinds().indexOf('run_failed')
    }

    def "a cached task's publishes are flagged reused"() {
        given:
        observer.onFlowCreate(s3Session(true))
        observer.onTaskCached(event(trace([name: 'align (s1)', process: 'align', attempt: 1]), taskWithOutputs('s3://b/work/ab/cd', [:])))

        when:
        observer.onFilePublish(new FilePublishEvent(s3Path('s3://b/work/ab/cd/o.bam'), s3Path('s3://b/results/o.bam')))
        observer.onFlowComplete()

        then:
        with(outputsEvents().find { it.role == 'published' }) {
            task_key == 'align (s1)'
            reused == true
        }
    }

    def "publishes flush once 200 are buffered"() {
        given: 'a frozen clock, so only the count can trigger a flush'
        observer.clock = { 1000L } as LongSupplier
        observer.onFlowCreate(s3Session(true))

        when: 'one short of the threshold'
        (1..199).each { i ->
            observer.onFilePublish(new FilePublishEvent(s3Path("s3://b/in/${i}"), s3Path("s3://b/results/${i}.txt")))
        }

        then:
        outputsEvents().isEmpty()

        when: 'the 200th publish'
        observer.onFilePublish(new FilePublishEvent(s3Path('s3://b/in/200'), s3Path('s3://b/results/200.txt')))

        then: 'one batch holding all 200, and the buffer is empty again'
        outputsEvents().size() == 1
        outputsEvents()[0].outputs.size() == 200

        when:
        observer.onFlowComplete()

        then: 'nothing left to flush'
        outputsEvents().size() == 1
    }

    def "publishes flush once 2s have passed since the last flush"() {
        given:
        long now = 10_000L
        observer.clock = { now } as LongSupplier
        observer.onFlowCreate(s3Session(true))

        when:
        observer.onFilePublish(new FilePublishEvent(s3Path('s3://b/in/1'), s3Path('s3://b/results/1.txt')))
        now += 1999L
        observer.onFilePublish(new FilePublishEvent(s3Path('s3://b/in/2'), s3Path('s3://b/results/2.txt')))

        then:
        outputsEvents().isEmpty()

        when:
        now += 1L
        observer.onFilePublish(new FilePublishEvent(s3Path('s3://b/in/3'), s3Path('s3://b/results/3.txt')))

        then:
        outputsEvents().size() == 1
        outputsEvents()[0].outputs.size() == 3
    }

    def "a quiet publish buffer is flushed by the timer's tick once the interval has passed"() {
        given:
        long now = 10_000L
        observer.clock = { now } as LongSupplier
        observer.onFlowCreate(s3Session(true))
        observer.onFilePublish(new FilePublishEvent(s3Path('s3://b/in/1'), s3Path('s3://b/results/1.txt')))

        when: 'a tick before the interval'
        now += 1999L
        observer.flushDuePublishes()

        then:
        outputsEvents().isEmpty()

        when: 'a tick once it has passed, with no further publish'
        now += 1L
        observer.flushDuePublishes()

        then:
        outputsEvents().size() == 1
    }

    def "the publish timer runs between flow create and complete"() {
        given:
        final timed = new BiffixObserver('run-1', poster)
        timed.onFlowCreate(s3Session(true))

        when: 'one publish, then nothing: only the timer can flush it'
        timed.onFilePublish(new FilePublishEvent(s3Path('s3://b/in/1'), s3Path('s3://b/results/1.txt')))

        then:
        new PollingConditions(timeout: 3 * PublishBuffer.FLUSH_MILLIS / 1000.0d + 1).eventually {
            assert outputsEvents().size() == 1
        }

        when:
        timed.onFlowComplete()

        then: 'stopped'
        timed.@publishTimer == null
    }
}
