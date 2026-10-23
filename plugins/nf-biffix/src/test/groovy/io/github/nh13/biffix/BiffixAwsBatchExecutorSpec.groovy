package io.github.nh13.biffix

import java.nio.file.Paths
import java.time.Instant

import nextflow.BuildInfo
import nextflow.SysEnv
import nextflow.cloud.aws.batch.AwsBatchExecutor
import nextflow.cloud.aws.batch.AwsBatchTaskHandler
import nextflow.cloud.aws.batch.AwsOptions
import nextflow.cloud.aws.batch.model.RegisterJobDefinitionModel
import nextflow.cloud.aws.config.AwsConfig
import nextflow.exception.ProcessSubmitException
import nextflow.executor.Executor
import nextflow.executor.ExecutorFactory
import nextflow.executor.TaskArrayExecutor
import nextflow.processor.TaskArrayRun
import nextflow.processor.TaskConfig
import nextflow.processor.TaskRun
import nextflow.util.ServiceName
import org.pf4j.PluginManager
import software.amazon.awssdk.awscore.exception.AwsErrorDetails
import software.amazon.awssdk.http.SdkHttpResponse
import software.amazon.awssdk.services.batch.BatchClient
import software.amazon.awssdk.services.batch.model.BatchException
import software.amazon.awssdk.services.batch.model.DescribeJobDefinitionsRequest
import software.amazon.awssdk.services.batch.model.DescribeJobDefinitionsResponse
import software.amazon.awssdk.services.batch.model.JobDefinition
import software.amazon.awssdk.services.batch.model.RegisterJobDefinitionRequest
import software.amazon.awssdk.services.batch.model.RegisterJobDefinitionResponse
import spock.lang.Specification

/**
 * The biffix AWS Batch executor is nf-amazon's under the `awsbatch` name, except that every job
 * definition it registers is tagged with the run's project, and is reused only by that project.
 * Mirrors nf-amazon's own AwsBatchTaskHandlerTest (a Spy handler over a mocked BatchClient).
 */
class BiffixAwsBatchExecutorSpec extends Specification {

    static final String ROLE_A = 'arn:aws:iam::123456789012:role/biffix-run-a'
    static final String LOGS_A = '/biffix/project-a/tasks'

    /** The aws.batch options the coordinator writes (a job role, a log group, the host AWS CLI). */
    static AwsOptions options(String role = ROLE_A, String logs = LOGS_A) {
        new AwsOptions(awsConfig: new AwsConfig(
            region: 'us-east-1',
            batch: [jobRole: role, logsGroup: logs, cliPath: '/opt/biffix/aws-cli/bin/aws']))
    }

    /** A task running `image`. */
    TaskRun task(String image) {
        Mock(TaskRun) {
            getContainer() >> image
            getConfig() >> Mock(TaskConfig)
            getWorkDir() >> Paths.get('/work/ab/cdef')
            getName() >> 'ALIGN (1)'
        }
    }

    /** A handler (biffix's for a project, nf-amazon's for null) for `task`, over `client`. */
    AwsBatchTaskHandler handler(String project, TaskRun task, BatchClient client, AwsOptions opts = options()) {
        final executor = Mock(AwsBatchExecutor) { getAwsOptions() >> opts }
        final h = project == null
            ? Spy(AwsBatchTaskHandler, constructorArgs: [task, executor]) { fusionEnabled() >> false }
            : Spy(BiffixAwsBatchTaskHandler, constructorArgs: [task, executor, project]) { fusionEnabled() >> false }
        // nf-amazon's tests set the (private) client the same way; it is declared on AwsBatchTaskHandler.
        final field = AwsBatchTaskHandler.getDeclaredField('client')
        field.accessible = true
        field.set(h, client)
        return h
    }

    static String token(AwsBatchTaskHandler h, TaskRun task) {
        h.makeJobDefRequest(task).parameters['nf-token']
    }

    static RegisterJobDefinitionResponse registered(RegisterJobDefinitionRequest req, int revision) {
        RegisterJobDefinitionResponse.builder().jobDefinitionName(req.jobDefinitionName()).revision(revision).build()
    }

    // (1) the definition is born tagged

    def 'a registered definition carries the project tag next to nextflow.io/*'() {
        given:
        def client = Mock(BatchClient)
        def h = handler('project-a', task('ubuntu:24.04'), client)
        def model = new RegisterJobDefinitionModel().jobDefinitionName('nf-ubuntu-24-04')
        RegisterJobDefinitionRequest sent = null

        when:
        def name = h.createJobDef(model)

        then:
        1 * client.registerJobDefinition(_ as RegisterJobDefinitionRequest) >> { RegisterJobDefinitionRequest r -> sent = r; registered(r, 4) }
        name == 'nf-ubuntu-24-04:4'
        sent.tags()['biffix:project'] == 'project-a'
        sent.tags()['nextflow.io/version'] == BuildInfo.version
        Instant.parse(sent.tags()['nextflow.io/createdAt'])
        // Never the run: a definition is reused by every later run of the project.
        !sent.tags().keySet().any { it == 'biffix:run' }
    }

    def 'with no project the definition is registered as nf-amazon registers it'() {
        given:
        def client = Mock(BatchClient)
        def h = handler('', task('ubuntu:24.04'), client)
        RegisterJobDefinitionRequest sent = null

        when:
        h.createJobDef(new RegisterJobDefinitionModel().jobDefinitionName('nf-ubuntu-24-04'))

        then:
        1 * client.registerJobDefinition(_) >> { RegisterJobDefinitionRequest r -> sent = r; registered(r, 1) }
        sent.tags().keySet() == ['nextflow.io/createdAt', 'nextflow.io/version'] as Set
    }

    // (2) the nf-token is per project and stable

    def 'the nf-token differs between projects, is stable for one, and is nf-amazon\'s without a project'() {
        given:
        def t = task('quay.io/biocontainers/bwa:0.7.18')
        def client = Mock(BatchClient)

        expect:
        // Two handlers (two runs) of one project agree.
        token(handler('project-a', t, client), t) == token(handler('project-a', t, client), t)
        // Two projects never share a definition, even with the same role and log group.
        token(handler('project-a', t, client), t) != token(handler('project-b', t, client), t)
        // A project's token is not the one nf-amazon gave its definitions before (untagged ones).
        token(handler('project-a', t, client), t) != token(handler(null, t, client), t)
        // No project: exactly nf-amazon's token.
        token(handler('', t, client), t) == token(handler(null, t, client), t)
    }

    def 'the request is otherwise nf-amazon\'s: role, log group, the host CLI mount'() {
        given:
        def t = task('ubuntu:24.04')
        def ours = handler('project-a', t, Mock(BatchClient)).makeJobDefRequest(t)
        def theirs = handler(null, t, Mock(BatchClient)).makeJobDefRequest(t)

        expect:
        ours.jobDefinitionName == theirs.jobDefinitionName
        ours.jobDefinitionName.startsWith('nf-')
        ours.containerProperties.toString() == theirs.containerProperties.toString()
        ours.containerProperties.jobRoleArn == ROLE_A
        // (getLogConfiguration is @Memoized in nf-amazon: it must work on the subclass.)
        ours.containerProperties.logConfiguration.options()['awslogs-group'] == LOGS_A
        ours.containerProperties.mountPoints[0].sourceVolume() == 'aws-cli'
        // Tags are added at registration, not in the request the token is computed from.
        ours.tags == null
    }

    // (3) findJobDef reuse, end to end through nf-amazon's resolveJobDefinition

    def 'a project reuses its own tagged definition and registers none'() {
        given:
        def image = 'quay.io/biocontainers/samtools:1.20'
        def t = task(image)
        def client = Mock(BatchClient)
        def h = handler('project-a', t, client)
        def req = h.makeJobDefRequest(t)
        def own = JobDefinition.builder()
            .jobDefinitionName(req.jobDefinitionName).status('ACTIVE').revision(7)
            .parameters(['nf-token': req.parameters['nf-token']])
            .tags(['biffix:project': 'project-a']).build()

        when:
        def name = h.resolveJobDefinition(t)

        then:
        1 * client.describeJobDefinitions(_ as DescribeJobDefinitionsRequest) >> DescribeJobDefinitionsResponse.builder().jobDefinitions(own).build()
        0 * client.registerJobDefinition(_)
        name == "${req.jobDefinitionName}:7"
    }

    def 'an untagged definition from before, or another project\'s, is not reused: a tagged one is registered'() {
        given:
        def image = 'quay.io/biocontainers/fastp:0.23.4'
        def t = task(image)
        def client = Mock(BatchClient)
        def h = handler('project-a', t, client)
        def name = h.makeJobDefRequest(t).jobDefinitionName
        def before = JobDefinition.builder().jobDefinitionName(name).status('ACTIVE').revision(1)
            .parameters(['nf-token': token(handler(null, t, client), t)]).build()
        def other = JobDefinition.builder().jobDefinitionName(name).status('ACTIVE').revision(2)
            .parameters(['nf-token': token(handler('project-b', t, client), t)])
            .tags(['biffix:project': 'project-b']).build()
        RegisterJobDefinitionRequest sent = null

        when:
        def resolved = h.resolveJobDefinition(t)

        then:
        1 * client.describeJobDefinitions(_) >> DescribeJobDefinitionsResponse.builder().jobDefinitions(before, other).build()
        1 * client.registerJobDefinition(_) >> { RegisterJobDefinitionRequest r -> sent = r; registered(r, 3) }
        resolved == "$name:3"
        sent.tags()['biffix:project'] == 'project-a'
        sent.parameters()['nf-token'] == token(h, t)

        when: 'a later task of the project, in the same run'
        def again = handler('project-a', t, client).resolveJobDefinition(t)

        then: 'nf-amazon\'s per-JVM cache answers'
        0 * client.describeJobDefinitions(_)
        0 * client.registerJobDefinition(_)
        again == "$name:3"
    }

    def 'a definition copying our nf-token is reused only if tagged with our project'() {
        given:
        def t = task('quay.io/biocontainers/multiqc:1.25')
        def client = Mock(BatchClient)
        def h = handler('project-a', t, client)
        def req = h.makeJobDefRequest(t)
        def ours = req.parameters['nf-token']
        def poisoned = JobDefinition.builder().jobDefinitionName(req.jobDefinitionName).status('ACTIVE').revision(5)
            .parameters(['nf-token': ours]).tags(['biffix:project': 'project-b']).build()
        def untagged = JobDefinition.builder().jobDefinitionName(req.jobDefinitionName).status('ACTIVE').revision(6)
            .parameters(['nf-token': ours]).build()
        def inactive = JobDefinition.builder().jobDefinitionName(req.jobDefinitionName).status('INACTIVE').revision(7)
            .parameters(['nf-token': ours]).tags(['biffix:project': 'project-a']).build()
        def own = JobDefinition.builder().jobDefinitionName(req.jobDefinitionName).status('ACTIVE').revision(8)
            .parameters(['nf-token': ours]).tags(['biffix:project': 'project-a']).build()

        when: 'only revisions another project (or no one) tagged carry our token'
        def found = h.findJobDef(req.jobDefinitionName, ours)

        then: 'none is reused, so resolveJobDefinition registers a tagged one'
        1 * client.describeJobDefinitions(_) >> DescribeJobDefinitionsResponse.builder().jobDefinitions(poisoned, untagged, inactive).build()
        found == null

        when: 'our own tagged revision is among them'
        found = h.findJobDef(req.jobDefinitionName, ours)

        then: 'it is the one reused'
        1 * client.describeJobDefinitions(_) >> DescribeJobDefinitionsResponse.builder().jobDefinitions(poisoned, untagged, own).build()
        found == "${req.jobDefinitionName}:8"
    }

    def 'with no project a definition is found as nf-amazon finds it, tags or not'() {
        given:
        def t = task('ubuntu:24.04')
        def client = Mock(BatchClient)
        def h = handler('', t, client)
        def req = h.makeJobDefRequest(t)
        def untagged = JobDefinition.builder().jobDefinitionName(req.jobDefinitionName).status('ACTIVE').revision(2)
            .parameters(['nf-token': req.parameters['nf-token']]).build()

        when:
        def found = h.findJobDef(req.jobDefinitionName, req.parameters['nf-token'])

        then:
        1 * client.describeJobDefinitions(_) >> DescribeJobDefinitionsResponse.builder().jobDefinitions(untagged).build()
        found == "${req.jobDefinitionName}:2"
    }

    def 'a Batch 5xx while finding a definition is a submit failure Nextflow retries'() {
        given:
        def client = Mock(BatchClient)
        def h = handler('project-a', task('ubuntu:24.04'), client)
        def error = BatchException.builder().statusCode(503)
            .awsErrorDetails(AwsErrorDetails.builder().errorCode('ServiceUnavailable')
                .sdkHttpResponse(SdkHttpResponse.builder().statusCode(503).build()).build())
            .build()

        when:
        h.findJobDef('nf-ubuntu-24-04', 'tok')

        then:
        1 * client.describeJobDefinitions(_) >> { throw error }
        thrown(ProcessSubmitException)
    }

    // (4) selectable as `awsbatch`, and still nf-amazon's executor everywhere Nextflow looks

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
        def executor = new BiffixAwsBatchExecutor('project-a')

        expect:
        executor instanceof AwsBatchExecutor
        executor instanceof TaskArrayExecutor
        executor.isContainerNative()
        executor.containerConfigEngine() == 'docker'
        executor.getArrayIndexName() == 'AWS_BATCH_JOB_ARRAY_INDEX'
    }

    def 'its handlers are biffix\'s, with the project from BIFFIX_PROJECT'() {
        given:
        SysEnv.push([BIFFIX_PROJECT: 'project-a'])
        def executor = new BiffixAwsBatchExecutor()
        SysEnv.pop()
        def t = new TaskRun(name: 'ALIGN (1)', workDir: Paths.get('/work/ab/cdef'))

        when:
        def h = executor.createTaskHandler(t)

        then:
        h instanceof BiffixAwsBatchTaskHandler
        (h as BiffixAwsBatchTaskHandler).project == 'project-a'
        executor.project == 'project-a'
        withEnv([:]) { new BiffixAwsBatchExecutor().project } == null
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

    def 'a job array\'s children are biffix handlers nf-amazon can update'() {
        given:
        def executor = Mock(AwsBatchExecutor) { getArrayTaskId(_, _) >> { String id, int i -> "$id:$i".toString() } }
        def children = (0..1).collect { new BiffixAwsBatchTaskHandler(task('ubuntu:24.04'), executor, 'project-a') }
        def array = new TaskArrayRun(name: 'ALIGN (array)', workDir: Paths.get('/work/ar/ray'), children: children)
        def parent = new BiffixAwsBatchTaskHandler(array, executor, 'project-a')

        when:
        parent.updateStatus('job-1', 'queue-a')

        then:
        children*.getJobId() == ['job-1:0', 'job-1:1']
    }

    def 'the plugin registers the executor as an extension'() {
        expect:
        getClass().getResource('/META-INF/extensions.idx').text.readLines()
            .contains('io.github.nh13.biffix.BiffixAwsBatchExecutor')
    }
}
