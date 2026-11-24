package dev.jraghavan.inkread

/**
 * The reader-facing message for a document that failed to open. The core throws
 * `"[<status>] <detail>"` (see `CoreError::status_code` in `reader-core/src/error.rs`); the detail
 * is backend diagnostics (e.g. a pdfium error enum) that mean nothing to a reader, so only the
 * status code picks the wording. Pure + host-testable.
 */
object OpenFailure {
    private const val UNSUPPORTED_FORMAT = 2
    private const val CORRUPT_DOCUMENT = 3
    private const val DRM_PROTECTED = 4

    private val STATUS = Regex("""^\[(\d+)]""")

    /** The status code at the head of a core error message, or null when there is none. */
    internal fun statusOf(coreMessage: String?): Int? =
        coreMessage?.let { STATUS.find(it.trimStart())?.groupValues?.get(1)?.toIntOrNull() }

    fun message(fileName: String, coreMessage: String?): String = when (statusOf(coreMessage)) {
        UNSUPPORTED_FORMAT, CORRUPT_DOCUMENT ->
            "Couldn't open \u201C$fileName\u201D \u2014 it isn't a PDF, EPUB, CBZ or text file, or it is damaged."
        DRM_PROTECTED -> "\u201C$fileName\u201D is DRM-protected and can't be opened."
        else -> "Couldn't open \u201C$fileName\u201D."
    }

    /** Shown when a picked file is refused at import (see [Books.acceptsImport]). */
    const val NOT_A_DOCUMENT = "That file isn't a PDF, EPUB, CBZ or text file."
}
