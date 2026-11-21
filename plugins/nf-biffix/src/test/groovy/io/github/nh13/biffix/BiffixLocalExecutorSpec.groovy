package io.github.nh13.biffix

import java.nio.file.Path

import nextflow.Session
import nextflow.executor.Executor
import nextflow.executor.ExecutorFactory
import nextflow.executor.local.LocalExecutor
import nextflow.executor.local.NativeTaskHandler
import nextflow.processor.TaskConfig
import nextflow.processor.TaskRun
import nextflow.script.ScriptType
import org.pf4j.PluginManager
import spock.lang.Specification
import spock.lang.TempDir

/**
 * The biffix local executor is Nextflow's own under the `local` name, except that a task's launch
 * process does not inherit the run's ingest token: a `beforeScript` or the task's own shell runs
 * outside any container and can print its environment into `.command.log`, which the coordinator
 * uploads when the task fails.
 */
class BiffixLocalExecutorSpec extends Specification {

    @TempDir
    Path workDir

    def 'the executor takes the local name over Nextflow\'s own'() {
        given:
        def manager = Mock(PluginManager) { getExtensionClasses(Executor) >> [BiffixLocalExecutor] }

        expect:
        ExecutorFactory.findNameByClass(BiffixLocalExecutor as Class<Executor>) == 'local'
        new ExecutorFactory(manager).getExecutorClass('local') == BiffixLocalExecutor
        // Still Nextflow's local executor everywhere it checks the class.
        LocalExecutor.isAssignableFrom(BiffixLocalExecutor)
    }

    def 'a script task gets the token-free handler, a native exec task Nextflow\'s own'() {
        given:
        final executor = new BiffixLocalExecutor(session: Stub(Session))

        expect:
        executor.createTaskHandler(task(ScriptType.SCRIPTLET)) instanceof BiffixLocalTaskHandler
        executor.createTaskHandler(task(ScriptType.GROOVY)) instanceof NativeTaskHandler
    }

    def 'a task launch inherits no ingest token, and keeps the rest of the environment'() {
        given: 'the launch process builder Nextflow makes, which copies the coordinator environment'
        final executor = new BiffixLocalExecutor(session: Stub(Session))
        final handler = new BiffixLocalTaskHandler(task(ScriptType.SCRIPTLET), executor) {
            @Override
            protected ProcessBuilder localProcessBuilder() {
                final pb = new ProcessBuilder('true')
                pb.environment().put('BIFFIX_INGEST_TOKEN', 'sekrit')
                pb.environment().put('AWS_REGION', 'us-east-1')
                return pb
            }

            @Override
            boolean fusionEnabled() { false }
        }

        when:
        final env = handler.createLaunchProcessBuilder().environment()

        then:
        !env.containsKey('BIFFIX_INGEST_TOKEN')
        env.get('AWS_REGION') == 'us-east-1'
    }

    private TaskRun task(ScriptType type) {
        return new TaskRun(name: 'T (1)', workDir: workDir, type: type, config: new TaskConfig())
    }
}
