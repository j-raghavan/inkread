package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for [Books.importExtension] (issue #125). The core opens PDF, EPUB, CBZ, and plain
 * text ([`reader-core` `DocFormat`]); the shell must store a picked file under a core-supported
 * extension so it opens with the right backend — plain text has no magic bytes, so its extension is
 * the only signal, and the pre-#125 code that forced everything to `.pdf`/`.epub` broke it.
 */
class BooksImportTest {

    @Test
    fun preservesEveryCoreSupportedExtension() {
        assertEquals("pdf", Books.importExtension("paper.pdf"))
        assertEquals("epub", Books.importExtension("novel.epub"))
        assertEquals("cbz", Books.importExtension("comic.cbz"))
        assertEquals("txt", Books.importExtension("notes.txt"))
        assertEquals("text", Books.importExtension("notes.text"))
    }

    @Test
    fun isCaseInsensitive() {
        assertEquals("cbz", Books.importExtension("COMIC.CBZ"))
        assertEquals("txt", Books.importExtension("Notes.Txt"))
    }

    @Test
    fun unknownOrMissingExtensionDefaultsToPdf() {
        // The core still content-sniffs on open, so a disguised PDF/ZIP resolves by its bytes; only a
        // truly unknown container falls back to the pdf backend — and [Books.acceptsImport] refuses
        // one whose bytes aren't a PDF or ZIP (e.g. a Supernote .mark sidecar) before it is stored.
        assertEquals("pdf", Books.importExtension("annotated.mark"))
        assertEquals("pdf", Books.importExtension("noextension"))
        assertEquals("pdf", Books.importExtension("archive.zip"))
        assertEquals("pdf", Books.importExtension("")) // empty display name
        assertEquals("pdf", Books.importExtension("file.")) // trailing dot → empty suffix
    }

    @Test
    fun usesTheLastDotSegment() {
        assertEquals("epub", Books.importExtension("My.Book.v2.epub"))
        // A dotfile whose whole name is a supported suffix still resolves by that suffix.
        assertEquals("txt", Books.importExtension(".txt"))
    }

    // ---- acceptsImport: refuse files the core can't open instead of storing them as a broken PDF ----

    private val zipHead = "PK\u0003\u0004\u0014\u0000mimetypeapplication/epub+zip".toByteArray(Charsets.ISO_8859_1)
    private val pdfHead = "%PDF-1.7\n".toByteArray(Charsets.US_ASCII)
    // The real header of a Supernote annotation sidecar (`<book>.epub.mark`).
    private val markHead = "markSN_FILE_VER_20260016.\u0001\u0000\u0000<FILE_TYPE:MARK>".toByteArray(Charsets.ISO_8859_1)

    @Test
    fun rejectsSupernoteMarkSidecar() {
        // Regression: picking `Book.epub.mark` (next to `Book.epub`) imported it as a PDF that
        // could never open, leaving the reader stuck on "Loading…".
        assertFalse(Books.acceptsImport("Wonder Book of Geometry, The - David Acheson.epub.mark", markHead))
    }

    @Test
    fun trustsSupportedExtensionsRegardlessOfBytes() {
        // Plain text has no signature; the core reports a damaged file of a known type on open.
        assertTrue(Books.acceptsImport("notes.txt", "hello".toByteArray()))
        assertTrue(Books.acceptsImport("book.epub", zipHead))
        assertTrue(Books.acceptsImport("paper.PDF", ByteArray(0)))
    }

    @Test
    fun acceptsRecognizableBytesUnderAnUnknownName() {
        assertTrue(Books.acceptsImport("noextension", pdfHead))
        assertTrue(Books.acceptsImport("download.bin", zipHead))
        // PDFs may carry junk before the header (within the first KiB).
        assertTrue(Books.acceptsImport("scan", ByteArray(100) { 0x20 } + pdfHead))
        assertFalse(Books.acceptsImport("scan", ByteArray(Books.SNIFF_BYTES) { 0x20 } + pdfHead))
    }

    @Test
    fun rejectsUnknownNameWithUnknownBytes() {
        assertFalse(Books.acceptsImport("noextension", "just some bytes".toByteArray()))
        assertFalse(Books.acceptsImport("empty", ByteArray(0)))
    }
}
