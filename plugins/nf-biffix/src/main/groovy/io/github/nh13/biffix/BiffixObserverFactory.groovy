package io.github.nh13.biffix

import groovy.transform.CompileStatic
import nextflow.Session
import nextflow.trace.TraceObserverV2
import nextflow.trace.TraceObserverFactoryV2

/**
 * Registers {@link BiffixObserver} with Nextflow's trace-observer SPI (the V2 API used by
 * current Nextflow). Declared as the plugin's extension point (build.gradle
 * {@code extensionPoints}), so enabling the plugin in a run whose environment carries the biffix
 * config is all it takes to start reporting to biffix. The coordinator enables it in a
 * {@code plugins {}} block of its {@code -c} config, or (for a runtime layer with the workflow's
 * own plugins) as one of {@code NXF_PLUGINS_DEFAULT}, never with {@code -plugins}, which would
 * replace the pipeline's own list (docker/coordinator/lib/nextflow.sh). (The plugin's other
 * extension point, {@link BiffixContainerResolver}, is independent of this one.)
 */
@CompileStatic
class BiffixObserverFactory implements TraceObserverFactoryV2 {

    @Override
    Collection<TraceObserverV2> create(Session session) {
        return [new BiffixObserver()] as Collection<TraceObserverV2>
    }
}
