package io.github.nh13.biffix

import groovy.transform.CompileStatic
import groovy.util.logging.Slf4j
import nextflow.cloud.aws.batch.AwsBatchExecutor
import nextflow.cloud.aws.batch.AwsBatchTaskHandler
import nextflow.cloud.aws.batch.AwsContainerOptionsMapper
import nextflow.exception.ProcessUnrecoverableException
import nextflow.processor.TaskRun
import nextflow.util.CmdLineOptionMap
import software.amazon.awssdk.services.batch.model.ContainerProperties

/**
 * nf-amazon's task handler, submitting each task to the job definition the biffix backend
 * registered for its image (the run's job definition map,
 * {@link BiffixAwsBatchExecutor#MAP_ENV}) instead of registering one itself.
 *
 * A coordinator runs workflow code, so it is not trusted to register job definitions: a definition
 * can ask for a privileged container or mount host paths, on compute environments every project
 * shares. The backend registers one task definition per (project, tenant image) at launch, with
 * only what a task needs (the image, the project's run role and task log group, the read-only host
 * AWS CLI), and IAM lets the coordinator submit to those alone. So {@link #getJobDefinition} never
 * reaches nf-amazon's {@code resolveJobDefinition}, which would describe and register definitions.
 *
 * nf-amazon builds a definition from the task too: its {@code containerOptions}, {@code aws.batch}
 * volumes, Fusion (a privileged container), Fargate and an execution role. A pre-registered
 * definition cannot honour those, so a task that asks for any of them fails, naming the setting,
 * rather than running without it.
 *
 * Without a map (outside biffix) this is nf-amazon's handler, unchanged.
 *
 * Deliberately not {@code @CompileStatic} as a class, like its superclass (whose client proxy
 * relies on dynamic dispatch).
 */
@Slf4j
class BiffixAwsBatchTaskHandler extends AwsBatchTaskHandler {

    static final String JOB_DEFINITION = 'job-definition://'

    /**
     * The {@code docker run} options nf-amazon's {@link AwsContainerOptionsMapper} turns into job
     * definition settings, as they are spelled in a {@code containerOptions} directive: what the
     * error message names. Any other option the mapper ignores, as nf-amazon does.
     */
    static final List<String> DEFINITION_OPTIONS = [
        'privileged', 'env', 'e', 'user', 'u', 'read-only', 'ulimit',
        'shm-size', 'tmpfs', 'memory-swap', 'init', 'memory-swappiness',
    ].asImmutable()

    /** The run's job definitions, or null (then this is nf-amazon's handler, unchanged). */
    private final ImageMap jobDefinitions

    BiffixAwsBatchTaskHandler(TaskRun task, AwsBatchExecutor executor, ImageMap jobDefinitions) {
        super(task, executor)
        this.jobDefinitions = jobDefinitions
    }

    ImageMap getJobDefinitions() { jobDefinitions }

    @Override
    protected String getJobDefinition(TaskRun task) {
        if( jobDefinitions == null )
            return super.getJobDefinition(task)
        final container = task.getContainer()
        final process = task.lazyName()
        if( !container || container.startsWith(JOB_DEFINITION) )
            throw new ProcessUnrecoverableException(
                "Process `$process` has container '${container ?: ''}': biffix runs a task only on the job definition it registered for the task's image")
        checkDefinitionSettings(task, process)
        final definition = jobDefinitions.resolve(container)
        if( !definition )
            throw new ProcessUnrecoverableException(
                "Process `$process` runs image '$container', which has no biffix task job definition for this run "
                    + "-- the backend registers one for each image of the workflow version at launch; re-register the workflow version and resubmit")
        log.debug "[biffix] Process `$process`: image '$container' -> job definition $definition"
        return definition
    }

    /**
     * Fail a task that asks for a setting nf-amazon would have put in the job definition it
     * registered, since the pre-registered one cannot carry it.
     */
    protected void checkDefinitionSettings(TaskRun task, String process) {
        if( fusionEnabled() )
            throw unsupported(process, 'Fusion (`fusion.enabled`), which needs a privileged container')
        final opts = getAwsOptions()
        if( opts.getVolumes() )
            throw unsupported(process, "`aws.batch.volumes` (${opts.getVolumes().join(', ')})")
        if( opts.isFargateMode() )
            throw unsupported(process, 'Fargate (`aws.batch.platformType`)')
        if( opts.getExecutionRole() )
            throw unsupported(process, '`aws.batch.executionRole`')
        final options = task.config.getContainerOptionsMap()
        if( definitionProperties(options) != ContainerProperties.builder().build() )
            throw unsupported(process, "the `containerOptions` ${definitionOptions(options)}")
        // The consumable-resources hint (nf-amazon 3.9+) is also a definition setting; read by
        // name, since the hints directive is newer than the Nextflow this compiles against.
        final hints = task.config.get('hints')
        if( hints instanceof Map && ((Map) hints).keySet().any { it in ['consumableResources', 'awsbatch/consumableResources'] } )
            throw unsupported(process, 'the `awsbatch/consumableResources` hint')
    }

    /** What nf-amazon would set in a job definition for {@code options}. */
    @CompileStatic
    private static ContainerProperties definitionProperties(CmdLineOptionMap options) {
        return AwsContainerOptionsMapper.createContainerProperties(options).toBatchContainerProperties()
    }

    /** The definition options among {@code options}, as written (`--privileged`, `-e`). */
    @CompileStatic
    private static String definitionOptions(CmdLineOptionMap options) {
        final names = DEFINITION_OPTIONS.findAll { options.exists(it) }.collect { it.size() == 1 ? "-$it" : "--$it" }
        return names ? names.join(', ') : options.toString()
    }

    private static ProcessUnrecoverableException unsupported(String process, String setting) {
        new ProcessUnrecoverableException(
            "Process `$process` uses $setting, which biffix does not support: its tasks run on a job "
                + "definition biffix registered, which cannot carry job definition settings")
    }
}
