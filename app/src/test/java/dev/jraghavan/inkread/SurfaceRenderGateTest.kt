package dev.jraghavan.inkread

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for [SurfaceRenderGate] (#186).
 *
 * Two failure modes sit on either side of this decision, and both are expensive: rendering a repeat
 * callback wastes a full page layout and an EPD refresh on the open path, while skipping a render
 * that was needed leaves a SurfaceView showing black. The cases below pin both edges.
 */
class SurfaceRenderGateTest {

    @Test
    fun theFirstSizeAlwaysRenders() {
        val gate = SurfaceRenderGate()
        gate.onSurfaceCreated()
        assertTrue(gate.needsRender(1920, 2560, documentOpen = false))
    }

    /** The #186 case: Android delivers surfaceChanged twice at the same size. */
    @Test
    fun aRepeatCallbackAtTheSameSizeDoesNotRenderAgain() {
        val gate = SurfaceRenderGate()
        gate.onSurfaceCreated()
        assertTrue(gate.needsRender(1920, 2560, documentOpen = false))
        assertFalse(gate.needsRender(1920, 2560, documentOpen = true))
        assertFalse(gate.needsRender(1920, 2560, documentOpen = true))
    }

    /** A rotation or resize is a real change and must redraw. */
    @Test
    fun aDifferentSizeRenders() {
        val gate = SurfaceRenderGate()
        gate.onSurfaceCreated()
        assertTrue(gate.needsRender(1920, 2560, documentOpen = false))
        assertTrue(gate.needsRender(2560, 1920, documentOpen = true))
        assertFalse("settled at the new size", gate.needsRender(2560, 1920, documentOpen = true))
        assertTrue("and back again", gate.needsRender(1920, 2560, documentOpen = true))
    }

    /**
     * The trap in the cheap version of this fix: leaving the reader and returning destroys and
     * recreates the surface at the SAME size. Nothing has been drawn to the new surface, so
     * "same size" alone would leave the panel black.
     */
    @Test
    fun aRecreatedSurfaceRendersEvenAtTheSameSize() {
        val gate = SurfaceRenderGate()
        gate.onSurfaceCreated()
        assertTrue(gate.needsRender(1920, 2560, documentOpen = false))
        assertFalse(gate.needsRender(1920, 2560, documentOpen = true))

        gate.onSurfaceCreated() // back from elsewhere: new surface, same size
        assertTrue(
            "a new surface has nothing drawn on it yet",
            gate.needsRender(1920, 2560, documentOpen = true),
        )
        assertFalse("settled again", gate.needsRender(1920, 2560, documentOpen = true))
    }

    /**
     * Before a document is open there is nothing to be redundant about — that call is the open
     * itself, and skipping it would mean never opening the book.
     */
    @Test
    fun withNoDocumentOpenEveryCallRenders() {
        val gate = SurfaceRenderGate()
        gate.onSurfaceCreated()
        repeat(3) {
            assertTrue("call $it", gate.needsRender(1920, 2560, documentOpen = false))
        }
    }
}
