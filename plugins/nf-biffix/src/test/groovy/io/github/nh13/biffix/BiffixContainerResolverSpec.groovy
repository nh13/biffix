package io.github.nh13.biffix

import groovy.json.JsonOutput
import groovy.json.JsonSlurper
import java.nio.file.Files
import nextflow.container.resolver.ContainerInfo
import nextflow.container.resolver.ContainerResolver
import nextflow.container.resolver.DefaultContainerResolver
import nextflow.exception.AbortOperationException
import nextflow.processor.TaskRun
import spock.lang.Specification

/**
 * Tasks run the tenant copy of their container, looked up in the run's image map as written or as
 * Nextflow resolves it; anything the map lacks fails the task.
 */
class BiffixContainerResolverSpec extends Specification {

    static final String TENANT = '123456789012.dkr.ecr.us-east-1.amazonaws.com/biffix-workers'
    static final String FASTQC = "$TENANT@sha256:${'f' * 64}"
    static final String UBUNTU = "$TENANT@sha256:${'u' * 64}"
    static final String ALIGN = "$TENANT@sha256:${'a' * 64}"

    static String mapFile(Object content) {
        def file = Files.createTempFile('images', '.json')
        file.text = content instanceof String ? content : JsonOutput.toJson(content)
        return file.toString()
    }

    BiffixContainerResolver resolver(Map<String, String> entries, ContainerResolver fallback) {
        return new BiffixContainerResolver(mapFile(entries), fallback)
    }

    /** Nextflow's resolution as nf-core configures it: `docker.registry = 'quay.io'`. */
    static class QuayRegistry extends DefaultContainerResolver {
        @Override
        ContainerInfo resolveImage(TaskRun task, String image) {
            final registry = image.tokenize('/')[0]
            return new ContainerInfo(image, registry.contains('.') ? image : "quay.io/$image")
        }
    }

    ContainerResolver quayRegistry() { new QuayRegistry() }

    def 'a declared container runs its tenant copy, as written or as Nextflow resolves it'() {
        given:
        def r = resolver(
            ['quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0': FASTQC, 'ubuntu:24.04': UBUNTU],
            quayRegistry())

        expect:
        r.enabled()
        // nf-core writes the ref without a registry; docker.registry supplies it.
        r.resolveImage(new TaskRun(name: 'FASTQC (1)'), 'biocontainers/fastqc:0.12.1--hdfd78af_0').target == FASTQC
        r.resolveImage(new TaskRun(name: 'FASTQC (1)'), 'quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0').target == FASTQC
        // A normalized spelling of a declared ref.
        r.resolveImage(new TaskRun(name: 'FASTQC (1)'), 'docker.io/library/ubuntu:24.04').hashKey == UBUNTU
    }

    def 'a hit as written wins over the spelling Nextflow resolves it to'() {
        given: 'the map names both the bare ref and its quay.io resolution, to different images'
        def r = resolver(['biocontainers/x:1': FASTQC, 'quay.io/biocontainers/x:1': UBUNTU], quayRegistry())

        expect:
        r.resolveImage(new TaskRun(name: 'X (1)'), 'biocontainers/x:1').target == FASTQC
    }

    def 'a container biffix did not import fails the task, naming it'() {
        given:
        def r = resolver(['ubuntu:24.04': UBUNTU], quayRegistry())

        when:
        r.resolveImage(new TaskRun(name: 'SEQKIT (1)'), 'quay.io/biocontainers/seqkit:2.9.0--h9ee0642_0')

        then:
        def e = thrown(AbortOperationException)
        e.message.contains('SEQKIT (1)')
        e.message.contains("'quay.io/biocontainers/seqkit:2.9.0--h9ee0642_0'")
        e.message.contains('images:')
    }

    def 'a bare images: name matches only as written, never as a Docker Hub image'() {
        given: 'a recipe image, keyed by its images: name alone'
        def r = resolver(['align': ALIGN], quayRegistry())

        expect:
        r.resolveImage(new TaskRun(name: 'ALIGN (1)'), 'align').target == ALIGN
        new ImageMap(['align': ALIGN]).resolve('align:latest') == null
        new ImageMap(['align': ALIGN]).resolve('docker.io/library/align:latest') == null

        when: 'a task names the Docker Hub image of the same name'
        r.resolveImage(new TaskRun(name: 'ALIGN (1)'), 'align:latest')

        then:
        thrown(AbortOperationException)
    }

    def 'a task with no container is left to Nextflow'() {
        given:
        def fallback = Mock(ContainerResolver)
        def r = resolver(['ubuntu:24.04': UBUNTU], fallback)

        when:
        def none = r.resolveImage(new TaskRun(name: 'FASTQC (1)'), null)

        then:
        1 * fallback.resolveImage(_, null) >> ContainerInfo.EMPTY
        none == ContainerInfo.EMPTY
    }

    def 'a job definition fails the task unless the map names it verbatim'() {
        given:
        def fallback = Mock(ContainerResolver)
        def r = resolver(['ubuntu:24.04': UBUNTU], fallback)

        when: 'the map lacks it: it would run whatever image (and job role) the definition names'
        r.resolveImage(new TaskRun(name: 'FASTQC (1)'), 'job-definition://my-def')

        then: 'it fails, without being handed to Nextflow'
        0 * fallback.resolveImage(_, _)
        def e = thrown(AbortOperationException)
        e.message.contains("'job-definition://my-def'")

        when: 'the map names it'
        def named = resolver(['job-definition://my-def': 'job-definition://my-def'], fallback)
            .resolveImage(new TaskRun(name: 'FASTQC (1)'), 'job-definition://my-def')

        then:
        0 * fallback.resolveImage(_, _)
        named.target == 'job-definition://my-def'
    }

    def 'without a map the resolver is off'() {
        expect:
        !new BiffixContainerResolver(null, new DefaultContainerResolver()).enabled()
    }

    def 'an unreadable image map fails the task, naming the map'() {
        given:
        def r = new BiffixContainerResolver(path, quayRegistry())

        when:
        r.resolveImage(new TaskRun(name: 'X (1)'), 'ubuntu:24.04')

        then:
        def e = thrown(AbortOperationException)
        e.message.startsWith("BIFFIX_IMAGE_MAP_FILE=$path: cannot read the run's image map")
        e.message.contains(why)

        where:
        path                               | why
        '/nonexistent/images.json'         | 'Unable to process file'
        mapFile('{not json')               | 'JsonException'
        mapFile('["a list"]')              | 'an image map is a JSON object'
        mapFile('{"ubuntu": 3}')           | 'entry `ubuntu` is not an image reference'
    }

    def 'references normalize the way Docker (and biffix-workflow) read them'() {
        expect:
        ImageMap.normalize(ref) == want

        where:
        ref                                        | want
        'ubuntu'                                   | 'docker.io/library/ubuntu:latest'
        'biocontainers/fastqc:0.12.1--hdfd78af_0'  | 'docker.io/biocontainers/fastqc:0.12.1--hdfd78af_0'
        'registry-1.docker.io/library/ubuntu:24.04'| 'docker.io/library/ubuntu:24.04'
        'localhost:5000/coord'                     | 'localhost:5000/coord:latest'
        "quay.io/x/y:1@sha256:${'A' * 64}"         | "quay.io/x/y:1@sha256:${'a' * 64}"
        "quay.io/b/bwa@sha256:${'a' * 64}"         | "quay.io/b/bwa@sha256:${'a' * 64}"
        'Quay.IO/x/y:1'                            | 'quay.io/x/y:1'
    }

    /** biffix-workflow's cases (schemas/image-ref-cases.json, from the Rust ImageRef tests). */
    static List<Map> sharedCases() {
        final candidates = ['../../schemas/image-ref-cases.json', 'schemas/image-ref-cases.json']
        final file = candidates.collect { new File(it) }.find { it.exists() }
        assert file != null: "image-ref-cases.json not found from ${System.getProperty('user.dir')}"
        return new JsonSlurper().parse(file) as List<Map>
    }

    def 'references normalize exactly as biffix-workflow writes the map keys'() {
        expect: 'every reference biffix-workflow accepts normalizes to the same spelling here'
        ImageMap.normalize(input as String) == normalized

        where:
        [input, normalized] << sharedCases().findAll { it.normalized != null }.collect { [it.input, it.normalized] }
    }

    def 'the shared cases cover both accepted and rejected references'() {
        expect:
        sharedCases().any { it.normalized == null }
        sharedCases().count { it.normalized != null } >= 10
    }
}
