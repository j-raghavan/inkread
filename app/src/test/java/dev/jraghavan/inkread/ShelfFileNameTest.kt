package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * Host JVM tests for [Books.disambiguatingFileName] (#227).
 *
 * A reader holding several copies of one work sees the same metadata title on every row, with no
 * way to tell which file is which. The file name settles it — but only when it says something the
 * title doesn't, or every row on an ordinary shelf grows a second line repeating itself.
 */
class ShelfFileNameTest {

    @Test
    fun aFileNamedAfterItsTitleAddsNothing() {
        assertNull(Books.disambiguatingFileName("Moby-Dick", "Moby-Dick.epub"))
    }

    /** The case that motivated it: same work, different files. */
    @Test
    fun aFileNameThatDiffersFromTheTitleIsShown() {
        assertEquals("moby-dick-v2.epub", Books.disambiguatingFileName("Moby-Dick", "moby-dick-v2.epub"))
        assertEquals("Moby-Dick (1).epub", Books.disambiguatingFileName("Moby-Dick", "Moby-Dick (1).epub"))
    }

    /** Metadata titles arrive from EPUB packages with stray whitespace more often than not. */
    @Test
    fun surroundingWhitespaceInTheTitleIsNotADifference() {
        assertNull(Books.disambiguatingFileName("  Moby-Dick ", "Moby-Dick.epub"))
    }

    /** A re-import can change only the case of a name; that is not worth a second line. */
    @Test
    fun caseAloneIsNotADifference() {
        assertNull(Books.disambiguatingFileName("moby-dick", "Moby-Dick.epub"))
    }

    /** Only the final extension is stripped — a dotted title must still match its own file. */
    @Test
    fun onlyTheExtensionIsStripped() {
        assertNull(Books.disambiguatingFileName("Vol. 1. Whales", "Vol. 1. Whales.pdf"))
        assertEquals("vol1.pdf", Books.disambiguatingFileName("Vol. 1. Whales", "vol1.pdf"))
    }

    /** A file with no extension at all still compares against the whole name. */
    @Test
    fun anExtensionlessFileComparesWhole() {
        assertNull(Books.disambiguatingFileName("notes", "notes"))
        assertEquals("notes", Books.disambiguatingFileName("Reading list", "notes"))
    }
}
