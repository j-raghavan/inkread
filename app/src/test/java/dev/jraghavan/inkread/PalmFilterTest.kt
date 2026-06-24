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

    // ---- isPenActive: the pinch-zoom gate (suppress pinch while the writing hand rests). ----

    private fun penActive(
        hovering: Boolean = false,
        stroke: Boolean = false,
        sinceStylus: Long = 10_000L,
    ) = PalmFilter.isPenActive(
        penHovering = hovering,
        strokeInProgress = stroke,
        msSinceStylus = sinceStylus,
        palmRejectMs = rejectMs,
    )

    @Test fun pinchSuppressedWhilePenHovering() {
        assertTrue(penActive(hovering = true))
    }

    @Test fun pinchSuppressedMidStroke() {
        assertTrue(penActive(stroke = true))
    }

    @Test fun pinchSuppressedWithinPenWindow() {
        // Palm landing right after a pen event (the resting hand during writing) ⇒ pen is active.
        assertTrue(penActive(sinceStylus = 500L))
    }

    @Test fun deliberatePinchAllowedWhenPenIdle() {
        // Pen lifted away and quiet past the reject window ⇒ a two-finger pinch is intentional zoom.
        assertFalse(penActive(sinceStylus = 10_000L))
    }

    @Test fun penActiveAtExactRejectBoundaryIsActive() {
        // The `<=` boundary: a finger landing exactly PALM_REJECT_MS after the pen is still the
        // writing hand (pinch stays suppressed). Pins the off-by-one edge.
        assertTrue(penActive(sinceStylus = rejectMs))
    }

    @Test fun penIdleOneMsPastRejectBoundary() {
        // One ms beyond the window with no hover/stroke ⇒ pen idle, pinch allowed.
        assertFalse(penActive(sinceStylus = rejectMs + 1))
    }

    /** Refactor guard: extracting [PalmFilter.isPenActive] out of [PalmFilter.isPalm] must not drift
     *  the truth table — a small single-pointer finger within any pen-active window is still a palm. */
    @Test fun isPalmUnchangedByPenActiveExtraction() {
        assertTrue(palm(sinceStylus = 500L, majorPx = 60f))
        assertTrue(palm(hovering = true, majorPx = 60f))
        assertTrue(palm(stroke = true, majorPx = 60f))
        // Complement: no pen signals ⇒ a fingertip is NOT a palm (navigation still works).
        assertFalse(palm(sinceStylus = rejectMs + 1, majorPx = 60f))
    }

    // ---- isPinchPalm: the size-aware pinch gate (suppress a palm-heel "pinch" even when pen idle). ----

    private fun pinchPalm(
        penActive: Boolean = false,
        maxMajorPx: Float = 0f,
        viewH: Int = panel,
    ) = PalmFilter.isPinchPalm(
        penActive = penActive,
        maxTouchMajorPx = maxMajorPx,
        viewHeightPx = viewH,
        touchMajorFrac = frac,
    )

    @Test fun pinchSuppressedWhenPenActiveRegardlessOfSize() {
        // Pen active dominates: even tiny fingertip contacts are the writing hand → no zoom.
        assertTrue(pinchPalm(penActive = true, maxMajorPx = 40f))
    }

    @Test fun pinchSuppressedByPalmSizedContactWhilePenIdle() {
        // The #49 hole: pen idle (window lapsed, hover exited) but a palm-heel contact (≥154px on a
        // 2560px panel) must still suppress the pinch instead of zooming the page.
        for (major in listOf(159f, 175f, 221f, 240f)) {
            assertTrue("major=$major should suppress the pinch", pinchPalm(penActive = false, maxMajorPx = major))
        }
    }

    @Test fun genuineTwoFingerPinchAllowedWhenPenIdle() {
        // Two real fingertips (small majors), pen idle ⇒ a deliberate zoom is allowed through.
        assertFalse(pinchPalm(penActive = false, maxMajorPx = 60f))
    }

    @Test fun pinchPalmAtExactSizeBoundaryIsPalm() {
        // The `>=` size boundary: a contact exactly at frac*height is a palm. Pins the edge.
        assertTrue(pinchPalm(penActive = false, maxMajorPx = panel * frac))
    }

    @Test fun pinchAllowedJustBelowSizeBoundary() {
        // The asymmetric half of the boundary: one px under frac*height is NOT a palm, so a real
        // pinch isn't eaten. Guards against a regression flipping `>=` to `>` or shifting the gate up.
        assertFalse(pinchPalm(penActive = false, maxMajorPx = panel * frac - 1f))
    }

    @Test fun pinchPalmZeroMajorIsNotPalmWhilePenIdle() {
        // Floor: a device that reports no contact major (0) must fall back to penActive only — never
        // a palm by size — so a genuine pinch isn't blocked where touch-major is unreported.
        assertFalse(pinchPalm(penActive = false, maxMajorPx = 0f))
    }

    @Test fun pinchPalmUnknownViewHeightSkipsSizeTerm() {
        // Before the surface is sized, the size term must not fire on a huge major — only penActive
        // can suppress, so a genuine early pinch isn't wrongly eaten.
        assertFalse(pinchPalm(penActive = false, maxMajorPx = 9999f, viewH = 0))
        assertTrue(pinchPalm(penActive = true, maxMajorPx = 9999f, viewH = 0))
    }
}
