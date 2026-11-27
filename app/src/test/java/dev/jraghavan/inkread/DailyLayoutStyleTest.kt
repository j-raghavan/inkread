package dev.jraghavan.inkread

import dev.jraghavan.inkread.DailyController.LayoutStyle
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for the daily layout-style presets (ADR-INKREAD-0018 D3). The tokens must match the
 * Rust `DailyLayoutStyle` (the reader core derives the column count and band from the stamped token),
 * and a legacy/unknown token must fall back to Papernews so a reader who never chose a style still
 * opens the two-column paper with the band.
 */
class DailyLayoutStyleTest {

    @Test
    fun tokensRoundTrip() {
        for (style in LayoutStyle.entries) {
            assertEquals(style, LayoutStyle.fromToken(style.token))
        }
    }

    @Test
    fun eachStyleHasADistinctLabelAndABlurb() {
        // The Settings row and the chooser (ADR-INKREAD-0018 D4) show these, so every style needs a
        // non-blank name and one-line blurb, and the names must not collide.
        val labels = LayoutStyle.entries.map { it.label }
        assertEquals(labels.size, labels.toSet().size)
        for (style in LayoutStyle.entries) {
            assertTrue(style.label.isNotBlank())
            assertTrue(style.blurb.isNotBlank())
        }
    }

    @Test
    fun legacyOrUnknownTokenDefaultsToPapernews() {
        // No style stored yet (null), or a token from a future/other build: keep the band.
        assertEquals(LayoutStyle.PAPERNEWS, LayoutStyle.fromToken(null))
        assertEquals(LayoutStyle.PAPERNEWS, LayoutStyle.fromToken(""))
        assertEquals(LayoutStyle.PAPERNEWS, LayoutStyle.fromToken("broadsheet"))
    }
}
