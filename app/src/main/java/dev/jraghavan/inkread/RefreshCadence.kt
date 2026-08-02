package dev.jraghavan.inkread

/**
 * Page-turn cadence for the periodic full (flashing) EPD refresh (#99). e-ink panels accumulate
 * ghosting across the partial refreshes that page-turns normally use; this decides when a page-turn
 * should force a FULL clear instead.
 *
 * Pure + host-testable — no Android or device types. The reader owns the actual `EinkAdapter` call;
 * this only counts. The counter is touched only on the engine thread that commits page-turns, but
 * [interval] can be changed from the settings UI thread, so it is `@Volatile`.
 */
class RefreshCadence(interval: Int) {

    /** Full-refresh interval in page-turns; 0 (or less) = Off. Updated when the setting changes. */
    @Volatile
    var interval: Int = interval

    private var sinceFull = 0

    /**
     * Record one committed page-turn and report whether THIS turn should trigger a full refresh.
     * When Off ([interval] <= 0) it never triggers and holds the counter at rest. Otherwise it fires
     * on every Nth turn and restarts the count.
     */
    fun onPageTurn(): Boolean {
        if (interval <= 0) {
            sinceFull = 0
            return false
        }
        sinceFull++
        if (sinceFull >= interval) {
            sinceFull = 0
            return true
        }
        return false
    }

    /** Restart the count — e.g. after a manual "Refresh now", so the next auto-flash is a full N away. */
    fun reset() {
        sinceFull = 0
    }
}
