package io.github.nh13.biffix

import com.fasterxml.jackson.databind.JsonNode
import com.fasterxml.jackson.databind.ObjectMapper
import com.networknt.schema.JsonSchema
import com.networknt.schema.JsonSchemaFactory
import com.networknt.schema.SpecVersion
import groovy.json.JsonOutput
import spock.lang.Shared
import spock.lang.Specification

/**
 * The nf-biffix observer's emitted events must conform to the canonical engine wire contract
 * ({@code schemas/wire.engine.schema.json}) — the same schema the Rust reducer folds. This validates
 * every kind the observer emits, wrapped in the shared envelope, against that schema, and rejects
 * any property its kind does not declare (the schema tolerates unknown fields, so validation alone
 * would miss a misspelt or renamed optional field). It also enforces EXHAUSTIVENESS: every
 * {@code kind} the schema defines must have a builder sample here or be listed in
 * {@link #NOT_EMITTED} with the reason this plugin never sends it (the structural analog of the
 * Snakemake plugin's {@code test_schema_conformance.py}), so a kind added to the schema fails the
 * build until someone classifies it.
 */
class BiffixEventsSpec extends Specification {

    /** Schema kinds this plugin never emits, and why. */
    static final Map<String, String> NOT_EMITTED = [
        run_progress: 'TraceObserverV2 exposes no authoritative task total; the projection derives progress from task states',
    ]

    @Shared
    JsonSchema schema

    @Shared
    JsonNode schemaTree

    @Shared
    ObjectMapper mapper = new ObjectMapper()

    def setupSpec() {
        // Read the canonical schema (not a copy), relative to the gradle test working dir
        // (plugins/nf-biffix); fall back to the repo-root spelling for other invocations.
        final candidates = ['../../schemas/wire.engine.schema.json', 'schemas/wire.engine.schema.json']
        final file = candidates.collect { new File(it) }.find { it.exists() }
        assert file != null: "wire.engine.schema.json not found from ${System.getProperty('user.dir')}"
        final text = file.text
        schema = JsonSchemaFactory.getInstance(SpecVersion.VersionFlag.V202012).getSchema(text)
        schemaTree = mapper.readTree(text)
    }

    private Set validate(Map<String, Object> event) {
        return schema.validate(mapper.readTree(JsonOutput.toJson(event)))
    }

    private static Map<String, Object> env(Map<String, Object> body) {
        return BiffixEvents.envelope(1, 'run-1', 1710000000.5d, body)
    }

    /**
     * One emitted sample per wire kind (enveloped), with every optional field set, so each field
     * name is checked. The exhaustiveness test ties this (with {@link #NOT_EMITTED}) to the schema.
     */
    private static Map<String, Map<String, Object>> samples() {
        return [
            run_started   : env(BiffixEvents.runStarted('3f6c2a1e-0000-4000-8000-000000000000')),
            run_succeeded : env(BiffixEvents.runSucceeded()),
            run_failed    : env(BiffixEvents.runFailed()),
            run_dag       : env(BiffixEvents.runDag(['align', 'collect'], [['align', 'collect']])),
            task_created  : env(BiffixEvents.taskCreated('merge', 'merge', ['align (s2)', 'align (s1)'])),
            task_submitted: env(BiffixEvents.taskSubmitted('align (s1)', 1, 'batch-job-abc', '123456789012.dkr.ecr.us-east-1.amazonaws.com/wf/align@sha256:' + 'a' * 64)),
            task_succeeded: env(BiffixEvents.taskSucceeded('collect', 2, '12')),
            task_failed   : env(BiffixEvents.taskFailed('collect', 1, '11')),
            task_cached   : env(BiffixEvents.taskCached('align (s1)', 1, '3')),
            engine_task_verdict: env(BiffixEvents.engineTaskVerdict('align (s1) #1', 1, 'missing_output', 'Missing output file(s) `o.bam` expected by process `align (s1)`')),
            task_retrying : env(BiffixEvents.taskRetrying('align (s1)', 1, 2)),
            task_outputs  : env(BiffixEvents.taskOutputs('align (s1)', 1, 'intermediate', true,
                [[s3_uri: 's3://b/work/ab/cd/o.bam', path: 'o.bam', etag: 'e1', size: 42L]])),
        ]
    }

    /**
     * Each kind's schema variant (the object whose {@code properties.kind.const}, or
     * {@code properties.kind.enum}, names it).
     */
    private static Map<String, JsonNode> variants(JsonNode node) {
        final Map<String, JsonNode> found = [:]
        if (node.isObject()) {
            final props = node.get('properties')
            if (props != null && props.isObject()) {
                final kindProp = props.get('kind')
                if (kindProp?.get('const')?.isTextual()) {
                    found[kindProp.get('const').asText()] = node
                }
                else if (kindProp != null && kindProp.get('enum')?.isArray()) {
                    kindProp.get('enum').each { found[it.asText()] = node }
                }
            }
            node.fields().each { found.putAll(variants(it.value)) }
        }
        else if (node.isArray()) {
            node.each { found.putAll(variants(it)) }
        }
        return found
    }

    /** The property names the schema declares for {@code kind}: the envelope's plus its variant's. */
    private Set<String> declared(String kind) {
        final names = [] as Set<String>
        schemaTree.get('properties')?.fieldNames()?.each { names << it }
        variants(schemaTree)[kind]?.get('properties')?.fieldNames()?.each { names << it }
        return names
    }

    def "every schema kind is sampled or classified as not emitted, and no extra"() {
        given:
        final defined = variants(schemaTree).keySet()
        final sampled = samples().keySet()
        final classified = sampled + NOT_EMITTED.keySet()

        expect: 'the schema defines at least the kinds we know about'
        !defined.isEmpty()

        and: 'every schema kind is classified (a new kind must add a builder or a NOT_EMITTED reason)'
        (defined - classified).isEmpty()

        and: 'nothing classified is a kind the schema does not define'
        (classified - defined).isEmpty()

        and: 'no kind is both sampled and not emitted'
        sampled.intersect(NOT_EMITTED.keySet()).isEmpty()
    }

    def "every emitted event kind conforms to the wire schema and uses only declared fields"() {
        expect:
        validate(event).isEmpty()
        event.kind == kind
        (event.keySet() - declared(kind)).isEmpty()

        where:
        [kind, event] << samples().collect { k, v -> [k, v] }
    }

    def "the builders put each field where the wire contract reads it"() {
        expect: 'run_dag: nodes by id, edges from → to'
        BiffixEvents.runDag(['a', 'b'], [['a', 'b']]).dag == [nodes: [[id: 'a'], [id: 'b']], edges: [[from: 'a', to: 'b']]]

        and: 'task_outputs keeps each ref’s path, etag and size'
        BiffixEvents.taskOutputs('t', 1, 'intermediate', false, [[s3_uri: 's3://b/o', path: 'o', etag: 'e', size: 7L]]).outputs ==
            [[s3_uri: 's3://b/o', path: 'o', etag: 'e', size: 7L]]

        and: 'a local terminal carries its attempt and execution'
        BiffixEvents.taskFailed('t', 2, '9') == [kind: 'task_failed', task_key: 't', attempt: 2, execution_id: '9']
        BiffixEvents.taskSucceeded('t', 3, '10') == [kind: 'task_succeeded', task_key: 't', attempt: 3, execution_id: '10']
        BiffixEvents.taskCached('t', 1, '4') == [kind: 'task_cached', task_key: 't', attempt: 1, execution_id: '4']

        and: 'task_submitted carries the image the job runs'
        BiffixEvents.taskSubmitted('t', 1, 'job-1', 'r/img@sha256:ab') ==
            [kind: 'task_submitted', task_key: 't', attempt: 1, batch_job_id: 'job-1', image: 'r/img@sha256:ab']

        and: 'a verdict names the attempt, the category and the reason'
        BiffixEvents.engineTaskVerdict('t', 2, 'app_error', 'exit 3') ==
            [kind: 'engine_task_verdict', task_key: 't', verdict: 'failed', attempt: 2, category: 'app_error', reason: 'exit 3']
    }

    def "a verdict's reason is cut to REASON_MAX characters"() {
        when:
        final reason = BiffixEvents.engineTaskVerdict('t', 1, null, 'x' * (BiffixEvents.REASON_MAX + 50)).reason as String

        then:
        reason.length() == BiffixEvents.REASON_MAX
        reason.endsWith('…')
        BiffixEvents.engineTaskVerdict('t', 1, null, 'x' * BiffixEvents.REASON_MAX).reason == 'x' * BiffixEvents.REASON_MAX
    }

    def "a verdict's reason is cut on a code point, never inside a surrogate pair"() {
        given: 'an astral character (two UTF-16 units) where a cut by units would fall'
        final face = new String(Character.toChars(0x1F600))
        final head = 'x' * (BiffixEvents.REASON_MAX - 2)

        when:
        final reason = BiffixEvents.engineTaskVerdict('t', 1, null, head + face * 5).reason as String

        then: 'REASON_MAX code points, as the Snakemake plugin counts them, the pair kept whole'
        reason == head + face + '…'
        reason.codePointCount(0, reason.length()) == BiffixEvents.REASON_MAX
        !reason.codePoints().anyMatch { Character.getType(it) == Character.SURROGATE }

        and: 'under the limit in code points, though over it in UTF-16 units, it is kept whole'
        BiffixEvents.engineTaskVerdict('t', 1, null, head + face * 2).reason == head + face * 2
    }

    def "a nullable optional field is omitted, not emitted as null"() {
        expect: 'task_created with no group omits the key entirely (matching the Snakemake producer)'
        !BiffixEvents.taskCreated('align (s1)', null, null).containsKey('group')
        !BiffixEvents.taskCreated('align (s1)', 'align', []).containsKey('deps')
        BiffixEvents.taskCreated('m', 'm', ['b', 'a', 'b']).deps == ['a', 'b']
        validate(env(BiffixEvents.taskCreated('align (s1)', null, null))).isEmpty()
        !BiffixEvents.runStarted(null).containsKey('session_id')
        validate(env(BiffixEvents.runStarted(null))).isEmpty()

        and: 'a terminal with no attempt or execution omits them'
        BiffixEvents.taskFailed('t', null, null) == [kind: 'task_failed', task_key: 't']
        validate(env(BiffixEvents.taskFailed('t', null, null))).isEmpty()

        and: 'a submit with no known image omits it'
        !BiffixEvents.taskSubmitted('t', 1, 'job-1', null).containsKey('image')
        validate(env(BiffixEvents.taskSubmitted('t', 1, 'job-1', null))).isEmpty()

        and: 'a verdict with only its task omits attempt, category and reason (an empty reason too)'
        BiffixEvents.engineTaskVerdict('t', null, null, '') == [kind: 'engine_task_verdict', task_key: 't', verdict: 'failed']
        validate(env(BiffixEvents.engineTaskVerdict('t', null, null, null))).isEmpty()

        and: 'an empty run_dag is still valid'
        validate(env(BiffixEvents.runDag([], []))).isEmpty()
    }

    def "task_outputs sorts and de-duplicates its refs and omits null or false fields"() {
        when:
        final ev = BiffixEvents.taskOutputs('merge', null, 'published', false, [
            [s3_uri: 's3://b/out/z.txt', path: 'z.txt'],
            [s3_uri: 's3://b/out/a.txt'],
            [s3_uri: 's3://b/out/z.txt', path: 'dup.txt'],
        ])

        then: 'sorted by s3_uri, first occurrence kept, absent path not emitted as null'
        ev.outputs == [[s3_uri: 's3://b/out/a.txt'], [s3_uri: 's3://b/out/z.txt', path: 'z.txt']]
        !ev.containsKey('attempt')
        !ev.containsKey('reused')
        ev.role == 'published'
        validate(env(ev)).isEmpty()

        and: 'a run-level, reused batch omits task_key and carries reused: true'
        final run = BiffixEvents.taskOutputs(null, null, 'published', true, [[s3_uri: 's3://b/o']])
        !run.containsKey('task_key')
        run.reused == true
        validate(env(run)).isEmpty()

        and: 'an empty batch is still valid'
        validate(env(BiffixEvents.taskOutputs('t', 2, 'intermediate', false, []))).isEmpty()
    }

    def "the envelope stamps the shared fields"() {
        when:
        final ev = BiffixEvents.envelope(1, 'run-9', 42.0d, BiffixEvents.taskSucceeded('t', 1, null))

        then:
        ev.schema_version == 1
        ev.run_id == 'run-9'
        ev.ts == 42.0d
        ev.kind == 'task_succeeded'
        ev.task_key == 't'
    }

    def "an event without one of its required fields is rejected"() {
        given: 'a valid sample with the one field removed'
        final event = new LinkedHashMap<String, Object>(samples()[kind])
        assert validate(event).isEmpty()
        event.remove(field)

        expect:
        !validate(event).isEmpty()

        where:
        kind             | field
        'run_started'    | 'run_id'
        'run_started'    | 'ts'
        'task_submitted' | 'batch_job_id'
        'task_submitted' | 'attempt'
    }
}
