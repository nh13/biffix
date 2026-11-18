package io.github.nh13.biffix

import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths

import nextflow.SysEnv
import nextflow.cloud.aws.batch.AwsBatchExecutor
import nextflow.cloud.aws.batch.AwsBatchTaskHandler
import nextflow.cloud.aws.batch.AwsOptions
import nextflow.cloud.aws.config.AwsConfig
import nextflow.exception.AbortOperationException
import nextflow.exception.ProcessUnrecoverableException
import nextflow.executor.Executor
import nextflow.executor.ExecutorFactory
import nextflow.executor.TaskArrayExecutor
import nextflow.processor.TaskArrayRun
import nextflow.processor.TaskConfig
import nextflow.processor.TaskRun
import nextflow.util.CmdLineHelper
import nextflow.util.ServiceName
import org.pf4j.PluginManager
import software.amazon.awssdk.services.batch.BatchClient
import spock.lang.Specification
import spock.lang.TempDir

/**
 * The biffix AWS Batch executor is nf-amazon's under the `awsbatch` name, except that in a biffix
 * run every task is submitted to the job definition the backend registered for its image, and the
 * coordinator registers and describes none. Mirrors nf-amazon's own AwsBatchTaskHandlerTest (a Spy
 * handler over a mocked BatchClient).
 */
class BiffixAwsBatchExecutorSpec extends Specification {

    static final String IMAGE = '123456789012.dkr.ecr.us-east-1.amazonaws.com/workers@sha256:abc'
    static final String DEFINITION = 'arn:aws:batch:us-east-1:123456789012:job-definition/biffix-t_task-nf-0123abcd:1'
    static final String LATEST = '123456789012.dkr.ecr.us-east-1.amazonaws.com/tools:latest'
    static final String LATEST_DEFINITION = 'arn:aws:batch:us-east-1:123456789012:job-definition/biffix-t_task-nf-4567ef01:3'

    static final ImageMap MAP = new ImageMap([(IMAGE): DEFINITION, (LATEST): LATEST_DEFINITION])

    @TempDir
    Path tmp

    /** The aws.batch options the coordinator writes (a job role, a log group, the host AWS CLI), plus `batch`. */
    static AwsOptions options(Map batch = [:]) {
        new AwsOptions(awsConfig: new AwsConfig(
            region: 'us-east-1',
            batch: [jobRole: 'arn:aws:iam::123456789012:role/biffix-run-a', logsGroup: '/biffix/project-a/tasks',
                    cliPath: '/opt/biffix/aws-cli/bin/aws'] + batch))
    }

    /** A task running `image`, with `containerOptions` if given. */
    TaskRun task(String image, String containerOptions = null) {
        final config = Mock(TaskConfig) {
            getContainerOptionsMap() >> (containerOptions ? CmdLineHelper.parseGnuArgs(containerOptions) : CmdLineHelper.parseGnuArgs(''))
        }
        Mock(TaskRun) {
            getContainer() >> image
            getConfig() >> config
            getWorkDir() >> Paths.get('/work/ab/cdef')
            getName() >> 'ALIGN (1)'
            lazyName() >> 'ALIGN (1)'
        }
    }

    /** A biffix handler with `map` (null: none) for `task`, over `client`. */
    AwsBatchTaskHandler handler(ImageMap map, TaskRun task, BatchClient client, AwsOptions opts = options(), boolean fusion = false) {
        final executor = Mock(AwsBatchExecutor) { getAwsOptions() >> opts }
        final h = Spy(BiffixAwsBatchTaskHandler, constructorArgs: [task, executor, map]) { fusionEnabled() >> fusion }
        // nf-amazon's tests set the (private) client the same way; it is declared on AwsBatchTaskHandler.
        final field = AwsBatchTaskHandler.getDeclaredField('client')
        field.accessible = true
        field.set(h, client)
        return h
    }

    // (1) a mapped image runs on its definition, and nothing is registered or described

    def 'a mapped image runs on the backend\'s definition, with no Batch call'() {
        given:
        def client = Mock(BatchClient)
        def t = task(IMAGE)
        def h = handler(MAP, t, client)

        when:
        def definition = h.getJobDefinition(t)

        then:
        definition == DEFINITION
        0 * h.resolveJobDefinition(_)
        0 * h.createJobDef(_)
        0 * client._
    }

    def 'the submit request nf-amazon builds names the backend\'s definition, with no Batch call'() {
        given:
        def client = Mock(BatchClient)
        def t = Mock(TaskRun) {
            getName() >> 'ALIGN (1)'
            lazyName() >> 'ALIGN (1)'
            getContainer() >> IMAGE
            getWorkDir() >> Paths.get('/work/ab/cdef')
            getConfig() >> new TaskConfig(memory: '8GB', cpus: 4)
        }
        def h = handler(MAP, t, client)

        when:
        def req = h.newSubmitRequest(t)

        then:
        1 * h.getSubmitCommand() >> ['bash', '-c', 'something']
        1 * h.maxSpotAttempts() >> 0
        1 * h.getJobQueue(t) >> 'queue1'
        1 * h.getEnvironmentVars() >> []
        0 * h.resolveJobDefinition(_)
        0 * h.createJobDef(_)
        0 * client._
        req.jobDefinition() == DEFINITION
        req.jobQueue() == 'queue1'
        req.containerOverrides().command() == ['bash', '-c', 'something']
    }

    def 'the image is looked up normalized too'() {
        given:
        def client = Mock(BatchClient)
        // No tag is `:latest`, and a registry host is case-insensitive.
        def t = task('123456789012.DKR.ECR.us-east-1.amazonaws.com/tools')
        def h = handler(MAP, t, client)

        expect:
        h.getJobDefinition(t) == LATEST_DEFINITION
        MAP.resolve('ubuntu:24.04') == null
    }

    def 'an image with no definition fails the task, unrecoverably'() {
        given:
        def client = Mock(BatchClient)
        def t = task('123456789012.dkr.ecr.us-east-1.amazonaws.com/workers@sha256:def')
        def h = handler(MAP, t, client)

        when:
        h.getJobDefinition(t)

        then:
        def e = thrown(ProcessUnrecoverableException)
        e.message.contains('workers@sha256:def')
        e.message.contains('no biffix task job definition')
        0 * h.resolveJobDefinition(_)
        0 * client._
    }

    def 'a job-definition:// container fails the task, unrecoverably'() {
        given:
        def client = Mock(BatchClient)
        def t = task('job-definition://x')
        def h = handler(new ImageMap(['job-definition://x': DEFINITION]), t, client)

        when:
        h.getJobDefinition(t)

        then:
        def e = thrown(ProcessUnrecoverableException)
        e.message.contains('job-definition://x')
        0 * client._
    }

    // (2) a setting nf-amazon would put in the definition fails the task rather than being dropped

    def 'containerOptions a definition would carry fail the task, naming them: #opts'() {
        given:
        def client = Mock(BatchClient)
        def t = task(IMAGE, opts)
        def h = handler(MAP, t, client)

        when:
        h.getJobDefinition(t)

        then:
        def e = thrown(ProcessUnrecoverableException)
        e.message.contains('containerOptions')
        names.every { e.message.contains(it) }
        0 * client._

        where:
        opts                                | names
        '--privileged'                      | ['--privileged']
        '--shm-size 1g'                     | ['--shm-size']
        '--ulimit nofile=1:2'               | ['--ulimit']
        '-e FOO=bar --user 1000 --init'     | ['-e', '--user', '--init']
        '--read-only --gpus all'            | ['--read-only']
    }

    def 'containerOptions nf-amazon ignores on Batch do not fail the task: #opts'() {
        given:
        def t = task(IMAGE, opts)
        def h = handler(MAP, t, Mock(BatchClient))

        expect:
        h.getJobDefinition(t) == DEFINITION

        where:
        opts << ['--gpus all', '--cpus 2 --network host', '']
    }

    def 'aws.batch.volumes fail the task'() {
        given:
        def t = task(IMAGE)
        def h = handler(MAP, t, Mock(BatchClient), options(volumes: '/data:/data:ro'))

        when:
        h.getJobDefinition(t)

        then:
        def e = thrown(ProcessUnrecoverableException)
        e.message.contains('aws.batch.volumes')
        e.message.contains('/data:/data:ro')
    }

    def 'Fusion, Fargate and an execution role fail the task'() {
        given:
        def t = task(IMAGE)
        def h = handler(MAP, t, Mock(BatchClient), options(batch), fusion)

        when:
        h.getJobDefinition(t)

        then:
        def e = thrown(ProcessUnrecoverableException)
        e.message.contains(setting)

        where:
        batch                                          | fusion | setting
        [:]                                            | true   | 'Fusion'
        [platformType: 'fargate']                      | false  | 'Fargate'
        [executionRole: 'arn:aws:iam::1:role/exec']    | false  | 'aws.batch.executionRole'
    }

    def 'a consumable-resources hint (nf-amazon 3.9+) fails the task: #key'() {
        given:
        def config = Mock(TaskConfig) {
            getContainerOptionsMap() >> CmdLineHelper.parseGnuArgs('')
            get('hints') >> [(key): [licenses: 1]]
        }
        def t = Mock(TaskRun) {
            getContainer() >> IMAGE
            getConfig() >> config
            getWorkDir() >> Paths.get('/work/ab/cdef')
            lazyName() >> 'ALIGN (1)'
        }
        def h = handler(MAP, t, Mock(BatchClient))

        when:
        h.getJobDefinition(t)

        then:
        def e = thrown(ProcessUnrecoverableException)
        e.message.contains('consumableResources')

        where:
        key << ['awsbatch/consumableResources', 'consumableResources']
    }

    // (3) outside biffix, nf-amazon's handler

    def 'with no map the definition is nf-amazon\'s to resolve'() {
        given:
        def t = task(IMAGE, '--privileged')
        def h = handler(null, t, Mock(BatchClient), options(volumes: '/data'))

        when:
        def definition = h.getJobDefinition(t)

        then:
        1 * h.resolveJobDefinition(t) >> 'nf-workers:1'
        definition == 'nf-workers:1'
    }

    // (4) the map file

    def 'the executor reads the map from BIFFIX_JOB_DEFINITION_MAP_FILE once and hands it to its handlers'() {
        given:
        def file = tmp.resolve('job-definitions.json')
        Files.writeString(file, """{"$IMAGE": "$DEFINITION"}""")
        def executor = withEnv([BIFFIX_JOB_DEFINITION_MAP_FILE: file.toString()]) { new BiffixAwsBatchExecutor() }
        def t = new TaskRun(name: 'ALIGN (1)', workDir: Paths.get('/work/ab/cdef'))

        when:
        def h = executor.createTaskHandler(t)

        then:
        h instanceof BiffixAwsBatchTaskHandler
        (h as BiffixAwsBatchTaskHandler).jobDefinitions.entries == [(IMAGE): DEFINITION]
        executor.mapFile == file.toString()

        when: 'the file changes under a running executor'
        Files.writeString(file, '{}')

        then: 'the map read first stands'
        executor.jobDefinitions().is((executor.createTaskHandler(t) as BiffixAwsBatchTaskHandler).jobDefinitions)
        executor.jobDefinitions().size() == 1

        when: 'no variable'
        def none = withEnv([:]) { new BiffixAwsBatchExecutor() }

        then: 'no map, so nf-amazon\'s handler'
        none.jobDefinitions() == null
        (none.createTaskHandler(t) as BiffixAwsBatchTaskHandler).jobDefinitions == null
    }

    def 'an unreadable or malformed map stops the run: #why'() {
        given:
        def file = tmp.resolve('job-definitions.json')
        if( content != null )
            Files.writeString(file, content)
        def executor = new BiffixAwsBatchExecutor(file.toString())

        when:
        executor.jobDefinitions()

        then:
        def e = thrown(AbortOperationException)
        e.message.contains('BIFFIX_JOB_DEFINITION_MAP_FILE')
        e.message.contains(reason)

        where:
        why               | content                         | reason
        'missing'         | null                            | 'job-definitions.json'
        'not JSON'        | 'nope'                          | 'JsonException'
        'not an object'   | '["arn"]'                       | 'the map is not a JSON object'
        'not a string'    | """{"$IMAGE": 1}"""             | "entry `$IMAGE` is not a non-empty string"
        'empty value'     | """{"$IMAGE": ""}"""            | "entry `$IMAGE` is not a non-empty string"
        'empty key'       | """{"": "$DEFINITION"}"""       | 'an entry has an empty key'
    }

    /** {@code body} run with {@code env} as the whole environment Nextflow reads (SysEnv). */
    private static <T> T withEnv(Map<String, String> env, Closure<T> body) {
        SysEnv.push(env)
        try {
            return body.call()
        } finally {
            SysEnv.pop()
        }
    }

    // (5) selectable as `awsbatch`, and still nf-amazon's executor everywhere Nextflow looks

    def 'the executor takes the awsbatch name over nf-amazon\'s in either discovery order'() {
        given:
        def manager = Mock(PluginManager) { getExtensionClasses(Executor) >> order }

        expect:
        ExecutorFactory.findNameByClass(BiffixAwsBatchExecutor as Class<Executor>) == 'awsbatch'
        BiffixAwsBatchExecutor.getAnnotation(ServiceName).important()
        new ExecutorFactory(manager).getExecutorClass('awsbatch') == BiffixAwsBatchExecutor

        where:
        order << [[AwsBatchExecutor, BiffixAwsBatchExecutor], [BiffixAwsBatchExecutor, AwsBatchExecutor]]
    }

    def 'it is an AwsBatchExecutor and a TaskArrayExecutor, as Nextflow and nf-amazon check'() {
        given:
        def executor = new BiffixAwsBatchExecutor(null as String)

        expect:
        executor instanceof AwsBatchExecutor
        executor instanceof TaskArrayExecutor
        executor.isContainerNative()
        executor.containerConfigEngine() == 'docker'
        executor.getArrayIndexName() == 'AWS_BATCH_JOB_ARRAY_INDEX'
    }

    def 'a job array\'s children are biffix handlers nf-amazon can update, and the array runs on the image\'s definition'() {
        given:
        def executor = Mock(AwsBatchExecutor) {
            getArrayTaskId(_, _) >> { String id, int i -> "$id:$i".toString() }
            getAwsOptions() >> options()
        }
        def children = (0..1).collect { new BiffixAwsBatchTaskHandler(task(IMAGE), executor, MAP) }
        def array = Spy(new TaskArrayRun(name: 'ALIGN (array)', workDir: Paths.get('/work/ar/ray'), children: children, config: new TaskConfig())) {
            getContainer() >> IMAGE
        }
        def parent = Spy(BiffixAwsBatchTaskHandler, constructorArgs: [array, executor, MAP]) { fusionEnabled() >> false }

        when:
        parent.updateStatus('job-1', 'queue-a')

        then:
        children*.getJobId() == ['job-1:0', 'job-1:1']
        parent.getJobDefinition(array) == DEFINITION
    }

    def 'the plugin registers the executor as an extension'() {
        expect:
        getClass().getResource('/META-INF/extensions.idx').text.readLines()
            .contains('io.github.nh13.biffix.BiffixAwsBatchExecutor')
    }
}
