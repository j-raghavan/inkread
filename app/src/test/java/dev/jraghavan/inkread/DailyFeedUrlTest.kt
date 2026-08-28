package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Host JVM tests for re-pointing a followed feed at a new URL (#166).
 *
 * The interesting part is the byline. A feed added by pasting a URL is bylined with its host, so it
 * should follow the URL; a curated feed is bylined "BBC News", and re-deriving would rename it to
 * `feeds.bbci.co.uk` for the sake of a URL correction.
 */
class DailyFeedUrlTest {

    private fun added(url: String) = DailyController.Source(DailyController.bylineFor(url), url)

    @Test
    fun bylineIsTheHostWithoutWww() {
        assertEquals("example.com", DailyController.bylineFor("https://www.example.com/feed.xml"))
        assertEquals("hnrss.org", DailyController.bylineFor("https://hnrss.org/frontpage"))
    }

    /** A mistyped entry still shows something recognisable rather than an empty row. */
    @Test
    fun anUnparseableUrlFallsBackToItself() {
        assertEquals("not a url", DailyController.bylineFor("not a url"))
    }

    @Test
    fun editingTheUrlMovesTheBylineWhenItCameFromOne() {
        val s = added("https://example.com/feed.xml")
        val moved = DailyController.withUrl(s, "https://other.org/rss")
        assertEquals("https://other.org/rss", moved.url)
        assertEquals("other.org", moved.name)
    }

    /** The case that stops a URL fix from renaming a curated feed to its host. */
    @Test
    fun aCuratedBylineSurvivesAUrlEdit() {
        val bbc = DailyController.Source("BBC News", "https://feeds.bbci.co.uk/news/rss.xml")
        val moved = DailyController.withUrl(bbc, "https://feeds.bbci.co.uk/news/world/rss.xml")
        assertEquals("https://feeds.bbci.co.uk/news/world/rss.xml", moved.url)
        assertEquals("BBC News", moved.name)
    }

    /** An emptied field is a slip, not an instruction to blank the feed. */
    @Test
    fun aBlankUrlLeavesTheSourceAlone() {
        val s = added("https://example.com/feed.xml")
        assertEquals(s, DailyController.withUrl(s, "   "))
    }

    /** Everything else about a source survives being re-pointed. */
    @Test
    fun mutingAndTheArticleLimitSurviveAUrlEdit() {
        val s = DailyController.Source("example.com", "https://example.com/feed.xml", enabled = false, limit = 12)
        val moved = DailyController.withUrl(s, "https://example.com/atom.xml")
        assertEquals(false, moved.enabled)
        assertEquals(12, moved.limit)
    }

    @Test
    fun anUnchangedUrlIsANoOp() {
        val s = added("https://example.com/feed.xml")
        assertEquals(s, DailyController.withUrl(s, "https://example.com/feed.xml"))
    }
}
