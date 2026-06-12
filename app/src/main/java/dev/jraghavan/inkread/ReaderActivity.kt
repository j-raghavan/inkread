package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.net.Uri
import android.os.Bundle
import android.util.Log
import android.view.GestureDetector
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.widget.Toast
import dev.jraghavan.inkread.eink.EinkAdapter
import dev.jraghavan.inkread.eink.SupernoteEinkAdapter
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.Executors

/**
 * The M0/M1a reader Activity (RR1-FR2, RR21) — **DEVICE-UNVERIFIED**.
 *
 * Owns the [SurfaceView], drives the JNI round-trip (init → open → render → blit), and on a
 * tap forwards a [Gesture] to the core, then hands the returned [RefreshCommand] stream to
 * the [EinkAdapter] for execution. The Rust core owns all document/policy logic; this shell
 * only marshals + presents (IR-1/IR-2).
 *
 * ## Threading (RR21-FR4 / RR24): engine calls off the UI thread
 * Opening + rendering a PDF (pdfium) can take seconds on an image-heavy page; doing it on the
 * main thread froze the app (ANR-precursor). All document/engine work runs on a **single**
 * background executor (`engine`) — single-threaded so pdfium access is serialized (the core
 * assumes one worker thread). The UI thread only enqueues tasks and shows a quick "Loading…"
 * frame. Engine-thread-only fields ([docHandle], [bitmap], [renderBuffer], [viewW]/[viewH]) are
 * touched solely on that thread; a re-entrant close is safe (Amendment 2).
 */
class ReaderActivity : Activity(), SurfaceHolder.Callback {

    private lateinit var surfaceView: SurfaceView
    private val adapter: EinkAdapter = SupernoteEinkAdapter()

    /** Single worker thread for all engine/JNI/document work (serialized per RR21). */
    private val engine = Executors.newSingleThreadExecutor { r -> Thread(r, "inkread-engine") }

    // ---- engine-thread-only state ----
    private var docHandle: Long = 0L
    private var bitmap: Bitmap? = null
    private var renderBuffer: ByteBuffer? = null
    private var viewW = 0
    private var viewH = 0

    private val loadingBg = Paint().apply { color = Color.WHITE }
    private val loadingText = Paint().apply {
        color = Color.DKGRAY
        textSize = 48f
        isAntiAlias = true
        textAlign = Paint.Align.CENTER
    }

    /** Shell-side state: which book to reopen on launch (RR27); the page itself lives in the core. */
    private val prefs by lazy { getSharedPreferences(PREFS, MODE_PRIVATE) }

    /** Tap zones for page turns + long-press to open a PDF (RR22/RR25). */
    private val gestures by lazy {
        GestureDetector(this, object : GestureDetector.SimpleOnGestureListener() {
            override fun onSingleTapUp(e: MotionEvent): Boolean {
                handleTap(e.x)
                return true
            }

            override fun onLongPress(e: MotionEvent) = openPicker()
        })
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Prove the JNI boundary up front (RR1-AC2). Cheap; fine on the UI thread.
        Log.i(TAG, "core: ${NativeBridge.nativeHello()}")

        surfaceView = SurfaceView(this)
        surfaceView.holder.addCallback(this)
        surfaceView.setOnTouchListener { _, event -> gestures.onTouchEvent(event) }
        setContentView(surfaceView)
    }

    // ---- SurfaceHolder lifecycle → core (RR21-FR4) ----

    override fun surfaceCreated(holder: SurfaceHolder) { /* size arrives in surfaceChanged */ }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        // Hand the slow open+render to the engine thread; show feedback immediately.
        engine.execute { onSurfaceSized(width, height) }
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) { /* keep the doc open across resizes */ }

    override fun onPause() {
        super.onPause()
        // Persist the reading position when backgrounded (RR27) — async on the engine thread.
        engine.execute { savePosition() }
    }

    /** Launch the system file picker for a PDF (RR22). */
    private fun openPicker() {
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = "application/pdf"
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        try {
            startActivityForResult(intent, REQ_OPEN_DOC)
        } catch (e: android.content.ActivityNotFoundException) {
            Toast.makeText(this, "No file picker available", Toast.LENGTH_SHORT).show()
        }
    }

    @Deprecated("startActivityForResult is fine for this single-Activity shell (no AndroidX).")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQ_OPEN_DOC || resultCode != RESULT_OK) return
        val uri = data?.data ?: return
        // Best-effort: keep read access in case we re-import later (the open path copies bytes now).
        try {
            contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
        } catch (e: SecurityException) {
            Log.w(TAG, "no persistable permission for $uri: ${e.message}")
        }
        engine.execute { importAndOpen(uri) }
    }

    override fun onDestroy() {
        engine.execute { closeDocument() }
        engine.shutdown() // lets the queued close run, then stops the worker
        super.onDestroy()
    }

    // ---- engine-thread work ----

    private fun onSurfaceSized(width: Int, height: Int) {
        viewW = width
        viewH = height
        bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        // A direct, tightly-packed RGBA buffer the core renders into (Fork 4 / Amendment 5).
        renderBuffer = ByteBuffer.allocateDirect(width * height * 4).order(ByteOrder.LITTLE_ENDIAN)

        drawLoading() // quick feedback while the (slow) open runs
        openDocumentIfNeeded()
        renderAndBlit()
    }

    private fun openDocumentIfNeeded() {
        if (docHandle != 0L) return
        val book = resolveBook()
        if (book == null) {
            Log.w(TAG, "no book to open; long-press to pick a PDF (RR22)")
            runOnUiThread {
                Toast.makeText(this, "Long-press to open a PDF", Toast.LENGTH_LONG).show()
            }
            return
        }
        openBook(book.first, book.second)
    }

    /**
     * Choose which book to open: the last-opened one (RR27 — reopen where you were), else the
     * bring-up `sample.pdf` placed via adb. Returns `(filesystemPath, stableBookId)` or null.
     */
    private fun resolveBook(): Pair<String, String>? {
        val savedPath = prefs.getString(KEY_BOOK_PATH, null)
        val savedId = prefs.getString(KEY_BOOK_ID, null)
        if (savedPath != null && savedId != null && File(savedPath).exists()) {
            return savedPath to savedId
        }
        // External files dir first, then internal filesDir — the latter is adb/`run-as`-writable
        // under Android 11 scoped storage, so a bring-up PDF can be placed without SAF.
        val sample = listOf(
            File(getExternalFilesDir(null), "sample.pdf"),
            File(filesDir, "sample.pdf"),
        ).firstOrNull(File::exists) ?: return null
        return sample.absolutePath to sample.name
    }

    /**
     * Open `path` with a SQLite store keyed by `bookId` so the reading position + e-ink settings
     * resume per document (RR12 / RR27). The store lives under app storage.
     */
    private fun openBook(path: String, bookId: String) {
        val capsBytes = WireCodec.encodeCapabilities(adapter.capabilities())
        NativeBridge.nativeInit(capsBytes)
        val dbPath = File(filesDir, "reader.db").absolutePath
        docHandle = try {
            NativeBridge.nativeOpenDocumentWithStore(path, capsBytes, viewW, viewH, DPI, dbPath, bookId)
        } catch (e: RuntimeException) {
            Log.e(TAG, "open failed: ${e.message}")
            0L
        }
        if (docHandle != 0L) {
            Log.i(
                TAG,
                "opened $bookId: ${NativeBridge.nativePageCount(docHandle)} pages, " +
                    "resumed at page ${NativeBridge.nativeCurrentPage(docHandle)}",
            )
        }
    }

    /**
     * Import a SAF-picked PDF (RR22): copy its bytes into app storage, remember it as the current
     * book, then swap the open document on the engine thread. Runs on the engine thread (IO +
     * serialized engine access, RR21). The book id is the content URI (clamped) so the position
     * resumes per document even though the bytes are re-copied to a fixed path.
     */
    private fun importAndOpen(uri: Uri) {
        val dest = File(filesDir, "current.pdf")
        try {
            contentResolver.openInputStream(uri)?.use { input ->
                dest.outputStream().use { out -> input.copyTo(out) }
            } ?: run {
                Log.e(TAG, "cannot open stream for $uri")
                return
            }
        } catch (e: Exception) {
            Log.e(TAG, "import failed: ${e.message}")
            return
        }
        val bookId = uri.toString().take(BOOK_ID_MAX)
        prefs.edit()
            .putString(KEY_BOOK_PATH, dest.absolutePath)
            .putString(KEY_BOOK_ID, bookId)
            .apply()

        closeDocument() // saves + closes the previous book before swapping
        drawLoading()
        openBook(dest.absolutePath, bookId)
        renderAndBlit()
    }

    private fun renderAndBlit() {
        val handle = docHandle
        val buf = renderBuffer ?: return
        val bmp = bitmap ?: return
        if (handle == 0L) return

        try {
            buf.clear()
            NativeBridge.nativeRenderPage(handle, buf)
        } catch (e: RuntimeException) {
            Log.e(TAG, "render failed: ${e.message}")
            return
        }
        buf.rewind()
        bmp.copyPixelsFromBuffer(buf)
        blit { canvas -> canvas.drawBitmap(bmp, 0f, 0f, null) }
    }

    /** Draw a centered "Loading…" frame so the open doesn't look like a freeze. */
    private fun drawLoading() {
        blit { canvas ->
            canvas.drawColor(Color.WHITE)
            canvas.drawRect(0f, 0f, canvas.width.toFloat(), canvas.height.toFloat(), loadingBg)
            canvas.drawText("Loading…", canvas.width / 2f, canvas.height / 2f, loadingText)
        }
    }

    /** Lock the surface, run [draw], and post — null-safe across surface destroy (any thread). */
    private inline fun blit(draw: (Canvas) -> Unit) {
        val holder = surfaceView.holder
        val canvas: Canvas =
            try {
                holder.lockCanvas() ?: return
            } catch (_: IllegalStateException) {
                return // surface went away mid-blit
            }
        try {
            draw(canvas)
        } finally {
            runCatching { holder.unlockCanvasAndPost(canvas) }
        }
    }

    // ---- input (UI thread) → engine ----

    /** Route a tap by zone (RR25-FR3): left third = prev, right third = next, center = contents. */
    private fun handleTap(x: Float) {
        val third = surfaceView.width / 3f
        when {
            x < third -> postGesture(Gesture.PREV_PAGE)
            x > 2 * third -> postGesture(Gesture.NEXT_PAGE)
            else -> openTocMenu()
        }
    }

    /** Apply a page-turn gesture on the engine thread, then render + refresh (RR25). */
    private fun postGesture(gesture: Gesture) {
        engine.execute {
            if (docHandle == 0L) return@execute
            val commandBytes = try {
                NativeBridge.nativeOnGesture(docHandle, gesture.code)
            } catch (e: RuntimeException) {
                Log.e(TAG, "gesture failed: ${e.message}")
                return@execute
            }
            renderAndBlit()
            // Execute the policy's refresh stream on the panel (RR2-FR3).
            adapter.executeAll(WireCodec.decodeCommands(commandBytes))
        }
    }

    /** Fetch the TOC on the engine thread, then show the chooser on the UI thread (RR11-FR2). */
    private fun openTocMenu() {
        engine.execute {
            if (docHandle == 0L) return@execute
            val toc = try {
                WireCodec.decodeToc(NativeBridge.nativeToc(docHandle))
            } catch (e: RuntimeException) {
                Log.e(TAG, "toc failed: ${e.message}")
                return@execute
            }
            runOnUiThread { showTocDialog(toc) }
        }
    }

    private fun showTocDialog(toc: List<TocItem>) {
        if (toc.isEmpty()) {
            Toast.makeText(this, "No contents in this document", Toast.LENGTH_SHORT).show()
            return
        }
        // Indent by depth; show the 1-based page for resolvable entries (RR11-FR2).
        val labels = toc.map { item ->
            val indent = "    ".repeat(item.depth)
            val page = item.targetPage?.let { "  ·  p${it + 1}" } ?: ""
            indent + item.title + page
        }.toTypedArray()

        AlertDialog.Builder(this)
            .setTitle("Contents")
            .setItems(labels) { _, which ->
                val page = toc[which].targetPage ?: return@setItems // label-only: no jump
                engine.execute {
                    if (docHandle == 0L) return@execute
                    val commandBytes = try {
                        NativeBridge.nativeJumpToPage(docHandle, page)
                    } catch (e: RuntimeException) {
                        Log.e(TAG, "jump failed: ${e.message}")
                        return@execute
                    }
                    renderAndBlit()
                    adapter.executeAll(WireCodec.decodeCommands(commandBytes))
                }
            }
            .show()
    }

    /** Persist the current reading position (RR12-FR3 / RR27); store-less / closed = no-op. */
    private fun savePosition() {
        if (docHandle == 0L) return
        try {
            NativeBridge.nativeSavePosition(docHandle)
        } catch (e: RuntimeException) {
            Log.e(TAG, "save position failed: ${e.message}")
        }
    }

    private fun closeDocument() {
        val h = docHandle
        docHandle = 0L // zero BEFORE the call so a re-entrant close is a no-op (Amendment 2)
        if (h == 0L) return
        try {
            NativeBridge.nativeSavePosition(h) // last-chance save before teardown (RR27)
        } catch (e: RuntimeException) {
            Log.e(TAG, "save position failed: ${e.message}")
        }
        NativeBridge.nativeCloseDocument(h)
    }

    private companion object {
        const val TAG = "ReaderActivity"
        const val DPI = 226 // Supernote-class panel density (approx); refined per device.
        const val REQ_OPEN_DOC = 1 // startActivityForResult request code for the PDF picker.
        const val PREFS = "inkread"
        const val KEY_BOOK_PATH = "book_path" // copied PDF under app storage (RR27).
        const val KEY_BOOK_ID = "book_id" // stable per-book identity (content URI, clamped).
        const val BOOK_ID_MAX = 512 // mirrors BookId::MAX_LEN in the core.
    }
}
