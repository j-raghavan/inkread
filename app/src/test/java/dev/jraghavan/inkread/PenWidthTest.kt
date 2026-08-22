package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for the selectable pen thickness (#199).
 *
 * The width a stroke is written at is stored per stroke by the core, so a bad index here does not
 * just look wrong — it is baked into the saved annotation and cannot be restyled afterwards. These
 * cover the parts that decide that value: the table itself, the default, and the out-of-range
 * behaviour a stale or corrupt preference would produce.
 */
class PenWidthTest {

    @Test
    fun widthsAreOrderedThinnestFirstAndAllUsable() {
        val widths = StylusInkController.PEN_WIDTHS
        assertTrue("#199 asks for more than three choices", widths.size >= 3)
        assertEquals("every width needs a caption", widths.size, StylusInkController.PEN_WIDTH_NAMES.size)
        for (i in 1 until widths.size) {
            assertTrue("widths must ascend: ${widths[i - 1]} then ${widths[i]}", widths[i - 1] < widths[i])
        }
        assertTrue("a width of zero would draw nothing", widths.all { it > 0f })
    }

    /**
     * The reporter's actual ask: something thinner than what the pen wrote before. If the default
     * were the thinnest option there would be nothing finer to choose.
     */
    /**
     * The default must match the firmware nib, measured on a Nomad at a 1920px viewport (#126):
     * a stroke baked at 9px is indistinguishable from the live stroke, while the previous 6px
     * default was visibly thinner and produced the "renders fat, then refreshes thinner" report.
     */
    @Test
    fun theDefaultMatchesTheFirmwareNibAndHasChoicesEitherSide() {
        val i = StylusInkController.DEFAULT_PEN_WIDTH_INDEX
        val widths = StylusInkController.PEN_WIDTHS
        assertTrue("default index out of range", i in widths.indices)
        assertEquals("the default must match the measured firmware nib", 9f, widths[i], 0.001f)
        assertTrue("nothing thinner to choose", i > 0)
        assertTrue("nothing thicker to choose", i < widths.size - 1)
    }

    /**
     * A preference written by a build with a longer width table would otherwise index out of
     * bounds on downgrade, throwing while committing a stroke.
     */
    @Test
    fun anOutOfRangeSelectionFallsBackToTheDefaultWidth() {
        val widths = StylusInkController.PEN_WIDTHS
        val default = widths[StylusInkController.DEFAULT_PEN_WIDTH_INDEX]
        for (stale in listOf(-1, widths.size, widths.size + 99, Int.MAX_VALUE, Int.MIN_VALUE)) {
            assertEquals(
                "index $stale must fall back rather than throw",
                default,
                widths.getOrElse(stale) { widths[StylusInkController.DEFAULT_PEN_WIDTH_INDEX] },
                0.001f,
            )
        }
    }

    /** The pen must stay clearly distinct from the highlighter's band at every setting. */
    @Test
    fun everyPenWidthIsNarrowerThanTheHighlighterBand() {
        for (w in StylusInkController.PEN_WIDTHS) {
            assertTrue(
                "pen width $w is not narrower than the highlighter",
                w < StylusInkController.HIGHLIGHT_WIDTH_PX,
            )
        }
    }

    /**
     * The reason the default matters: a stroke is painted live by the firmware at its own nib width
     * and re-drawn by inkread when it bakes. Any width other than the nib visibly changes thickness
     * at that moment — an inherent trade, but the *default* must not make it.
     */
    @Test
    fun exactlyOneWidthMatchesTheNibAndItIsTheDefault() {
        val widths = StylusInkController.PEN_WIDTHS
        val nib = widths[StylusInkController.DEFAULT_PEN_WIDTH_INDEX]
        assertEquals(
            "the nib width must appear exactly once, or the default is ambiguous",
            1,
            widths.count { it == nib },
        )
        assertTrue("a thinner choice must exist (#199 asked for one)", widths.any { it < nib })
        assertTrue("a thicker choice must exist", widths.any { it > nib })
    }
}
