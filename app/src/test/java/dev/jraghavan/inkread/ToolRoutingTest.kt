package dev.jraghavan.inkread

import android.view.MotionEvent
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Host JVM tests for [Tool.forStylus] — which tool a stylus event drives (#158).
 *
 * The bug this guards: an inverted pen (`TOOL_TYPE_ERASER`) used to fall through to the ink path,
 * so the firmware eraser cleared the panel while the app committed the sweep as a Pen stroke —
 * a refresh then showed the original stroke *and* an eraser-shaped scribble. No emulator reproduces
 * the EMR pen, so this pure routing decision is the deterministic guard against a regression.
 *
 * The `TOOL_TYPE_*` values are compile-time constants, so they inline here without an Android
 * runtime.
 */
class ToolRoutingTest {

    @Test
    fun invertedPenErasesWhateverThePaletteSays() {
        for (palette in Tool.entries) {
            assertEquals(
                "palette=$palette with an inverted pen must erase",
                Tool.ERASER,
                Tool.forStylus(palette, MotionEvent.TOOL_TYPE_ERASER),
            )
        }
    }

    @Test
    fun penTipFollowsThePalette() {
        for (palette in Tool.entries) {
            assertEquals(palette, Tool.forStylus(palette, MotionEvent.TOOL_TYPE_STYLUS))
        }
    }

    /** The eraser palette still erases with the pen tip — the ordinary way to erase. */
    @Test
    fun eraserPaletteErasesWithThePenTip() {
        assertEquals(Tool.ERASER, Tool.forStylus(Tool.ERASER, MotionEvent.TOOL_TYPE_STYLUS))
    }

    /** Only the eraser end overrides; an unknown/finger tool type is left to the palette (the
     *  caller already gates on the tool type, so this must not silently reroute). */
    @Test
    fun otherToolTypesLeaveThePaletteAlone() {
        assertEquals(Tool.PEN, Tool.forStylus(Tool.PEN, MotionEvent.TOOL_TYPE_FINGER))
        assertEquals(Tool.LASSO, Tool.forStylus(Tool.LASSO, MotionEvent.TOOL_TYPE_UNKNOWN))
    }
}
