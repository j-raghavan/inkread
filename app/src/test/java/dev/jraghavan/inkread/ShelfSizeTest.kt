package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Host JVM tests for [Books.humanSize] — the figure a reader decides on when clearing space.
 *
 * The grain is deliberate: sizes are shown to answer "is removing this worth it", so MB resolution
 * is what matters and a book must never read as "0 MB" and look free to keep.
 */
class ShelfSizeTest {

    @Test
    fun booksReadInMegabytes() {
        assertEquals("4 MB", Books.humanSize(4L * 1024 * 1024))
        assertEquals("512 MB", Books.humanSize(512L * 1024 * 1024))
    }

    @Test
    fun largeCollectionsReadInGigabytes() {
        assertEquals("1.0 GB", Books.humanSize(1024L * 1024 * 1024))
        assertEquals("2.5 GB", Books.humanSize((2.5 * 1024 * 1024 * 1024).toLong()))
    }

    @Test
    fun smallFilesReadInKilobytesAndNeverRoundToZero() {
        // A sidecar of a few hundred bytes is real; showing "0 KB" would suggest nothing is there.
        assertEquals("1 KB", Books.humanSize(200))
        assertEquals("1 KB", Books.humanSize(1024))
        assertEquals("64 KB", Books.humanSize(64L * 1024))
    }

    @Test
    fun nothingIsZero() {
        assertEquals("0 KB", Books.humanSize(0))
    }

    /** Locale-independent: a comma decimal separator would be a formatting bug on a device set to
     *  a European locale, and the string is parsed by eye, not by a machine. */
    @Test
    fun gigabytesUseADotWhateverTheLocale() {
        val previous = java.util.Locale.getDefault()
        try {
            java.util.Locale.setDefault(java.util.Locale.GERMANY)
            assertEquals("1.5 GB", Books.humanSize((1.5 * 1024 * 1024 * 1024).toLong()))
        } finally {
            java.util.Locale.setDefault(previous)
        }
    }
}
