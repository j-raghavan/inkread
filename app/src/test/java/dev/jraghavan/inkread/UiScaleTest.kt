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
        assertEquals(0, DisplayPrefs.nearestUiScaleIndex(0.9f)) // exact min
        assertEquals(DisplayPrefs.UI_SCALES.size - 1, DisplayPrefs.nearestUiScaleIndex(1.5f)) // exact max
        // Off-grid inputs snap to the nearest step, and out-of-range clamps to an end.
        assertEquals(1, DisplayPrefs.nearestUiScaleIndex(1.02f)) // nearest 1.0
        assertEquals(0, DisplayPrefs.nearestUiScaleIndex(0.1f)) // below range -> min
        assertEquals(DisplayPrefs.UI_SCALES.size - 1, DisplayPrefs.nearestUiScaleIndex(9f)) // above range -> max
    }

    @Test
    fun textAndUiScaleIndicesAreIndependent() {
        // The DRY nearestIndex refactor must not conflate the two scales' arrays.
        assertEquals(1.0, DisplayPrefs.TEXT_SCALES[DisplayPrefs.nearestScaleIndex(1.0f)].toDouble(), 0.0)
        assertEquals(3.0, DisplayPrefs.TEXT_SCALES[DisplayPrefs.nearestScaleIndex(9f)].toDouble(), 0.0) // TEXT max 3.0
    }
}
