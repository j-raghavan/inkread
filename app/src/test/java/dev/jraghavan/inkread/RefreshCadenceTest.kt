package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for [RefreshCadence] (#99) — the pure page-turn counter that decides when to force
 * a periodic full EPD refresh. The `EinkAdapter` call it gates is device-side; the cadence logic is
 * fully host-testable.
 */
class RefreshCadenceTest {

    /** Count how many of [turns] page-turns fire a full refresh. */
    private fun fires(cadence: RefreshCadence, turns: Int): Int =
        (1..turns).count { cadence.onPageTurn() }

    @Test
    fun offNeverFires() {
        assertEquals(0, fires(RefreshCadence(0), 100))
        assertEquals(0, fires(RefreshCadence(-1), 100)) // any non-positive is Off
    }

    @Test
    fun everyPageFiresWhenIntervalIsOne() {
        assertEquals(100, fires(RefreshCadence(1), 100))
    }

    @Test
    fun firesOnExactlyEveryNthTurn() {
        val c = RefreshCadence(3)
        val pattern = (1..7).map { c.onPageTurn() }
        // turns 3 and 6 flash; 1,2,4,5,7 do not.
        assertEquals(listOf(false, false, true, false, false, true, false), pattern)
    }

    @Test
    fun resetRestartsTheCount() {
        val c = RefreshCadence(3)
        c.onPageTurn(); c.onPageTurn() // two toward the next flash
        c.reset()
        // After reset it takes a full 3 more turns to flash again.
        assertFalse(c.onPageTurn())
        assertFalse(c.onPageTurn())
        assertTrue(c.onPageTurn())
    }

    @Test
    fun changingIntervalTakesEffectAndOffResetsTheCounter() {
        val c = RefreshCadence(5)
        c.onPageTurn(); c.onPageTurn() // 2 toward 5
        c.interval = 0 // Off must not fire and should clear progress
        assertFalse(c.onPageTurn())
        c.interval = 2 // fresh count: two turns to flash
        assertFalse(c.onPageTurn())
        assertTrue(c.onPageTurn())
    }
}
