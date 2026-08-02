package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
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
        // truly unknown container (e.g. a proprietary .mark) falls back to the pdf backend.
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
}
