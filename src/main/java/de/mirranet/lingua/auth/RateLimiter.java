package de.mirranet.lingua.auth;

import java.util.Iterator;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;

/**
 * Simple brute-force protection: after {@code maxFailures} failed attempts within the window,
 * the key is blocked until the window has passed.
 */
public final class RateLimiter {

    private final int maxFailures;
    private final long windowMillis;
    private final Map<String, Entry> entries = new ConcurrentHashMap<>();

    private static final class Entry {
        int failures;
        long windowStart;
    }

    public RateLimiter(int maxFailures, long windowMillis) {
        this.maxFailures = maxFailures;
        this.windowMillis = windowMillis;
    }

    public boolean isBlocked(String key) {
        Entry e = entries.get(key);
        if (e == null) {
            return false;
        }
        synchronized (e) {
            if (System.currentTimeMillis() - e.windowStart > windowMillis) {
                entries.remove(key, e);
                return false;
            }
            return e.failures >= maxFailures;
        }
    }

    public void recordFailure(String key) {
        long now = System.currentTimeMillis();
        Entry e = entries.computeIfAbsent(key, k -> {
            Entry n = new Entry();
            n.windowStart = now;
            return n;
        });
        synchronized (e) {
            if (now - e.windowStart > windowMillis) {
                e.windowStart = now;
                e.failures = 0;
            }
            e.failures++;
        }
        if (entries.size() > 10_000) {
            cleanup();
        }
    }

    public void reset(String key) {
        entries.remove(key);
    }

    private void cleanup() {
        long now = System.currentTimeMillis();
        for (Iterator<Entry> it = entries.values().iterator(); it.hasNext(); ) {
            Entry e = it.next();
            if (now - e.windowStart > windowMillis) {
                it.remove();
            }
        }
    }
}
