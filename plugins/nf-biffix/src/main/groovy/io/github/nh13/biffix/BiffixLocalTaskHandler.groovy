package io.github.nh13.biffix

import groovy.transform.CompileStatic
import nextflow.executor.local.LocalExecutor
import nextflow.executor.local.LocalTaskHandler
import nextflow.processor.TaskRun

/**
 * Nextflow's local task handler, launching each task (directly or through Fusion) without the
 * coordinator secrets in {@link #SECRET_ENV} in its environment; everything else it inherits as
 * before. See {@link BiffixLocalExecutor}.
 */
@CompileStatic
class BiffixLocalTaskHandler extends LocalTaskHandler {

    /** What a task's launch process must not inherit: the run's ingest capability token. */
    static final List<String> SECRET_ENV = ['BIFFIX_INGEST_TOKEN']

    BiffixLocalTaskHandler(TaskRun task, LocalExecutor executor) {
        super(task, executor)
    }

    @Override
    protected ProcessBuilder createLaunchProcessBuilder() {
        final builder = super.createLaunchProcessBuilder()
        final env = builder.environment()
        for( String name : SECRET_ENV )
            env.remove(name)
        return builder
    }
}
