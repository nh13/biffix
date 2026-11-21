package io.github.nh13.biffix

import groovy.json.JsonOutput
import groovy.json.JsonSlurper
import java.nio.file.Files
import nextflow.container.ApptainerConfig
import nextflow.container.ContainerConfig
import nextflow.container.DockerConfig
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

    def 'on AWS Batch the task runs the tenant reference, which is also its identity'() {
        given:
        def r = resolver(['quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0': FASTQC], quayRegistry())

        when:
        def info = r.resolveImage(new TaskRun(name: 'FASTQC (1)'), 'biocontainers/fastqc:0.12.1--hdfd78af_0')

        then: 'target and hashKey are the mapped reference, as before images could be local files'
        info.source == 'biocontainers/fastqc:0.12.1--hdfd78af_0'
        info.target == FASTQC
        info.hashKey == FASTQC
    }

    /** A task whose processes run under the given container config (its session's, in a real run). */
    static class EngineTask extends TaskRun {
        ContainerConfig containerConfig

        @Override
        ContainerConfig getContainerConfig() { containerConfig }
    }

    static EngineTask apptainerTask(String name) {
        return new EngineTask(name: name, containerConfig: new ApptainerConfig([enabled: true, registry: 'quay.io']))
    }

    def 'with Apptainer the task runs the tenant copy\'s image file and keeps the reference as its identity'() {
        given: 'Nextflow resolves the tenant reference to the image file in apptainer.libraryDir'
        final sif = '/scratch/apptainer/images/' + FASTQC.replace(':', '-').replace('/', '-') + '.img'
        def fallback = Mock(ContainerResolver)
        def r = resolver(['quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0': FASTQC], fallback)

        when: 'a task names the image without its registry, which apptainer.registry supplies'
        def info = r.resolveImage(apptainerTask('FASTQC (1)'), 'biocontainers/fastqc:0.12.1--hdfd78af_0')

        then: 'only the tenant copy goes through Nextflow, with its scheme: the declared image is never pulled to be looked up'
        1 * fallback.resolveImage(_, "docker://$FASTQC") >> new ContainerInfo(FASTQC, sif, sif)
        0 * fallback.resolveImage(_, _)
        info.target == sif
        info.hashKey == FASTQC
    }

    def 'with registryOverride, Docker and AWS Batch still run the tenant copy as mapped'() {
        given: 'Nextflow\'s own resolution, whose registryOverride rewrites even an absolute reference'
        def task = new EngineTask(name: 'FASTQC (1)',
            containerConfig: new DockerConfig([enabled: true, registry: 'quay.io', registryOverride: true]))
        def r = resolver(['quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0': FASTQC], new DefaultContainerResolver())

        expect: 'Nextflow alone would have rewritten the ECR host'
        new DefaultContainerResolver().resolveImage(task, FASTQC).target.startsWith('quay.io/')

        when:
        def info = r.resolveImage(task, 'biocontainers/fastqc:0.12.1--hdfd78af_0')

        then: 'the reference the run\'s job definition map names'
        info.target == FASTQC
        info.hashKey == FASTQC
    }

    def 'with registryOverride, Apptainer runs the image file the coordinator converted'() {
        given: 'the tenant copy\'s image file in apptainer.libraryDir, named as the coordinator names it'
        def library = Files.createTempDirectory('library')
        def sif = library.resolve(FASTQC.replace(':', '-').replace('/', '-') + '.img')
        sif.text = 'image'
        def task = new EngineTask(name: 'FASTQC (1)', containerConfig: new ApptainerConfig(
            [enabled: true, registry: 'quay.io', registryOverride: true, libraryDir: library.toString()]))
        def r = resolver(['quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0': FASTQC], new DefaultContainerResolver())

        when:
        def info = r.resolveImage(task, 'biocontainers/fastqc:0.12.1--hdfd78af_0')

        then: 'that file, not one named for a quay.io rewrite of the reference'
        info.target == sif.toString()
        info.hashKey == FASTQC
    }

    def 'unqualified mapped values use the converted file without applying the workflow registry'() {
        given:
        final library = Files.createTempDirectory('library')
        final sif = library.resolve('tool-1.img')
        sif.text = 'converted image'
        final task = new EngineTask(name: 'TOOL (1)', containerConfig: new ApptainerConfig(
            [enabled: true, registry: 'private.example.com', registryOverride: true,
                libraryDir: library.toString()]))
        final r = resolver(['declared/tool:1': 'tool:1'], new DefaultContainerResolver())

        when: 'the real Nextflow resolver looks up the same cache filename as the converter'
        final info = r.resolveImage(task, 'declared/tool:1')

        then:
        info.target == sif.toString()
        info.hashKey == 'tool:1'
    }

    def 'with Apptainer an image the map lacks still fails the task, without a pull'() {
        given:
        def fallback = Mock(ContainerResolver)
        def r = resolver(['ubuntu:24.04': UBUNTU], fallback)

        when:
        r.resolveImage(apptainerTask('SEQKIT (1)'), 'biocontainers/seqkit:2.9.0--h9ee0642_0')

        then:
        0 * fallback.resolveImage(_, _)
        def e = thrown(AbortOperationException)
        e.message.contains("'biocontainers/seqkit:2.9.0--h9ee0642_0'")
    }

    def 'the registry spelling of a reference is the one Nextflow pulls'() {
        expect:
        BiffixContainerResolver.registryName(new ApptainerConfig(config), image) == want

        where:
        config                                                        | image                         || want
        [enabled: true]                                               | 'biocontainers/x:1'           || 'biocontainers/x:1'
        [enabled: true, registry: 'quay.io']                          | 'biocontainers/x:1'           || 'quay.io/biocontainers/x:1'
        [enabled: true, registry: 'quay.io/']                         | 'biocontainers/x:1'           || 'quay.io/biocontainers/x:1'
        [enabled: true, registry: 'quay.io']                          | "$TENANT:1"                   || "$TENANT:1"
        [enabled: true, registry: 'quay.io']                          | 'docker://ubuntu:24'          || null
        [enabled: true, registry: 'quay.io']                          | 'https://depot.galaxyproject.org/singularity/x:1' || null
        [enabled: true, registry: 'quay.io']                          | '/images/x.sif'               || null
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
        mapFile('["a list"]')              | 'the map is not a JSON object'
        mapFile('{"ubuntu": 3}')           | 'entry `ubuntu` is not a non-empty string'
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
