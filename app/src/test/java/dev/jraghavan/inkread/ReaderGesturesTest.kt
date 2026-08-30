package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for [ReaderGestures] — the rules that decide what a finger meant.
 *
 * These are the rules a reader *feels* rather than sees. A swipe threshold slightly too high makes
 * the reader seem unresponsive; slightly too low and resting a hand turns the page. Neither shows up
 * as a crash, and neither is visible in a code review, which is why they are pinned here with the
 * relationships that actually matter — the strict thresholds must stay strictly stricter, the tap
 * thirds must tile the width, and the swipe direction must match the way the page moves.
 */
class ReaderGesturesTest {

    private val w = 1920f
    private val h = 2560f

    // ---- tap zones ----

    @Test
    fun theThirdsTileTheWidthWithNoGapOrOverlap() {
        for (frac in listOf(0.01f, 0.32f)) {
            assertEquals("x=$frac", ReaderGestures.Zone.PREV, ReaderGestures.zoneFor(w * frac, w))
        }
        for (frac in listOf(0.34f, 0.5f, 0.66f)) {
            assertEquals("x=$frac", ReaderGestures.Zone.CENTRE, ReaderGestures.zoneFor(w * frac, w))
        }
        for (frac in listOf(0.68f, 0.99f)) {
            assertEquals("x=$frac", ReaderGestures.Zone.NEXT, ReaderGestures.zoneFor(w * frac, w))
        }
    }

    @Test
    fun theThirdBoundariesBelongToTheCentre() {
        assertEquals(ReaderGestures.Zone.CENTRE, ReaderGestures.zoneFor(w / 3f, w))
        assertEquals(ReaderGestures.Zone.CENTRE, ReaderGestures.zoneFor(2f * w / 3f, w))
    }

    /** Before layout the surface reports 0 wide; a tap must not be routed as a page turn. */
    @Test
    fun anUnsizedPanelRoutesEverythingToTheCentre() {
        assertEquals(ReaderGestures.Zone.CENTRE, ReaderGestures.zoneFor(0f, 0f))
        assertEquals(ReaderGestures.Zone.CENTRE, ReaderGestures.zoneFor(500f, 0f))
    }

    @Test
    fun everyPointOnThePanelBelongsToExactlyOneZone() {
        for (i in 0..200) {
            val x = w * i / 200f
            val zones = listOf(ReaderGestures.Zone.PREV, ReaderGestures.Zone.CENTRE, ReaderGestures.Zone.NEXT)
            assertTrue("x=$x", ReaderGestures.zoneFor(x, w) in zones)
        }
    }

    // ---- bookmark corner ----

    @Test
    fun theBookmarkCornerIsTopRightOnly() {
        assertTrue(ReaderGestures.isBookmarkCorner(w * 0.97f, h * 0.02f, w, h))
        assertFalse("top-left", ReaderGestures.isBookmarkCorner(w * 0.03f, h * 0.02f, w, h))
        assertFalse("bottom-right", ReaderGestures.isBookmarkCorner(w * 0.97f, h * 0.9f, w, h))
        assertFalse("centre", ReaderGestures.isBookmarkCorner(w * 0.5f, h * 0.5f, w, h))
    }

    @Test
    fun theBookmarkCornerNeedsASizedPanel() {
        assertFalse(ReaderGestures.isBookmarkCorner(100f, 10f, 0f, h))
        assertFalse(ReaderGestures.isBookmarkCorner(100f, 10f, w, 0f))
    }

    /** The corner is defined as panel fractions, so it must land in the same place on both panels. */
    @Test
    fun theBookmarkCornerScalesWithThePanel() {
        for ((pw, ph) in listOf(1920f to 2560f, 1404f to 1872f)) {
            assertTrue(ReaderGestures.isBookmarkCorner(pw * 0.97f, ph * 0.02f, pw, ph))
            assertFalse(ReaderGestures.isBookmarkCorner(pw * 0.5f, ph * 0.5f, pw, ph))
        }
    }

    // ---- swipes ----

    @Test
    fun swipingLeftGoesForwardAndRightGoesBack() {
        assertEquals(1, ReaderGestures.swipeDelta(dx = -400f, dy = 0f, viewW = w))
        assertEquals(-1, ReaderGestures.swipeDelta(dx = 400f, dy = 0f, viewW = w))
    }

    @Test
    fun aShortTravelIsNotASwipe() {
        assertNull(ReaderGestures.swipeDelta(dx = -50f, dy = 0f, viewW = w))
        assertNull(ReaderGestures.swipeDelta(dx = 0f, dy = 0f, viewW = w))
    }

    @Test
    fun aMostlyVerticalTravelIsNotASwipe() {
        // Long enough horizontally, but the vertical component dominates.
        assertNull(ReaderGestures.swipeDelta(dx = -300f, dy = -600f, viewW = w))
    }

    @Test
    fun theSwipeFloorAppliesOnANarrowPanel() {
        // On a hypothetically tiny panel the fraction would allow an implausibly short flick; the
        // pixel floor is what stops a 20px twitch turning the page.
        assertNull(ReaderGestures.swipeDelta(dx = -80f, dy = 0f, viewW = 200f))
        assertEquals(1, ReaderGestures.swipeDelta(dx = -100f, dy = 0f, viewW = 200f))
    }

    // ---- the strict (palm-rescue) thresholds ----

    /**
     * The relationship that keeps #49 fixed: everything the strict rule accepts, the ordinary rule
     * accepts too — never the other way round. If that ever inverted, a resting palm would rescue
     * itself into a page turn while the reader was writing.
     */
    @Test
    fun theStrictRuleIsStrictlyStricter() {
        assertTrue(ReaderGestures.PALM_SWIPE_FRAC > ReaderGestures.SWIPE_FRAC)
        assertTrue(ReaderGestures.PALM_SWIPE_MIN_PX > ReaderGestures.SWIPE_MIN_PX)
        assertTrue(ReaderGestures.PALM_SWIPE_RATIO > ReaderGestures.SWIPE_RATIO)
        for (dx in -1000..1000 step 25) {
            for (dy in -600..600 step 50) {
                val strict = ReaderGestures.swipeDelta(dx.toFloat(), dy.toFloat(), w, strict = true)
                if (strict != null) {
                    assertEquals(
                        "dx=$dx dy=$dy accepted strictly must be accepted loosely",
                        strict,
                        ReaderGestures.swipeDelta(dx.toFloat(), dy.toFloat(), w),
                    )
                }
            }
        }
    }

    @Test
    fun aTravelBetweenTheTwoThresholdsIsAnOrdinarySwipeButNotAPalmRescue() {
        // 150px on a 1920 panel: past the ordinary 115px bar, short of the strict 192px one.
        assertEquals(1, ReaderGestures.swipeDelta(dx = -150f, dy = 0f, viewW = w))
        assertNull(ReaderGestures.swipeDelta(dx = -150f, dy = 0f, viewW = w, strict = true))
    }

    @Test
    fun aRestingPalmThatBarelyMovesIsNeverRescued() {
        for (dx in listOf(-30f, -5f, 0f, 5f, 30f)) {
            for (dy in listOf(-30f, 0f, 30f)) {
                assertNull("dx=$dx dy=$dy", ReaderGestures.swipeDelta(dx, dy, w, strict = true))
            }
        }
    }

    // ---- page-turn coalescing ----

    @Test
    fun aJumpLandsOnTheAccumulatedDelta() {
        assertEquals(11, ReaderGestures.jumpTarget(current = 10, delta = 1, pageCount = 100))
        assertEquals(19, ReaderGestures.jumpTarget(current = 10, delta = 9, pageCount = 100))
        assertEquals(1, ReaderGestures.jumpTarget(current = 10, delta = -9, pageCount = 100))
    }

    @Test
    fun aZeroDeltaIsANoOp() {
        assertNull(ReaderGestures.jumpTarget(current = 10, delta = 0, pageCount = 100))
    }

    /**
     * At the edges the target clamps onto the page the reader is already on, and that must report
     * "nothing to do" rather than a jump — on e-ink a re-render is a visible full-screen flash, so
     * mashing the forward edge on the last page would strobe the panel for no reason.
     */
    @Test
    fun jumpingPastAnEdgeIsANoOpRatherThanARepeatedRender() {
        assertNull("already on the last page", ReaderGestures.jumpTarget(99, +5, 100))
        assertNull("already on the first page", ReaderGestures.jumpTarget(0, -5, 100))
        assertEquals("clamps onto the last page", 99, ReaderGestures.jumpTarget(95, +50, 100))
        assertEquals("clamps onto the first page", 0, ReaderGestures.jumpTarget(5, -50, 100))
    }

    @Test
    fun anEmptyDocumentCannotBeJumpedInto() {
        assertNull(ReaderGestures.jumpTarget(0, +1, 0))
        assertNull(ReaderGestures.jumpTarget(0, -1, 0))
    }

    @Test
    fun aSinglePageDocumentHasNowhereToGo() {
        assertNull(ReaderGestures.jumpTarget(0, +1, 1))
        assertNull(ReaderGestures.jumpTarget(0, -1, 1))
    }

    // ---- double tap ----

    @Test
    fun aQuickNearbySecondTapIsADoubleTap() {
        assertTrue(ReaderGestures.isDoubleTap(500f, 500f, 500f, 500f, msSinceLast = 100))
        assertTrue(ReaderGestures.isDoubleTap(520f, 520f, 500f, 500f, msSinceLast = 250))
    }

    @Test
    fun aSlowSecondTapIsNotADoubleTap() {
        assertFalse(ReaderGestures.isDoubleTap(500f, 500f, 500f, 500f, msSinceLast = 400))
    }

    @Test
    fun aDistantSecondTapIsNotADoubleTap() {
        assertFalse(ReaderGestures.isDoubleTap(700f, 500f, 500f, 500f, msSinceLast = 100))
        assertFalse(ReaderGestures.isDoubleTap(500f, 700f, 500f, 500f, msSinceLast = 100))
    }

    @Test
    fun theSlopIsARadiusNotABox() {
        val slop = ReaderActivity.DOUBLE_TAP_SLOP_PX
        // Just inside the radius on the diagonal.
        val d = slop / 2f
        assertTrue(ReaderGestures.isDoubleTap(500f + d, 500f + d, 500f, 500f, 100))
        // Just outside it on the diagonal, though each axis alone is under the slop.
        val e = slop * 0.8f
        assertFalse(ReaderGestures.isDoubleTap(500f + e, 500f + e, 500f, 500f, 100))
    }
}
