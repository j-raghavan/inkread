package dev.jraghavan.inkread

import dev.jraghavan.inkread.ToolOptions.Side
import dev.jraghavan.inkread.ToolOptions.Companion.sideFor
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

    /** A vertical pill docked right — the default, and the one case the old code got right. */
    @Test
    fun a_pill_on_the_right_opens_the_column_to_its_left() {
        assertEquals(
            Side.LEFT_OF_PILL,
            sideFor(hasPill = true, horizontal = false, pillCenterX = 1830, hostWidth = hostW),
        )
    }

    /** The regression: docked left, the column used to stay on the right regardless. */
    @Test
    fun a_pill_on_the_left_opens_the_column_to_its_right() {
        assertEquals(
            Side.RIGHT_OF_PILL,
            sideFor(hasPill = true, horizontal = false, pillCenterX = 90, hostWidth = hostW),
        )
    }

    /** Docked across the top there is no side to open into, so it drops below the bar. */
    @Test
    fun a_bar_across_the_top_opens_the_column_beneath_it() {
        assertEquals(
            Side.BELOW_PILL,
            sideFor(hasPill = true, horizontal = true, pillCenterX = 960, hostWidth = hostW),
        )
    }

    /** Orientation wins over position: a top bar dropped below even when dragged to an edge. */
    @Test
    fun a_horizontal_bar_stays_below_wherever_it_is_dragged() {
        for (x in intArrayOf(0, 90, 960, 1830, hostW)) {
            assertEquals(
                "dragged to x=$x",
                Side.BELOW_PILL,
                sideFor(hasPill = true, horizontal = true, pillCenterX = x, hostWidth = hostW),
            )
        }
    }

    /** Before the palette has been laid out there is nothing to sit beside. */
    @Test
    fun with_no_palette_yet_the_column_falls_back_to_the_right_edge() {
        assertEquals(
            Side.RIGHT_EDGE,
            sideFor(hasPill = false, horizontal = false, pillCenterX = 0, hostWidth = hostW),
        )
    }

    /** Dead centre counts as the left half, so the column opens into the wider space to the right. */
    @Test
    fun a_pill_at_the_exact_centre_opens_to_its_right() {
        assertEquals(
            Side.RIGHT_OF_PILL,
            sideFor(hasPill = true, horizontal = false, pillCenterX = hostW / 2, hostWidth = hostW),
        )
    }

    /** The pill is draggable, so the side must track it across the midline, not stay where it docked. */
    @Test
    fun dragging_a_pill_across_the_midline_flips_the_side() {
        val justLeft = sideFor(true, horizontal = false, pillCenterX = hostW / 2 - 1, hostWidth = hostW)
        val justRight = sideFor(true, horizontal = false, pillCenterX = hostW / 2 + 1, hostWidth = hostW)
        assertEquals(Side.RIGHT_OF_PILL, justLeft)
        assertEquals(Side.LEFT_OF_PILL, justRight)
    }
}
