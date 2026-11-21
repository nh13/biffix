package io.github.nh13.biffix

import groovy.transform.CompileStatic
import groovy.transform.PackageScope
import groovy.util.logging.Slf4j
import nextflow.container.ContainerConfig
import nextflow.container.ContainerHandler
import nextflow.container.resolver.ContainerInfo
import nextflow.container.resolver.ContainerMeta
import nextflow.container.resolver.ContainerResolver
import nextflow.container.resolver.DefaultContainerResolver
import nextflow.exception.AbortOperationException
import nextflow.plugin.Priority
import nextflow.processor.TaskRun

/**
 * Runs every task on the tenant's copy of its container: the process's {@code container} (after
 * Nextflow's own resolution, e.g. `docker.registry`) is looked up in the run's image map
 * ({@code BIFFIX_IMAGE_MAP_FILE}, see {@link ImageMap}), and a task whose image biffix did not
 * import fails instead of pulling it from a public registry. That includes a
 * {@code job-definition://} container: an existing Batch job definition names its own image and
 * job role, so it runs only if the map names it verbatim (biffix's maps never do). Enabled only when
 * the map is set, so the plugin changes nothing outside biffix runs.
 *
 * What the task's container engine runs for the tenant copy: the reference itself on AWS Batch or
 * Docker, and for Apptainer or Singularity the local image file Nextflow resolves it to (for a
 * single-job run, the one the coordinator converted into {@code apptainer.libraryDir} before the
 * run); see {@link #engineTarget}. The tenant reference stays the container's
 * identity, its {@code hashKey}, so a task's hash and the image it reports do not depend on the
 * engine.
 *
 * Priority -20 puts it ahead of Nextflow's default resolver (and Wave's, at -10).
 */
@Slf4j
@CompileStatic
@Priority(-20)
class BiffixContainerResolver implements ContainerResolver {

    static final String MAP_ENV = 'BIFFIX_IMAGE_MAP_FILE'
    static final String JOB_DEFINITION = 'job-definition://'

    /** Where the map is. */
    private final String mapFile

    /** Nextflow's own resolution, applied before the lookup (and for a task with no container). */
    private final ContainerResolver fallback

    private ImageMap map

    /** The extension point Nextflow instantiates: the map named by {@link #MAP_ENV}. */
    BiffixContainerResolver() {
        this(System.getenv(MAP_ENV), new DefaultContainerResolver())
    }

    /** Test seam: the map file and the fallback resolver. */
    @PackageScope
    BiffixContainerResolver(String mapFile, ContainerResolver fallback) {
        this.mapFile = mapFile
        this.fallback = fallback
    }

    @Override
    boolean enabled() {
        return mapFile as boolean
    }

    @Override
    String defaultContainerPlatform() {
        return fallback.defaultContainerPlatform()
    }

    @Override
    ContainerInfo resolveImage(TaskRun task, String image) {
        // No container is Nextflow's to handle.
        if( !image )
            return fallback.resolveImage(task, image)
        // A job definition is looked up as written only: normalizing it, or resolving it through
        // Nextflow, would make no reference biffix imported.
        final map = imageMap()
        final jobDefinition = image.startsWith(JOB_DEFINITION)
        final mapped = jobDefinition
            ? map.resolveExact(image)
            : map.resolve(image) ?: map.resolve(lookupName(task, image))
        if( !mapped ) {
            final process = task?.processor?.name ?: task?.name ?: 'a process'
            throw new AbortOperationException(
                "Process `$process` uses container '$image', which biffix did not import for this "
                    + "workflow version: declare it under `images:` in biffix.yaml and re-register")
        }
        final target = jobDefinition ? mapped : engineTarget(task, mapped)
        log.debug "biffix: container '$image' -> '$mapped' (runs '$target')"
        return new ContainerInfo(image, target, mapped)
    }

    /**
     * What the task's engine runs for the tenant copy {@code mapped}: the copy itself on AWS Batch
     * and Docker, and its local image file for Apptainer and Singularity. Never Nextflow's registry
     * normalization of it: with {@code docker.registry} and {@code registryOverride}, that rewrites
     * even an absolute reference's host, so the tenant's ECR copy would become a reference to
     * another registry (on AWS Batch, one the run's job definition map does not name). For an image
     * file engine the copy goes through Nextflow's resolution as {@code docker://<copy>}: a reference
     * with a scheme skips the registry, and the image file's name drops the scheme, so it is the one
     * the coordinator converted into {@code apptainer.libraryDir}.
     */
    private String engineTarget(TaskRun task, String mapped) {
        final config = containerConfig(task)
        if( config == null || !(config.engine in IMAGE_FILE_ENGINES) )
            return mapped
        final ref = ContainerHandler.IMAGE_URL_PREFIX.matcher(mapped).find() ? mapped : 'docker://' + mapped
        return fallback.resolveImage(task, ref)?.target ?: mapped
    }

    /** The container engines that run a local image file, pulling one Nextflow lacks. */
    static final Set<String> IMAGE_FILE_ENGINES = ['apptainer', 'singularity'] as Set<String>

    /**
     * The spelling of {@code image} looked up when the map lacks it as written: the reference
     * Nextflow would pull, with the container config's {@code registry} applied. For an engine
     * that runs a local image file, Nextflow's resolution pulls the image to give that file's path
     * (here, an image biffix did not import, from a public registry), so the reference is worked
     * out as Nextflow does before it pulls; for any other engine, Nextflow's resolution is that
     * reference.
     */
    @PackageScope
    String lookupName(TaskRun task, String image) {
        final config = containerConfig(task)
        if( config != null && config.engine in IMAGE_FILE_ENGINES )
            return registryName(config, image)
        return fallback.resolveImage(task, image)?.target
    }

    /** The task's container config, or null when there is none to read (no session). */
    private static ContainerConfig containerConfig(TaskRun task) {
        try {
            return task?.getContainerConfig()
        }
        catch( Exception e ) {
            log.trace "biffix: no container config for the task: ${e.message}"
            return null
        }
    }

    /**
     * {@code image} with {@code config}'s registry applied, as Nextflow's Docker normalization
     * does (and its Apptainer and Singularity ones, before adding {@code docker://}); null for a
     * reference with a scheme or an image file path, which have no other spelling.
     */
    @PackageScope
    static String registryName(ContainerConfig config, String image) {
        if( image.startsWith('/') || image.startsWith('file://') || ContainerHandler.IMAGE_URL_PREFIX.matcher(image).find() )
            return null
        final registry = config.getRegistry()
        if( !registry )
            return image
        if( ContainerHandler.isAbsoluteDockerName(image) )
            return config.getRegistryOverride() ? ContainerHandler.overrideRegistryName(image, registry) : image
        return registry.endsWith('/') ? registry + image : registry + '/' + image
    }

    @Override
    boolean isContainerReady(String key) {
        return fallback.isContainerReady(key)
    }

    @Override
    ContainerMeta getContainerMeta(String key) {
        return fallback.getContainerMeta(key)
    }

    private synchronized ImageMap imageMap() {
        if( map == null ) {
            try {
                map = ImageMap.fromFile(mapFile)
            }
            catch( Exception e ) {
                throw new AbortOperationException(
                    "$MAP_ENV=$mapFile: cannot read the run's image map (${e.class.simpleName}: ${e.message})", e)
            }
            log.info "biffix: containers resolve through $mapFile (${map.size()} entries)"
        }
        return map
    }
}
