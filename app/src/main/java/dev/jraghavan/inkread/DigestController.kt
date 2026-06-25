package dev.jraghavan.inkread

import android.app.Activity
import android.content.ContentUris
import android.content.ContentValues
import android.net.Uri
import android.util.Log
import android.widget.Toast
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/**
 * Saves a lasso selection into the **Supernote Digest** (ADR-INKREAD-0010). The firmware calls this
 * feature "Knowledge": an exported `ContentProvider`
 * (`content://com.ratta.supernote.knowledge.provider`) backs the Digest app, gated by two
 * protectionLevel-`normal` permissions (declared in the manifest) that are auto-granted to this
 * sideloaded app at install. We mirror the native document reader's `insertDigest` map exactly so
 * the entry shows up — and opens at the right page — in the stock Digest app.
 *
 * This is the single place that names the vendor (IR-7): the rest of inkread speaks lasso bounds and
 * page numbers, never `com.ratta.*`.
 *
 * **v1 scope (page-level).** `content` is the PDF text under the selection; `source_page` makes the
 * Digest entry jump back to the page. The precise in-page highlight needs mupdf-compatible character
 * offsets (`startPosition`/`endPosition`) which pdfium can't reproduce 1:1 yet, so the location is
 * emitted page-only (`start=end=0`) for now — see the location note below.
 */
class DigestController(private val host: Host) {

    /** What digest-write needs from the reader shell. */
    interface Host {
        /** Context for toasts / `runOnUiThread`. */
        val activity: Activity

        /** The open document handle (`0` = none). */
        val docHandle: Long

        /** Filesystem path of the open document (the private copy), or null if none. */
        val currentDocPath: String?

        /** Run [block] on the single engine thread (serializes native access). */
        fun engineExecute(block: () -> Unit)
    }

    private val activity: Activity get() = host.activity

    /**
     * Save the lasso selection on [page] (bounds in normalized doc coords `[nx0,ny0,nx1,ny1]`) to the
     * Digest. Extracts the PDF text under the selection on the engine thread (native), then inserts
     * via the Knowledge provider. No-ops with a toast when there's nothing to save.
     */
    fun addDigest(page: Int, boundsNorm: FloatArray) {
        val path = host.currentDocPath
        if (path == null || host.docHandle == 0L || boundsNorm.size != 4) {
            toast("Nothing to add to Digest")
            return
        }
        host.engineExecute {
            val text = try {
                WireCodec.decodeSelection(
                    NativeBridge.nativeTextInRect(
                        host.docHandle, page, boundsNorm[0], boundsNorm[1], boundsNorm[2], boundsNorm[3],
                    ),
                ).text.trim()
            } catch (e: RuntimeException) {
                Log.e(TAG, "textInRect failed: ${e.message}"); ""
            }
            if (text.isEmpty()) {
                toast("No selectable text under the selection")
                return@engineExecute
            }
            val ok = insertDigest(path, page, text, selectionAnchor(page, boundsNorm))
            toast(if (ok) "Added to Digest" else "Couldn't add to Digest")
        }
    }

    /**
     * Save already-selected text on [page] to the Digest. Used by the printed-text lasso path, which
     * has resolved the selection itself — so no native extraction is needed, just the insert.
     * [boundsNorm] (the selection's normalized bounding rect, or null) yields the reflow-stable anchor.
     */
    fun addDigestText(page: Int, content: String, boundsNorm: FloatArray?) {
        val path = host.currentDocPath
        val text = content.trim()
        if (path == null || host.docHandle == 0L || text.isEmpty()) {
            toast("Nothing to add to Digest")
            return
        }
        host.engineExecute {
            val ok = insertDigest(path, page, text, boundsNorm?.let { selectionAnchor(page, it) })
            toast(if (ok) "Added to Digest" else "Couldn't add to Digest")
        }
    }

    /**
     * The reflow-stable `{"start":…,"end":…}` PinPosition anchor for the selection rect, or null for
     * fixed-layout PDF / an empty selection (the core returns an empty string, #46). Engine thread.
     */
    private fun selectionAnchor(page: Int, boundsNorm: FloatArray): String? {
        if (boundsNorm.size != 4) return null
        return try {
            NativeBridge.nativeSelectionPins(
                host.docHandle, page, boundsNorm[0], boundsNorm[1], boundsNorm[2], boundsNorm[3],
            ).ifEmpty { null }
        } catch (e: RuntimeException) {
            Log.e(TAG, "selectionPins failed: ${e.message}"); null
        }
    }

    /**
     * Insert one Digest row, mirroring the native document reader's `KnowledgeDatabaseManager`:
     * `content`, `source_type=1` (document), `source_path`, `source_page`, and a `metadata` JSON that
     * nests `document_location_data` + `source_size`. `creation_time`/`last_modified_time` are left
     * for the provider to fill, exactly as the firmware does.
     */
    private fun insertDigest(srcPath: String, page: Int, content: String, anchorJson: String?): Boolean {
        val values = ContentValues().apply {
            put(COL_CONTENT, content)
            put(COL_SOURCE_TYPE, SOURCE_TYPE_DOCUMENT)
            put(COL_SOURCE_PATH, srcPath)
            put(COL_SOURCE_PAGE, page.toString())
            put(COL_METADATA, buildMetadata(srcPath, page, anchorJson))
        }
        return try {
            val uri = activity.contentResolver.insert(Uri.parse(INSERT_URI), values)
            val id = uri?.let { ContentUris.parseId(it) } ?: -1L
            Log.i(TAG, "DIAG digest insert → id=$id page=$page len=${content.length}")
            id > 0
        } catch (e: SecurityException) {
            // WRITE_KNOWLEDGE missing (provider absent on a non-Supernote device, or perm denied).
            Log.e(TAG, "digest insert denied: ${e.message}"); false
        } catch (e: RuntimeException) {
            Log.e(TAG, "digest insert failed: ${e.message}"); false
        }
    }

    /**
     * `metadata` JSON. `document_location_data` is itself a JSON string — a `[{chapter,page,
     * startPosition,endPosition}]` array (mupdf coordinate model), emitted page-only (`start=end=0`):
     * the entry opens at [page] but doesn't restore a precise PDF highlight span until we calibrate
     * pdfium offsets against a real mupdf sample. `source_size` is the source byte size. For a
     * reflowable doc, [anchorJson] adds an inkread-private reflow-stable PinPosition anchor (#46).
     */
    private fun buildMetadata(srcPath: String, page: Int, anchorJson: String?): String {
        val location = JSONArray().put(
            JSONObject()
                .put("chapter", 0)
                .put("page", page)
                .put("startPosition", 0)
                .put("endPosition", 0),
        )
        val size = runCatching { File(srcPath).length() }.getOrDefault(0L)
        val metadata = JSONObject()
            .put(KEY_DOCUMENT_LOCATION_DATA, location.toString())
            .put(KEY_SOURCE_SIZE, size)
        // Reflowable docs (EPUB / reflowed PDF) carry an inkread-private reflow-stable PinPosition
        // anchor under our own key (#46). The stock Digest app ignores unknown keys and the vendor
        // `document_location_data` is untouched, so fixed-layout PDF behaviour is unchanged.
        if (anchorJson != null) {
            metadata.put(KEY_INKREAD_ANCHOR, JSONObject(anchorJson))
        }
        return metadata.toString()
    }

    private fun toast(msg: String) =
        activity.runOnUiThread { Toast.makeText(activity, msg, Toast.LENGTH_SHORT).show() }

    private companion object {
        const val TAG = "DigestController"

        // --- Supernote "Knowledge" provider contract (the only vendor-named surface; IR-7). ---
        const val INSERT_URI = "content://com.ratta.supernote.knowledge.provider/knowledge/insert"

        const val SOURCE_TYPE_DOCUMENT = 1 // 1=DOCUMENT(PDF), 2=NOTE, 3=PASTEBOARD, 4=SELF_ADD

        const val COL_CONTENT = "content"
        const val COL_SOURCE_TYPE = "source_type"
        const val COL_SOURCE_PATH = "source_path"
        const val COL_SOURCE_PAGE = "source_page"
        const val COL_METADATA = "metadata"

        const val KEY_DOCUMENT_LOCATION_DATA = "document_location_data"
        const val KEY_SOURCE_SIZE = "source_size"

        // inkread-private (not a vendor key): the reflow-stable PinPosition anchor for EPUB digests.
        const val KEY_INKREAD_ANCHOR = "inkread_anchor"
    }
}
