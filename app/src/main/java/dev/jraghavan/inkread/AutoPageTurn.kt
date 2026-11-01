package dev.jraghavan.inkread

/**
 * Auto page-turn (#97): advance one page on a fixed interval for hands-free reading, until any
 * manual interaction or the end of the document stops it. Kotlin-only; it drives the reader's
 * EXISTING page-turn path rather than touching the core.
 *
 * Lives as its own small controller like [StylusInkController] / [BottomBarController] (SRP). The
 * timer is injected as a schedule/cancel pair rather than an `android.os.Handler`, so the whole
 * start / tick / stop / end-of-document state machine is host-testable on the JVM with no Android
 * framework; the reader wires those two lambdas to its main-thread handler. [Host.advanceOnePage]
 * reuses the coalesced page-turn path, which no-ops at the last page, so this controller stops
 * itself when there is no next page to turn to.
 *
 * It generates no touch events of its own (it calls the turn path directly), so the reader can stop
 * it on any manual touch without the auto-turn cancelling itself.
 */
class AutoPageTurn(
    private val host: Host,
    /** Post [run] to fire after [delayMs]; the reader backs this with its main-thread handler. */
    private val schedule: (delayMs: Long, run: Runnable) -> Unit,
    /** Drop a pending [run] posted by [schedule]; called with the same instance each time. */
    private val cancel: (run: Runnable) -> Unit,
) {
    interface Host {
        /** Current 0-based page, read on the main thread just before each scheduled turn. */
        val currentPage: Int

        /** Total pages, or 0 before a document is open. */
        val pageCount: Int

        /** Advance exactly one page via the reader's normal (coalesced, refreshing) turn path. */
        fun advanceOnePage()
    }

    /** Whether auto-turn is currently running. */
    var isRunning = false
        private set

    /** The interval between turns in whole seconds, clamped to [MIN_INTERVAL_SEC]..[MAX_INTERVAL_SEC]. */
    var intervalSec = DEFAULT_INTERVAL_SEC
        private set

    private val tick = Runnable { onTick() }

    /**
     * Start (or restart) auto-turn at [requestedSec] seconds. A no-op left stopped if there is no
     * next page to turn to (already at or past the last page).
     */
    fun start(requestedSec: Int) {
        intervalSec = clampInterval(requestedSec)
        cancel(tick)
        if (!hasNextPage()) {
            isRunning = false
            return
        }
        isRunning = true
        schedule(intervalSec * 1000L, tick)
    }

    /** Stop auto-turn and drop any pending turn. Safe to call when already stopped. */
    fun stop() {
        isRunning = false
        cancel(tick)
    }

    private fun onTick() {
        if (!isRunning) return
        // Stop rather than turn once the last page is reached; the next tick after the final turn
        // lands here and ends the run without a wasted no-op turn.
        if (!hasNextPage()) {
            stop()
            return
        }
        host.advanceOnePage()
        schedule(intervalSec * 1000L, tick)
    }

    private fun hasNextPage(): Boolean = host.pageCount > 0 && host.currentPage < host.pageCount - 1

    companion object {
        const val MIN_INTERVAL_SEC = 15
        const val MAX_INTERVAL_SEC = 90
        const val DEFAULT_INTERVAL_SEC = 30

        /** Interval choices offered in the reader's chooser, in seconds (the issue's 15 to 90 range). */
        val PRESETS = intArrayOf(15, 30, 45, 60, 90)

        /** Clamp a requested interval to the supported range. */
        fun clampInterval(sec: Int): Int = sec.coerceIn(MIN_INTERVAL_SEC, MAX_INTERVAL_SEC)
    }
}
