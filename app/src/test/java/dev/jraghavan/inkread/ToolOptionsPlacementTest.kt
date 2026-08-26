package dev.jraghavan.inkread

import dev.jraghavan.inkread.ToolOptions.Companion.sideFor
import dev.jraghavan.inkread.ToolOptions.Side
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Host JVM tests for where the tool-options column opens (#200).
 *
 * The reported bug: the colour and thickness choices always appeared on the right, as though the
 * palette were still docked there. Moving the bar to the left edge or across the top left the
 * column stranded on the far side of the page from the tool that opened it.
 *
 * A 1920x2560 Manta panel stands in below.
 */
class ToolOptionsPlacementTest {

    private val hostW = 1920
    private val hostH = 2560

    private fun pill(
        left: Int,
        top: Int,
        right: Int,
        bottom: Int,
        horizontal: Boolean = false,
        dockedStart: Boolean = false,
    ) = ToolPalette.Placement(left, top, right, bottom, horizontal, dockedStart)

    /** A vertical pill, centred at [centerX], the width the expanded strip actually is. */
    private fun verticalPillAt(centerX: Int) = pill(centerX - 56, 900, centerX + 56, 1660)

    /** A horizontal bar, centred at [centerY], spanning from the left dock. */
    private fun horizontalBarAt(centerY: Int, dockedStart: Boolean = true) =
        pill(20, centerY - 68, 1040, centerY + 68, horizontal = true, dockedStart = dockedStart)

    @Test
    fun aPillOnTheRightOpensTheColumnToItsLeft() {
        assertEquals(Side.LEFT_OF_PILL, sideFor(verticalPillAt(1830), hostW, hostH))
    }

    /** The regression: docked left, the column used to stay on the right regardless. */
    @Test
    fun aPillOnTheLeftOpensTheColumnToItsRight() {
        assertEquals(Side.RIGHT_OF_PILL, sideFor(verticalPillAt(90), hostW, hostH))
    }

    /** The pill is draggable, so the side must track it across the midline, not stay where it docked. */
    @Test
    fun draggingAPillAcrossTheMidlineFlipsTheSide() {
        assertEquals(Side.RIGHT_OF_PILL, sideFor(verticalPillAt(hostW / 2 - 1), hostW, hostH))
        assertEquals(Side.LEFT_OF_PILL, sideFor(verticalPillAt(hostW / 2 + 1), hostW, hostH))
    }

    @Test
    fun aBarNearTheTopOpensTheColumnBeneathIt() {
        assertEquals(Side.BELOW_PILL, sideFor(horizontalBarAt(150), hostW, hostH))
    }

    /**
     * The bug an earlier version of this test enshrined: `horizontal` is an orientation, not a
     * position. The bar is dragged over the whole page, and assuming it stays near the top pushed
     * the options column off the bottom of the screen, where it was silently clipped.
     */
    @Test
    fun aBarDraggedDownThePageOpensTheColumnAboveIt() {
        assertEquals(Side.ABOVE_PILL, sideFor(horizontalBarAt(hostH - 200), hostW, hostH))
    }

    @Test
    fun aBarCrossingTheVerticalMidlineFlipsTheSide() {
        assertEquals(Side.BELOW_PILL, sideFor(horizontalBarAt(hostH / 2 - 1), hostW, hostH))
        assertEquals(Side.ABOVE_PILL, sideFor(horizontalBarAt(hostH / 2 + 1), hostW, hostH))
    }

    /** Orientation picks the axis; only position picks the direction along it. */
    @Test
    fun orientationDecidesTheAxisIndependentlyOfPosition() {
        for (x in intArrayOf(0, 480, 960, 1440, hostW)) {
            val bar = pill(x, 100, x + 400, 236, horizontal = true, dockedStart = true)
            assertEquals("bar at x=$x", Side.BELOW_PILL, sideFor(bar, hostW, hostH))
        }
        for (y in intArrayOf(0, 640, 1280, 1920, hostH)) {
            val column = pill(1700, y, 1812, y + 400, horizontal = false, dockedStart = false)
            assertEquals("pill at y=$y", Side.LEFT_OF_PILL, sideFor(column, hostW, hostH))
        }
    }
}
