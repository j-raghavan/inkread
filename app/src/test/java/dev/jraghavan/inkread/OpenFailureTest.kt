package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Host JVM tests for [OpenFailure]: core open errors become a reader-facing message. */
class OpenFailureTest {

    // Verbatim from the device log of a `.epub.mark` sidecar opened as a PDF.
    private val pdfiumFormatError =
        "[3] this file appears damaged: pdfium: PdfiumLibraryInternalError(\n    FormatError,\n)"

    @Test
    fun parsesTheCoreStatusCode() {
        assertEquals(3, OpenFailure.statusOf(pdfiumFormatError))
        assertEquals(4, OpenFailure.statusOf("[4] this file is protected and can't be opened"))
        assertNull(OpenFailure.statusOf("no status here"))
        assertNull(OpenFailure.statusOf(null))
    }

    @Test
    fun damagedOrUnsupportedHidesBackendDiagnostics() {
        val msg = OpenFailure.message("book.pdf", pdfiumFormatError)
        assertTrue(msg.contains("book.pdf"))
        assertTrue(!msg.contains("pdfium") && !msg.contains("FormatError"))
        assertEquals(msg, OpenFailure.message("book.pdf", "[2] unsupported file type: x"))
    }

    @Test
    fun drmAndUnknownFailures() {
        assertTrue(OpenFailure.message("a.epub", "[4] protected").contains("DRM"))
        assertEquals("Couldn't open \u201Ca.epub\u201D.", OpenFailure.message("a.epub", null))
        assertEquals("Couldn't open \u201Ca.epub\u201D.", OpenFailure.message("a.epub", "[10] storage error: x"))
    }
}
