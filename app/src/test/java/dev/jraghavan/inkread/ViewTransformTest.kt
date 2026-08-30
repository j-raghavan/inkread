package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for [ViewTransform] — the page↔view map every coordinate in the reader passes
 * through (RR5-FR3).
 *
 * Before this was a type the same four functions were restated in five places and nothing checked
 * that the forward and inverse maps agreed. A disagreement is not a crash: ink lands slightly off
 * the stroke you drew, a lasso selects the wrong glyphs, a link stops responding where it is drawn.
 * So the load-bearing tests here are round trips, at every combination of zoom and pan.
 */
class ViewTransformTest {

    private val manta = ViewTransform(1920, 2560)

    // ---- fit ----

    @Test
    fun atFitTheMapIsThePlainViewportScale() {
        assertEquals(0f, manta.nToVx(0f), 0.001f)
        assertEquals(1920f, manta.nToVx(1f), 0.001f)
        assertEquals(960f, manta.nToVx(0.5f), 0.001f)
        assertEquals(2560f, manta.nToVy(1f), 0.001f)
        assertEquals(0.5f, manta.vToNx(960f), 0.001f)
        assertEquals(0.25f, manta.vToNy(640f), 0.001f)
    }

    @Test
    fun atFitThereIsNoOverscanAndNothingIsZoomed() {
        assertEquals(0f, manta.overX, 0.001f)
        assertEquals(0f, manta.overY, 0.001f)
        assertFalse(manta.isZoomed)
        assertTrue(manta.withZoom(1.01f).isZoomed)
    }

    // ---- the round trip, which is the point ----

    @Test
    fun viewToPageToViewIsTheIdentityAtEveryZoomAndPan() {
        for (zoom in listOf(1f, 1.5f, 2f, 3.3f, 5f)) {
            for (pan in listOf(0f, 0.25f, 0.5f, 1f)) {
                val t = ViewTransform(1920, 2560, zoom, pan, pan)
                for (vx in listOf(1f, 480f, 960f, 1919f)) {
                    val back = t.nToVx(t.vToNx(vx))
                    assertEquals("zoom $zoom pan $pan vx $vx", vx, back, 0.05f)
                }
                for (vy in listOf(1f, 640f, 1280f, 2559f)) {
                    val back = t.nToVy(t.vToNy(vy))
                    assertEquals("zoom $zoom pan $pan vy $vy", vy, back, 0.05f)
                }
            }
        }
    }

    @Test
    fun pageToViewToPageIsTheIdentityForVisibleContent() {
        val t = ViewTransform(1920, 2560, zoom = 2f, panX = 0.5f, panY = 0.5f)
        // At zoom 2 with pan 0.5 the visible page spans the middle half of each axis.
        for (n in listOf(0.3f, 0.5f, 0.7f)) {
            assertEquals(n, t.vToNx(t.nToVx(n)), 0.001f)
            assertEquals(n, t.vToNy(t.nToVy(n)), 0.001f)
        }
    }

    @Test
    fun pageCoordinatesAreClampedToThePage() {
        val t = ViewTransform(1920, 2560, zoom = 2f, panX = 0f, panY = 0f)
        assertEquals(0f, t.vToNx(-5000f), 0.001f)
        assertEquals(1f, t.vToNx(50_000f), 0.001f)
        assertEquals(0f, t.vToNy(-5000f), 0.001f)
        assertEquals(1f, t.vToNy(50_000f), 0.001f)
    }

    // ---- overscan ----

    @Test
    fun overscanGrowsWithZoom() {
        assertEquals(1920f, manta.withZoom(2f).overX, 0.001f)
        assertEquals(2560f, manta.withZoom(2f).overY, 0.001f)
        assertEquals(3840f, manta.withZoom(3f).overX, 0.001f)
    }

    @Test
    fun panningRightMovesTheViewportLeftOverThePage() {
        val t = ViewTransform(1920, 2560, zoom = 2f, panX = 0.5f, panY = 0.5f)
        // Dragging the content right (+dx) should reveal content to its left → pan decreases.
        val (px, _) = t.panAfterDrag(dx = 480f, dy = 0f)
        assertTrue("dragging right decreases pan", px < 0.5f)
        val (px2, _) = t.panAfterDrag(dx = -480f, dy = 0f)
        assertTrue("dragging left increases pan", px2 > 0.5f)
    }

    @Test
    fun panIsClampedToTheMargins() {
        val t = ViewTransform(1920, 2560, zoom = 2f, panX = 0.5f, panY = 0.5f)
        val (px, py) = t.panAfterDrag(dx = 100_000f, dy = 100_000f)
        assertEquals(0f, px, 0.001f)
        assertEquals(0f, py, 0.001f)
        val (px2, py2) = t.panAfterDrag(dx = -100_000f, dy = -100_000f)
        assertEquals(1f, px2, 0.001f)
        assertEquals(1f, py2, 0.001f)
    }

    /** At fit there is no overscan, so a drag must not divide by zero or move the page. */
    @Test
    fun draggingAtFitIsANoOp() {
        val (px, py) = manta.panAfterDrag(dx = 500f, dy = 500f)
        assertEquals(0f, px, 0.001f)
        assertEquals(0f, py, 0.001f)
    }

    // ---- focal anchoring: the maths a pinch and a double-tap share ----

    /**
     * The contract: after anchoring, the page point that was under the finger is still under the
     * finger. This is what makes a pinch feel attached to the paper, and it is now one
     * implementation used by both gestures — this test is what keeps them agreeing.
     */
    @Test
    fun anchoringKeepsThePointUnderTheFingerWhereItWas() {
        for (zoom in listOf(1.5f, 2f, 4f)) {
            val t = ViewTransform(1920, 2560, zoom)
            for ((fx, fy) in listOf(960f to 1280f, 600f to 800f, 1400f to 2000f)) {
                val nx = 0.5f
                val ny = 0.5f
                val (px, py) = t.panAnchoring(nx, ny, fx, fy)
                val anchored = t.withPan(px, py)
                // Where the anchoring is reachable (not clamped at a margin), the point lands back
                // under the finger.
                if (px > 0f && px < 1f) {
                    assertEquals("zoom $zoom fx $fx", fx, anchored.nToVx(nx), 0.5f)
                }
                if (py > 0f && py < 1f) {
                    assertEquals("zoom $zoom fy $fy", fy, anchored.nToVy(ny), 0.5f)
                }
            }
        }
    }

    @Test
    fun anchoringAtFitCentresOnTheOrigin() {
        val (px, py) = manta.panAnchoring(0.5f, 0.5f, 960f, 1280f)
        assertEquals(0f, px, 0.001f)
        assertEquals(0f, py, 0.001f)
    }

    // ---- lengths ----

    @Test
    fun aScreenLengthShrinksInPageUnitsAsZoomGrows() {
        assertEquals(0.5f, manta.lenToNorm(960f), 0.001f)
        assertEquals(0.25f, manta.withZoom(2f).lenToNorm(960f), 0.001f)
        assertEquals(0.125f, manta.withZoom(4f).lenToNorm(960f), 0.001f)
    }

    // ---- degenerate input ----

    /**
     * The surface reports 0×0 before it is laid out, and the reader builds a transform from
     * whatever the viewport currently says. Dividing by it would produce NaN coordinates that then
     * propagate silently into ink and hit-testing, so every divisor is guarded.
     */
    @Test
    fun anUnsizedViewportYieldsZerosRatherThanNaN() {
        val t = ViewTransform(0, 0)
        assertEquals(0f, t.vToNx(100f), 0.001f)
        assertEquals(0f, t.vToNy(100f), 0.001f)
        assertEquals(0f, t.lenToNorm(100f), 0.001f)
        assertFalse(t.vToNx(100f).isNaN())
        assertFalse(t.lenToNorm(100f).isNaN())
    }

    @Test
    fun aZeroZoomYieldsZerosRatherThanNaN() {
        val t = ViewTransform(1920, 2560, zoom = 0f)
        assertEquals(0f, t.vToNx(100f), 0.001f)
        assertEquals(0f, t.vToNy(100f), 0.001f)
        assertEquals(0f, t.lenToNorm(100f), 0.001f)
    }

    @Test
    fun withZoomAndWithPanReplaceOnlyWhatTheyName() {
        val t = ViewTransform(1920, 2560, zoom = 2f, panX = 0.3f, panY = 0.4f)
        val z = t.withZoom(3f)
        assertEquals(3f, z.zoom, 0.001f)
        assertEquals(0.3f, z.panX, 0.001f)
        assertEquals(0.4f, z.panY, 0.001f)
        val p = t.withPan(0.1f, 0.2f)
        assertEquals(2f, p.zoom, 0.001f)
        assertEquals(0.1f, p.panX, 0.001f)
        assertEquals(0.2f, p.panY, 0.001f)
    }
}
