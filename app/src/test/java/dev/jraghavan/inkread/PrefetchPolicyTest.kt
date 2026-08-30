package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** Host JVM tests for [PrefetchPolicy] — read-ahead, and every condition that suppresses it. */
class PrefetchPolicyTest {

    private fun next(
        current: Int = 10,
        direction: Int = 1,
        pageCount: Int = 100,
        zoom: Float = 1f,
        lastPrefetched: Int = -1,
    ) = PrefetchPolicy.nextPage(current, direction, pageCount, zoom, lastPrefetched)

    @Test
    fun readingForwardWarmsTheNextPage() {
        assertEquals(11, next(current = 10, direction = 1))
    }

    @Test
    fun readingBackwardWarmsThePreviousPage() {
        assertEquals(9, next(current = 10, direction = -1))
    }

    /** A zoomed render is viewport-specific, so caching the next page at this zoom caches something
     *  the reader will never see at that magnification. */
    @Test
    fun zoomingSuppressesReadAhead() {
        assertNull(next(zoom = 1.5f))
        assertNull(next(zoom = 5f))
    }

    @Test
    fun readAheadResumesOnceBackAtFit() {
        assertEquals(11, next(zoom = 1f))
    }

    @Test
    fun thereIsNothingPastEitherEndOfTheDocument() {
        assertNull("past the last page", next(current = 99, direction = 1, pageCount = 100))
        assertNull("before the first", next(current = 0, direction = -1, pageCount = 100))
    }

    @Test
    fun theLastAndFirstPagesAreStillWarmable() {
        assertEquals(99, next(current = 98, direction = 1, pageCount = 100))
        assertEquals(0, next(current = 1, direction = -1, pageCount = 100))
    }

    /**
     * The dedupe, and why it matters: opening the bar, drawing a selection and toggling a bookmark
     * all re-enter the render path on the *same* page. Without this each one re-enqueues the same
     * prefetch onto the serial engine thread, queueing work in front of the reader's next action.
     */
    @Test
    fun thePageAlreadyWarmedIsNotWarmedAgain() {
        assertNull(next(current = 10, direction = 1, lastPrefetched = 11))
    }

    @Test
    fun aDifferentPageIsStillWarmedAfterAnEarlierPrefetch() {
        assertEquals(11, next(current = 10, direction = 1, lastPrefetched = 42))
    }

    @Test
    fun anEmptyDocumentWarmsNothing() {
        assertNull(next(current = 0, direction = 1, pageCount = 0))
    }

    @Test
    fun aSinglePageDocumentWarmsNothing() {
        assertNull(next(current = 0, direction = 1, pageCount = 1))
        assertNull(next(current = 0, direction = -1, pageCount = 1))
    }

    /** Turning around mid-book warms the other way immediately, rather than waiting a page. */
    @Test
    fun reversingDirectionWarmsTheOtherWay() {
        assertEquals(11, next(current = 10, direction = 1, lastPrefetched = 9))
        assertEquals(9, next(current = 10, direction = -1, lastPrefetched = 11))
    }
}
