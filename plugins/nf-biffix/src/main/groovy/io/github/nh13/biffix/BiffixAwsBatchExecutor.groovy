package io.github.nh13.biffix

import groovy.transform.CompileStatic
import groovy.transform.PackageScope
import groovy.util.logging.Slf4j
import nextflow.SysEnv
import nextflow.cloud.aws.batch.AwsBatchExecutor
import nextflow.processor.TaskHandler
import nextflow.processor.TaskRun
import nextflow.util.ServiceName

/**
 * nf-amazon's AWS Batch executor, taking over the `awsbatch` name, whose tasks register every job
 * definition tagged with the run's project ({@link BiffixAwsBatchTaskHandler}). That tag is what lets
 * IAM refuse a coordinator another project's `nf-…` definition (which runs as that project's run
 * role).
 *
 * It keeps the name `awsbatch` rather than adding one, so everything Nextflow keys on the name
 * still applies: `executor.$awsbatch.*` settings, `task.executor`, the trace/Platform executor
 * field, nf-amazon's auto-start, and — above all — a pipeline that names `executor 'awsbatch'`
 * itself (a process directive or a `withName`/`withLabel` selector) still gets the tagging
 * executor rather than stock nf-amazon's. {@code important = true} makes it win the name over
 * nf-amazon's whatever order the plugins' extensions are discovered in (ExecutorFactory.init0).
 *
 * Without {@link #PROJECT_ENV} it behaves exactly as nf-amazon's (no tag, the same nf-token), so the
 * plugin changes nothing outside biffix runs.
 */
@Slf4j
@CompileStatic
@ServiceName(value = 'awsbatch', important = true)
class BiffixAwsBatchExecutor extends AwsBatchExecutor {

    static final String PROJECT_ENV = 'BIFFIX_PROJECT'

    /** The run's project (the coordinator's {@code BIFFIX_PROJECT}), or null outside biffix. */
    private final String project

    /** The instance Nextflow's ExecutorFactory creates: the project from the environment. */
    BiffixAwsBatchExecutor() {
        this(SysEnv.get(PROJECT_ENV))
    }

    /** Test seam: the project. */
    @PackageScope
    BiffixAwsBatchExecutor(String project) {
        this.project = project ?: null
    }

    String getProject() { project }

    @Override
    protected void register() {
        super.register()
        if( project )
            log.info "biffix: AWS Batch job definitions are registered tagged ${BiffixAwsBatchTaskHandler.PROJECT_TAG}=$project"
        else
            log.warn "biffix: $PROJECT_ENV is not set -- AWS Batch job definitions are registered untagged"
    }

    @Override
    TaskHandler createTaskHandler(TaskRun task) {
        assert task
        assert task.workDir
        return new BiffixAwsBatchTaskHandler(task, this, project)
    }
}
