package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.net.Uri
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.widget.Toast
import dev.jraghavan.inkread.eink.EinkAdapter
import dev.jraghavan.inkread.eink.SupernoteEinkAdapter
import dev.jraghavan.inkread.eink.SupernoteInk
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

    /** Firmware stylus-ink client (RR19): the stylus inks via the Supernote pen daemon, the finger
     *  navigates. Claimed on focus, released on pause. */
    private val ink: SupernoteInk by lazy { SupernoteInk(this) }

    /** Uptime (ms) of the last stylus event (touch or hover). A finger touch within
     *  [PALM_REJECT_MS] of it is treated as a resting palm and ignored for navigation. */
    @Volatile private var lastStylusMs = 0L

    /** Single worker thread for all engine/JNI/document work (serialized per RR21). */
    private val engine = Executors.newSingleThreadExecutor { r -> Thread(r, "inkread-engine") }

    // ---- engine-thread-only state ----
    private var docHandle: Long = 0L
    private var bitmap: Bitmap? = null
    private var renderBuffer: ByteBuffer? = null
    private var viewW = 0
    private var viewH = 0

    /** Current page's links (RR11-FR3); written on the engine thread after each render, read on
     * the UI thread for tap hit-testing. */
    @Volatile private var currentLinks: List<LinkRect> = emptyList()

    // ---- handwriting (RR19) ----
    /** Per-book stroke store; engine-thread only. Null until a book is open. */
    private var inkStore: InkStore? = null
    /** 0-based page the strokes are keyed to; set on the engine thread after each render. */
    private var currentPage = 0
    /** The in-progress stroke as interleaved view-px x,y; UI-thread only. */
    private val strokeBuf = ArrayList<Float>()
    private val mainHandler = Handler(Looper.getMainLooper())
    /** Safety net for a swallowed stylus ACTION_UP: commit the stroke after a brief pen pause. */
    private val strokeFinalize = Runnable { finalizeStroke() }

    private val inkPaint = Paint().apply {
        color = Color.BLACK
        style = Paint.Style.STROKE
        strokeWidth = INK_STROKE_WIDTH // match the firmware needle (baked was thinner than live)
        strokeCap = Paint.Cap.ROUND
        strokeJoin = Paint.Join.ROUND
        isAntiAlias = true
    }
    private val inkDotPaint = Paint().apply { color = Color.BLACK; style = Paint.Style.FILL; isAntiAlias = true }

    private val loadingBg = Paint().apply { color = Color.WHITE }
    private val loadingText = Paint().apply {
        color = Color.DKGRAY
        textSize = 48f
        isAntiAlias = true
        textAlign = Paint.Align.CENTER
    }

    /** Shell-side state: which book to reopen on launch (RR27); the page itself lives in the core. */
    private val prefs by lazy { getSharedPreferences(PREFS, MODE_PRIVATE) }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Prove the JNI boundary up front (RR1-AC2). Cheap; fine on the UI thread.
        Log.i(TAG, "core: ${NativeBridge.nativeHello()}")

        surfaceView = SurfaceView(this)
        surfaceView.holder.addCallback(this)
        // Input model (RR19/RR25): the STYLUS inks (the firmware paints it — the app never
        // navigates on the pen), the FINGER navigates. Two device quirks shape this:
        //   • GMX swallows ACTION_UP for finger, so taps are handled on ACTION_DOWN.
        //   • While writing, the palm rests on the glass as a finger touch — reject any finger
        //     touch within PALM_REJECT_MS of a stylus event (touch or hover; see
        //     dispatchGenericMotionEvent).
        surfaceView.setOnTouchListener { _, event ->
            val tool = event.getToolType(0)
            if (tool == MotionEvent.TOOL_TYPE_STYLUS || tool == MotionEvent.TOOL_TYPE_ERASER) {
                // The firmware paints the live ink; the app captures the same points to bake +
                // persist them (RR19). The app never navigates on the pen.
                lastStylusMs = SystemClock.uptimeMillis()
                captureStylus(event)
            } else if (event.actionMasked == MotionEvent.ACTION_DOWN &&
                event.pointerCount == 1 &&
                tool == MotionEvent.TOOL_TYPE_FINGER &&
                SystemClock.uptimeMillis() - lastStylusMs > PALM_REJECT_MS
            ) {
                Log.i(TAG, "DIAG tap-down finger x=${event.x} y=${event.y}")
                handleTap(event.x, event.y)
            }
            true
        }
        // The panel refresh is routed through the view's context (Supernote "eink" service).
        adapter.attachView(surfaceView)
        setContentView(surfaceView)
    }

    // ---- SurfaceHolder lifecycle → core (RR21-FR4) ----

    override fun surfaceCreated(holder: SurfaceHolder) { /* size arrives in surfaceChanged */ }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        // Hand the slow open+render to the engine thread; show feedback immediately.
        engine.execute { onSurfaceSized(width, height) }
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) { /* keep the doc open across resizes */ }

    // Stylus hover arrives on the generic-motion channel (not onTouch). Stamp it so a palm resting
    // while the pen is near the glass is rejected for navigation even if the firmware consumes the
    // pen's touch stream.
    override fun dispatchGenericMotionEvent(event: MotionEvent): Boolean {
        val tool = event.getToolType(0)
        if (tool == MotionEvent.TOOL_TYPE_STYLUS || tool == MotionEvent.TOOL_TYPE_ERASER) {
            lastStylusMs = SystemClock.uptimeMillis()
        }
        return super.dispatchGenericMotionEvent(event)
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        // Re-claim the firmware ink path on every focus gain — the firmware releases pen ownership
        // when another window (e.g. the picker) takes focus. After this the stylus inks (RR19).
        if (hasFocus) {
            val ok = ink.setup()
            Log.i(TAG, "ink setup on focus: available=$ok")
        }
    }

    override fun onResume() {
        super.onResume()
        // Claim the firmware ink path as soon as we're foreground. Belt-and-suspenders with
        // onWindowFocusChanged: the Supernote's window-focus events are flaky (the window can go
        // "Gone" right after launch), so onResume is the reliable foreground signal.
        val ok = ink.setup()
        Log.i(TAG, "ink setup on resume: available=$ok")
    }

    override fun onPause() {
        super.onPause()
        ink.teardown() // release the firmware ink claim + clear the overlay
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
        adapter.refreshFull() // first page carries no command stream → refresh the panel (RR2-FR4)
    }

    private fun openDocumentIfNeeded() {
        if (docHandle != 0L) return
        val book = resolveBook()
        if (book == null) {
            // Nothing to resume → open the file picker straight away (no dead "Loading…" screen).
            Log.i(TAG, "no book to resume; opening the file picker (RR22)")
            runOnUiThread { openPicker() }
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
            // Per-book handwriting sidecar (RR19); the book id is a content URI, so hash it for a
            // filesystem-safe name.
            inkStore = InkStore(File(filesDir, "ink/${bookId.hashCode()}.json")).also { it.load() }
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
        adapter.refreshFull() // imported book's first page has no command stream → refresh
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
        currentPage = NativeBridge.nativeCurrentPage(handle)
        // Bake this page's saved handwriting onto the rendered page (RR19) before blitting.
        inkStore?.let { store ->
            val cv = Canvas(bmp)
            for (s in store.strokesFor(currentPage)) drawStroke(cv, s)
        }
        blit { canvas -> canvas.drawBitmap(bmp, 0f, 0f, null) }
        // Cache this page's links for tap hit-testing (RR11-FR3). Cheap; pdfium caches per page.
        currentLinks = try {
            WireCodec.decodeLinks(NativeBridge.nativePageLinks(handle, currentPage))
        } catch (e: RuntimeException) {
            Log.e(TAG, "links fetch failed: ${e.message}")
            emptyList()
        }
        Log.i(TAG, "DIAG page $currentPage: ${currentLinks.size} links ${currentLinks.take(3).map { it.targetPage ?: it.uri }}")
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

    /**
     * Route a tap: a tapped link wins (RR11-FR3), else tap zones (RR25-FR3 — left third = prev,
     * right third = next, center = contents). The page fills the viewport (stretched render), so
     * the hit-test is the normalized tap `(x/w, y/h)` against the link rects.
     */
    // ---- handwriting capture (RR19) ----

    /** Accumulate the stylus stroke; commit on UP (or a debounced pen-pause if UP is swallowed). */
    private fun captureStylus(e: MotionEvent) {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                strokeBuf.clear()
                strokeBuf.add(e.x); strokeBuf.add(e.y)
                armStrokeTimeout()
            }
            MotionEvent.ACTION_MOVE -> {
                for (i in 0 until e.historySize) {
                    strokeBuf.add(e.getHistoricalX(i)); strokeBuf.add(e.getHistoricalY(i))
                }
                strokeBuf.add(e.x); strokeBuf.add(e.y)
                armStrokeTimeout()
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                strokeBuf.add(e.x); strokeBuf.add(e.y)
                mainHandler.removeCallbacks(strokeFinalize)
                finalizeStroke()
            }
        }
    }

    private fun armStrokeTimeout() {
        mainHandler.removeCallbacks(strokeFinalize)
        mainHandler.postDelayed(strokeFinalize, STROKE_PAUSE_MS)
    }

    /** Hand the captured stroke to the engine thread for persistence (UI thread). */
    private fun finalizeStroke() {
        if (strokeBuf.size < 2) { strokeBuf.clear(); return }
        val raw = strokeBuf.toFloatArray()
        strokeBuf.clear()
        engine.execute { commitStroke(raw) }
    }

    /** Normalize + store the stroke; it bakes into the page on the next render (engine thread). */
    private fun commitStroke(raw: FloatArray) {
        val w = viewW; val h = viewH
        val store = inkStore
        if (store == null || w == 0 || h == 0) return
        val norm = FloatArray(raw.size)
        var i = 0
        while (i + 1 < raw.size) {
            norm[i] = (raw[i] / w).coerceIn(0f, 1f)
            norm[i + 1] = (raw[i + 1] / h).coerceIn(0f, 1f)
            i += 2
        }
        store.addStroke(currentPage, norm)
        // The firmware overlay already shows this stroke live; it is baked from storage on the next
        // full render (page turn / revisit), so no immediate re-blit is needed here.
    }

    /** Draw one normalized stroke onto [canvas] at the current view size. */
    private fun drawStroke(canvas: Canvas, norm: FloatArray) {
        val w = viewW.toFloat(); val h = viewH.toFloat()
        if (norm.size == 2) {
            canvas.drawCircle(norm[0] * w, norm[1] * h, inkPaint.strokeWidth / 2f, inkDotPaint)
            return
        }
        val path = Path()
        path.moveTo(norm[0] * w, norm[1] * h)
        var i = 2
        while (i + 1 < norm.size) { path.lineTo(norm[i] * w, norm[i + 1] * h); i += 2 }
        canvas.drawPath(path, inkPaint)
    }

    private fun handleTap(x: Float, y: Float) {
        val w = surfaceView.width.toFloat()
        val h = surfaceView.height.toFloat()
        if (w > 0f && h > 0f) {
            val link = currentLinks.firstOrNull { it.contains(x / w, y / h) }
            if (link != null) {
                Log.i(TAG, "DIAG handleTap link hit -> ${link.targetPage ?: link.uri}")
                followLink(link)
                return
            }
        }
        val third = w / 3f
        val zone = if (x < third) "PREV" else if (x > 2 * third) "NEXT" else "TOC"
        Log.i(TAG, "DIAG handleTap x=$x w=$w -> $zone (${currentLinks.size} links, no hit)")
        when (zone) {
            "PREV" -> postGesture(Gesture.PREV_PAGE)
            "NEXT" -> postGesture(Gesture.NEXT_PAGE)
            else -> openTocMenu()
        }
    }

    /** Follow a tapped link (RR11-FR3): internal → jump+render+refresh; external → open URL. */
    private fun followLink(link: LinkRect) {
        val page = link.targetPage
        if (page != null) {
            postJump(page)
            return
        }
        link.uri?.let { openExternalUri(it) }
    }

    /** Open an http(s) link in the system browser; refuse other schemes (safety). */
    private fun openExternalUri(uri: String) {
        val parsed = runCatching { Uri.parse(uri) }.getOrNull()
        val scheme = parsed?.scheme?.lowercase()
        if (parsed == null || (scheme != "http" && scheme != "https")) {
            Toast.makeText(this, "Unsupported link", Toast.LENGTH_SHORT).show()
            return
        }
        try {
            startActivity(Intent(Intent.ACTION_VIEW, parsed))
        } catch (e: android.content.ActivityNotFoundException) {
            Toast.makeText(this, "No app to open this link", Toast.LENGTH_SHORT).show()
        }
    }

    /** Jump to an absolute page on the engine thread, then render + refresh (RR11-FR1). */
    private fun postJump(page: Int) {
        engine.execute {
            if (docHandle == 0L) return@execute
            val commandBytes = try {
                NativeBridge.nativeJumpToPage(docHandle, page)
            } catch (e: RuntimeException) {
                Log.e(TAG, "jump failed: ${e.message}")
                return@execute
            }
            ink.clearAll() // wipe the firmware ink overlay so it doesn't bleed onto the new page
            renderAndBlit()
            adapter.executeAll(WireCodec.decodeCommands(commandBytes))
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
            ink.clearAll() // wipe the firmware ink overlay so it doesn't bleed onto the new page
            renderAndBlit()
            // Execute the policy's refresh stream on the panel (RR2-FR3).
            adapter.executeAll(WireCodec.decodeCommands(commandBytes))
        }
    }

    /** Fetch the TOC on the engine thread, then show the chooser on the UI thread (RR11-FR2). */
    private fun openTocMenu() {
        engine.execute {
            // No document open yet → go straight to the picker.
            if (docHandle == 0L) {
                runOnUiThread { openPicker() }
                return@execute
            }
            val toc = try {
                WireCodec.decodeToc(NativeBridge.nativeToc(docHandle))
            } catch (e: RuntimeException) {
                Log.e(TAG, "toc failed: ${e.message}")
                emptyList()
            }
            runOnUiThread { showReaderMenu(toc) }
        }
    }

    /**
     * The center-tap reader menu: "Open another PDF…" first (this replaces the old long-press,
     * which collided with slow stylus taps), then the document's contents (RR11-FR2).
     */
    private fun showReaderMenu(toc: List<TocItem>) {
        // Indent TOC entries by depth; show the 1-based page for resolvable entries.
        val tocLabels = toc.map { item ->
            val indent = "    ".repeat(item.depth)
            val page = item.targetPage?.let { "  ·  p${it + 1}" } ?: ""
            indent + item.title + page
        }
        val labels = (listOf("📂  Open another PDF…") + tocLabels).toTypedArray()

        AlertDialog.Builder(this)
            .setTitle(if (toc.isEmpty()) "Menu" else "Contents")
            .setItems(labels) { _, which ->
                if (which == 0) openPicker()
                else toc[which - 1].targetPage?.let { postJump(it) } // label-only entries don't navigate
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
        inkStore = null // strokes are already persisted per-stroke; drop the per-book store
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
        const val PALM_REJECT_MS = 1000L // ignore a finger tap within this long of a stylus event.
        const val STROKE_PAUSE_MS = 600L // commit a stroke after this pen-pause (swallowed-UP net).
        const val INK_STROKE_WIDTH = 6f // baked-ink line width (px) tuned to match the firmware pen.
    }
}
