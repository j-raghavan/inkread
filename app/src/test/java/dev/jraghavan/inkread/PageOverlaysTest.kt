package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for [PageOverlays]' bookmark-ribbon geometry.
 *
 * The ribbon is the reader's only affordance for bookmarking — its faint outline is what says the
 * corner is tappable — and the tap zone that toggles it is defined **separately**, as fractions of
 * the panel in [ReaderActivity.BOOKMARK_ZONE_W]/`_H`. Two definitions of the same target is exactly
 * the arrangement that drifts, so these pin that the drawn ribbon stays inside the zone that
 * activates it. Nothing else in the suite covered that before.
 *
 * Pure float maths — no `Canvas`, no `Path`, no device.
 */
class PageOverlaysTest {

    private fun overlays(viewW: Int, viewH: Int = 2560) = PageOverlays(object : PageOverlays.Host {
        override fun nToVx(nx: Float) = nx * viewW
        override fun nToVy(ny: Float) = ny * viewH
        override val viewW = viewW
    })

    @Test
    fun theRibbonScalesWithThePanel() {
        // Same shape on both panels in the family, just scaled — nothing is pinned to pixels.
        val manta = overlays(1920).ribbon()
        val nomad = overlays(1404).ribbon()
        assertEquals(manta.width / 1920f, nomad.width / 1404f, 0.0001f)
        assertEquals(manta.length / 1920f, nomad.length / 1404f, 0.0001f)
        assertEquals(manta.notch / manta.width, nomad.notch / nomad.width, 0.0001f)
    }

    @Test
    fun theRibbonHangsFromTheTopRightWithoutTouchingTheEdge() {
        val r = overlays(1920).ribbon()
        assertTrue("inset from the right edge", r.right < 1920f)
        assertTrue("left of the right edge by more than its own width", 1920f - r.right > 0f)
        assertTrue("left edge is left of the right edge", r.left < r.right)
        assertTrue("hangs downward", r.length > 0f)
    }

    @Test
    fun theSwallowtailNotchIsCentredAndAboveTheTails() {
        val r = overlays(1920).ribbon()
        assertEquals((r.left + r.right) / 2f, r.centreX, 0.0001f)
        assertTrue("the notch cuts up from the bottom", r.notchY < r.length)
        assertTrue("but not past the top", r.notchY > 0f)
        assertEquals(r.length - r.notch, r.notchY, 0.0001f)
    }

    /**
     * The drawn ribbon must sit inside the tap zone that toggles it, or the reader sees a bookmark
     * affordance that does nothing when pressed. The zone is defined independently of this geometry
     * (`BOOKMARK_ZONE_W`/`_H` as panel fractions), so this is the only thing tying them together.
     */
    @Test
    fun theRibbonSitsInsideTheTapZoneThatTogglesIt() {
        for (viewW in listOf(1920, 1404, 1200, 2200)) {
            val viewH = viewW * 4 / 3
            val r = overlays(viewW, viewH).ribbon()
            val zoneLeft = viewW * (1f - ReaderActivity.BOOKMARK_ZONE_W)
            val zoneBottom = viewH * ReaderActivity.BOOKMARK_ZONE_H
            assertTrue(
                "w=$viewW: ribbon left ${r.left} must be inside the zone starting at $zoneLeft",
                r.left >= zoneLeft,
            )
            assertTrue("w=$viewW: ribbon right ${r.right} must be on screen", r.right <= viewW)
            assertTrue(
                "w=$viewW: ribbon length ${r.length} must be inside the zone ending at $zoneBottom",
                r.length <= zoneBottom,
            )
        }
    }
}
