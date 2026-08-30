package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Host JVM tests for [ZoomPolicy] — how the reader gets from one zoom to the next. */
class ZoomPolicyTest {

    private val max = ReaderActivity.MAX_ZOOM_UI
    private val step = ReaderActivity.ZOOM_STEP

    @Test
    fun steppingMultipliesByTheFactor() {
        assertEquals(2.8f, ZoomPolicy.stepped(2f, step, max), 0.001f)
        assertEquals(1.4285715f, ZoomPolicy.stepped(2f, 1f / step, max), 0.001f)
    }

    @Test
    fun steppingIsClampedToTheCeiling() {
        assertEquals(max, ZoomPolicy.stepped(max, step, max), 0.001f)
        assertEquals(max, ZoomPolicy.stepped(4.9f, 10f, max), 0.001f)
    }

    @Test
    fun steppingNeverGoesBelowFit() {
        assertEquals(1f, ZoomPolicy.stepped(1f, 1f / step, max), 0.001f)
        assertEquals(1f, ZoomPolicy.stepped(2f, 0.001f, max), 0.001f)
    }

    /**
     * The deadband, and why it exists: a pinch leaves an arbitrary float near 1. Without the snap,
     * `zoom > 1f` stays true on a page that looks exactly like fit, so the reader keeps drawing a
     * minimap and keeps routing edge taps as page turns instead of opening the menu.
     */
    @Test
    fun aZoomInsideTheDeadbandSnapsExactlyToFit() {
        for (z in listOf(1.0001f, 1.005f, ZoomPolicy.FIT_DEADBAND)) {
            val out = ZoomPolicy.stepped(z, 1f, max)
            assertEquals("z=$z", 1f, out, 0f)
            assertTrue("z=$z must read as fit", ZoomPolicy.isFit(out))
        }
    }

    @Test
    fun justOutsideTheDeadbandStaysZoomed() {
        val out = ZoomPolicy.stepped(1.02f, 1f, max)
        assertTrue(out > 1f)
        assertFalse(ZoomPolicy.isFit(out))
    }

    @Test
    fun steppingDownFromJustAboveTheDeadbandLandsOnFit() {
        // The realistic path out of zoom: repeated `−` presses must terminate at exactly 1f rather
        // than asymptotically approaching it.
        var z = 5f
        repeat(20) { z = ZoomPolicy.stepped(z, 1f / step, max) }
        assertEquals(1f, z, 0f)
    }

    // ---- double-tap toggle ----

    @Test
    fun doubleTapFromFitJumpsIn() {
        assertEquals(2f, ZoomPolicy.doubleTapTarget(1f, 2f, max), 0.001f)
    }

    @Test
    fun doubleTapWhileZoomedReturnsToFit() {
        for (z in listOf(1.5f, 2f, 5f)) {
            assertEquals("z=$z", 1f, ZoomPolicy.doubleTapTarget(z, 2f, max), 0.001f)
        }
    }

    @Test
    fun theDoubleTapToggleAlwaysUndoesItself() {
        val inZoom = ZoomPolicy.doubleTapTarget(1f, 2f, max)
        assertEquals(1f, ZoomPolicy.doubleTapTarget(inZoom, 2f, max), 0.001f)
    }

    @Test
    fun theDoubleTapTargetIsClampedToTheCeiling() {
        assertEquals(max, ZoomPolicy.doubleTapTarget(1f, 99f, max), 0.001f)
    }

    // ---- fit crossing ----

    @Test
    fun leavingFitIsOnlyTheCrossingOutward() {
        assertTrue(ZoomPolicy.leavingFit(1f, 2f))
        assertFalse("already zoomed", ZoomPolicy.leavingFit(2f, 3f))
        assertFalse("returning to fit", ZoomPolicy.leavingFit(2f, 1f))
        assertFalse("staying at fit", ZoomPolicy.leavingFit(1f, 1f))
    }

    @Test
    fun isFitTreatsExactlyOneAsFit() {
        assertTrue(ZoomPolicy.isFit(1f))
        assertTrue(ZoomPolicy.isFit(0.5f))
        assertFalse(ZoomPolicy.isFit(1.5f))
    }

    @Test
    fun aNoChangeIsDetectable() {
        assertTrue(ZoomPolicy.isNoChange(2f, 2f))
        assertFalse(ZoomPolicy.isNoChange(2f, 2.8f))
    }
}
