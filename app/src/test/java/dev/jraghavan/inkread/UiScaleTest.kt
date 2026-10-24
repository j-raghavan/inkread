package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for the menu/chrome scale math (#133 / #281) — the pure `DisplayPrefs` and
 * `ToolPalette` companion logic behind the Menu Size presets, the slider and the toolbar's sizes.
 * `Ink` can't run here (its object init touches `Resources.getSystem()`), so the testable contract
 * lives in those companions.
 */
class UiScaleTest {

    @Test
    fun defaultScaleIsThePresentSizing() {
        // 1.0 must be an exact preset and the default, so a fresh install renders at today's sizes.
        // Named by value, not index: adding a preset shifts every index but not what 1.0 matches.
        assertEquals(1.0f, DisplayPrefs.UI_SCALES[DisplayPrefs.uiScalePresetIndex(1.0f)], 0f)
        assertEquals(1.0, DisplayPrefs.sliderToUiScale(DisplayPrefs.uiScaleToSlider(1.0f)).toDouble(), 0.0)
    }

    @Test
    fun labelsAndScalesAlign() {
        assertEquals(DisplayPrefs.UI_SCALES.size, DisplayPrefs.UI_SCALE_LABELS.size)
    }

    /** #281: every preset must sit exactly on a slider position, or a preset tap would move the
     *  slider to a neighbouring size and the drag could never land back on the preset. */
    @Test
    fun everyPresetIsASliderPosition() {
        DisplayPrefs.UI_SCALES.forEachIndexed { i, scale ->
            val back = DisplayPrefs.sliderToUiScale(DisplayPrefs.uiScaleToSlider(scale))
            assertEquals("preset $scale", scale.toDouble(), back.toDouble(), 0.0)
            assertEquals(i, DisplayPrefs.uiScalePresetIndex(back))
        }
    }

    /** Every slider position round-trips, and the ends are the range's ends. */
    @Test
    fun sliderPositionsRoundTrip() {
        for (pos in 0..DisplayPrefs.UI_SCALE_SLIDER_MAX) {
            assertEquals(pos, DisplayPrefs.uiScaleToSlider(DisplayPrefs.sliderToUiScale(pos)))
        }
        assertEquals(DisplayPrefs.UI_SCALE_MIN, DisplayPrefs.sliderToUiScale(0), 0f)
        assertEquals(DisplayPrefs.UI_SCALE_MAX, DisplayPrefs.sliderToUiScale(DisplayPrefs.UI_SCALE_SLIDER_MAX), 0f)
        // Out of range clamps rather than running off the track.
        assertEquals(0, DisplayPrefs.uiScaleToSlider(0.1f))
        assertEquals(DisplayPrefs.UI_SCALE_SLIDER_MAX, DisplayPrefs.uiScaleToSlider(9f))
    }

    /** Between two presets the control highlights none and the readout says the percentage. */
    @Test
    fun aSizeBetweenPresetsLightsNoPreset() {
        assertEquals(-1, DisplayPrefs.uiScalePresetIndex(0.8f))
        assertEquals("80%", DisplayPrefs.uiScaleLabel(0.8f))
        assertEquals("XS", DisplayPrefs.uiScaleLabel(0.7f))
        assertEquals("M", DisplayPrefs.uiScaleLabel(1.0f))
    }

    /** The slider spans exactly XS to 2XL: its ends are the end presets. */
    @Test
    fun theSliderRunsFromXsTo2xl() {
        assertEquals(DisplayPrefs.UI_SCALES.first(), DisplayPrefs.UI_SCALE_MIN, 0f)
        assertEquals(DisplayPrefs.UI_SCALES.last(), DisplayPrefs.UI_SCALE_MAX, 0f)
    }

    /** A size saved by the old XS (0.8) or S (0.9) moves to that preset's new value; anything else,
     *  including a deliberate slider position next to them, is kept. */
    @Test
    fun theOldXsAndSMoveToTheirNewPresets() {
        assertEquals(DisplayPrefs.UI_SCALES[0], DisplayPrefs.migratedUiScale(0.8f), 0f)
        assertEquals(DisplayPrefs.UI_SCALES[1], DisplayPrefs.migratedUiScale(0.9f), 0f)
        for (kept in floatArrayOf(0.75f, 0.85f, 0.95f, 1.0f, 1.15f, 1.5f)) {
            assertEquals(kept, DisplayPrefs.migratedUiScale(kept), 0f)
        }
    }

    @Test
    fun textAndUiScaleIndicesAreIndependent() {
        assertEquals(1.0, DisplayPrefs.TEXT_SCALES[DisplayPrefs.nearestScaleIndex(1.0f)].toDouble(), 0.0)
        assertEquals(3.0, DisplayPrefs.TEXT_SCALES[DisplayPrefs.nearestScaleIndex(9f)].toDouble(), 0.0) // TEXT max 3.0
    }

    /** Every size the slider can reach, smallest first. */
    private val sliderScales = (0..DisplayPrefs.UI_SCALE_SLIDER_MAX).map { DisplayPrefs.sliderToUiScale(it) }

    /**
     * #200. The reader asked for smaller icons "but with some margin to keep some distance between
     * button for ease of activation". Uniform scaling cannot do that — it preserves every ratio, so
     * the buttons stay exactly as crowded. A smaller Menu Size must leave proportionally MORE
     * clearance around each icon, not the same — at every slider position, not just the presets.
     *
     * This is the assertion that fails against a uniform-scaling implementation.
     */
    @Test
    fun aSmallerMenuSizeWidensTheClearanceAroundEachIcon() {
        val clearance = sliderScales.map { scale ->
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

    /** Menu Size M is the design button: 32dp in 60dp, before the panel scales it. */
    @Test
    fun theDefaultMenuSizeReproducesTheOriginalButton() {
        assertEquals(32.0, ToolPalette.glyphDp(1.0f).toDouble(), 1e-4)
        assertEquals(60.0, ToolPalette.boxDp(1.0f).toDouble(), 1e-4)
    }

    /**
     * #281: the toolbar must really follow the setting. The old sizing held the spacing fixed and
     * floored the button at 48dp, so XS came out at 53dp against M's 60dp. Assert the button shrinks
     * at every step down, and that XS is a genuinely compact size — this fails against that sizing.
     */
    @Test
    fun theToolbarShrinksWithEveryStepDown() {
        for (i in 1 until sliderScales.size) {
            assertTrue(
                "box must grow with the scale: ${sliderScales[i - 1]} -> ${sliderScales[i]}",
                ToolPalette.boxDp(sliderScales[i]) > ToolPalette.boxDp(sliderScales[i - 1]),
            )
        }
        val m = ToolPalette.boxDp(1.0f)
        assertTrue("XS must be at most 75% of M", ToolPalette.boxDp(DisplayPrefs.UI_SCALES[0]) <= 0.75f * m)
        assertTrue("S must be clearly smaller than M", ToolPalette.boxDp(DisplayPrefs.UI_SCALES[1]) <= 0.9f * m)
    }

    /** The glyph keeps at least the smallest size's own inset on each side, at every size. A
     *  smaller fixed inset than designed, or a box that stops growing, fails here. */
    @Test
    fun theGlyphFitsItsTargetAtEverySize() {
        val minSlack = 2 * ToolPalette.insetDp(DisplayPrefs.UI_SCALE_MIN)
        for (scale in sliderScales) {
            val slack = ToolPalette.boxDp(scale) - ToolPalette.glyphDp(scale)
            assertTrue("scale $scale leaves ${slack}dp around the glyph", slack >= minSlack - 1e-4f)
        }
    }

    /**
     * #281: the toolbar is a share of the panel, not a fixed dp. On the reference Manta the scale
     * is exactly 1, so its toolbar is unchanged; a Nomad's 749dp short side (1404px at density 300)
     * makes Menu Size M a 44dp button.
     */
    @Test
    fun theToolbarFollowsThePanel() {
        assertEquals(1f, ToolPalette.panelScale(ToolPalette.REFERENCE_SHORT_DP), 0f)
        val nomadShortDp = 1404f / (300f / 160f)
        assertEquals(60.0 * nomadShortDp / 1024.0, (ToolPalette.boxDp(1.0f) * ToolPalette.panelScale(nomadShortDp)).toDouble(), 1e-3)
    }
}
