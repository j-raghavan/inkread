package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Test
import java.io.File

/**
 * Host JVM tests for [Books.sortEntries] — the shelf's ordering (#227).
 *
 * A shelf with one order is only usable up to a point: past a few dozen books, "which did I read
 * last" and "what is taking up the space" are the questions being asked, and alphabetical answers
 * neither.
 */
class ShelfSortTest {

    private fun entry(name: String, opened: Long = 0L, size: Long = 0L) =
        Books.ShelfEntry(File("/books/$name.epub"), name, opened, size)

    private fun names(entries: List<Books.ShelfEntry>) = entries.map { it.title }

    @Test
    fun titleOrdersAlphabeticallyIgnoringCase() {
        val shelf = listOf(entry("zebra"), entry("Apple"), entry("mango"))
        assertEquals(listOf("Apple", "mango", "zebra"), names(Books.sortEntries(shelf, Books.ShelfSort.TITLE)))
    }

    @Test
    fun recentPutsTheMostRecentlyOpenedFirst() {
        val shelf = listOf(entry("old", opened = 100), entry("newest", opened = 300), entry("mid", opened = 200))
        assertEquals(listOf("newest", "mid", "old"), names(Books.sortEntries(shelf, Books.ShelfSort.RECENT)))
    }

    @Test
    fun sizePutsTheBiggestFirstSoSpaceCanBeReclaimed() {
        val shelf = listOf(entry("small", size = 10), entry("huge", size = 900), entry("mid", size = 100))
        assertEquals(listOf("huge", "mid", "small"), names(Books.sortEntries(shelf, Books.ShelfSort.SIZE)))
    }

    /**
     * Ties break on title in every mode. Without it the order of equal keys is whatever the
     * filesystem handed back, so a shelf of never-opened books could reshuffle between visits — on
     * a panel that repaints in whole frames, that reads as a glitch.
     */
    @Test
    fun equalKeysFallBackToTitleSoTheOrderIsStable() {
        val shelf = listOf(entry("pear", opened = 5), entry("apple", opened = 5), entry("fig", opened = 5))
        assertEquals(listOf("apple", "fig", "pear"), names(Books.sortEntries(shelf, Books.ShelfSort.RECENT)))
        assertEquals(listOf("apple", "fig", "pear"), names(Books.sortEntries(shelf.reversed(), Books.ShelfSort.RECENT)))
    }

    /** Books predating the last-opened stamp arrive as 0 and must sort last, not first. */
    @Test
    fun neverOpenedBooksSortAfterOpenedOnes() {
        val shelf = listOf(entry("never", opened = 0), entry("read", opened = 50))
        assertEquals(listOf("read", "never"), names(Books.sortEntries(shelf, Books.ShelfSort.RECENT)))
    }

    @Test
    fun anEmptyShelfSortsToNothing() {
        assertEquals(emptyList<String>(), names(Books.sortEntries(emptyList(), Books.ShelfSort.SIZE)))
    }

    /** A stored preference from a future build, or a corrupt one, must not strand the shelf. */
    @Test
    fun anUnknownStoredSortFallsBackToTitle() {
        assertEquals(Books.ShelfSort.TITLE, Books.ShelfSort.of(null))
        assertEquals(Books.ShelfSort.TITLE, Books.ShelfSort.of("SOMETHING_ELSE"))
        assertEquals(Books.ShelfSort.RECENT, Books.ShelfSort.of("RECENT"))
    }
}
