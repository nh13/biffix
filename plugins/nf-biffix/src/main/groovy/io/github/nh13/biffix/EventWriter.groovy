package io.github.nh13.biffix

import groovy.json.JsonOutput
import groovy.transform.CompileStatic
import groovy.util.logging.Slf4j

import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.StandardOpenOption

/**
 * The durable events file (the archival replay copy): the same
 * file contract as the Snakemake logger's {@code writer.py}. One JSON line per enveloped RunEvent,
 * appended as the observer emits it, to {@code BIFFIX_EVENT_FILE} (default
 * {@value #DEFAULT_EVENT_FILE}) relative to the directory Nextflow starts in. The coordinator runs
 * Nextflow from the workflow's source root and reads the file there
 * ({@code docker/coordinator/lib/events.sh events_file}), uploads it under the run's prefix and
 * replays it through ingest at exit, so an event the live poster drops still lands (the backend
 * dedupes the repeats on their dedup keys).
 *
 * The file covers ONE Nextflow invocation: the first write truncates it, so it never carries a
 * previous invocation's events. A write after {@link #close} (a late event) reopens it for append.
 * Every line is flushed as it is written, so a crash loses at most the line in flight.
 *
 * Never breaks the engine: the first I/O failure is logged once and disables the file for the rest
 * of the run (the live POSTs carry on).
 */
@Slf4j
@CompileStatic
class EventWriter {

    /** The file name when BIFFIX_EVENT_FILE is unset; the same default as the Snakemake logger. */
    static final String DEFAULT_EVENT_FILE = '.biffix_events.jsonl'

    private final Path path
    private final Object lock = new Object()
    private Writer out = null
    /** The first open truncates (a fresh invocation); later ones append. */
    private boolean opened = false
    private boolean failed = false

    EventWriter(Path path) {
        this.path = path
    }

    /** The writer the coordinator's environment names, relative to {@code cwd}. */
    static EventWriter fromEnv(Map<String, String> env, Path cwd) {
        final name = env.get('BIFFIX_EVENT_FILE') ?: DEFAULT_EVENT_FILE
        return new EventWriter(cwd.resolve(name))
    }

    /** As {@link #fromEnv(Map, Path)}, relative to the process's working directory. */
    static EventWriter fromEnv(Map<String, String> env) {
        return fromEnv(env, Paths.get('').toAbsolutePath())
    }

    Path getPath() {
        return path
    }

    /** Append one event as a JSON line. Never throws. */
    void write(Map<String, Object> event) {
        final String line
        try {
            line = JsonOutput.toJson(event)
        } catch (Throwable t) {
            log.warn("nf-biffix: could not serialize a ${event?.get('kind')} event for the events file "
                + "(swallowed): ${t.class.simpleName}")
            return
        }
        synchronized (lock) {
            if (failed) {
                return
            }
            try {
                if (out == null) {
                    final parent = path.toAbsolutePath().parent
                    if (parent != null) {
                        Files.createDirectories(parent)
                    }
                    out = Files.newBufferedWriter(path, StandardCharsets.UTF_8, StandardOpenOption.CREATE,
                        StandardOpenOption.WRITE,
                        opened ? StandardOpenOption.APPEND : StandardOpenOption.TRUNCATE_EXISTING)
                    opened = true
                }
                out.write(line)
                out.write('\n')
                out.flush()
            } catch (Throwable t) {
                failed = true
                log.warn("nf-biffix: writing the events file ${path} failed; it is disabled for the rest of "
                    + "the run (live ingest continues): ${t.class.simpleName}: ${t.message}")
                closeQuietly()
            }
        }
    }

    /** Close the file (a later write reopens it for append). Never throws. */
    void close() {
        synchronized (lock) {
            closeQuietly()
        }
    }

    private void closeQuietly() {
        try {
            out?.close()
        } catch (Throwable ignored) {
            // Every line was already flushed.
        }
        out = null
    }
}
