package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class ThumbnailGridModelTest {

    @Test
    fun currentPageOpensInsideItsNinePageWindow() {
        val window = ThumbnailGridModel.windowFor(currentPage = 17, total = 30)

        assertEquals(9, window.start)
        assertEquals(18, window.endExclusive)
        assertEquals((9 until 18).toList(), window.pages)
        assertTrue(17 in window.pages)
    }

    @Test
    fun finalWindowIsPartialAndClamped() {
        val window = ThumbnailGridModel.windowAt(start = 27, total = 31)

        assertEquals(27, window.start)
        assertEquals(31, window.endExclusive)
        assertEquals(listOf(27, 28, 29, 30), window.pages)
    }

    @Test
    fun pagingMovesByOneGridWithoutLeavingDocument() {
        assertEquals(9, ThumbnailGridModel.shift(start = 0, direction = 1, total = 20))
        assertEquals(18, ThumbnailGridModel.shift(start = 9, direction = 1, total = 20))
        assertEquals(18, ThumbnailGridModel.shift(start = 18, direction = 1, total = 20))
        assertEquals(9, ThumbnailGridModel.shift(start = 18, direction = -1, total = 20))
        assertEquals(0, ThumbnailGridModel.shift(start = 0, direction = -1, total = 20))
    }

    @Test
    fun thumbnailSizingPreservesViewportAspectRatio() {
        assertEquals(
            ThumbnailGridModel.Size(width = 300, height = 400),
            ThumbnailGridModel.fitSize(
                sourceWidth = 1200,
                sourceHeight = 1600,
                maxWidth = 300,
                maxHeight = 500,
            ),
        )
        assertEquals(
            ThumbnailGridModel.Size(width = 225, height = 300),
            ThumbnailGridModel.fitSize(
                sourceWidth = 1200,
                sourceHeight = 1600,
                maxWidth = 400,
                maxHeight = 300,
            ),
        )
    }

    @Test
    fun emptyDocumentsProduceNoWindowOrBitmapSize() {
        assertEquals(emptyList<Int>(), ThumbnailGridModel.windowFor(currentPage = 0, total = 0).pages)
        assertEquals(
            ThumbnailGridModel.Size(0, 0),
            ThumbnailGridModel.fitSize(0, 1600, 300, 400),
        )
    }
}
