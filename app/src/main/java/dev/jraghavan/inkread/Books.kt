package dev.jraghavan.inkread

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.provider.OpenableColumns
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.InputStream
import java.io.OutputStream

/**
 * A minimal on-device book library (RR17) for the shell: imported PDFs live under
 * `filesDir/books/` (kept, not overwritten) so [HomeActivity] and the reader's Library popup can
 * list and reopen them. The Rust core owns reading state per book id; here a book's **file name**
 * is its stable id.
 */
object Books {
    /** The books directory (created on demand). */
    fun dir(context: Context): File = File(context.filesDir, "books").apply { mkdirs() }

    /** Supported document extensions (the core dispatches a backend by extension). */
    private val SUPPORTED = setOf("pdf", "epub")

    /** Every imported document (PDF or EPUB), sorted by name (case-insensitive). */
    fun list(context: Context): List<File> =
        dir(context)
            .listFiles { f -> f.isFile && f.extension.lowercase() in SUPPORTED }
            ?.sortedBy { it.name.lowercase() }
            ?: emptyList()

    /**
     * Copy a SAF-picked document into the books dir under a sanitized name derived from its display
     * name, **preserving the extension** (`.epub` → reflowable, else `.pdf`) so the core opens the
     * right backend. Returns the stored file (or null on failure). A re-import of the same display
     * name overwrites — the name is the book's identity. Runs IO; call off the UI thread.
     */
    fun importFrom(context: Context, uri: Uri): File? {
        val raw = queryName(context, uri) ?: "document"
        val ext = if (raw.substringAfterLast('.', "").equals("epub", ignoreCase = true)) "epub" else "pdf"
        val dest = File(dir(context), "${sanitize(raw)}.$ext")
        return try {
            val ok = context.contentResolver.openInputStream(uri)?.use { input ->
                dest.outputStream().use { out -> copyCapped(input, out, MAX_IMPORT_BYTES) }
            } ?: return null
            // Over the cap: the core would reject this at open anyway (2 GiB), so don't keep a
            // huge/partial copy wasting storage. Match the native limit, fail before fully writing.
            if (!ok) { dest.delete(); return null }
            dest
        } catch (e: Exception) {
            dest.delete() // never leave a partial copy behind on an IO failure
            null
        }
    }

    /** Stream [input] → [out], aborting once more than [max] bytes have been read (mirrors the native
     *  open cap so a file the core would refuse never gets fully written). Returns false if the source
     *  exceeded the cap, true otherwise. */
    private fun copyCapped(input: InputStream, out: OutputStream, max: Long): Boolean {
        val buf = ByteArray(64 * 1024)
        var total = 0L
        while (true) {
            val n = input.read(buf)
            if (n < 0) return true
            total += n
            if (total > max) return false
            out.write(buf, 0, n)
        }
    }

    /** A human title for a stored book file (drop the extension). */
    fun title(file: File): String = file.nameWithoutExtension

    // ---- real document metadata (title/author/page position), captured by the reader on open ----

    private fun meta(context: Context) =
        context.getSharedPreferences("bookmeta", Context.MODE_PRIVATE)

    /**
     * Record the real document metadata for book `id` (from the core's `DocumentMetadata` + page
     * count), so the home/library can show the actual title/author and reading position rather than
     * the filename. Blank title/author are not stored (the filename stays the fallback).
     */
    fun setMeta(context: Context, id: String, title: String, author: String, pages: Int) {
        if (id.isEmpty()) return
        meta(context).edit().apply {
            if (title.isNotBlank()) putString("$id.title", title.trim())
            if (author.isNotBlank()) putString("$id.author", author.trim())
            if (pages > 0) putInt("$id.pages", pages)
        }.apply()
    }

    /** Record the current 0-based page for book `id` (written by the reader on save). */
    fun setPage(context: Context, id: String, page: Int) {
        if (id.isEmpty()) return
        meta(context).edit().putInt("$id.page", page.coerceAtLeast(0)).apply()
    }

    /** The stored real title for book `id`, or null if unknown (caller falls back to the filename). */
    fun metaTitle(context: Context, id: String): String? =
        meta(context).getString("$id.title", null)?.ifBlank { null }

    fun metaAuthor(context: Context, id: String): String? =
        meta(context).getString("$id.author", null)?.ifBlank { null }

    /** Total pages for book `id` (0 if unknown). */
    fun metaPages(context: Context, id: String): Int = meta(context).getInt("$id.pages", 0)

    /** Current 0-based page for book `id` (0 if unknown). */
    fun metaPage(context: Context, id: String): Int = meta(context).getInt("$id.page", 0)

    /** The display title for a recent: the real document title if captured, else the file name. */
    fun displayTitle(context: Context, r: Recent): String =
        metaTitle(context, r.id) ?: File(r.path).nameWithoutExtension

    // ---- first-page thumbnails (RR17-FR5) ----

    private fun thumbsDir(context: Context): File = File(context.filesDir, "thumbnails").apply { mkdirs() }

    /** The cached first-page thumbnail PNG for book `id` (may not exist yet). */
    fun thumbFile(context: Context, id: String): File = File(thumbsDir(context), "${id.hashCode()}.png")

    /** Scale a rendered page down and cache it as the book's first-page thumbnail. Best-effort. */
    fun saveThumbnail(context: Context, id: String, page: Bitmap) {
        if (page.width <= 0 || page.height <= 0) return
        try {
            val w = THUMB_W
            val h = (page.height.toFloat() / page.width * w).toInt().coerceAtLeast(1)
            val thumb = Bitmap.createScaledBitmap(page, w, h, true)
            thumbFile(context, id).outputStream().use { thumb.compress(Bitmap.CompressFormat.PNG, 90, it) }
        } catch (e: Exception) {
            // a missing thumbnail just shows a placeholder — never fatal.
        }
    }

    /** Load a book's cached thumbnail, or null if none. */
    fun loadThumbnail(context: Context, id: String): Bitmap? {
        val f = thumbFile(context, id)
        return if (f.exists()) BitmapFactory.decodeFile(f.absolutePath) else null
    }

    // ---- recently opened (RR17) ----

    /** A recently-opened book: its stable id (file name) and current stored path. */
    data class Recent(val id: String, val path: String) {
        val title: String get() = File(path).nameWithoutExtension
    }

    private fun recentsFile(context: Context): File = File(context.filesDir, "recents.json")

    /** Recently-opened books, most-recent first, skipping any whose file no longer exists. */
    fun recents(context: Context): List<Recent> {
        val f = recentsFile(context)
        if (!f.exists()) return emptyList()
        return try {
            val arr = JSONArray(f.readText())
            (0 until arr.length()).mapNotNull {
                val o = arr.getJSONObject(it)
                val path = o.optString("path")
                val id = o.optString("id")
                if (path.isNotEmpty() && File(path).exists()) Recent(id, path) else null
            }
        } catch (e: Exception) {
            emptyList()
        }
    }

    /** Record a book as most-recently opened (dedup by path, capped). */
    fun pushRecent(context: Context, id: String, path: String) {
        val updated = (listOf(Recent(id, path)) + recents(context).filterNot { it.path == path }).take(RECENTS_MAX)
        try {
            val arr = JSONArray()
            for (r in updated) arr.put(JSONObject().put("id", r.id).put("path", r.path))
            recentsFile(context).writeText(arr.toString())
        } catch (e: Exception) {
            // a lost recents entry is cosmetic.
        }
    }

    /** Per-book read progress (0–100), written by the reader on save; shown on the home shelf. */
    fun setProgress(context: Context, id: String, percent: Int) {
        if (id.isEmpty()) return
        context.getSharedPreferences("progress", Context.MODE_PRIVATE)
            .edit().putInt(id, percent.coerceIn(0, 100)).apply()
    }

    fun progress(context: Context, id: String): Int =
        context.getSharedPreferences("progress", Context.MODE_PRIVATE).getInt(id, 0)

    private const val THUMB_W = 360
    private const val RECENTS_MAX = 12
    // Cap a SAF import at the native open limit (2 GiB) so an oversized/unbounded source stream is
    // rejected before it's fully copied into app storage, not after (the review's import-cap note).
    private const val MAX_IMPORT_BYTES = 2L shl 30

    private fun queryName(context: Context, uri: Uri): String? =
        context.contentResolver
            .query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
            ?.use { c -> if (c.moveToFirst()) c.getString(0) else null }

    private fun sanitize(name: String): String {
        val stem = name.substringBeforeLast('.').ifBlank { "document" }
        return stem.replace(Regex("[^A-Za-z0-9 ._-]"), "_").trim().take(80).ifBlank { "document" }
    }
}
