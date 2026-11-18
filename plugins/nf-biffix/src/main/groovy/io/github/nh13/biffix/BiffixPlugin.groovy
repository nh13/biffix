package io.github.nh13.biffix

import groovy.transform.CompileStatic
import nextflow.plugin.BasePlugin
import org.pf4j.PluginWrapper

/**
 * nf-biffix — biffix run tracking for Nextflow.
 *
 * Three extension points (build.gradle {@code extensionPoints}):
 * <ul>
 *   <li>{@link BiffixObserverFactory} registers {@link BiffixObserver}, which reports the run/task
 *       lifecycle to the biffix control-plane ingest route — the Nextflow analog of the
 *       {@code snakemake-logger-plugin-biffix} logger plugin (see it for the event mapping);</li>
 *   <li>{@link BiffixContainerResolver}, which takes over container resolution whenever
 *       {@code BIFFIX_IMAGE_MAP_FILE} is set: each task runs the tenant's copy of its image from the
 *       run's image map, and a task whose image is not in the map fails; and</li>
 *   <li>{@link BiffixAwsBatchExecutor}, nf-amazon's AWS Batch executor under the same `awsbatch`
 *       name, which runs each task on the job definition the biffix backend registered for its
 *       image, named in the run's job definition map ({@code BIFFIX_JOB_DEFINITION_MAP_FILE}),
 *       and registers none; without the map it behaves as stock nf-amazon. It needs nf-amazon
 *       (build.gradle {@code requirePlugins}).</li>
 * </ul>
 */
@CompileStatic
class BiffixPlugin extends BasePlugin {

    BiffixPlugin(PluginWrapper wrapper) {
        super(wrapper)
    }
}
