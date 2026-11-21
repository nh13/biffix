package io.github.nh13.biffix

import groovy.transform.CompileStatic
import nextflow.executor.SupportedScriptTypes
import nextflow.executor.local.LocalExecutor
import nextflow.processor.TaskHandler
import nextflow.processor.TaskRun
import nextflow.script.ScriptType
import nextflow.util.ServiceName

/**
 * Nextflow's local executor, taking over the `local` name, whose script tasks launch without the
 * run's ingest token in their environment ({@link BiffixLocalTaskHandler}). Nextflow starts a
 * local task's `.command.run` with the whole coordinator environment, and a `beforeScript` (or the
 * task's own shell, outside any container) can print it into `.command.log`, which the coordinator
 * uploads when the task fails. The observer keeps the token: it read it when the plugin started.
 *
 * It keeps the name `local`, as {@link BiffixAwsBatchExecutor} keeps `awsbatch`, so everything
 * Nextflow keys on the name still applies: `executor.$local.*` settings (and the single-job budget
 * the observer writes there), `task.executor`, and a pipeline's own `executor 'local'`. Nextflow's
 * built-in executors yield their name to a plugin's (ExecutorFactory.init0). A native `exec:` task
 * runs in the Nextflow process itself, which has no launch environment to clean, so it keeps
 * Nextflow's handler.
 */
@CompileStatic
@ServiceName('local')
@SupportedScriptTypes([ScriptType.SCRIPTLET, ScriptType.GROOVY])
class BiffixLocalExecutor extends LocalExecutor {

    @Override
    TaskHandler createTaskHandler(TaskRun task) {
        if( task.type == ScriptType.GROOVY )
            return super.createTaskHandler(task)
        return new BiffixLocalTaskHandler(task, this)
    }
}
