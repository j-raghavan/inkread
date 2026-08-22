package dev.jraghavan.inkread

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File

/**
 * Host JVM tests for [Books.hasNotes] — the `HAS NOTES` flag on the shelf (#227).
 *
 * The flag was reported as always-on, and it was: it tested whether the `.inkread` sidecar existed,
 * but the core stamps a `metadata.json` in there on the first open of *any* document to bind the
 * sidecar to that document's identity. So every book that had ever been opened claimed handwriting,
 * which also made the Remove dialog warn about notes that were not there.
 *
 * These fixtures build the sidecar the way the core lays it out (`SidecarPaths`), so the shapes
 * below are the real ones rather than an approximation of them.
 */
class ShelfNotesFlagTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private fun book(name: String = "book.epub"): File = tmp.newFile(name)

    /** `book.epub` → sibling `book.inkread/`, as [Books.sidecarDir] maps it. */
    private fun sidecar(book: File): File =
        File(book.parentFile, "${book.nameWithoutExtension}.inkread").apply { mkdirs() }

    private fun annotations(book: File): File =
        File(sidecar(book), "annotations").apply { mkdirs() }

    @Test
    fun aBookThatWasNeverOpenedHasNoNotes() {
        assertFalse(Books.hasNotes(book()))
    }

    /** The regression: opening a book stamps the sidecar, and that used to read as handwriting. */
    @Test
    fun anOpenedButUnannotatedBookHasNoNotes() {
        val b = book()
        File(sidecar(b), "metadata.json").writeText("""{"fingerprint":"abc"}""")
        assertFalse("a stamped sidecar is not handwriting", Books.hasNotes(b))
    }

    /** An empty `annotations/` is what a page erased back to nothing leaves behind. */
    @Test
    fun anEmptyAnnotationsDirectoryHasNoNotes() {
        val b = book()
        annotations(b)
        assertFalse(Books.hasNotes(b))
    }

    @Test
    fun aCommittedStrokePageCountsAsNotes() {
        val b = book()
        File(annotations(b), "page-0001.inkbin").writeBytes(byteArrayOf(1, 2, 3))
        assertTrue(Books.hasNotes(b))
    }

    /**
     * A quarantined page is ink the reader can no longer see, so it must not claim otherwise — and
     * the suffix must not be matched loosely, which `endsWith(".inkbin")` would have done.
     */
    @Test
    fun aQuarantinedCorruptPageDoesNotCountAsNotes() {
        val b = book()
        File(annotations(b), "page-0001.inkbin.corrupt").writeBytes(byteArrayOf(1))
        assertFalse(Books.hasNotes(b))
    }

    /** Exports and thumbnails share the sidecar and are not handwriting either. */
    @Test
    fun otherSidecarContentIsNotMistakenForNotes() {
        val b = book()
        File(sidecar(b), "exports").apply { mkdirs() }
        File(File(sidecar(b), "exports"), "book-annotated.pdf").writeBytes(byteArrayOf(1))
        File(sidecar(b), "thumbnails").apply { mkdirs() }
        assertFalse(Books.hasNotes(b))
    }

    /** Two books in one directory must not borrow each other's ink. */
    @Test
    fun notesBelongToTheBookTheySitBeside() {
        val annotated = book("annotated.epub")
        val plain = book("plain.epub")
        File(annotations(annotated), "page-0001.inkbin").writeBytes(byteArrayOf(1))
        assertTrue(Books.hasNotes(annotated))
        assertFalse(Books.hasNotes(plain))
    }
}
