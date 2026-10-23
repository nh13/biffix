package io.github.nh13.biffix

import com.sun.net.httpserver.HttpServer
import groovy.json.JsonSlurper
import spock.lang.Specification

import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/**
 * IngestPoster's wire contract and delivery rules — the counterpart of the Snakemake plugin's
 * test_poster.py: it must target {@code <url>/events} with a bearer header and wrap events in the
 * batch envelope ({@code {events: [...]}}), batch rather than POST per event, bound batches by count
 * and bytes, isolate a rejected event by halving, retry only transient failures, bound close() by a
 * deadline, keep bindings under buffer pressure, and no-op (never open a connection) when url or
 * token is missing. Driven against a real in-process JDK HTTP server so the actual request shaping
 * is exercised.
 */
class IngestPosterSpec extends Specification {

    HttpServer server
    ConcurrentLinkedQueue<Map<String, Object>> received

    def setup() {
        received = new ConcurrentLinkedQueue<>()
        server = HttpServer.create(new InetSocketAddress('127.0.0.1', 0), 0)
        server.createContext('/events') { exchange ->
            final body = exchange.requestBody.text
            received << [
                method: exchange.requestMethod,
                auth  : exchange.requestHeaders.getFirst('Authorization'),
                path  : exchange.requestURI.path,
                body  : new JsonSlurper().parseText(body),
            ]
            exchange.sendResponseHeaders(202, -1)
            exchange.close()
        }
        server.start()
    }

    /** Released at cleanup, so a handler that "never answers" frees its thread. */
    CountDownLatch release = new CountDownLatch(1)

    def cleanup() {
        release.countDown()
        server?.stop(0)
    }

    /**
     * Replace the /events route with {@code handler(exchange, body)}, which returns the status to
     * answer (and may record the body). Runs on a pool so a slow handler blocks no other request.
     */
    private void route(Closure<Integer> handler) {
        server.stop(0)
        server = HttpServer.create(new InetSocketAddress('127.0.0.1', 0), 0)
        server.executor = Executors.newCachedThreadPool { Runnable r ->
            final t = new Thread(r)
            t.daemon = true
            t
        }
        server.createContext('/events') { exchange ->
            final bytes = exchange.requestBody.bytes
            final int status = handler.call(exchange, bytes)
            exchange.sendResponseHeaders(status, -1)
            exchange.close()
        }
        server.start()
    }

    private void record(byte[] bytes) {
        received << [body: new JsonSlurper().parse(bytes), size: bytes.length]
    }

    /** A URL nothing listens on (a port bound, then closed): connections are refused. */
    private static String deadUrl() {
        final s = new ServerSocket(0, 1, InetAddress.getByName('127.0.0.1'))
        final port = s.localPort
        s.close()
        return "http://127.0.0.1:${port}"
    }

    private String baseUrl() {
        return "http://127.0.0.1:${server.address.port}"
    }

    def "post targets /events with a bearer header and the batch envelope"() {
        given:
        final poster = new IngestPoster(baseUrl(), 'tok-123')
        final event = [run_id: 'run-1', kind: 'task_created', task_key: 'align:sample=A'] as Map<String, Object>

        when:
        poster.post(event)
        poster.close()

        then:
        received.size() == 1
        with(received.poll()) {
            method == 'POST'
            auth == 'Bearer tok-123'
            path == '/events'
            // The backend expects IngestRequest { events: [...] }, not a bare event.
            body == [events: [event]]
        }
    }

    def "a trailing slash on the base url does not double up the /events path"() {
        given:
        final poster = new IngestPoster(baseUrl() + '/', 'tok')

        when:
        poster.post([kind: 'run_started', run_id: 'r'] as Map<String, Object>)
        poster.close()

        then:
        received.size() == 1
        received.poll().path == '/events'
    }

    private static List<Map<String, Object>> events(int n) {
        return (0..<n).collect { i -> [run_id: 'run-1', kind: 'task_created', task_key: "t${i}".toString()] as Map<String, Object> }
    }

    private List<List> batches() {
        return received.collect { (it.body as Map).events as List }
    }

    def "events are buffered and sent as one batch on close"() {
        given:
        def poster = new IngestPoster(baseUrl(), 'tok', 500, 60_000L, 0L)

        when:
        events(3).each { poster.post(it) }

        then: 'nothing is sent before a flush'
        received.isEmpty()

        when:
        poster.close()

        then:
        batches() == [events(3)]
    }

    def "batches never exceed the max and keep order"() {
        given:
        def poster = new IngestPoster(baseUrl(), 'tok', 4, 60_000L, 0L)

        when:
        events(10).each { poster.post(it) }
        poster.close()

        then:
        batches().every { it.size() <= 4 }
        batches().flatten() == events(10)
    }

    def "a full batch is sent without waiting for the interval"() {
        given:
        def poster = new IngestPoster(baseUrl(), 'tok', 5, 60_000L, 0L)

        when:
        events(5).each { poster.post(it) }
        final deadline = System.currentTimeMillis() + 5_000
        while (received.isEmpty() && System.currentTimeMillis() < deadline) {
            Thread.sleep(10)
        }

        then:
        batches() == [events(5)]

        cleanup:
        poster.close()
    }

    def "a partial batch is sent after the flush interval"() {
        given:
        def poster = new IngestPoster(baseUrl(), 'tok', 500, 50L, 0L)

        when:
        poster.post(events(1)[0])
        final deadline = System.currentTimeMillis() + 5_000
        while (received.isEmpty() && System.currentTimeMillis() < deadline) {
            Thread.sleep(10)
        }

        then:
        batches() == [events(1)]

        cleanup:
        poster.close()
    }

    def "a 5xx is retried and a lone event the route rejects is dropped, not retried"() {
        given: 'a route that fails the first request with the given status'
        server.removeContext('/events')
        int calls = 0
        server.createContext('/events') { exchange ->
            final body = exchange.requestBody.text
            calls++
            if (calls == 1) {
                exchange.sendResponseHeaders(status, -1)
            } else {
                received << [body: new JsonSlurper().parseText(body)]
                exchange.sendResponseHeaders(202, -1)
            }
            exchange.close()
        }
        def poster = new IngestPoster(baseUrl(), 'tok', 500, 60_000L, 0L)

        when:
        poster.post(events(1)[0])
        poster.close()

        then:
        calls == expectedCalls
        batches() == (expectedCalls == 2 ? [events(1)] : [])

        where:
        status | expectedCalls
        503    | 2
        429    | 2
        408    | 2
        400    | 1
        413    | 1
        403    | 1
    }

    def "events a 2xx refuses are dropped, not resent"() {
        given: 'a route that accepts every batch but refuses its third event'
        final calls = new AtomicInteger()
        server.removeContext('/events')
        server.createContext('/events') { exchange ->
            calls.incrementAndGet()
            record(exchange.requestBody.bytes)
            final answer = '{"appended":3,"duplicates":0,"refused":[{"index":2,"reason":"Batch job j is not a job of this run"}]}'.bytes
            exchange.sendResponseHeaders(202, answer.length)
            exchange.responseBody.write(answer)
            exchange.close()
        }
        def poster = new IngestPoster(baseUrl(), 'tok', 500, 60_000L, 0L)

        when:
        events(4).each { poster.post(it) }
        poster.close()

        then: 'one request: a refused event is not resent, since a resend is refused again'
        calls.get() == 1
        batches() == [events(4)]
    }

    def "a 2xx answer's refused events are read by index, and anything else refuses nothing"() {
        expect:
        IngestPoster.refused(answer).collect { [it.index, it.reason] } == want

        where:
        answer                                                         | want
        '{"appended":1,"duplicates":0,"refused":[{"index":1,"reason":"r"}]}' | [[1, 'r']]
        '{"appended":1,"duplicates":0,"refused":[]}'                   | []
        '{"appended":1,"duplicates":0}'                                | []
        ''                                                             | []
        'not json'                                                     | []
        '[1]'                                                          | []
        '{"refused":[{"index":-1},{"index":"2"},{"reason":"x"},3]}'    | []
    }

    def "a batch that keeps failing transiently gets exactly three attempts, then is dropped"() {
        given:
        final calls = new AtomicInteger()
        route { ex, bytes -> calls.incrementAndGet(); 503 }
        def poster = new IngestPoster(baseUrl(), 'tok', 500, 60_000L, 0L)

        when:
        poster.post(events(1)[0])
        poster.close()

        then: 'no exception, three attempts (MAX_ATTEMPTS), nothing delivered'
        noExceptionThrown()
        calls.get() == 3
        received.isEmpty()
    }

    def "a 429 with Retry-After waits that long before the retry"() {
        given:
        final times = new ConcurrentLinkedQueue<Long>()
        route { ex, bytes ->
            times << System.nanoTime()
            if (times.size() == 1) {
                ex.responseHeaders.add('Retry-After', '1')
                return 429
            }
            202
        }
        def poster = new IngestPoster(baseUrl(), 'tok', 500, 60_000L, 0L)

        when:
        poster.post(events(1)[0])
        poster.close()

        then:
        final t = times as List<Long>
        t.size() == 2
        (t[1] - t[0]) / 1_000_000L >= 900
    }

    def "an unreachable ingest never throws and close returns promptly"() {
        given:
        def poster = new IngestPoster(deadUrl(), 'tok', 2, 60_000L, 0L)

        when:
        final start = System.currentTimeMillis()
        events(6).each { poster.post(it) }
        poster.close()
        poster.post(events(1)[0])
        final took = System.currentTimeMillis() - start

        then: 'nothing threw (an exception fails the feature)'
        took < 10_000
    }

    def "close() is bounded by its deadline when ingest accepts but never answers"() {
        given:
        final calls = new AtomicInteger()
        route { ex, bytes -> calls.incrementAndGet(); release.await(60, TimeUnit.SECONDS); 202 }
        // 200 ms requests, a 1.5 s close budget; batches cut by bytes (~500 events each), and a count
        // bound the buffer never reaches, so nothing is sent before close().
        def poster = new IngestPoster(baseUrl(), 'tok', 5_000, 60_000L, 0L, 200L, 1_500L, 30_000)

        when:
        events(2000).each { poster.post(it) }
        final start = System.currentTimeMillis()
        poster.close()
        final took = System.currentTimeMillis() - start

        then: 'one batch pays its retries; the rest are abandoned rather than retried in turn'
        took < 1_500L + 2_000L
        calls.get() == 3
        poster.abandonedCount >= 1000
    }

    def "close() waits for the batches the flusher already took"() {
        given: 'a healthy but slow ingest'
        route { ex, bytes -> Thread.sleep(300); record(bytes); 202 }
        def poster = new IngestPoster(baseUrl(), 'tok', 10, 50L, 0L)

        when: 'the flusher takes the buffer, then close() comes while it is sending'
        events(40).each { poster.post(it) }
        Thread.sleep(100)
        poster.close()

        then: 'every event was delivered before close() returned'
        batches().flatten().size() == 40
        poster.abandonedCount == 0
    }

    def "a post after close does not wait on the network"() {
        given:
        route { ex, bytes -> release.await(10, TimeUnit.SECONDS); 202 }
        def poster = new IngestPoster(baseUrl(), 'tok', 500, 60_000L, 0L)
        poster.close()

        when:
        final start = System.currentTimeMillis()
        poster.post(events(1)[0])
        final took = System.currentTimeMillis() - start

        then:
        took < 500
    }

    def "a 413 or whole-batch 400 is halved until only the offending event is dropped"() {
        given: 'a route that rejects any batch holding the bad event'
        final calls = new AtomicInteger()
        route { ex, bytes ->
            calls.incrementAndGet()
            final evs = (new JsonSlurper().parse(bytes) as Map).events as List<Map>
            if (evs.any { it.task_key == 'bad' }) {
                return status
            }
            record(bytes)
            202
        }
        def poster = new IngestPoster(baseUrl(), 'tok', 500, 60_000L, 0L)
        final all = events(8)
        all[5] = [run_id: 'run-1', kind: 'task_created', task_key: 'bad'] as Map<String, Object>

        when:
        all.each { poster.post(it) }
        poster.close()

        then: 'every other event is delivered, in order, within a few requests'
        batches().flatten() == all.findAll { it.task_key != 'bad' }
        calls.get() <= 7

        where:
        status << [413, 400]
    }

    def "a route that rejects everything stops at the split budget"() {
        given:
        final calls = new AtomicInteger()
        route { ex, bytes -> calls.incrementAndGet(); 400 }
        def poster = new IngestPoster(baseUrl(), 'tok', 500, 60_000L, 0L)

        when:
        events(500).each { poster.post(it) }
        poster.close()

        then:
        calls.get() == IngestPoster.SPLIT_POST_BUDGET
    }

    def "batches never exceed the byte bound, and an event over the route's limit is dropped unsent"() {
        given:
        route { ex, bytes -> record(bytes); 202 }
        def poster = new IngestPoster(baseUrl(), 'tok', 500, 60_000L, 0L, 5_000L, 30_000L, 1_000)
        final pad = 'x' * 200
        final small = (0..<20).collect { i ->
            [run_id: 'run-1', kind: 'task_created', task_key: "t${i}".toString(), pad: pad] as Map<String, Object>
        }
        final huge = [run_id: 'run-1', kind: 'task_outputs', task_key: 'huge',
                      pad: 'y' * (IngestPoster.ROUTE_MAX_BODY_BYTES + 1)] as Map<String, Object>

        when:
        small.take(10).each { poster.post(it) }
        poster.post(huge)
        small.drop(10).each { poster.post(it) }
        poster.close()

        then:
        received.every { (it.size as int) <= 1_000 }
        batches().size() > 1
        batches().flatten() == small
    }

    def "an event that cannot be serialized is dropped and the flusher keeps delivering"() {
        given:
        route { ex, bytes -> record(bytes); 202 }
        def poster = new IngestPoster(baseUrl(), 'tok', 500, 50L, 0L)
        final Map<String, Object> cyclic = [run_id: 'run-1', kind: 'task_created', task_key: 'loop'] as Map<String, Object>
        cyclic.self = cyclic

        when:
        poster.post(cyclic)
        Thread.sleep(300)
        poster.post(events(1)[0])
        final deadline = System.currentTimeMillis() + 5_000
        while (received.isEmpty() && System.currentTimeMillis() < deadline) {
            Thread.sleep(10)
        }

        then: 'the later event is still delivered by the live flusher'
        batches() == [events(1)]

        cleanup:
        poster.close()
    }

    def "the events posted before close are the close drain's, not the late sender's"() {
        given: "a flusher still sending its batch when close() starts"
        final sending = new CountDownLatch(1)
        route { ex, bytes ->
            final keys = ((new JsonSlurper().parse(bytes) as Map).events as List<Map>)*.task_key
            if (keys == ['t0', 't1']) {
                sending.countDown()
                release.await(10, TimeUnit.SECONDS)
            }
            record(bytes)
            202
        }
        def poster = new IngestPoster(baseUrl(), 'tok', 2, 60_000L, 0L)
        final before = [run_id: 'run-1', kind: 'run_succeeded', task_key: 'b'] as Map<String, Object>

        when: 'an event before close, then one during it, which starts the late sender'
        events(2).each { poster.post(it) }
        sending.await(10, TimeUnit.SECONDS)
        poster.post(before)
        final closing = Thread.start { poster.close() }
        while (!poster.@closed) {
            Thread.sleep(10)
        }
        poster.post(events(4)[3])
        while (poster.@lateThread != null && !poster.@buffer.isEmpty()) {
            Thread.sleep(10)
        }
        release.countDown()
        closing.join(10_000)
        final deadline = System.currentTimeMillis() + 10_000
        while (received.size() < 3 && System.currentTimeMillis() < deadline) {
            Thread.sleep(10)
        }

        then: 'the late sender sent only the event posted after close'
        batches()*.collect { it.task_key }.toSet() == [['t0', 't1'], ['b'], ['t3']] as Set
    }

    def "a 401 stops every later send"() {
        given:
        final calls = new AtomicInteger()
        route { ex, bytes -> calls.incrementAndGet(); 401 }
        def poster = new IngestPoster(baseUrl(), 'tok', 500, 60_000L, 0L, 5_000L, 30_000L, 100)

        when: 'three events, one per batch (the byte bound), then a late one'
        events(3).each { poster.post(it) }
        poster.close()
        poster.post(events(1)[0])

        then:
        calls.get() == 1
        poster.abandonedCount == 3
    }

    def "a full buffer keeps task_submitted and drops the oldest descriptive events"() {
        given: 'a buffer the flusher never drains before close'
        route { ex, bytes -> record(bytes); 202 }
        def poster = new IngestPoster(baseUrl(), 'tok', 30_000, 600_000L, 0L)
        final binding = [run_id: 'run-1', kind: 'task_submitted', task_key: 'first', batch_job_id: 'j1'] as Map<String, Object>
        final created = events(IngestPoster.MAX_BUFFERED + 500)

        when:
        poster.post(binding)
        created.each { poster.post(it) }
        poster.close()

        then: 'the overflow trims to 90% once, dropping only the oldest task_created events'
        final delivered = batches().flatten()
        final excess = IngestPoster.MAX_BUFFERED + 1 - (int) (IngestPoster.MAX_BUFFERED * 0.9)
        poster.overflowDroppedCount == excess
        delivered[0] == binding
        delivered.drop(1) == created.drop(excess)
    }

    def "a full buffer of kept kinds alone stays bounded and keeps the newest"() {
        given: 'nothing expendable to drop: every event is a task_submitted binding'
        route { ex, bytes -> record(bytes); 202 }
        def poster = new IngestPoster(baseUrl(), 'tok', 30_000, 600_000L, 0L)
        final bindings = (0..<(IngestPoster.MAX_BUFFERED + 500)).collect { i ->
            [run_id: 'run-1', kind: 'task_submitted', task_key: "t${i}".toString(), batch_job_id: "j${i}".toString()] as Map<String, Object>
        }

        when:
        bindings.each { poster.post(it) }
        poster.close()

        then: 'the overflow trims to 90% once, dropping the oldest bindings'
        final excess = IngestPoster.MAX_BUFFERED + 1 - (int) (IngestPoster.MAX_BUFFERED * 0.9)
        poster.overflowDroppedCount == excess
        batches().flatten() == bindings.drop(excess)
    }

    def "close is idempotent and a late event is still sent"() {
        given:
        def poster = new IngestPoster(baseUrl(), 'tok', 500, 60_000L, 0L)

        when:
        poster.close()
        poster.close()
        poster.post(events(1)[0])
        // A late event goes out on a background thread.
        final deadline = System.currentTimeMillis() + 5_000
        while (received.isEmpty() && System.currentTimeMillis() < deadline) {
            Thread.sleep(10)
        }

        then:
        batches() == [events(1)]
    }

    def "post no-ops when the url or token is missing"() {
        when:
        [new IngestPoster(null, 'tok'), new IngestPoster(baseUrl(), null)].each {
            it.post([kind: 'run_started'] as Map<String, Object>)
            it.close()
        }

        then: 'no connection was made'
        received.isEmpty()
    }

    /**
     * biffix-core's {@code ingest_limits} is the one source of the route's limits and the poster
     * policy both plugins follow; {@code cargo xtask emit-schemas} writes it to
     * {@code schemas/ingest-limits.json}, read here from the repo (relative to the gradle test
     * working dir, plugins/nf-biffix, or the repo root). The counterpart of the Snakemake plugin's
     * test_ingest_limits.py.
     */
    def "the limits match the ones biffix-core defines"() {
        given:
        final candidates = ['../../schemas/ingest-limits.json', 'schemas/ingest-limits.json']
        final file = candidates.collect { new File(it) }.find { it.exists() }
        assert file != null: "ingest-limits.json not found from ${System.getProperty('user.dir')}"
        final limits = new JsonSlurper().parse(file) as Map
        final route = limits.route as Map
        final policy = limits.poster as Map

        expect: 'the route limits, and nothing else under route'
        route.keySet() == ['max_batch_events', 'max_body_bytes'] as Set
        IngestPoster.MAX_BATCH == route.max_batch_events
        IngestPoster.ROUTE_MAX_BODY_BYTES == route.max_body_bytes

        and: 'the poster policy, value for value'
        policy == [
            max_batch_bytes      : IngestPoster.MAX_BATCH_BYTES,
            max_buffered_events  : IngestPoster.MAX_BUFFERED,
            max_attempts         : IngestPoster.MAX_ATTEMPTS,
            request_timeout_ms   : IngestPoster.REQUEST_TIMEOUT_MS,
            flush_interval_ms    : IngestPoster.FLUSH_INTERVAL_MS,
            retry_backoff_ms     : IngestPoster.RETRY_BACKOFF_MS,
            max_retry_after_ms   : IngestPoster.MAX_RETRY_AFTER_MS,
            close_timeout_ms     : IngestPoster.CLOSE_TIMEOUT_MS,
            drop_warn_interval_ms: IngestPoster.DROP_WARN_INTERVAL_MS,
            split_post_budget    : IngestPoster.SPLIT_POST_BUDGET,
            max_outputs_per_event: BiffixObserver.TASK_OUTPUTS_CHUNK,
            keep_under_pressure  : policy.keep_under_pressure,
        ]
        (policy.keep_under_pressure as List).toSet() == IngestPoster.KEEP_UNDER_PRESSURE
        (policy.keep_under_pressure as List).size() == IngestPoster.KEEP_UNDER_PRESSURE.size()
    }

    def "the two-argument constructor applies the policy's defaults"() {
        when:
        def poster = new IngestPoster(null, null)

        then:
        poster.@maxBatch == IngestPoster.MAX_BATCH
        poster.@flushIntervalMs == IngestPoster.FLUSH_INTERVAL_MS
        poster.@retryBackoffMs == IngestPoster.RETRY_BACKOFF_MS
        poster.@timeoutMs == IngestPoster.REQUEST_TIMEOUT_MS
        poster.@closeTimeoutMs == IngestPoster.CLOSE_TIMEOUT_MS
        poster.@maxBatchBytes == IngestPoster.MAX_BATCH_BYTES
    }
}
