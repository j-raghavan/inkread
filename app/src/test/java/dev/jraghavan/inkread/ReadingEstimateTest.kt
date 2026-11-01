package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * Host JVM tests for the bottom bar's "time left" policy (#98). The minutes themselves come from the
 * Rust core; here we pin when the label shows and how it reads, with a stubbed estimator.
 */
class ReadingEstimateTest {

    /** A stand-in for the Rust bridge: one minute per page left. */
    private val oneMinPerPage: (Int) -> Int = { it }

    @Test
    fun showsTheEstimateWithPagesAhead() {
        // Page 1 of 10 → 9 pages left → "~9 min left" under the stub.
        assertEquals("~9 min left", ReadingEstimate.label(0, 10, oneMinPerPage))
    }

    @Test
    fun hiddenWhenTotalIsUnknownOrSinglePage() {
        assertNull("unknown total (coerced to 1)", ReadingEstimate.label(0, 1, oneMinPerPage))
        assertNull("zero total", ReadingEstimate.label(0, 0, oneMinPerPage))
    }

    @Test
    fun hiddenOnTheLastPage() {
        assertNull("nothing left to read", ReadingEstimate.label(9, 10, oneMinPerPage))
    }

    @Test
    fun hiddenWhenTheEstimateRoundsToNothing() {
        assertNull("a zero estimate is not shown", ReadingEstimate.label(0, 10) { 0 })
    }

    @Test
    fun doesNotConsultTheEstimatorWhenNothingIsLeft() {
        // On the last page the estimator must not even be called (it would be a wasted JNI hop).
        var called = false
        ReadingEstimate.label(9, 10) { called = true; 5 }
        assertEquals(false, called)
    }
}
