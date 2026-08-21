package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for per-feed article limits (#193): the clamp that decides how many articles a
 * source contributes, and the round-robin that orders them into the issue.
 */
class PerFeedLimitTest {

    @Test
    fun theDefaultLimitIsUnchangedFromBeforeItWasConfigurable() {
        assertEquals(5, DailyController.PER_SOURCE)
        assertEquals(
            "an existing feed must keep the behaviour it had",
            DailyController.PER_SOURCE,
            DailyController.Source("x", "u").limit,
        )
    }

    /**
     * Out-of-range values are clamped on read as well as write. Zero is the dangerous one: it would
     * silently contribute nothing while the source still shows as active, which reads as a broken
     * feed rather than a setting.
     */
    @Test
    fun aLimitIsHeldInsideItsRange() {
        assertEquals(DailyController.MIN_PER_SOURCE, DailyController.clampLimit(0))
        assertEquals(DailyController.MIN_PER_SOURCE, DailyController.clampLimit(-7))
        assertEquals(DailyController.MIN_PER_SOURCE, DailyController.clampLimit(Int.MIN_VALUE))
        assertEquals(DailyController.MAX_PER_SOURCE, DailyController.clampLimit(999))
        assertEquals(DailyController.MAX_PER_SOURCE, DailyController.clampLimit(Int.MAX_VALUE))
        assertEquals(3, DailyController.clampLimit(3))
        assertTrue(DailyController.MIN_PER_SOURCE >= 1)
        assertTrue(DailyController.MAX_PER_SOURCE > DailyController.PER_SOURCE)
    }

    @Test
    fun everySourceGetsFrontOfIssuePresenceBeforeAnySecondArticle() {
        val a = listOf("a1", "a2", "a3")
        val b = listOf("b1", "b2")
        val c = listOf("c1")
        assertEquals(
            listOf("a1", "b1", "c1", "a2", "b2", "a3"),
            DailyController.interleaveByRank(listOf(a, b, c)),
        )
    }

    /**
     * The bug this shape prevents: interleaving to a fixed count would stop at the default and
     * silently discard everything a higher-limit source contributed past it — a feed set to 10
     * would deliver 5, with no error anywhere.
     */
    @Test
    fun aSourceAboveTheDefaultLimitKeepsAllOfItsArticles() {
        val big = (1..10).map { "big$it" }
        val small = listOf("small1", "small2")
        val out = DailyController.interleaveByRank(listOf(big, small))
        assertEquals("no article may be dropped", big.size + small.size, out.size)
        assertTrue("the tail of the long source survived", out.contains("big10"))
        assertEquals("front of issue is still round-robin", listOf("big1", "small1", "big2"), out.take(3))
    }

    @Test
    fun emptyAndRaggedInputsAreHandled() {
        assertTrue(DailyController.interleaveByRank(emptyList<List<String>>()).isEmpty())
        assertTrue(DailyController.interleaveByRank(listOf(emptyList<String>(), emptyList())).isEmpty())
        // A source that fetched nothing must not punch a hole in the ordering.
        assertEquals(
            listOf("a1", "b1", "a2"),
            DailyController.interleaveByRank(listOf(listOf("a1", "a2"), emptyList(), listOf("b1"))),
        )
    }
}
