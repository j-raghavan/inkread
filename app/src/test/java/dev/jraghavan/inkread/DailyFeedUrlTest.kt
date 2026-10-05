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

    // ── Names (#268) ──────────────────────────────────────────────────────────────────────────────

    /** The reported case: a feed added as `rss.elpais.com` takes the name its feed gives. */
    @Test
    fun aHostNamedSourceTakesItsFeedTitle() {
        val s = added("https://rss.elpais.com/feed.xml")
        assertEquals("EL PAÍS", DailyController.named(s, "EL PAÍS").name)
    }

    /** A feed must not overwrite a curated byline or a name the reader typed, on any compile. */
    @Test
    fun aCuratedOrTypedNameIsNeverReplacedByTheFeed() {
        val bbc = DailyController.Source("BBC News", "https://feeds.bbci.co.uk/news/rss.xml")
        assertEquals(bbc, DailyController.named(bbc, "BBC News - Home"))
        val typed = DailyController.Source("El País", "https://rss.elpais.com/feed.xml")
        assertEquals(typed, DailyController.named(typed, "EL PAÍS"))
    }

    @Test
    fun aMissingOrBlankFeedTitleChangesNothing() {
        val s = added("https://example.com/feed.xml")
        assertEquals(s, DailyController.named(s, null))
        assertEquals(s, DailyController.named(s, "   "))
    }

    /** Once named from its feed, a later URL fix keeps the name (it is no longer the host). */
    @Test
    fun aFeedTitleSurvivesAUrlEdit() {
        val s = DailyController.named(added("https://rss.elpais.com/feed.xml"), "EL PAÍS")
        assertEquals("EL PAÍS", DailyController.withUrl(s, "https://feeds.elpais.com/rss").name)
    }

    @Test
    fun aRenameIsApplied() {
        val s = added("https://rss.elpais.com/feed.xml")
        val out = DailyController.edited(s, "  El País  ", s.url)
        assertEquals("El País", out.name)
        assertEquals(s.url, out.url)
    }

    /** Rename and URL change in one edit: the typed name wins over the host the URL would derive. */
    @Test
    fun aRenameAndAUrlEditTogetherKeepTheTypedName() {
        val s = added("https://rss.elpais.com/feed.xml")
        val out = DailyController.edited(s, "El País", "https://feeds.elpais.com/rss")
        assertEquals("El País", out.name)
        assertEquals("https://feeds.elpais.com/rss", out.url)
    }

    /** A name left untouched lets a host-derived byline follow the URL, as before #268. */
    @Test
    fun anUntouchedNameStillFollowsTheUrl() {
        val s = added("https://example.com/feed.xml")
        assertEquals("other.org", DailyController.edited(s, s.name, "https://other.org/rss").name)
    }

    /** An emptied name field is a slip: the current name stays, and a feed title can still adopt it. */
    @Test
    fun aBlankNameKeepsTheCurrentOne() {
        val s = added("https://example.com/feed.xml")
        assertEquals("example.com", DailyController.edited(s, "  ", s.url).name)
    }

    /** A typed name survives the next compile's feed title. */
    @Test
    fun aRenamedSourceIsNotRenamedBackByItsFeed() {
        val renamed = DailyController.edited(added("https://rss.elpais.com/feed.xml"), "El País", "https://rss.elpais.com/feed.xml")
        assertEquals("El País", DailyController.named(renamed, "EL PAÍS").name)
    }
}
