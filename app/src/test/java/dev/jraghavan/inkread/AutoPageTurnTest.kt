package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for [AutoPageTurn] (#97). The schedule/cancel pair is faked so the timer runs
 * deterministically: [fire] simulates the handler firing the pending turn, with no real clock.
 */
class AutoPageTurnTest {

    /** A reader stand-in: a mutable current page that [advanceOnePage] moves forward, bounded. */
    private class FakeHost(var page: Int, val pages: Int) : AutoPageTurn.Host {
        var turns = 0
        override val currentPage get() = page
        override val pageCount get() = pages
        override fun advanceOnePage() {
            turns++
            if (page < pages - 1) page++ // mirror the real path: no-op at the last page
        }
    }

    /** Captures the one pending runnable and the delay it was scheduled with. */
    private class FakeTimer {
        var pending: Runnable? = null
        var lastDelayMs = 0L
        var scheduleCount = 0
        val schedule: (Long, Runnable) -> Unit = { delay, run ->
            lastDelayMs = delay; pending = run; scheduleCount++
        }
        val cancel: (Runnable) -> Unit = { pending = null }

        /** Fire the pending runnable the way the handler would. */
        fun fire() {
            val r = pending ?: error("nothing scheduled to fire")
            pending = null
            r.run()
        }
    }

    private fun setup(page: Int, pages: Int): Triple<AutoPageTurn, FakeHost, FakeTimer> {
        val host = FakeHost(page, pages)
        val timer = FakeTimer()
        return Triple(AutoPageTurn(host, timer.schedule, timer.cancel), host, timer)
    }

    @Test
    fun clampsIntervalToRange() {
        assertEquals(15, AutoPageTurn.clampInterval(1))
        assertEquals(15, AutoPageTurn.clampInterval(15))
        assertEquals(30, AutoPageTurn.clampInterval(30))
        assertEquals(90, AutoPageTurn.clampInterval(90))
        assertEquals(90, AutoPageTurn.clampInterval(1000))
    }

    @Test
    fun startSchedulesAtTheClampedIntervalInMillis() {
        val (auto, _, timer) = setup(page = 0, pages = 10)
        auto.start(45)
        assertTrue(auto.isRunning)
        assertEquals(45, auto.intervalSec)
        assertEquals(45_000L, timer.lastDelayMs)
        assertEquals(1, timer.scheduleCount)
    }

    @Test
    fun eachTickAdvancesExactlyOnePageAndReschedules() {
        val (auto, host, timer) = setup(page = 0, pages = 10)
        auto.start(30)
        timer.fire()
        assertEquals(1, host.turns)
        assertEquals(1, host.page)
        assertTrue(auto.isRunning)
        timer.fire()
        assertEquals(2, host.turns)
        assertEquals(2, host.page)
        assertTrue(auto.isRunning)
    }

    @Test
    fun stopsAtTheLastPageWithoutAWastedTurn() {
        val (auto, host, timer) = setup(page = 8, pages = 10) // one turn from the last page (index 9)
        auto.start(30)
        timer.fire() // page 8 -> 9 (last)
        assertEquals(9, host.page)
        assertEquals(1, host.turns)
        assertTrue(auto.isRunning)
        timer.fire() // now at the last page: stop, no further turn
        assertFalse(auto.isRunning)
        assertEquals(1, host.turns)
        assertNull(timer.pending)
    }

    @Test
    fun startingAtTheLastPageDoesNotRun() {
        val (auto, host, timer) = setup(page = 9, pages = 10)
        auto.start(30)
        assertFalse(auto.isRunning)
        assertNull(timer.pending)
        assertEquals(0, host.turns)
    }

    @Test
    fun startingWithNoDocumentDoesNotRun() {
        val (auto, _, timer) = setup(page = 0, pages = 0)
        auto.start(30)
        assertFalse(auto.isRunning)
        assertNull(timer.pending)
    }

    @Test
    fun stopCancelsThePendingTurn() {
        val (auto, host, timer) = setup(page = 0, pages = 10)
        auto.start(30)
        auto.stop()
        assertFalse(auto.isRunning)
        assertNull(timer.pending)
        // A late fire after stop is inert even if the handler already dequeued it.
        auto.start(30)
        val r = timer.pending!!
        auto.stop()
        r.run()
        assertEquals(0, host.turns)
    }

    @Test
    fun restartingUpdatesTheIntervalAndDropsTheOldTimer() {
        val (auto, _, timer) = setup(page = 0, pages = 10)
        auto.start(30)
        auto.start(60)
        assertEquals(60, auto.intervalSec)
        assertEquals(60_000L, timer.lastDelayMs)
        // Two schedules (one per start); the first pending turn was cancelled by the restart.
        assertEquals(2, timer.scheduleCount)
    }
}
