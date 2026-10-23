package io.github.nh13.biffix

import groovy.json.JsonOutput
import groovy.json.JsonSlurper
import groovy.transform.CompileStatic
import groovy.transform.PackageScope
import groovy.util.logging.Slf4j

import java.net.URI
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import java.nio.charset.StandardCharsets
import java.time.Duration
import java.util.concurrent.atomic.AtomicLong

/**
 * POST biffix RunEvents to the ingest route.
 *
 * The Nextflow counterpart of the Snakemake logger's {@code poster.py}, with the same delivery
 * rules: it targets the same bearer-authed {@code {BIFFIX_INGEST_URL}/events} endpoint with the same
 * batch envelope ({@code IngestRequest { events: [...] }}) and the same best-effort contract. A failed
 * POST is logged and swallowed: the observer also appends every event to the durable events file
 * ({@link EventWriter}), which the coordinator uploads and replays through ingest at exit, and the
 * EventBridge stream carries the Batch jobs' terminals, so ingest loss never breaks a run.
 *
 * Events are buffered and POSTed in batches by a background daemon thread: a batch goes out once
 * {@code maxBatch} events are waiting (the route's cap) or {@code flushIntervalMs} after the previous
 * send, and {@link #close} drains whatever is left. One request per event made a large DAG's
 * start-up a request storm and blocked Nextflow's observer thread on every POST.
 *
 * Delivery rules (as {@code poster.py}, which {@code IngestPosterSpec} and {@code test_poster.py}
 * pin as one contract; the one difference is noted in the last rule):
 * <ul>
 *   <li>A batch is bounded by event count ({@code maxBatch}) AND by serialized size
 *       ({@link #MAX_BATCH_BYTES}), since the route answers 413 on either. An event that alone is
 *       over the route's body limit can never be delivered and is dropped without being sent.</li>
 *   <li>A 413 or a 400 rejects the whole batch, so the batch is halved and each half resent until
 *       the offending event is alone; only that one event is dropped (logged by kind and task key),
 *       within {@link #SPLIT_POST_BUDGET} requests per batch. 401/403 apply to the whole token or
 *       run, so they drop the batch without splitting (a 401 also stops all later sends).</li>
 *   <li>A 2xx can still refuse single events (a {@code task_submitted} whose Batch job claim the
 *       route could not verify): the answer's {@code refused} names each by its index in the batch,
 *       and the rest landed. A resend would be refused again, so each is logged (by kind, task key
 *       and reason) and dropped.</li>
 *   <li>Transient failures (connection errors, timeouts, 408, 429, 5xx) are retried with doubling
 *       backoff, or the response's {@code Retry-After}. Nothing a send throws kills the flusher.</li>
 *   <li>{@link #close} has an overall deadline ({@link #CLOSE_TIMEOUT_MS}), and once a batch at close
 *       has exhausted its retries (ingest is unreachable) the rest of the drain is abandoned rather
 *       than paying the retry cost batch after batch. Events posted after close go out on a
 *       background thread, one attempt per batch, never on the caller's thread. A backoff here
 *       naps in short steps, so a close deadline set while it waits cuts it short; poster.py's
 *       sleep is clipped only to a deadline already set when it began.</li>
 *   <li>A full buffer drops the bulky descriptive events first and keeps
 *       {@link #KEEP_UNDER_PRESSURE} (the {@code task_submitted} binding and the state changes).</li>
 * </ul>
 *
 * Uses only the JDK HttpClient so the plugin drags no HTTP dependency into the coordinator image.
 */
@Slf4j
@CompileStatic
class IngestPoster {

    // The route's limits and this delivery policy are defined once, in biffix-core's
    // `ingest_limits`, and written to schemas/ingest-limits.json; IngestPosterSpec fails if a value
    // here differs from that file. Change them there first.

    /** The ingest route rejects a larger batch with 413. */
    static final int MAX_BATCH = 500
    /** The route rejects a body over 5 MiB with 413. */
    static final int ROUTE_MAX_BODY_BYTES = 5 * 1024 * 1024
    /**
     * Batches are cut below the route's limit, leaving room for the Lambda payload limit (6 MB)
     * should the body be base64-encoded on the way (4 MiB * 4/3 < 6 MB).
     */
    static final int MAX_BATCH_BYTES = 4 * 1024 * 1024
    /**
     * Events queued for the flusher (while a send or its retries are in flight, or ingest is down);
     * beyond this, events are dropped (see {@link #KEEP_UNDER_PRESSURE} for which go first). A batch
     * that fails all its attempts is dropped, not re-queued: the events file replay re-sends it.
     */
    static final int MAX_BUFFERED = 20_000
    /** Attempts per batch for a transient failure (connection error, 408, 429, 5xx). */
    static final int MAX_ATTEMPTS = 3
    /** Per-request timeout (and connect timeout), unless the close deadline is nearer. */
    static final long REQUEST_TIMEOUT_MS = 5_000L
    /** The longest a batch waits for more events before it is sent. */
    static final long FLUSH_INTERVAL_MS = 1_000L
    /** The first retry's backoff, doubling per attempt. */
    static final long RETRY_BACKOFF_MS = 500L
    /** The overall time budget of close(): the flusher join plus the final drain. */
    static final long CLOSE_TIMEOUT_MS = 30_000L
    /** At most one "buffer full" warning per this interval (with the running total). */
    static final long DROP_WARN_INTERVAL_MS = 30_000L
    /** The longest a {@code Retry-After} is honoured for. */
    static final long MAX_RETRY_AFTER_MS = 30_000L
    /**
     * POSTs one batch may spend isolating the events the route rejects (413/400) by halving it. One
     * bad event in 500 costs about 19; a route that rejects everything (a contract mismatch) stops
     * here instead of costing about 2 requests per event.
     */
    static final int SPLIT_POST_BUDGET = 64
    /**
     * When the buffer overflows, these kinds are kept and the oldest of the others dropped first:
     * {@code task_submitted} carries the batch_job_id binding that joins the AWS Batch (EventBridge)
     * stream to the engine's, and the run/task state transitions are small and are what would
     * otherwise leave a task or run stuck until the exit replay ({@code engine_task_verdict} is one:
     * it fails an attempt AWS reports SUCCEEDED). The bulky descriptive events ({@code task_created}
     * with its deps, {@code run_dag}, {@code task_outputs}) go first. If the buffer holds nothing but
     * these kinds, the oldest of them are dropped.
     */
    static final Set<String> KEEP_UNDER_PRESSURE = Collections.unmodifiableSet(new HashSet<String>([
        'task_submitted', 'task_succeeded', 'task_failed', 'task_retrying', 'task_cached',
        'run_started', 'run_succeeded', 'run_failed', 'engine_task_verdict',
    ]))
    /** An overflow trims the buffer to this fraction of MAX_BUFFERED, so it is not re-scanned per post. */
    private static final double TRIM_TO = 0.9d

    private static final byte[] ENVELOPE_HEAD = '{"events":['.getBytes(StandardCharsets.UTF_8)
    private static final byte[] ENVELOPE_TAIL = ']}'.getBytes(StandardCharsets.UTF_8)
    private static final int COMMA = 0x2c

    @PackageScope
    static enum Outcome {
        SENT,
        /** A retry may succeed. */
        TRANSIENT,
        /** The route rejected the batch as a whole (413/400): halve it. */
        SPLIT,
        /** A retry cannot fix it (e.g. 401/403): drop the batch. */
        FINAL
    }

    @PackageScope
    static class Result {
        final Outcome outcome
        final String detail
        /** The response's Retry-After, in ms (0 when absent). */
        final long retryAfterMs

        Result(Outcome outcome, String detail, long retryAfterMs = 0L) {
            this.outcome = outcome
            this.detail = detail
            this.retryAfterMs = retryAfterMs
        }
    }

    /** One event and its serialized form. */
    private static class Item {
        final Map<String, Object> event
        final byte[] data

        Item(Map<String, Object> event, byte[] data) {
            this.event = event
            this.data = data
        }
    }

    private final String url
    /** What a warning names: host and path only (never the token, never URL userinfo). */
    private final String where
    private final String token
    private final HttpClient client
    private final int maxBatch
    private final int maxBatchBytes
    private final long flushIntervalMs
    private final long retryBackoffMs
    private final long timeoutMs
    private final long closeTimeoutMs

    private final Object lock = new Object()
    /**
     * Serializes sends so a close() drain never interleaves with the flusher's batch. Held only
     * around one batch's attempts, never across a split.
     */
    private final Object sendLock = new Object()
    private List<Map<String, Object>> buffer = new ArrayList<>()
    private boolean closed = false
    private Thread thread = null
    private Thread lateThread = null
    /** The System.nanoTime() by which close() gives up; set by close(), seen by the flusher too. */
    private volatile Long deadlineNanos = null
    /**
     * Circuit breaker: a batch exhausted its retries on transient failures and nothing has been
     * delivered since. At close (and for late events) this abandons the rest.
     */
    private volatile boolean down = false
    /** The URL or token cannot form an accepted request; every later send would fail the same way. */
    private volatile boolean broken = false
    /** Events the flusher has taken from the buffer and not finished with (for the close log). */
    private final AtomicLong inflight = new AtomicLong()
    private final AtomicLong abandoned = new AtomicLong()
    private long overflowDropped = 0L
    private Long lastDropWarnNanos = null

    IngestPoster(String ingestUrl, String token) {
        this(ingestUrl, token, MAX_BATCH, FLUSH_INTERVAL_MS, RETRY_BACKOFF_MS)
    }

    IngestPoster(String ingestUrl, String token, int maxBatch, long flushIntervalMs, long retryBackoffMs) {
        this(ingestUrl, token, maxBatch, flushIntervalMs, retryBackoffMs, REQUEST_TIMEOUT_MS, CLOSE_TIMEOUT_MS,
            MAX_BATCH_BYTES)
    }

    IngestPoster(String ingestUrl, String token, int maxBatch, long flushIntervalMs, long retryBackoffMs,
                 long timeoutMs, long closeTimeoutMs, int maxBatchBytes) {
        this.url = ingestUrl ? ingestUrl.replaceAll('/+$', '') + '/events' : null
        this.where = describe(this.url)
        this.token = token
        this.maxBatch = maxBatch
        this.flushIntervalMs = flushIntervalMs
        this.retryBackoffMs = retryBackoffMs
        this.timeoutMs = timeoutMs
        this.closeTimeoutMs = closeTimeoutMs
        this.maxBatchBytes = maxBatchBytes
        this.client = HttpClient.newBuilder()
            .connectTimeout(Duration.ofMillis(Math.max(1L, timeoutMs)))
            .build()
    }

    private static String describe(String url) {
        if (!url) {
            return ''
        }
        try {
            final u = URI.create(url)
            return "${u.host ?: ''}${u.path ?: ''}".toString()
        } catch (Exception ignored) {
            return '<invalid ingest url>'
        }
    }

    boolean isEnabled() {
        return url && token
    }

    /** Events abandoned at close or while ingest was known unreachable (test seam). */
    @PackageScope
    long getAbandonedCount() {
        return abandoned.get()
    }

    /** Events dropped from a full buffer (test seam). */
    @PackageScope
    long getOverflowDroppedCount() {
        synchronized (lock) {
            return overflowDropped
        }
    }

    /**
     * Queue one RunEvent for the next batch. Never does network I/O on the caller's thread and never
     * throws.
     */
    void post(Map<String, Object> event) {
        if (!enabled) {
            return
        }
        try {
            synchronized (lock) {
                if (closed) {
                    // A late event after close() (a second onFlowComplete, an aborted task's
                    // terminal) goes out on a background thread, one attempt per batch.
                    if (broken || down) {
                        abandoned.incrementAndGet()
                        return
                    }
                    buffer.add(event)
                    trimLocked()
                    if (lateThread == null) {
                        lateThread = new Thread({ runLate() } as Runnable, 'biffix-ingest-late')
                        lateThread.daemon = true
                        lateThread.start()
                    }
                    return
                }
                buffer.add(event)
                trimLocked()
                if (thread == null) {
                    thread = new Thread({ runFlusher() } as Runnable, 'biffix-ingest-poster')
                    thread.daemon = true
                    thread.start()
                }
                if (buffer.size() >= maxBatch) {
                    lock.notifyAll()
                }
            }
        } catch (Throwable t) {
            log.warn("biffix ingest: could not queue an event (swallowed): ${t.class.simpleName}")
        }
    }

    /**
     * Stop the flusher and send whatever is still buffered, within {@code closeTimeoutMs}.
     * Idempotent; never throws. Whatever is not delivered by the deadline, or once ingest is known to
     * be unreachable, is abandoned (and counted in a warning): the events file replay re-sends it.
     */
    void close() {
        Thread t
        List<Map<String, Object>> pending
        synchronized (lock) {
            if (closed) {
                // Later events belong to the late sender, not to a second drain.
                return
            }
            deadlineNanos = System.nanoTime() + closeTimeoutMs * 1_000_000L
            // The close drain judges reachability afresh; its first exhausted batch trips it.
            down = false
            closed = true
            // What was posted before close is the close drain's, taken with the flag: an event
            // posted from here on goes to the late sender, which must never take these (it gives a
            // batch one attempt, and close() would return before they are sent).
            pending = takeLocked()
            lock.notifyAll()
            t = thread
        }
        if (!enabled) {
            return
        }
        try {
            if (t != null && t != Thread.currentThread()) {
                // The flusher honours the deadline between batches and clips each attempt's timeout
                // and backoff to it, so it winds down by then; the grace covers the last return.
                t.join(Math.max(0L, remainingMs() ?: 0L) + 1_000L)
                if (t.alive) {
                    log.warn("biffix ingest: the flusher was still sending ${inflight.get()} event(s) at the "
                        + 'close deadline; abandoned them to the events file replay')
                }
            }
            sendAll(pending, false)
        } catch (Throwable e) {
            if (e instanceof InterruptedException) {
                Thread.currentThread().interrupt()
            }
            log.warn("biffix ingest: final flush failed (swallowed): ${e.class.simpleName}")
        }
        final long lost = abandoned.get()
        final long dropped = overflowDroppedCount
        if (lost || dropped) {
            log.warn("biffix ingest: ${lost + dropped} event(s) not delivered (${lost} abandoned at close or "
                + "while ingest was unreachable, ${dropped} dropped from a full buffer); the coordinator's "
                + 'events file replay re-sends them')
        }
    }

    /** The buffered events, leaving an empty buffer. Call with {@code lock} held. */
    private List<Map<String, Object>> takeLocked() {
        final taken = buffer
        buffer = new ArrayList<>()
        return taken
    }

    // ── background threads ──

    private void runFlusher() {
        try {
            while (true) {
                List<Map<String, Object>> drained
                synchronized (lock) {
                    if (!closed && buffer.size() < maxBatch) {
                        try {
                            lock.wait(flushIntervalMs)
                        } catch (InterruptedException ignored) {
                            // Leave the buffer for close() (or a restarted flusher).
                            Thread.currentThread().interrupt()
                            return
                        }
                    }
                    if (closed) {
                        return
                    }
                    drained = takeLocked()
                    inflight.set(drained.size())
                }
                try {
                    sendAll(drained, false)
                } catch (Throwable t) {
                    // One bad batch must never kill the flusher.
                    log.warn("biffix ingest: flusher error (swallowed; ${drained.size()} event(s) dropped): "
                        + t.class.simpleName)
                    if (t instanceof InterruptedException) {
                        Thread.currentThread().interrupt()
                        return
                    }
                } finally {
                    inflight.set(0L)
                }
            }
        } finally {
            // If the flusher ever exits while the poster is open, the next post() starts another.
            synchronized (lock) {
                if (thread == Thread.currentThread()) {
                    thread = null
                }
            }
        }
    }

    private void runLate() {
        while (true) {
            List<Map<String, Object>> drained
            synchronized (lock) {
                drained = takeLocked()
                if (drained.isEmpty()) {
                    lateThread = null
                    return
                }
            }
            try {
                sendAll(drained, true)
            } catch (Throwable t) {
                log.warn("biffix ingest: late send error (swallowed): ${t.class.simpleName}")
            }
        }
    }

    // ── the buffer ──

    /** Drop events from a buffer over MAX_BUFFERED (see KEEP_UNDER_PRESSURE). Holds {@link #lock}. */
    private void trimLocked() {
        if (buffer.size() <= MAX_BUFFERED) {
            return
        }
        final int excess = buffer.size() - (int) (MAX_BUFFERED * TRIM_TO)
        int toDrop = excess
        final kept = new ArrayList<Map<String, Object>>(buffer.size())
        for (Map<String, Object> event : buffer) {
            if (toDrop > 0 && !KEEP_UNDER_PRESSURE.contains(event.get('kind')?.toString())) {
                toDrop--
                continue
            }
            kept.add(event)
        }
        if (toDrop > 0) {
            // Nothing expendable left: the oldest of the rest go.
            kept.subList(0, toDrop).clear()
        }
        buffer = kept
        overflowDropped += excess
        final now = System.nanoTime()
        if (lastDropWarnNanos == null || now - lastDropWarnNanos >= DROP_WARN_INTERVAL_MS * 1_000_000L) {
            lastDropWarnNanos = now
            log.warn("biffix ingest buffer full (ingest unreachable?); ${overflowDropped} event(s) dropped so "
                + 'far, oldest descriptive events first (the events file replay re-sends them)')
        }
    }

    // ── sending ──

    /** Milliseconds left before the close deadline, or null when no close is under way. */
    private Long remainingMs() {
        final d = deadlineNanos
        return d == null ? null : Long.valueOf(Math.floorDiv(d - System.nanoTime(), 1_000_000L))
    }

    private boolean abandoning(boolean late) {
        if (broken) {
            return true
        }
        if (late) {
            // Sends after close() returned: the close deadline no longer applies.
            return down
        }
        final rem = remainingMs()
        if (rem == null) {
            return false
        }
        return down || rem <= 0L
    }

    private List<Item> encode(List<Map<String, Object>> events) {
        final items = new ArrayList<Item>(events.size())
        for (Map<String, Object> event : events) {
            byte[] data
            try {
                data = JsonOutput.toJson(event).getBytes(StandardCharsets.UTF_8)
            } catch (Throwable t) {
                log.warn("biffix ingest: dropped an unserializable ${event?.get('kind')} event "
                    + "(${event?.get('task_key')}): ${t.class.simpleName}")
                continue
            }
            if (data.length + ENVELOPE_HEAD.length + ENVELOPE_TAIL.length > ROUTE_MAX_BODY_BYTES) {
                log.warn("biffix ingest: dropped a ${event.get('kind')} event (${event.get('task_key')}) of "
                    + "${data.length} bytes, over the route's ${ROUTE_MAX_BODY_BYTES}-byte limit")
                continue
            }
            items.add(new Item(event, data))
        }
        return items
    }

    /** Cut items into batches of at most maxBatch events and maxBatchBytes of body. */
    private List<List<Item>> chunks(List<Item> items) {
        final out = new ArrayList<List<Item>>()
        List<Item> chunk = new ArrayList<Item>()
        final int empty = ENVELOPE_HEAD.length + ENVELOPE_TAIL.length
        long size = empty
        for (Item item : items) {
            long extra = item.data.length + (chunk.isEmpty() ? 0 : 1) // the separating comma
            if (!chunk.isEmpty() && (chunk.size() >= maxBatch || size + extra > maxBatchBytes)) {
                out.add(chunk)
                chunk = new ArrayList<Item>()
                size = empty
                extra = item.data.length
            }
            chunk.add(item)
            size += extra
        }
        if (!chunk.isEmpty()) {
            out.add(chunk)
        }
        return out
    }

    private void sendAll(List<Map<String, Object>> events, boolean late) {
        if (events.isEmpty()) {
            return
        }
        for (List<Item> chunk : chunks(encode(events))) {
            deliver(chunk, late, [SPLIT_POST_BUDGET] as int[])
        }
    }

    private static byte[] body(List<Item> items) {
        final out = new ByteArrayOutputStream()
        out.write(ENVELOPE_HEAD)
        for (int i = 0; i < items.size(); i++) {
            if (i > 0) {
                out.write(COMMA)
            }
            out.write(items.get(i).data)
        }
        out.write(ENVELOPE_TAIL)
        return out.toByteArray()
    }

    /** Send one batch; on a whole-batch rejection, halve it ({@code budget} is shared per batch). */
    private void deliver(List<Item> items, boolean late, int[] budget) {
        if (items.isEmpty()) {
            return
        }
        if (abandoning(late)) {
            abandoned.addAndGet(items.size())
            return
        }
        if (budget[0] <= 0) {
            log.warn("biffix ingest POST to ${where}: the route keeps rejecting this batch; dropped "
                + "${items.size()} event(s) without isolating further")
            return
        }
        budget[0]--
        final result = sendWithRetries(body(items), late)
        if (result.outcome == Outcome.SENT) {
            for (Refused r : refused(result.detail)) {
                if (r.index < items.size()) {
                    final event = items[r.index].event
                    log.warn("biffix ingest POST to ${where} refused one ${event.get('kind')} event "
                        + "(${event.get('task_key')}), dropped: ${scrub(r.reason)}")
                }
            }
            return
        }
        if (result.outcome == Outcome.SPLIT) {
            if (items.size() > 1) {
                // sendLock is released by now; each half takes it again.
                final int mid = Math.floorDiv(items.size(), 2)
                deliver(items.subList(0, mid), late, budget)
                deliver(items.subList(mid, items.size()), late, budget)
                return
            }
            final event = items[0].event
            log.warn("biffix ingest POST to ${where} rejected one ${event.get('kind')} event "
                + "(${event.get('task_key')}), dropped: ${result.detail}")
            return
        }
        // Never break the run on ingest failure: the events file replay and EventBridge are the backstop.
        log.warn("biffix ingest POST of ${items.size()} event(s) to ${where} failed; dropped them (the events "
            + "file replay re-sends them): ${result.detail}")
    }

    private Result sendWithRetries(byte[] body, boolean late) {
        final int attempts = late ? 1 : MAX_ATTEMPTS
        Result result = new Result(Outcome.TRANSIENT, 'not attempted')
        synchronized (sendLock) {
            for (int attempt = 1; attempt <= attempts; attempt++) {
                final Long rem = late ? null : remainingMs()
                if (rem != null && rem <= 0L) {
                    return new Result(Outcome.TRANSIENT, 'close deadline passed')
                }
                final long timeout = rem == null ? timeoutMs : Math.min(timeoutMs, rem)
                result = postOnce(body, timeout)
                if (result.outcome != Outcome.TRANSIENT) {
                    if (result.outcome == Outcome.SENT) {
                        down = false
                    }
                    return result
                }
                if (attempt < attempts) {
                    final long backoff = result.retryAfterMs > 0L
                        ? Math.min(result.retryAfterMs, MAX_RETRY_AFTER_MS)
                        : retryBackoffMs * (1L << (attempt - 1))
                    if (!pause(backoff, late)) {
                        break
                    }
                }
            }
        }
        // Retries exhausted on transient failures: ingest looks unreachable.
        down = true
        return result
    }

    /**
     * Sleep {@code ms}, in short naps so a close deadline set meanwhile cuts it short. False when
     * interrupted (the interrupt flag is kept) or the deadline passed.
     */
    private boolean pause(long ms, boolean late) {
        final long until = System.nanoTime() + ms * 1_000_000L
        while (true) {
            final long left = Math.floorDiv(until - System.nanoTime(), 1_000_000L)
            final Long rem = late ? null : remainingMs()
            if (rem != null && rem <= 0L) {
                return false
            }
            if (left <= 0L) {
                return true
            }
            try {
                Thread.sleep(Math.min(left, Math.min(100L, rem == null ? 100L : rem)))
            } catch (InterruptedException ignored) {
                Thread.currentThread().interrupt()
                return false
            }
        }
    }

    /** One event a 2xx answer's {@code refused} names: its index in the batch, and why. */
    @PackageScope
    static class Refused {
        final int index
        final String reason

        Refused(int index, String reason) {
            this.index = index
            this.reason = reason
        }
    }

    /**
     * The events a 2xx answer (the ingest summary) refuses; none when the answer is not a summary
     * (an older route's empty body), and an entry without a whole-number index is skipped.
     */
    @PackageScope
    static List<Refused> refused(String answer) {
        final List<Refused> out = []
        final Object parsed
        try {
            parsed = answer ? new JsonSlurper().parseText(answer) : null
        } catch (Exception ignored) {
            return out
        }
        if (!(parsed instanceof Map)) {
            return out
        }
        final Object entries = ((Map) parsed).get('refused')
        if (!(entries instanceof List)) {
            return out
        }
        for (Object entry : (List) entries) {
            if (!(entry instanceof Map)) {
                continue
            }
            final Object index = ((Map) entry).get('index')
            if (index instanceof Integer && ((Integer) index) >= 0) {
                out << new Refused((Integer) index, String.valueOf(((Map) entry).get('reason') ?: ''))
            }
        }
        return out
    }

    /**
     * One POST: its outcome and, for a failure, a short description (never the token); for a
     * success, the answer's body.
     */
    private Result postOnce(byte[] body, long timeout) {
        final HttpRequest req
        try {
            req = HttpRequest.newBuilder()
                .uri(URI.create(url))
                .timeout(Duration.ofMillis(Math.max(1L, timeout)))
                .header('Content-Type', 'application/json')
                .header('Authorization', "Bearer ${token}".toString())
                .POST(HttpRequest.BodyPublishers.ofByteArray(body))
                .build()
        } catch (IllegalArgumentException e) {
            // A URL or token that cannot form a request (no scheme, a newline in the token). Its
            // message can quote the header, so only the type is logged.
            markBroken("the ingest URL or token is invalid (${e.class.simpleName})")
            return new Result(Outcome.FINAL, e.class.simpleName)
        }
        try {
            final resp = client.send(req, HttpResponse.BodyHandlers.ofString())
            final status = resp.statusCode()
            if (status >= 200 && status < 300) {
                return new Result(Outcome.SENT, resp.body() ?: '')
            }
            final detail = "HTTP ${status}: ${excerpt(resp.body())}".toString()
            if (status == 408 || status == 429 || status >= 500) {
                return new Result(Outcome.TRANSIENT, detail, retryAfterMs(resp))
            }
            if (status == 400 || status == 413) {
                return new Result(Outcome.SPLIT, detail)
            }
            if (status == 401) {
                // The token is invalid or expired: no later send can succeed.
                markBroken("ingest refused the capability token (${detail})")
            }
            return new Result(Outcome.FINAL, detail)
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt()
            return new Result(Outcome.TRANSIENT, 'interrupted')
        } catch (Exception e) {
            // Connection errors, timeouts, protocol errors. The JDK's messages never carry headers.
            return new Result(Outcome.TRANSIENT, "${e.class.simpleName}: ${scrub(e.message)}".toString())
        }
    }

    private void markBroken(String why) {
        if (!broken) {
            broken = true
            log.warn("biffix ingest disabled: ${why}; later events go only to the events file")
        }
    }

    /** The response's Retry-After in ms (delta-seconds form only), else 0. */
    private static long retryAfterMs(HttpResponse<?> resp) {
        try {
            final v = resp.headers().firstValue('Retry-After').orElse(null)
            if (v != null && v.trim().isLong()) {
                return Math.max(0L, v.trim().toLong() * 1000L)
            }
        } catch (Exception ignored) {
            // Absent or malformed: the doubling backoff applies.
        }
        return 0L
    }

    /** The first bytes of an error response, which name what the route refused. */
    private String excerpt(String text) {
        final String t = text == null ? '' : text.take(200).split(/\s+/).findAll { it }.join(' ')
        return scrub(t)
    }

    private String scrub(String text) {
        if (text == null) {
            return ''
        }
        return token ? text.replace(token, '<token>') : text
    }
}
