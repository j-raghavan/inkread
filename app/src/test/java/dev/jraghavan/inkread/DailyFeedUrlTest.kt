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

    /**
     * Two URL edits in one Sources session, name untouched: the editor applies each edit to the row
     * as it stands, so the byline follows the URL both times and stays host-derived — still free to
     * take the feed's own name on the next compile.
     */
    @Test
    fun twoUrlEditsInOneSessionKeepTheBylineFollowingTheUrl() {
        var row = added("https://example.com/feed")
        row = DailyController.edited(row, row.name, "https://other.org/rss")
        row = DailyController.edited(row, row.name, "https://third.net/rss")
        assertEquals("third.net", row.name)
        assertEquals("EL PAÍS", DailyController.named(row, "EL PAÍS").name)
    }

    // ── Saving the editor over a compile that named a feed meanwhile ──────────────────────────────

    private val host = added("https://rss.elpais.com/feed.xml")
    private val adopted = host.copy(name = "EL PAÍS")

    /** The editor opened before the compile named the feed; its stale host name must not win. */
    @Test
    fun anAdoptedNameSurvivesSavingAnEditorOpenedBeforeIt() {
        val out = DailyController.keepAdoptedNames(listOf(host.copy(limit = 9)), listOf(host), listOf(adopted))
        assertEquals("EL PAÍS", out.single().name)
        assertEquals("the editor's other edits still apply", 9, out.single().limit)
    }

    /** A name the reader typed in the editor beats the feed's. */
    @Test
    fun aTypedNameBeatsAnAdoptedOne() {
        val typed = host.copy(name = "El País")
        assertEquals(typed, DailyController.keepAdoptedNames(listOf(typed), listOf(host), listOf(adopted)).single())
    }

    /** Nothing adopted meanwhile, or a source the editor re-pointed: the editor's list goes in as is. */
    @Test
    fun withoutAnAdoptionTheEditedListIsKept() {
        val edited = listOf(host)
        assertEquals(edited, DailyController.keepAdoptedNames(edited, listOf(host), listOf(host)))
        val moved = DailyController.withUrl(host, "https://feeds.elpais.com/rss")
        assertEquals(listOf(moved), DailyController.keepAdoptedNames(listOf(moved), listOf(host), listOf(adopted)))
    }

    // ── Adopting feed titles across the source list ───────────────────────────────────────────────

    @Test
    fun aFeedTitleIsAdoptedByUrl() {
        val a = added("https://rss.elpais.com/feed.xml")
        val b = DailyController.Source("BBC News", "https://feeds.bbci.co.uk/news/rss.xml")
        val out = DailyController.adoptTitles(listOf(a, b), mapOf(a.url to "EL PAÍS", b.url to "BBC News - Home"))
        assertEquals(listOf("EL PAÍS", "BBC News"), out.map { it.name })
    }

    /**
     * A title another source already goes by is not adopted: the front page groups by name, so the
     * two feeds would merge into one section.
     */
    @Test
    fun aTitleAlreadyInUseIsNotAdopted() {
        val curated = DailyController.Source("BBC News", "https://feeds.bbci.co.uk/news/rss.xml")
        val world = added("https://feeds.bbci.co.uk/news/world/rss.xml")
        val out = DailyController.adoptTitles(listOf(curated, world), mapOf(world.url to "bbc news"))
        assertEquals(world, out[1])
    }

    /** Two host-named feeds offering one title in the same pass: the first takes it, not both. */
    @Test
    fun twoFeedsOfferingOneTitleDoNotBothTakeIt() {
        val a = added("https://a.example/rss")
        val b = added("https://b.example/rss")
        val out = DailyController.adoptTitles(listOf(a, b), mapOf(a.url to "Same", b.url to "Same"))
        assertEquals(listOf("Same", "b.example"), out.map { it.name })
    }

    // ── Recognising an issue (#269) ───────────────────────────────────────────────────────────────

    @Test
    fun onlyFilesInTheDailyFolderAreIssues() {
        val files = java.nio.file.Files.createTempDirectory("files").toFile()
        java.io.File(files, "daily").mkdirs()
        assertEquals(true, DailyController.isIssue(files, java.io.File(files, "daily/inkread-daily-2026-10-05.epub")))
        assertEquals(false, DailyController.isIssue(files, java.io.File(files, "books/novel.epub")))
        assertEquals(false, DailyController.isIssue(files, java.io.File(files, "daily/sub/x.epub")))
        assertEquals("a path that only looks similar", false,
            DailyController.isIssue(files, java.io.File(files, "daily2/x.epub")))
    }
}
