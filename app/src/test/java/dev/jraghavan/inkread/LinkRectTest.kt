package dev.jraghavan.inkread

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for [LinkRect.contains] (#299). A link tap is hit-tested against a normalized rect
 * grown by a slop margin so small links stay tappable; the maths here is what decides whether a near
 * miss follows the link or falls through to a page turn, and it is not visible in a review.
 */
class LinkRectTest {

    private fun link(x0: Float, y0: Float, x1: Float, y1: Float) =
        LinkRect(x0, y0, x1, y1, targetPage = 1, uri = null)

    @Test
    fun exactHitNeedsNoPad() {
        val r = link(0.40f, 0.40f, 0.60f, 0.60f)
        assertTrue(r.contains(0.50f, 0.50f))
        assertTrue("edge is inclusive", r.contains(0.40f, 0.60f))
    }

    @Test
    fun aMissWithNoPadStaysAMiss() {
        val r = link(0.40f, 0.40f, 0.60f, 0.60f)
        assertFalse(r.contains(0.62f, 0.50f))
    }

    @Test
    fun padGrowsTheTargetOnEverySide() {
        val r = link(0.40f, 0.40f, 0.60f, 0.60f)
        val pad = 0.03f
        assertTrue("just right of x1", r.contains(0.62f, 0.50f, pad))
        assertTrue("just left of x0", r.contains(0.38f, 0.50f, pad))
        assertTrue("just below y1", r.contains(0.50f, 0.62f, pad))
        assertTrue("just above y0", r.contains(0.50f, 0.38f, pad))
    }

    @Test
    fun padHasAFiniteReach() {
        val r = link(0.40f, 0.40f, 0.60f, 0.60f)
        assertFalse("beyond the pad still misses", r.contains(0.66f, 0.50f, 0.03f))
    }
}
