package io.github.nh13.biffix

import groovy.transform.CompileStatic
import groovy.transform.PackageScope

/**
 * The observer's buffer of publish refs, by task key (null ⇒ run-level). Publishes are grouped
 * so the immutable log gets one {@code task_outputs} event per task per flush, not one per file (a
 * large pipeline publishes thousands); the poster already batches the POSTs themselves.
 *
 * A flush is due once {@link #FLUSH_COUNT} refs are buffered, or once {@link #FLUSH_MILLIS} have
 * passed since the last flush. The count is checked on every add; the age is checked on every add
 * and by the observer's timer, so a quiet period after a burst still flushes within about twice
 * the interval. Every method takes the time from the caller, so the buffer itself is clock-free.
 * Thread-safe: publishes arrive from Nextflow's publish threads and the timer.
 */
@PackageScope
@CompileStatic
class PublishBuffer {

    /** Flush once this many refs are buffered, so one event stays bounded. */
    static final int FLUSH_COUNT = 200
    /** ... or once this long has passed since the last flush, so outputs appear promptly. */
    static final long FLUSH_MILLIS = 2000L

    /** A plain map under the monitor: the null key must be allowed, and the count must be atomic with the add. */
    private final Map<String, List<Map>> pending = new LinkedHashMap<String, List<Map>>()
    private int count = 0
    private long lastFlushMillis = 0L

    /** Buffer {@code ref} under {@code tkey}; the drained buffer when a flush is now due, else null. */
    synchronized Map<String, List<Map>> add(String tkey, Map ref, long now) {
        if (lastFlushMillis == 0L) {
            lastFlushMillis = now
        }
        List<Map> refs = pending.get(tkey)
        if (refs == null) {
            refs = new ArrayList<Map>()
            pending.put(tkey, refs)
        }
        refs.add(ref)
        count++
        return count >= FLUSH_COUNT || now - lastFlushMillis >= FLUSH_MILLIS ? drain(now) : null
    }

    /** The drained buffer when it holds refs and the interval has passed, else null (the timer's check). */
    synchronized Map<String, List<Map>> drainIfDue(long now) {
        return count > 0 && now - lastFlushMillis >= FLUSH_MILLIS ? drain(now) : null
    }

    /** Take (and reset) the whole buffer. */
    synchronized Map<String, List<Map>> drain(long now) {
        final drained = new LinkedHashMap<String, List<Map>>(pending)
        pending.clear()
        count = 0
        lastFlushMillis = now
        return drained
    }
}
