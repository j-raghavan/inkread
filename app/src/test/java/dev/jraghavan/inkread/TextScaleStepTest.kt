package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for stepping the reflow text size (#212).
 *
 * The bottom bar's Text +/− and a pinch both go through this, so a disagreement here would show up
 * as the button and the gesture doing different things — which is the defect #212 is about.
 */
class TextScaleStepTest {

    private val scales = DisplayPrefs.TEXT_SCALES

    @Test
    fun steppingMovesOnePresetAtATime() {
        val mid = scales[4] // 1.0x
        assertEquals(5, DisplayPrefs.steppedScaleIndex(mid, +1))
        assertEquals(3, DisplayPrefs.steppedScaleIndex(mid, -1))
        assertEquals(4, DisplayPrefs.steppedScaleIndex(mid, 0))
    }

    /**
     * At the ends the index must not move. The caller compares it against the current index to
     * decide whether to repaginate at all — wrapping, or overshooting into a clamp that still
     * differs, would trigger a full repagination that changes nothing on every tap.
     */
    @Test
    fun steppingPastEitherEndHoldsStill() {
        val largest = scales.last()
        val smallest = scales.first()
        assertEquals(scales.size - 1, DisplayPrefs.steppedScaleIndex(largest, +1))
        assertEquals(scales.size - 1, DisplayPrefs.steppedScaleIndex(largest, +99))
        assertEquals(0, DisplayPrefs.steppedScaleIndex(smallest, -1))
        assertEquals(0, DisplayPrefs.steppedScaleIndex(smallest, -99))
    }

    /** Stepping is defined for any stored scale, including one no longer in the preset list. */
    @Test
    fun anOffPresetScaleStepsFromItsNearestNeighbour() {
        val between = (scales[4] + scales[5]) / 2f
        val near = DisplayPrefs.nearestScaleIndex(between)
        assertEquals(near + 1, DisplayPrefs.steppedScaleIndex(between, +1))
        // A wildly out-of-range stored value still lands inside the table.
        assertTrue(DisplayPrefs.steppedScaleIndex(99f, +1) in scales.indices)
        assertTrue(DisplayPrefs.steppedScaleIndex(0.001f, -1) in scales.indices)
    }

    @Test
    fun theScaleTableIsAscendingSoAStepIsAlwaysInThatDirection() {
        for (i in 1 until scales.size) {
            assertTrue("TEXT_SCALES must ascend at $i", scales[i] > scales[i - 1])
        }
    }
}
