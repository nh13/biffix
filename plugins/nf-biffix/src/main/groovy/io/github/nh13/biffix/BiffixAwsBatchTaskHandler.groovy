package io.github.nh13.biffix

import java.lang.reflect.Field

import groovy.transform.CompileStatic
import groovy.transform.PackageScope
import groovy.util.logging.Slf4j
import nextflow.cloud.aws.batch.AwsBatchExecutor
import nextflow.cloud.aws.batch.AwsBatchTaskHandler
import nextflow.cloud.aws.batch.model.RegisterJobDefinitionModel
import nextflow.exception.ProcessSubmitException
import nextflow.processor.TaskRun
import software.amazon.awssdk.services.batch.BatchClient
import software.amazon.awssdk.services.batch.model.BatchException
import software.amazon.awssdk.services.batch.model.DescribeJobDefinitionsRequest
import software.amazon.awssdk.services.batch.model.JobDefinition

/**
 * nf-amazon's task handler, registering each job definition tagged with the run's project:
 * <ul>
 *   <li>{@link #createJobDef} adds {@code biffix:project=<project>} next to nf-amazon's own
 *       {@code nextflow.io/*} tags, so every definition is born tagged (a tag-on-create, which IAM
 *       can require: `aws:RequestTag/biffix:project` on RegisterJobDefinition); and</li>
 *   <li>{@link #configJobDefRequest} adds the project to the hashing tokens behind the definition's
 *       {@code nf-token} parameter, which {@code findJobDef} matches to reuse a definition. So a
 *       definition is reused only by the project it is tagged with, and never one registered
 *       before this handler (untagged; its token lacks the project), which IAM would refuse; and</li>
 *   <li>{@link #findJobDef} reuses a definition only if it is also tagged with the project. The
 *       token alone is not proof: another project's coordinator could register a definition whose
 *       {@code nf-token} parameter copies ours (tagged with its own project, which is all IAM lets
 *       it register). Reusing it would not escape a role -- IAM refuses the submit -- but every
 *       task of the image would fail. Such a definition is skipped and a tagged one registered.</li>
 * </ul>
 * The project only, never the run: a definition is reused by every later run of the project, so a
 * run tag would name whichever run registered it.
 *
 * Deliberately not {@code @CompileStatic} as a class, like its superclass (whose client proxy
 * relies on dynamic dispatch); only the methods that touch the SDK directly are.
 */
@Slf4j
class BiffixAwsBatchTaskHandler extends AwsBatchTaskHandler {

    /** The Batch tag naming a resource's project (infra compute_stack.py PROJECT_TAG). */
    static final String PROJECT_TAG = 'biffix:project'

    /** The run's project, or null (then this is nf-amazon's handler, unchanged). */
    private final String project

    BiffixAwsBatchTaskHandler(TaskRun task, AwsBatchExecutor executor, String project) {
        super(task, executor)
        this.project = project ?: null
    }

    /** Test seam: nf-amazon's test-only constructor, with a project. */
    @PackageScope
    BiffixAwsBatchTaskHandler(String project) {
        super()
        this.project = project ?: null
    }

    String getProject() { project }

    @Override
    protected RegisterJobDefinitionModel configJobDefRequest(TaskRun task, List hashingTokens) {
        final result = super.configJobDefRequest(task, hashingTokens)
        if( project )
            hashingTokens.add("$PROJECT_TAG=$project".toString())
        return result
    }

    /**
     * nf-amazon's {@code findJobDef} (an ACTIVE revision of {@code name} whose {@code nf-token}
     * is {@code jobId}, as {@code name:revision}), which also requires the revision to be tagged
     * {@code biffix:project=<project>}. Without a project, nf-amazon's.
     */
    @Override
    @CompileStatic
    protected String findJobDef(String name, String jobId) {
        if( !project )
            return super.findJobDef(name, jobId)
        final matches = describeJobDefinitions(name).findAll { JobDefinition it -> it.status() == 'ACTIVE' && it.parameters()?.get('nf-token') == jobId }
        final own = matches.find { JobDefinition it -> it.tags()?.get(PROJECT_TAG) == project }
        if( !own && matches )
            log.warn "[biffix] Job definition $name has an ACTIVE revision with this task's nf-token not tagged $PROJECT_TAG=$project (revision ${matches*.revision()}) -- not reused; registering one"
        return own ? "$name:${own.revision()}" : null
    }

    /**
     * The definitions named {@code name}, as nf-amazon's {@code findJobDef} reads them: bypassing
     * the client's rate-limiting proxy (this runs inside a submit the proxy already throttles), and
     * a 5xx raised as a submit failure Nextflow retries. The first page only, like nf-amazon's.
     */
    @CompileStatic
    private List<JobDefinition> describeJobDefinitions(String name) {
        final req = DescribeJobDefinitionsRequest.builder().jobDefinitionName(name).build()
        try {
            return bypassProxy(batchClient()).describeJobDefinitions(req).jobDefinitions()
        }
        catch( BatchException e ) {
            if( e.awsErrorDetails().sdkHttpResponse().statusCode() >= 500 )
                throw new ProcessSubmitException("Failed to describe job definitions: $name - Reason: ${e.awsErrorDetails().errorCode()}", e)
            throw e
        }
    }

    /** nf-amazon keeps the handler's Batch client private (and the executor's package-scoped). */
    private static final Field CLIENT = clientField()

    private static Field clientField() {
        final f = AwsBatchTaskHandler.getDeclaredField('client')
        f.accessible = true
        return f
    }

    @CompileStatic
    private BatchClient batchClient() {
        CLIENT.get(this) as BatchClient
    }

    @Override
    protected String createJobDef(RegisterJobDefinitionModel model) {
        if( project )
            model.addTagsEntry(PROJECT_TAG, project)
        return super.createJobDef(model)
    }
}
