package io.github.nh13.biffix

import groovy.json.JsonSlurper
import groovy.transform.CompileStatic

/**
 * The images a biffix run may use: each declared image reference to the tenant copy to run,
 * `<tenant workflow repo>@sha256:…`. The backend writes it per run (`images.json`, see
 * crates/biffix-backend/src/images/map.rs, which already keys every reference by its normalized
 * spelling too) and the coordinator hands it to Nextflow as {@code BIFFIX_IMAGE_MAP_FILE}.
 *
 * A key that is a bare name (no `/`, `:` or `@` — an image built from a recipe, or an imported
 * image, named by its `images:` name) matches only as written: its normalized spelling would be a
 * Docker Hub image (`align` = `docker.io/library/align:latest`), and a task naming that must not
 * silently run the tenant's image of that name. This is nf-biffix's own rule: the pinned Snakemake
 * AWS Batch executor's image map normalizes every key, bare names included (and drops a tag beside
 * a digest), so the two engines resolve the same map differently until the executor follows suit.
 */
@CompileStatic
class ImageMap {

    static final String DOCKER_HUB = 'docker.io'

    private final Map<String, String> mapping = [:]

    ImageMap(Map<String, String> entries) {
        entries.each { ref, image -> mapping[ref] = image }
        // Normalized spellings too, unless the map names that spelling itself (or the key is a bare name).
        entries.each { ref, image ->
            if( !isBareName(ref) )
                mapping.putIfAbsent(normalize(ref), image)
        }
    }

    /** Read a JSON object of reference to reference. Throws on a missing or malformed file. */
    static ImageMap fromFile(String path) {
        def data = new JsonSlurper().parse(new File(path))
        if( !(data instanceof Map) )
            throw new IllegalArgumentException("$path: an image map is a JSON object")
        Map<String, String> entries = [:]
        (data as Map).each { k, v ->
            if( !(v instanceof String) || !v )
                throw new IllegalArgumentException("$path: entry `$k` is not an image reference")
            entries[k as String] = v as String
        }
        return new ImageMap(entries)
    }

    /**
     * The image to run for {@code ref} (as written, then normalized), or {@code null}. A task's
     * bare `ubuntu` still finds a declared `ubuntu:latest` (the normalized spellings meet); what a
     * bare key never gets is a normalized alias, so `align:latest` does not find the recipe `align`.
     */
    String resolve(String ref) {
        if( !ref )
            return null
        return mapping[ref] ?: mapping[normalize(ref)]
    }

    /** The image the map names for {@code ref} exactly as written, or {@code null}. */
    String resolveExact(String ref) {
        return ref ? mapping[ref] : null
    }

    int size() { mapping.size() }

    /** A name with no registry, repository path, tag or digest (`align`, `$default`). */
    static boolean isBareName(String ref) {
        final r = ref.trim()
        return !r.contains('/') && !r.contains(':') && !r.contains('@')
    }

    /**
     * The canonical spelling of an image reference, as Docker reads it (and as biffix-workflow's
     * {@code ImageRef} writes it): the first path component is a registry only when it has a
     * `.` or `:` or is `localhost` (and it is lowercased: host names are case-insensitive), else
     * the image is on Docker Hub (a one-part name under `library/`); no tag and no digest means
     * `:latest`. Unlike {@code ImageRef::parse} it does not validate: a reference
     * biffix-workflow would reject normalizes to some spelling no map key has, so the lookup fails
     * closed. Parity with the Rust port is pinned by {@code schemas/image-ref-cases.json}, which
     * both test suites read.
     */
    static String normalize(String ref) {
        String name = ref.trim()
        String digest = null
        int at = name.indexOf('@')
        if( at >= 0 ) {
            digest = name.substring(at + 1).toLowerCase()
            name = name.substring(0, at)
        }
        String tag = null
        int slash = name.lastIndexOf('/')
        int colon = name.lastIndexOf(':')
        if( colon > slash ) {
            tag = name.substring(colon + 1)
            name = name.substring(0, colon)
        }
        String registry = DOCKER_HUB
        String repository = name
        int first = name.indexOf('/')
        if( first > 0 ) {
            String head = name.substring(0, first)
            if( head.contains('.') || head.contains(':') || head == 'localhost' ) {
                registry = head.toLowerCase()
                repository = name.substring(first + 1)
            }
        }
        if( registry == 'index.docker.io' || registry == 'registry-1.docker.io' )
            registry = DOCKER_HUB
        if( registry == DOCKER_HUB && !repository.contains('/') )
            repository = "library/$repository"
        if( !tag && !digest )
            tag = 'latest'
        def out = new StringBuilder("$registry/$repository")
        if( tag )
            out.append(':').append(tag)
        if( digest )
            out.append('@').append(digest)
        return out.toString()
    }
}
