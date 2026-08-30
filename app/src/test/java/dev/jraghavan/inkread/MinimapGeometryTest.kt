package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for [MinimapController]'s geometry and pan maths.
 *
 * This code had no tests before it was a class — it was private to a 2,134-line Activity, so the
 * only way to exercise it was on a device whose screencap comes back black. Extracting it made the
 * arithmetic reachable, and the arithmetic is where a minimap goes wrong: [MinimapController.draw]
 * and [MinimapController.onTouch] both consult [MinimapController.geometry], so if the panel is
 * drawn anywhere other than where the hit-test looks, the `+` button silently stops working.
 *
 * `panFor` and `window` are the round trip that makes the drag feel right: dragging the thumbnail
 * to a point should put that point at the centre of the visible window.
 *
 * Pure float maths — no `Canvas`, no `Bitmap`, no device.
 */
class MinimapGeometryTest {

    private class FakeHost(
        override val viewW: Int = 1920,
        override val viewH: Int = 2560,
        override var zoom: Float = 2f,
    ) : MinimapController.Host {
        override var panX = 0f
        override var panY = 0f
        var applied = 0
        var zoomedBy = 0f
        override fun setPan(x: Float, y: Float) { panX = x; panY = y }
        override fun applyZoom() { applied++ }
        override fun zoomBy(factor: Float) { zoomedBy = factor }
        override fun throttledPreview(block: () -> Unit) = block()
        // 1dp = 1px keeps the expected values readable; density is not what these tests are about.
        override fun dpInt(v: Int) = v
    }

    private fun host(block: FakeHost.() -> Unit = {}) = FakeHost().apply(block)

    // ---- geometry ----

    @Test
    fun geometryIsNullUntilTheViewIsSized() {
        assertNull(MinimapController(FakeHost(viewW = 0, viewH = 0)).geometry())
        assertNull(MinimapController(FakeHost(viewW = 1920, viewH = 0)).geometry())
    }

    @Test
    fun theCardSitsInTheTopRightAtOneFifthOfTheViewport() {
        val h = host()
        val g = MinimapController(h).geometry()
        assertNotNull(g)
        g!!
        assertEquals(384f, g.tw, 0.001f) // 1920 / 5
        assertEquals(512f, g.th, 0.001f) // 2560 / 5
        assertEquals(1920f - 384f - 8f, g.left, 0.001f) // right edge, minus an 8dp margin
        assertEquals(8f, g.top, 0.001f)
    }

    /**
     * The two buttons must tile the thumbnail's width exactly, with no gap and no overlap: a gap is
     * a dead strip down the middle of the control, and an overlap makes one button steal the
     * other's taps.
     */
    @Test
    fun theMinusAndPlusButtonsTileTheCardWidth() {
        val g = MinimapController(host()).geometry()!!
        val y = g.buttonTop + g.buttonH / 2f
        // Every x across the row belongs to exactly one button: no dead strip, no overlap.
        for (frac in listOf(0.01f, 0.25f, 0.49f)) {
            assertTrue("x at $frac should be −", g.inMinus(g.left + g.tw * frac, y))
            assertFalse("x at $frac should not be +", g.inPlus(g.left + g.tw * frac, y))
        }
        for (frac in listOf(0.51f, 0.75f, 0.99f)) {
            assertTrue("x at $frac should be +", g.inPlus(g.left + g.tw * frac, y))
            assertFalse("x at $frac should not be −", g.inMinus(g.left + g.tw * frac, y))
        }
        // The split itself resolves one way only.
        assertTrue(g.inMinus(g.split, y))
        assertFalse(g.inPlus(g.split, y))
    }

    @Test
    fun theButtonRowSitsBelowTheThumbnailAndDoesNotOverlapIt() {
        val g = MinimapController(host())!!.geometry()!!
        assertTrue("button row must clear the thumb", g.buttonTop >= g.top + g.th)
        assertEquals(48f, g.buttonH, 0.001f)
        // A touch on the thumbnail is never also a touch on a button, and vice versa.
        val mid = g.left + g.tw / 2f
        assertTrue(g.inThumb(mid, g.top + g.th / 2f))
        assertFalse(g.inMinus(mid, g.top + g.th / 2f))
        assertFalse(g.inPlus(mid, g.top + g.th / 2f))
        assertFalse(g.inThumb(mid, g.buttonTop + g.buttonH / 2f))
    }

    // ---- pan maths ----

    @Test
    fun panIsZeroAtFit() {
        val m = MinimapController(host { zoom = 1f })
        val p = m.panFor(1f, 0.5f, 0.5f)
        assertEquals(0f, p[0], 0.001f)
        assertEquals(0f, p[1], 0.001f)
    }

    @Test
    fun draggingToTheCentreLeavesThePanCentred() {
        val m = MinimapController(host())
        val p = m.panFor(2f, 0.5f, 0.5f)
        assertEquals(0.5f, p[0], 0.001f)
        assertEquals(0.5f, p[1], 0.001f)
    }

    @Test
    fun panIsClampedAtTheEdges() {
        val m = MinimapController(host())
        val topLeft = m.panFor(2f, 0f, 0f)
        assertEquals(0f, topLeft[0], 0.001f)
        assertEquals(0f, topLeft[1], 0.001f)
        val bottomRight = m.panFor(2f, 1f, 1f)
        assertEquals(1f, bottomRight[0], 0.001f)
        assertEquals(1f, bottomRight[1], 0.001f)
        // Out-of-range input (a drag that left the thumb) clamps rather than running away.
        val over = m.panFor(2f, 5f, -5f)
        assertEquals(1f, over[0], 0.001f)
        assertEquals(0f, over[1], 0.001f)
    }

    /**
     * The round trip that makes the drag feel direct: pan to a point, and the visible window's
     * centre lands on that point. If these two drifted apart the thumbnail would show the window
     * somewhere other than where the reader just dragged it.
     *
     * Only where the point is reachable. At low zoom the window is most of the page — two thirds of
     * it at 1.5× — so its centre cannot get within half a window of an edge, and the pan clamps
     * instead. That clamp is correct: it is what stops the page scrolling past its own margin.
     */
    @Test
    fun theWindowCentresOnThePointThatWasDraggedTo() {
        val m = MinimapController(host())
        for (z in listOf(1.5f, 2f, 3f, 6f)) {
            val half = (1f / z) / 2f
            for (t in listOf(0.1f, 0.25f, 0.5f, 0.75f, 0.9f)) {
                val p = m.panFor(z, t, t)
                val w = m.window(z, p[0], p[1])
                val centre = w[0] + w[2] / 2f
                if (t in half..(1f - half)) {
                    assertEquals("zoom $z target $t: reachable, so centred", t, centre, 0.001f)
                } else {
                    // Unreachable: the window sits flush against the nearer edge.
                    val expected = if (t < half) half else 1f - half
                    assertEquals("zoom $z target $t: clamped to the edge", expected, centre, 0.001f)
                }
            }
        }
    }

    @Test
    fun theWindowShrinksAsZoomGrows() {
        val m = MinimapController(host())
        assertEquals(1f, m.window(1f, 0f, 0f)[2], 0.001f)
        assertEquals(0.5f, m.window(2f, 0f, 0f)[2], 0.001f)
        assertEquals(0.25f, m.window(4f, 0f, 0f)[2], 0.001f)
    }

    @Test
    fun theWindowStaysInsideTheThumbnail() {
        val m = MinimapController(host())
        for (z in listOf(1.5f, 2f, 4f)) {
            for (pan in listOf(0f, 0.5f, 1f)) {
                val w = m.window(z, pan, pan)
                assertTrue("zoom $z pan $pan: left edge", w[0] >= -0.001f)
                assertTrue("zoom $z pan $pan: right edge", w[0] + w[2] <= 1.001f)
            }
        }
    }
}
