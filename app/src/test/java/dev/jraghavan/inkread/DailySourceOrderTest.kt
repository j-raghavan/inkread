package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Host JVM tests for reordering followed sources (#267). Source order decides which source leads
 * the front page and breaks ties in the issue's round-robin, so a move must land exactly where the
 * reader asked and never lose or duplicate a source.
 */
class DailySourceOrderTest {

    private val abcd = listOf("a", "b", "c", "d")

    @Test
    fun aSourceMovesUpAndDownOnePlace() {
        assertEquals(listOf("a", "c", "b", "d"), DailyController.moved(abcd, "c", -1))
        assertEquals(listOf("a", "c", "b", "d"), DailyController.moved(abcd, "b", +1))
    }

    /** The reported case: a custom feed added last, moved to the top. */
    @Test
    fun repeatedMovesBringTheLastSourceToTheTop() {
        var order = abcd
        repeat(3) { order = DailyController.moved(order, "d", -1) }
        assertEquals(listOf("d", "a", "b", "c"), order)
    }

    /** ▲ on the first row and ▼ on the last do nothing — no wrap-around. */
    @Test
    fun movesAreClampedAtTheEnds() {
        assertEquals(abcd, DailyController.moved(abcd, "a", -1))
        assertEquals(abcd, DailyController.moved(abcd, "d", +1))
    }

    @Test
    fun anUnknownItemOrAnEmptyListIsLeftAlone() {
        assertEquals(abcd, DailyController.moved(abcd, "z", -1))
        assertEquals(emptyList<String>(), DailyController.moved(emptyList(), "a", +1))
    }

    /**
     * A removed row stays in the staged order until Save but is off screen. Moving past it must
     * count only visible rows: with "b" removed, ▲ on "c" puts it above "a", not merely above the
     * invisible "b" (which would look like a tap that did nothing).
     */
    @Test
    fun hiddenRowsAreSteppedOverAndKeptAtTheEnd() {
        val out = DailyController.moved(abcd, "c", -1) { it == "b" }
        assertEquals(listOf("c", "a", "d", "b"), out)
        assertEquals("nothing lost or duplicated", abcd.sorted(), out.sorted())
    }

    /** A hidden item cannot be moved — its row is gone. */
    @Test
    fun aHiddenItemDoesNotMove() {
        assertEquals(abcd, DailyController.moved(abcd, "b", -1) { it == "b" })
    }

    // ── Front page sections ───────────────────────────────────────────────────────────────────────

    /** The reported case: TechCrunch moved to the top leads the front page. */
    @Test
    fun sectionsFollowTheReadersSourceOrder() {
        val firstSeen = listOf("Lobsters", "The Verge", "TechCrunch", "BBC News")
        val order = listOf("TechCrunch", "Lobsters", "BBC News", "The Verge")
        assertEquals(order, DailyController.inSourceOrder(firstSeen, order))
    }

    /** A source whose first article failed to fetch appears late in the issue, but not on the page. */
    @Test
    fun aLateFirstAppearanceDoesNotSinkASource() {
        val firstSeen = listOf("Lobsters", "The Verge", "Ars Technica")
        val order = listOf("Lobsters", "Ars Technica", "The Verge")
        assertEquals(order, DailyController.inSourceOrder(firstSeen, order))
    }

    /** A section renamed since the issue was compiled keeps its first-seen place after the rest. */
    @Test
    fun anUnknownSectionGoesAfterTheKnownOnesInFirstSeenOrder() {
        val firstSeen = listOf("old.example", "Lobsters", "rss.elpais.com", "BBC News")
        val order = listOf("BBC News", "EL PAÍS", "Lobsters")
        assertEquals(
            listOf("BBC News", "Lobsters", "old.example", "rss.elpais.com"),
            DailyController.inSourceOrder(firstSeen, order),
        )
    }

    /** Muted or removed sources in [order] that have no section are simply absent. */
    @Test
    fun sourcesWithoutASectionAreIgnored() {
        assertEquals(listOf("B"), DailyController.inSourceOrder(listOf("B"), listOf("A", "B", "C")))
    }

    /** Sections fill the left column first, so they read in order down the columns. */
    @Test
    fun columnsSplitNearTheMiddleInOrder() {
        assertEquals(2, DailyController.columnSplit(listOf(7, 7, 7, 7)))
        assertEquals(3, DailyController.columnSplit(listOf(7, 7, 7, 7, 7)))
        // A heavy first section takes the left column on its own.
        assertEquals(1, DailyController.columnSplit(listOf(22, 3, 3, 3)))
        assertEquals(1, DailyController.columnSplit(listOf(5)))
        assertEquals(0, DailyController.columnSplit(emptyList()))
    }
}
