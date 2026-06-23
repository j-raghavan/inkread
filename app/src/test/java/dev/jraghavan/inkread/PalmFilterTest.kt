package dev.jraghavan.inkread

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for [PalmFilter] (RR19 palm rejection). Inputs use the real contact metrics
 * captured on a Supernote Nomad via the temporary PALMDIAG logging: a 2560px panel, resting palms
 * reporting touch-major ≈ 95–240px (most 140–240), pressure 0.5–0.93. No emulator can reproduce the
 * EMR pen, so this pure logic is the deterministic guard against palm-rejection regressions.
 */
class PalmFilterTest {

    // Production constants (kept in sync with ReaderActivity).
    private val rejectMs = 1000L
    private val frac = 0.06f
    private val panel = 2560

    private fun palm(
        stylus: Boolean = false,
        pointers: Int = 1,
        hovering: Boolean = false,
        stroke: Boolean = false,
        sinceStylus: Long = 10_000L, // long ago by default (no recent pen)
        majorPx: Float = 0f,
        viewH: Int = panel,
    ) = PalmFilter.isPalm(
        isStylusTool = stylus,
        pointerCount = pointers,
        penHovering = hovering,
        strokeInProgress = stroke,
        msSinceStylus = sinceStylus,
        palmRejectMs = rejectMs,
        touchMajorPx = majorPx,
        viewHeightPx = viewH,
        touchMajorFrac = frac,
    )

    @Test fun stylusIsNeverPalm() {
        assertFalse(palm(stylus = true, majorPx = 240f, pointers = 2))
    }

    @Test fun secondPointerWhileWritingIsPalm() {
        assertTrue(palm(pointers = 2))
    }

    @Test fun fingerWhilePenHoveringIsPalm() {
        // The "at the outset" case: pen in proximity, no touch yet, small contact — must still reject.
        assertTrue(palm(hovering = true, majorPx = 90f, sinceStylus = 10_000L))
    }

    @Test fun fingerWithinPenWindowIsPalm() {
        assertTrue(palm(sinceStylus = 500L))
    }

    @Test fun fingerMidStrokeIsPalm() {
        assertTrue(palm(stroke = true))
    }

    /** The core regression: a large resting palm at the OUTSET (no pen event, not hovering) is now
     *  rejected by size, where the old 0.12 (307px) gate let it through. */
    @Test fun largePalmAtOutsetIsRejectedBySize() {
        // Observed palm majors on device — all should be palms with the 0.06 gate (154px).
        for (major in listOf(159f, 175f, 221f, 237f, 240f)) {
            assertTrue("major=$major should be a palm", palm(majorPx = major))
        }
    }

    @Test fun oldThresholdWouldHaveLeakedThesePalms() {
        // Documents the fix: with the OLD 0.12 gate these device palms were NOT rejected by size.
        for (major in listOf(159f, 175f, 237f)) {
            val rejectedOld = PalmFilter.isPalm(
                isStylusTool = false, pointerCount = 1, penHovering = false, strokeInProgress = false,
                msSinceStylus = 10_000L, palmRejectMs = rejectMs, touchMajorPx = major,
                viewHeightPx = panel, touchMajorFrac = 0.12f,
            )
            assertFalse("major=$major was leaked by the old 0.12 gate", rejectedOld)
        }
    }

    @Test fun genuineFingertipTapIsAllowed() {
        // A real fingertip tap: small contact, no pen nearby, not hovering ⇒ NOT a palm (navigation
        // and lookups must still work).
        assertFalse(palm(majorPx = 60f, sinceStylus = 10_000L))
    }

    @Test fun unknownViewHeightSkipsSizeTest() {
        // Before the surface is sized, the size test must not fire on a huge major (no false palm).
        assertFalse(palm(majorPx = 9999f, viewH = 0, sinceStylus = 10_000L))
    }
}
