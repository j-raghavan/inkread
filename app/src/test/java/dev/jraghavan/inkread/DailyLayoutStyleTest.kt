package dev.jraghavan.inkread

import dev.jraghavan.inkread.DailyController.LayoutStyle
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Host JVM tests for the daily layout-style presets (ADR-INKREAD-0018 D3). The token and column
 * count must match the Rust `DailyLayoutStyle`, and a legacy/unknown token must fall back to
 * Papernews so a reader who never chose a style still opens the two-column paper with the band.
 */
class DailyLayoutStyleTest {

    @Test
    fun eachStyleCarriesItsColumnCount() {
        // Mirrors the Rust `DailyLayoutStyle::columns`: the papers are two-column, Standard one.
        assertEquals(1, LayoutStyle.STANDARD.columns)
        assertEquals(2, LayoutStyle.PAPERNEWS.columns)
        assertEquals(2, LayoutStyle.VINTAGE.columns)
    }

    @Test
    fun tokensRoundTrip() {
        for (style in LayoutStyle.entries) {
            assertEquals(style, LayoutStyle.fromToken(style.token))
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
