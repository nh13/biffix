package io.github.nh13.biffix

import groovy.json.JsonSlurper
import java.nio.file.Files
import java.nio.file.Path
import nextflow.Session
import spock.lang.Specification
import spock.lang.TempDir

/**
 * The durable events file: the same contract as the Snakemake logger's writer.py (one JSON line per
 * enveloped event, named by BIFFIX_EVENT_FILE relative to the launch dir, fresh per invocation), and
 * the observer writes every event there before it POSTs it, so the coordinator's upload + replay
 * backstop covers Nextflow runs.
 */
class EventWriterSpec extends Specification {

    @TempDir
    Path dir

    private static List<Map> lines(Path file) {
        return file.readLines().findAll { it }.collect { new JsonSlurper().parseText(it) as Map }
    }

    def "each event is one JSON line, and the first write replaces a stale file"() {
        given:
        final file = dir.resolve('.biffix_events.jsonl')
        file.text = '{"stale":true}\n'
        final writer = new EventWriter(file)

        when:
        writer.write([kind: 'run_started', run_id: 'r1'] as Map<String, Object>)
        writer.write([kind: 'task_created', run_id: 'r1', task_key: 'A (1)'] as Map<String, Object>)
        writer.close()

        then:
        lines(file) == [[kind: 'run_started', run_id: 'r1'], [kind: 'task_created', run_id: 'r1', task_key: 'A (1)']]
    }

    def "a write after close appends"() {
        given:
        final file = dir.resolve('events.jsonl')
        final writer = new EventWriter(file)

        when:
        writer.write([kind: 'run_failed'] as Map<String, Object>)
        writer.close()
        writer.write([kind: 'task_outputs'] as Map<String, Object>)
        writer.close()

        then:
        lines(file)*.kind == ['run_failed', 'task_outputs']
    }

    def "the file is named by BIFFIX_EVENT_FILE relative to the launch dir, as the coordinator finds it"() {
        expect:
        EventWriter.fromEnv([:], dir).path == dir.resolve('.biffix_events.jsonl')
        EventWriter.fromEnv([BIFFIX_EVENT_FILE: 'ev.jsonl'], dir).path == dir.resolve('ev.jsonl')
    }

    def "a write failure never throws and disables the file"() {
        given: 'a path that is a directory'
        final blocked = Files.createDirectory(dir.resolve('blocked'))
        final writer = new EventWriter(blocked)

        when: 'the first write fails; then the path could be written, but the file stays disabled'
        writer.write([kind: 'run_started'] as Map<String, Object>)
        Files.delete(blocked)
        writer.write([kind: 'run_failed'] as Map<String, Object>)
        writer.close()

        then:
        noExceptionThrown()
        !Files.exists(blocked)
    }

    def "the observer writes every event to the file before posting it"() {
        given:
        final file = dir.resolve('.biffix_events.jsonl')
        final List<Map<String, Object>> posted = []
        final List<Integer> linesAtPost = []
        final poster = Stub(IngestPoster)
        poster.isEnabled() >> true
        poster.post(_) >> { args ->
            linesAtPost << lines(file).size()
            posted << (args[0] as Map<String, Object>)
        }
        final observer = new BiffixObserver('run-1', poster, new EventWriter(file))
        final session = Stub(Session) {
            isSuccess() >> true
            isAborted() >> false
            isCancelled() >> false
            getDag() >> null
            getUniqueId() >> UUID.fromString('3f6c2a1e-0000-4000-8000-000000000000')
        }

        when:
        observer.onFlowCreate(session)
        observer.onFlowComplete()

        then: 'the file holds exactly the posted envelopes, each written before its POST'
        posted*.kind == ['run_started', 'run_succeeded']
        linesAtPost == [1, 2]
        lines(file) == posted.collect { new JsonSlurper().parseText(groovy.json.JsonOutput.toJson(it)) }
    }
}
