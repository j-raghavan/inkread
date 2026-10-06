package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** Host JVM tests for TOC-derived navigation: chapter stops and the Daily article jump (#269). */
class TocNavTest {

    private fun item(depth: Int, page: Int?, title: String) = TocItem(depth, page, title)

    /** A Daily issue, round-robin pages: TC1=1, BBC1=2, TC2=3, Ars2=4, BBC2=5. */
    private val grouped = listOf(
        item(0, 0, "Cover"),
        item(0, 1, "TechCrunch"), item(1, 1, "TC 1"), item(1, 3, "TC 2"),
        item(0, 2, "BBC"), item(1, 2, "BBC 1"), item(1, 5, "BBC 2"),
        item(0, 4, "Ars"), item(1, 4, "Ars 2"),
    )

    /** The same issue compiled before #269: a flat TOC in reading order. */
    private val flat = listOf(
        item(0, 0, "Cover"), item(0, 1, "TC 1"), item(0, 2, "BBC 1"),
        item(0, 3, "TC 2"), item(0, 4, "Ars 2"), item(0, 5, "BBC 2"),
    )

    /** The headline the reader tapped opens that article in either layout — back issues included. */
    @Test
    fun aDailyArticleOpensAtItsOwnPageInBothLayouts() {
        for (toc in listOf(grouped, flat)) {
            assertEquals(listOf(1, 2, 3, 4, 5), (0..4).map { TocNav.dailyArticlePage(toc, it) })
        }
    }

    @Test
    fun anArticleIndexOutOfRangeOpensNothing() {
        assertNull(TocNav.dailyArticlePage(grouped, 5))
        assertNull(TocNav.dailyArticlePage(grouped, -1))
        assertNull(TocNav.dailyArticlePage(emptyList(), 0))
    }

    /** Chapter ›› steps through every article of a grouped issue, not just each source's first. */
    @Test
    fun aGroupedDailyIssueGetsOneChapterPerArticle() {
        val stops = TocNav.chapterStarts(grouped, perArticle = true)
        assertEquals(listOf(0, 1, 2, 3, 4, 5), stops.map { it.first })
        assertEquals("labels are the headlines, not the section names", "TC 1", stops[1].second)
    }

    /** A book's nested outline still steps by its top-level chapters — unchanged from before. */
    @Test
    fun aBooksNestedOutlineKeepsItsTopLevelChapters() {
        val book = listOf(
            item(0, 0, "Part One"), item(1, 0, "Ch 1"), item(1, 10, "Ch 2"),
            item(0, 20, "Part Two"), item(1, 20, "Ch 3"),
        )
        assertEquals(listOf(0 to "Part One", 20 to "Part Two"), TocNav.chapterStarts(book))
    }

    /** A book whose TOC lists an entry out of page order still steps by its top-level chapters. */
    @Test
    fun aBooksOutOfOrderTocKeepsItsTopLevelChapters() {
        val book = listOf(
            item(0, 0, "Ch 1"), item(1, 3, "1.1"), item(1, 6, "1.2"),
            item(0, 10, "Ch 2"), item(1, 12, "2.1"),
            item(0, 2, "Notes"), // listed last, but early in the book
        )
        assertEquals(listOf(0, 2, 10), TocNav.chapterStarts(book).map { it.first })
    }

    /** A targeted parent whose children have no target is still a stop. */
    @Test
    fun aParentWithUntargetedChildrenIsALeaf() {
        val toc = listOf(item(0, 0, "Cover"), item(0, 1, "Section"), item(1, null, "label only"))
        assertEquals(listOf(0, 1), TocNav.chapterStarts(toc, perArticle = true).map { it.first })
    }

    @Test
    fun aFlatOrUntargetedTocIsHandled() {
        assertEquals(listOf(0, 1, 2, 3, 4, 5), TocNav.chapterStarts(flat).map { it.first })
        assertEquals(emptyList<Pair<Int, String>>(), TocNav.chapterStarts(listOf(item(0, null, "x"))))
        assertEquals(emptyList<Pair<Int, String>>(), TocNav.chapterStarts(emptyList()))
    }

    /**
     * A single-source issue's grouped TOC happens to be in reading order, so only the Daily flag can
     * tell it from a book's Part/Chapter outline — without it, ›› would stop at the one section.
     */
    @Test
    fun aSingleSourceDailyIssueStillStepsPerArticle() {
        val oneSource = listOf(
            item(0, 0, "Cover"),
            item(0, 1, "TechCrunch"), item(1, 1, "TC 1"), item(1, 2, "TC 2"), item(1, 3, "TC 3"),
        )
        assertEquals(listOf(0, 1, 2, 3), TocNav.chapterStarts(oneSource, perArticle = true).map { it.first })
        assertEquals("TC 1", TocNav.chapterStarts(oneSource, perArticle = true)[1].second)
    }

    /** The Daily flag on a flat (pre-#269) back issue changes nothing. */
    @Test
    fun theDailyFlagOnAFlatBackIssueIsHarmless() {
        assertEquals(TocNav.chapterStarts(flat), TocNav.chapterStarts(flat, perArticle = true))
    }
}
