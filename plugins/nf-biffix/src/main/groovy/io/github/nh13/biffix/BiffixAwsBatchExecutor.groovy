package io.github.nh13.biffix

import groovy.transform.CompileStatic
import groovy.transform.PackageScope
import groovy.util.logging.Slf4j
import nextflow.SysEnv
import nextflow.cloud.aws.batch.AwsBatchExecutor
import nextflow.exception.AbortOperationException
import nextflow.processor.TaskHandler
import nextflow.processor.TaskRun
import nextflow.util.ServiceName

/**
 * nf-amazon's AWS Batch executor, taking over the `awsbatch` name, whose tasks run on the job
 * definitions the biffix backend registered for the run ({@link BiffixAwsBatchTaskHandler}, the
 * job definition map {@link #MAP_ENV} names) rather than ones the coordinator registers. IAM lets
 * a coordinator submit only to those definitions, and not register any.
 *
 * It keeps the name `awsbatch` rather than adding one, so everything Nextflow keys on the name
 * still applies: `executor.$awsbatch.*` settings, `task.executor`, the trace/Platform executor
 * field, nf-amazon's auto-start, and — above all — a pipeline that names `executor 'awsbatch'`
 * itself (a process directive or a `withName`/`withLabel` selector) still gets this executor
 * rather than stock nf-amazon's, which would try to register definitions. {@code important = true}
 * makes it win the name over nf-amazon's whatever order the plugins' extensions are discovered in
 * (ExecutorFactory.init0).
 *
 * Without {@link #MAP_ENV} it behaves exactly as nf-amazon's (it registers its own
 * definitions), so the plugin changes nothing outside biffix runs.
 */
@Slf4j
@CompileStatic
@ServiceName(value = 'awsbatch', important = true)
class BiffixAwsBatchExecutor extends AwsBatchExecutor {

    /**
     * The path of the run's job definition map: a JSON object from each tenant image the run may
     * use (`<tenant repo>@sha256:…`, what {@link BiffixContainerResolver} maps a task's container
     * to) to the ARN, with revision, of the task job definition the backend registered for it at
     * launch. The coordinator writes it from the run's inputs. Read with {@link ImageMap}'s rules
     * (the keys are tenant images, so a container spelled differently, an implied `:latest` or an
     * upper-case registry, still finds its definition).
     */
    static final String MAP_ENV = 'BIFFIX_JOB_DEFINITION_MAP_FILE'

    /** The job definition map's path (the coordinator's {@link #MAP_ENV}), or null outside biffix. */
    private final String mapFile

    private ImageMap jobDefinitions

    /** The instance Nextflow's ExecutorFactory creates: the map named by the environment. */
    BiffixAwsBatchExecutor() {
        this(SysEnv.get(MAP_ENV))
    }

    /** Test seam: the job definition map's path. */
    @PackageScope
    BiffixAwsBatchExecutor(String mapFile) {
        this.mapFile = mapFile ?: null
    }

    String getMapFile() { mapFile }

    @Override
    protected void register() {
        super.register()
        // Read the map now, so a bad one stops the run before any task is submitted.
        final map = jobDefinitions()
        if( map != null )
            log.info "biffix: tasks run on the job definitions biffix registered for this run (${map.entries.size()} images): ${map.entries}"
        else
            log.warn "biffix: ${MAP_ENV} is not set -- AWS Batch job definitions are registered by nf-amazon"
    }

    /**
     * The run's job definitions, read once, or null without {@link #mapFile}. Throws
     * {@link AbortOperationException} (the run cannot start) on a missing or unreadable file, or
     * one that is not an object of non-empty strings.
     */
    @PackageScope
    synchronized ImageMap jobDefinitions() {
        if( mapFile && jobDefinitions == null ) {
            try {
                jobDefinitions = ImageMap.fromFile(mapFile)
            }
            catch( Exception e ) {
                throw new AbortOperationException(
                    "$MAP_ENV=$mapFile: cannot read the run's job definition map (${e.class.simpleName}: ${e.message})", e)
            }
        }
        return jobDefinitions
    }

    @Override
    TaskHandler createTaskHandler(TaskRun task) {
        assert task
        assert task.workDir
        return new BiffixAwsBatchTaskHandler(task, this, jobDefinitions())
    }
}
