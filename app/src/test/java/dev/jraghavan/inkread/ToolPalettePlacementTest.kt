package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
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
}
