package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for the menu/chrome scale math (#133) — the pure `DisplayPrefs` companion logic
 * that maps a saved [DisplayPrefs.uiScale] to a segmented-control index. `Ink.sp` is a trivial
 * multiply and can't run here (Ink's object init touches `Resources.getSystem()`), so the testable
 * contract lives in `DisplayPrefs`.
 */
class UiScaleTest {

    @Test
    fun defaultScaleIsThePresentSizing() {
        // 1.0 must be an exact step and the default, so a fresh install renders at today's sizes.
        assertTrue("1.0 must be a UI_SCALES step", DisplayPrefs.UI_SCALES.any { it == 1.0f })
        assertEquals(1.0, DisplayPrefs.UI_SCALES[DisplayPrefs.nearestUiScaleIndex(1.0f)].toDouble(), 0.0)
    }

    @Test
    fun labelsAndScalesAlign() {
        assertEquals(DisplayPrefs.UI_SCALES.size, DisplayPrefs.UI_SCALE_LABELS.size)
    }

    @Test
    fun nearestUiScaleIndexSnapsToTheClosestStep() {
        assertEquals(0, DisplayPrefs.nearestUiScaleIndex(0.8f)) // exact min
        assertEquals(DisplayPrefs.UI_SCALES.size - 1, DisplayPrefs.nearestUiScaleIndex(1.5f)) // exact max
        // Off-grid inputs snap to the nearest step, and out-of-range clamps to an end.
        // Named by value, not index: adding a step shifts every index but not what 1.02 snaps to.
        assertEquals(
            1.0,
            DisplayPrefs.UI_SCALES[DisplayPrefs.nearestUiScaleIndex(1.02f)].toDouble(),
            0.0,
        )
        assertEquals(0, DisplayPrefs.nearestUiScaleIndex(0.1f)) // below range -> min
        assertEquals(DisplayPrefs.UI_SCALES.size - 1, DisplayPrefs.nearestUiScaleIndex(9f)) // above range -> max
    }

    @Test
    fun textAndUiScaleIndicesAreIndependent() {
        // The DRY nearestIndex refactor must not conflate the two scales' arrays.
        assertEquals(1.0, DisplayPrefs.TEXT_SCALES[DisplayPrefs.nearestScaleIndex(1.0f)].toDouble(), 0.0)
        assertEquals(3.0, DisplayPrefs.TEXT_SCALES[DisplayPrefs.nearestScaleIndex(9f)].toDouble(), 0.0) // TEXT max 3.0
    }

    /**
     * #200. The reader asked for smaller icons "but with some margin to keep some distance between
     * button for ease of activation". Uniform scaling cannot do that — it preserves every ratio, so
     * the buttons stay exactly as crowded. Only the glyph scales, so a smaller Menu Size must leave
     * proportionally MORE clearance around each icon, not the same.
     *
     * This is the assertion that fails against a uniform-scaling implementation.
     */
    @Test
    fun aSmallerMenuSizeWidensTheClearanceAroundEachIcon() {
        val steps = DisplayPrefs.UI_SCALES.sorted()
        val clearance = steps.map { scale ->
            val box = ToolPalette.boxDp(scale)
            (box - ToolPalette.glyphDp(scale)).toDouble() / box
        }
        for (i in 1 until clearance.size) {
            assertTrue(
                "clearance share must fall as the scale grows: ${clearance[i - 1]} -> ${clearance[i]}",
                clearance[i] < clearance[i - 1],
            )
        }
    }

    /** Menu Size M must reproduce the toolbar exactly as it was, or every reader's pill moves. */
    @Test
    fun theDefaultMenuSizeReproducesTheOriginalButton() {
        assertEquals(32, ToolPalette.glyphDp(1.0f))
        assertEquals(60, ToolPalette.boxDp(1.0f))
    }

    /**
     * No offered step may take the touch target under the platform floor. Asserted across every
     * step rather than the smallest, so adding one cannot quietly slip beneath it.
     */
    @Test
    fun noMenuSizeTakesTheToolTargetBelowTheFloor() {
        for (scale in DisplayPrefs.UI_SCALES) {
            assertTrue(
                "scale $scale gives ${ToolPalette.boxDp(scale)}dp",
                ToolPalette.boxDp(scale) >= ToolPalette.TOOL_BOX_MIN,
            )
        }
    }

    /** The glyph must stay strictly inside its target at every step, with room to read as an icon. */
    @Test
    fun theGlyphFitsItsTargetAtEveryStep() {
        for (scale in DisplayPrefs.UI_SCALES) {
            val slack = ToolPalette.boxDp(scale) - ToolPalette.glyphDp(scale)
            assertTrue("scale $scale leaves ${slack}dp around the glyph", slack >= 2 * ToolPalette.TOOL_INSET)
        }
    }
}
