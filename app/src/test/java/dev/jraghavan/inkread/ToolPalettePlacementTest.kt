package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for the floating toolbar's placement maths (#200).
 *
 * The reported bug: a collapsed puck dragged to the top edge expanded past the edge, taking the grip
 * with it — and with the grip off screen there was no way to collapse the pill again, so the only
 * escape was to reopen the document. The drag clamped against the *collapsed* size, and nothing
 * re-clamped once the pill grew.
 *
 * A 1920x2560 panel with a 96px puck and a 660px expanded pill stands in for a Nomad below.
 */
class ToolPalettePlacementTest {

    private val hostW = 1920
    private val hostH = 2560
    private val puck = 96
    private val pill = 660

    /** The escape hatch this whole issue is about: the top of the pill must stay on screen. */
    private fun topOf(ty: Float, viewHeight: Int): Float = (hostH - viewHeight) / 2f + ty

    private fun bottomOf(ty: Float, viewHeight: Int): Float = topOf(ty, viewHeight) + viewHeight

    @Test
    fun aPuckDraggedToTheTopEdgeStaysOnScreenWhenItExpands() {
        // Dragged as far up as the collapsed puck is allowed to go.
        val parked = ToolPalette.clampY(-99_999f, hostH, puck)
        assertEquals("puck should sit flush with the top", 0f, topOf(parked, puck), 0.5f)

        // Expanding from there: without the fix the pill's top goes negative — off screen.
        val naive = topOf(parked, pill)
        assertTrue("fixture must reproduce the bug: top was $naive", naive < 0f)

        val fixed = ToolPalette.clampY(ToolPalette.anchorY(parked, puck, pill), hostH, pill)
        assertTrue("expanded pill must not run off the top: ${topOf(fixed, pill)}", topOf(fixed, pill) >= -0.5f)
        assertTrue(
            "expanded pill must not run off the bottom: ${bottomOf(fixed, pill)}",
            bottomOf(fixed, pill) <= hostH + 0.5f,
        )
    }

    @Test
    fun aPuckDraggedToTheBottomEdgeStaysOnScreenWhenItExpands() {
        val parked = ToolPalette.clampY(99_999f, hostH, puck)
        assertEquals("puck should sit flush with the bottom", hostH.toFloat(), bottomOf(parked, puck), 0.5f)

        val fixed = ToolPalette.clampY(ToolPalette.anchorY(parked, puck, pill), hostH, pill)
        assertTrue("ran off the top: ${topOf(fixed, pill)}", topOf(fixed, pill) >= -0.5f)
        assertTrue("ran off the bottom: ${bottomOf(fixed, pill)}", bottomOf(fixed, pill) <= hostH + 0.5f)
    }

    /**
     * Away from the edges the grip must not jump: it is what the finger just tapped, and moving it
     * out from under the finger is the second complaint in #200.
     */
    @Test
    fun awayFromTheEdgesTheGripKeepsItsPosition() {
        val parked = 0f // centred
        val topBefore = topOf(parked, puck)
        val after = ToolPalette.clampY(ToolPalette.anchorY(parked, puck, pill), hostH, pill)
        assertEquals("the pill's top should not move", topBefore, topOf(after, pill), 0.5f)
    }

    /** Collapsing is the same problem in reverse and must also keep the grip still. */
    @Test
    fun collapsingAlsoKeepsTheGripPosition() {
        val expanded = 200f
        val topBefore = topOf(expanded, pill)
        val after = ToolPalette.clampY(ToolPalette.anchorY(expanded, pill, puck), hostH, puck)
        assertEquals(topBefore, topOf(after, puck), 0.5f)
    }

    @Test
    fun horizontalOffsetOnlyEverMovesThePillLeftAndStaysInside() {
        assertEquals("cannot move right of its anchor", 0f, ToolPalette.clampX(500f, hostW, puck), 0.01f)
        assertEquals(-100f, ToolPalette.clampX(-100f, hostW, puck), 0.01f)
        assertEquals(
            "cannot pass the left edge",
            -(hostW - puck).toFloat(),
            ToolPalette.clampX(-99_999f, hostW, puck),
            0.01f,
        )
    }

    /**
     * Degenerate geometry must not produce a NaN or an inverted range — `coerceIn` throws when the
     * bounds cross, which would take the reader down while merely opening a toolbar.
     */
    @Test
    fun aViewLargerThanItsHostIsClampedRatherThanCrashing() {
        assertEquals("no slack: stays centred", 0f, ToolPalette.clampY(500f, 100, 4000), 0.01f)
        assertEquals(0f, ToolPalette.clampX(-500f, 100, 4000), 0.01f)
        // Zero-sized host (before the first layout pass) is the same story.
        assertEquals(0f, ToolPalette.clampY(10f, 0, 0), 0.01f)
        assertEquals(0f, ToolPalette.clampX(-10f, 0, 0), 0.01f)
    }

    // ── Remembering where the pill was parked (#200) ──────────────────────────────────────────────

    /** The pill's top edge in host coordinates, for an arbitrary host — the placement that matters. */
    private fun topIn(ty: Float, hostHeight: Int, viewHeight: Int): Float =
        (hostHeight - viewHeight) / 2f + ty

    /** The pill's left edge in host coordinates (the base anchor is END, so the offset is negative). */
    private fun leftIn(tx: Float, hostWidth: Int, viewWidth: Int): Float =
        (hostWidth - viewWidth) + tx

    /**
     * The everyday case: park the puck, close the document, reopen it on the same panel. The pill
     * must come back to the same pixel, or "remembering" is worse than not remembering.
     */
    @Test
    fun aParkedPositionComesBackToTheSamePlaceOnTheSamePanel() {
        val tx = -300f
        val ty = 200f

        val restoredX = ToolPalette.translationXFor(ToolPalette.fractionX(tx, hostW, puck), hostW, puck)
        val restoredY = ToolPalette.translationYFor(ToolPalette.fractionY(ty, hostH, puck), hostH, puck)

        assertEquals(tx, restoredX, 0.5f)
        assertEquals(ty, restoredY, 0.5f)
    }

    /**
     * The correction the reporter asked for in the same breath as the feature: "It should account
     * for wrong positions (keep into the limit of the screen)."
     *
     * A puck parked flush with the bottom in portrait describes a point that is *past* the bottom
     * of a landscape panel. Restoring the raw position would hang the grip off the edge — exactly
     * the unreachable-grip trap this issue was opened about, only now it would survive a restart.
     */
    @Test
    fun aPositionSavedInPortraitIsPulledBackOntoALandscapeScreen() {
        val parked = ToolPalette.clampY(99_999f, hostH, puck) // flush with the portrait bottom
        val fraction = ToolPalette.fractionY(parked, hostH, puck)

        val landscapeH = hostW // the panel turned on its side
        val naive = fraction * landscapeH - (landscapeH - puck) / 2f
        assertTrue(
            "fixture must describe an off-screen point: bottom was ${topIn(naive, landscapeH, puck) + puck}",
            topIn(naive, landscapeH, puck) + puck > landscapeH,
        )

        val restored = ToolPalette.translationYFor(fraction, landscapeH, puck)
        assertTrue("ran off the top: ${topIn(restored, landscapeH, puck)}", topIn(restored, landscapeH, puck) >= -0.5f)
        assertTrue(
            "ran off the bottom: ${topIn(restored, landscapeH, puck) + puck}",
            topIn(restored, landscapeH, puck) + puck <= landscapeH + 0.5f,
        )
    }

    /** The same story sideways: a left-flush park stays left-flush on a wider panel. */
    @Test
    fun aHorizontalParkIsRestoredAgainstTheNewWidth() {
        val parked = ToolPalette.clampX(-99_999f, hostW, puck) // flush with the left edge
        val fraction = ToolPalette.fractionX(parked, hostW, puck)

        val widerW = hostH
        val restored = ToolPalette.translationXFor(fraction, widerW, puck)
        assertEquals("should still sit flush left", 0f, leftIn(restored, widerW, puck), 0.5f)
    }

    /**
     * The pill is saved at whatever size it happened to be and restored collapsed, because it always
     * opens collapsed. Anchoring on the top-left corner is what makes that a no-op rather than a
     * drift of half the height difference.
     */
    @Test
    fun aPositionSavedWhileExpandedRestoresTheCollapsedPuckToTheSameCorner() {
        val parked = ToolPalette.clampY(-99_999f, hostH, pill) // expanded, flush with the top
        val cornerBefore = topIn(parked, hostH, pill)

        val restored = ToolPalette.translationYFor(ToolPalette.fractionY(parked, hostH, pill), hostH, puck)

        assertEquals("the corner the grip sits in must not move", cornerBefore, topIn(restored, hostH, puck), 0.5f)
    }

    /**
     * Never parked, or a preference file that predates this feature: both arrive as NaN and must
     * open at the default dock rather than propagating a NaN into a layout pass.
     */
    @Test
    fun anUnsetPositionOpensAtTheDefaultDock() {
        assertEquals(0f, ToolPalette.translationXFor(Float.NaN, hostW, puck), 0.01f)
        assertEquals(0f, ToolPalette.translationYFor(Float.NaN, hostH, puck), 0.01f)
        assertEquals(0f, ToolPalette.translationXFor(Float.POSITIVE_INFINITY, hostW, puck), 0.01f)
        assertEquals(0f, ToolPalette.translationYFor(Float.NEGATIVE_INFINITY, hostH, puck), 0.01f)
    }

    /**
     * A stored pair decodes to a position only when both halves are real. The absent-preference
     * default is NaN, and letting one through would put the pill nowhere at all.
     */
    @Test
    fun onlyAFullyFinitePairDecodesToAPosition() {
        assertEquals(ToolPalette.Position(0.25f, 0.75f), ToolPalette.Position.of(0.25f, 0.75f))
        assertNull("never parked", ToolPalette.Position.of(Float.NaN, Float.NaN))
        assertNull("half-written pair", ToolPalette.Position.of(0.25f, Float.NaN))
        assertNull("half-written pair", ToolPalette.Position.of(Float.NaN, 0.75f))
        assertNull(ToolPalette.Position.of(Float.POSITIVE_INFINITY, 0.5f))
    }

    /** Before the first layout pass the host has no size; reading or restoring must stay finite. */
    @Test
    fun aZeroSizedHostNeitherDividesByZeroNorMoves() {
        assertEquals(0f, ToolPalette.fractionX(10f, 0, 0), 0.01f)
        assertEquals(0f, ToolPalette.fractionY(10f, 0, 0), 0.01f)
        assertEquals(0f, ToolPalette.translationXFor(0.5f, 0, 0), 0.01f)
        assertEquals(0f, ToolPalette.translationYFor(0.5f, 0, 0), 0.01f)
    }
}
