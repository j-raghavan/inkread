package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.net.Uri
import android.os.Environment
import android.provider.Settings
import android.util.Log
import android.widget.Toast
import java.io.File

/**
 * Annotation export to PDF (ADR-INKREAD-0005), extracted from `ReaderActivity` (SRP). Owns the
 * export-mode chooser, the all-files-access gate, and the engine-thread write that lands an
 * `-annotated.pdf` in a Supernote Partner-synced folder.
 *
 * inkread reads a PRIVATE copy of the source PDF, so it never overwrites the original in place; it
 * writes beside the original when that file can be found in the synced roots, else into the default
 * export folder.
 */
class ExportController(private val host: Host) {

    /** What export needs from the reader shell. */
    interface Host {
        /** Context for dialogs/toasts and `runOnUiThread`. */
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
     * Export the annotations into the PDF (ADR-INKREAD-0005). Lets the user pick editable PDF
     * annotations vs. flattened (baked-in) content, then writes it back. Writing modifies a file in
     * synced public storage, so confirm first; the heavy lifting is on the engine thread.
     */
    fun showExportDialog() {
        val path = host.currentDocPath
        if (path == null || host.docHandle == 0L) {
            Toast.makeText(activity, "No open document to export", Toast.LENGTH_SHORT).show()
            return
        }
        // inkread reads a PRIVATE copy of the PDF; to make the export visible on the desktop it must
        // land in a Partner-synced PUBLIC folder, which needs all-files access (Android 11+).
        if (!Environment.isExternalStorageManager()) {
            AlertDialog.Builder(activity, R.style.InkDialog)
                .setTitle("Allow file access to export")
                .setMessage("To save annotated PDFs into your synced $EXPORT_DIR_NAME folder (so they appear on your computer), inkread needs \"All files access\". Grant it on the next screen, then export again.")
                .setPositiveButton("Open settings") { _, _ ->
                    val uri = Uri.parse("package:${activity.packageName}")
                    runCatching {
                        activity.startActivity(Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, uri))
                    }.onFailure {
                        activity.startActivity(Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION))
                    }
                }
                .setNegativeButton("Cancel", null)
                .show()
            return
        }
        // NOTE: AlertDialog shows EITHER a message OR an items list, not both — choices go in labels.
        AlertDialog.Builder(activity, R.style.InkDialog)
            .setTitle("Export annotated PDF to $EXPORT_DIR_NAME")
            .setItems(
                arrayOf(
                    "Editable annotations (Adobe / Preview)",
                    "Flatten — shows everywhere (incl. Partner app)",
                ),
            ) { _, which ->
                val flatten = which == 1
                Toast.makeText(activity, "Exporting…", Toast.LENGTH_SHORT).show()
                host.engineExecute { runExport(path, flatten) }
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    /**
     * Write the annotated PDF to public storage (engine thread). inkread only holds a private copy
     * of the source (opened via the picker); it writes the result into a Partner-synced location:
     *  - safe (default): a `-annotated.pdf` **beside the original** if found, else into [EXPORT_DIR_NAME];
     *  - overwrite ([AppSettings.overwriteOnExport]): back onto the located original in place — only
     *    when that original is actually found in the synced roots, else it falls back to the safe copy.
     */
    private fun runExport(srcPath: String, flatten: Boolean) {
        val srcName = File(srcPath).name
        val baseName = srcName.removeSuffix(".pdf").removeSuffix(".PDF")
        val originalParent = findOriginalParent(srcName)
        val overwrote = AppSettings.overwriteOnExport(activity) && originalParent != null
        val outFile = if (overwrote) {
            File(originalParent, srcName) // replace the user's original in place
        } else {
            val outDir = originalParent ?: File(Environment.getExternalStorageDirectory(), EXPORT_DIR_NAME)
            outDir.mkdirs()
            File(outDir, "$baseName-annotated.pdf")
        }
        val ok = try {
            NativeBridge.nativeExportPdf(host.docHandle, outFile.absolutePath, flatten)
            Log.i(TAG, "DIAG export OK → ${outFile.absolutePath} (flatten=$flatten)")
            true
        } catch (e: RuntimeException) {
            Log.e(TAG, "export failed: ${e.message}"); false
        }
        val rel = outFile.absolutePath
            .removePrefix(Environment.getExternalStorageDirectory().absolutePath + "/")
        activity.runOnUiThread {
            val msg = when {
                !ok -> "Export failed"
                overwrote -> "Replaced $rel — sync to see it"
                else -> "Saved to $rel — sync to see it"
            }
            Toast.makeText(activity, msg, Toast.LENGTH_LONG).show()
        }
    }

    /** Find the folder holding the original PDF (so the export lands beside it). Searches the
     *  Supernote-synced roots a few levels deep; null if not found (then the caller uses a default). */
    private fun findOriginalParent(fileName: String): File? {
        val root = Environment.getExternalStorageDirectory()
        for (dir in SYNCED_DIRS) {
            val r = File(root, dir)
            if (!r.isDirectory) continue
            val hit = r.walkTopDown().maxDepth(5)
                .firstOrNull { it.isFile && it.name == fileName }
            if (hit != null) return hit.parentFile
        }
        return null
    }

    private companion object {
        const val TAG = "ExportController"

        /** Default export folder under external storage (a Supernote Partner-synced location). */
        const val EXPORT_DIR_NAME = "Document"

        /** Supernote-synced roots searched to place the export beside the original PDF. */
        val SYNCED_DIRS = arrayOf("Document", "EXPORT", "Note", "INBOX", "MyStyle", "Download")
    }
}
