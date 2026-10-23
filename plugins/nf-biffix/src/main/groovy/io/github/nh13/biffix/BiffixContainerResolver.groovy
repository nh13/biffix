package io.github.nh13.biffix

import groovy.transform.CompileStatic
import groovy.transform.PackageScope
import groovy.util.logging.Slf4j
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
        final mapped = image.startsWith(JOB_DEFINITION)
            ? map.resolveExact(image)
            : map.resolve(image) ?: map.resolve(fallback.resolveImage(task, image)?.target)
        if( !mapped ) {
            final process = task?.processor?.name ?: task?.name ?: 'a process'
            throw new AbortOperationException(
                "Process `$process` uses container '$image', which biffix did not import for this "
                    + "workflow version: declare it under `images:` in biffix.yaml and re-register")
        }
        log.debug "biffix: container '$image' -> '$mapped'"
        return new ContainerInfo(image, mapped, mapped)
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
