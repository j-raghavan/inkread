package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.graphics.Typeface
import android.graphics.drawable.ColorDrawable
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
import android.app.Dialog
import android.text.InputType
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.Window
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.SeekBar
import android.widget.TextView
import dev.jraghavan.inkread.eink.EinkAdapter
import java.net.HttpURLConnection
import java.net.URL
import kotlin.math.max
import kotlin.math.min
import org.json.JSONObject
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
    /** Per-book bookmarks (RR16); engine-thread only. */
    private var bookmarks: Bookmarks? = null
    /** Total pages in the open doc; cached so the bottom-bar slider can read it on the UI thread. */
    @Volatile private var pageCount = 0
    /** Stable id of the open book (its file name); keys thumbnails + the bookmarks file. */
    @Volatile private var currentBookId = ""

    // ---- dictionary (RR12 / D4) ----
    /** Open `Dict` handle (0 = not opened). Engine-thread only. */
    private var dictHandle = 0L
    /** When true, the stylus SELECTS text (for a lookup) instead of inking. */
    @Volatile private var selectMode = false
    /** In-progress selection stroke as interleaved view-px x,y; UI-thread only. */
    private val selBuf = ArrayList<Float>()
    /** Net for a swallowed stylus UP during selection (mirrors [strokeFinalize]). */
    private val selectionFinalize = Runnable { finalizeSelection() }
    /** The in-progress stroke as interleaved view-px x,y; UI-thread only. */
    private val strokeBuf = ArrayList<Float>()
    private val mainHandler = Handler(Looper.getMainLooper())
    /** Safety net for a swallowed stylus ACTION_UP: commit the stroke after a brief pen pause. */
    private val strokeFinalize = Runnable { finalizeStroke() }

    /** A finger tap is acted on only after a short confirm window with NO stylus activity — a palm
     *  resting before a pen stroke is cancelled the moment the stylus event (touch or hover)
     *  arrives (forward-looking palm rejection, RR19/RR25). UI-thread only. */
    private var pendingTapX = 0f
    private var pendingTapY = 0f
    private val pendingTap = Runnable {
        if (SystemClock.uptimeMillis() - lastStylusMs > PALM_REJECT_MS && strokeBuf.isEmpty()) {
            handleTap(pendingTapX, pendingTapY)
        } else {
            Log.i(TAG, "DIAG tap suppressed at fire (stylus active → palm)")
        }
    }

    // ---- launch intent (from HomeActivity), read on the UI thread, consumed on the engine thread ----
    @Volatile private var requestPick = false
    @Volatile private var requestedPath: String? = null
    @Volatile private var requestedId: String? = null

    private val inkPaint = Paint().apply {
        color = Color.BLACK
        style = Paint.Style.STROKE
        strokeWidth = INK_STROKE_WIDTH // match the firmware needle (baked was thinner than live)
        strokeCap = Paint.Cap.ROUND
        strokeJoin = Paint.Join.ROUND
        isAntiAlias = true
    }
    private val inkDotPaint = Paint().apply { color = Color.BLACK; style = Paint.Style.FILL; isAntiAlias = true }
    private val bookmarkPaint = Paint().apply { color = Color.BLACK; style = Paint.Style.FILL; isAntiAlias = true }

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

        // What did HomeActivity ask us to open? (a specific book / the picker / else resume.)
        requestPick = intent.getBooleanExtra(EXTRA_PICK, false)
        requestedPath = intent.getStringExtra(EXTRA_BOOK_PATH)
        requestedId = intent.getStringExtra(EXTRA_BOOK_ID)

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
                // persist them (RR19). The app never navigates on the pen — and a stylus event
                // cancels any pending finger tap (that finger was a resting palm).
                lastStylusMs = SystemClock.uptimeMillis()
                mainHandler.removeCallbacks(pendingTap)
                if (selectMode) captureSelection(event) else captureStylus(event)
            } else if (event.actionMasked == MotionEvent.ACTION_DOWN &&
                tool == MotionEvent.TOOL_TYPE_FINGER
            ) {
                maybeScheduleTap(event)
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
        mainHandler.removeCallbacks(pendingTap) // drop any deferred tap when we leave the foreground
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
        engine.execute {
            closeDocument()
            closeDict()
        }
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
        val path = requestedPath
        val id = requestedId
        when {
            // 1) An explicit book chosen on Home / in the Library.
            path != null && id != null -> {
                rememberBook(path, id)
                openBook(path, id)
            }
            // 2) Home asked for the file picker.
            requestPick -> {
                requestPick = false
                Log.i(TAG, "launch: opening the file picker (RR22)")
                runOnUiThread { openPicker() }
            }
            // 3) Default: resume the last book (or the bring-up sample), else pick.
            else -> {
                val book = resolveBook()
                if (book == null) {
                    Log.i(TAG, "no book to resume; opening the file picker (RR22)")
                    runOnUiThread { openPicker() }
                } else {
                    openBook(book.first, book.second)
                }
            }
        }
    }

    /** Remember the current book so Home's "Continue" and a relaunch resume it (RR27). */
    private fun rememberBook(path: String, id: String) {
        prefs.edit().putString(KEY_BOOK_PATH, path).putString(KEY_BOOK_ID, id).apply()
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
            // Per-book sidecars (RR19/RR16): handwriting + bookmarks, keyed by the book id.
            inkStore = InkStore(File(filesDir, "ink/${bookId.hashCode()}.json")).also { it.load() }
            bookmarks = Bookmarks(File(filesDir, "bookmarks/${bookId.hashCode()}.json")).also { it.load() }
            currentBookId = bookId
            pageCount = NativeBridge.nativePageCount(docHandle)
            Books.pushRecent(this, bookId, path)
            Log.i(
                TAG,
                "opened $bookId: $pageCount pages, resumed at page ${NativeBridge.nativeCurrentPage(docHandle)}",
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
        val dest = Books.importFrom(this, uri)
        if (dest == null) {
            Log.e(TAG, "import failed for $uri")
            runOnUiThread { Toast.makeText(this, "Couldn't open that PDF", Toast.LENGTH_SHORT).show() }
            return
        }
        openSwap(dest.absolutePath, dest.name)
    }

    /** Swap the open document to (`path`, `id`) on the engine thread: remember, close, open, render. */
    private fun openSwap(path: String, id: String) {
        rememberBook(path, id)
        closeDocument() // saves + closes the previous book before swapping
        drawLoading()
        openBook(path, id)
        renderAndBlit()
        adapter.refreshFull() // the new book's first page has no command stream → refresh
    }

    /** Open a Library book in place (invoked from the reader popup; engine thread). */
    private fun openFromLibrary(file: File) {
        engine.execute { openSwap(file.absolutePath, file.name) }
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
        val cv = Canvas(bmp)
        inkStore?.let { store -> for (s in store.strokesFor(currentPage)) drawStroke(cv, s) }
        // A small dog-ear marks a bookmarked page (RR16).
        if (bookmarks?.has(currentPage) == true) drawBookmarkCorner(cv)
        // Cache the first page as the book's thumbnail, once (RR17-FR5).
        if (currentPage == 0 && currentBookId.isNotEmpty() && !Books.thumbFile(this, currentBookId).exists()) {
            Books.saveThumbnail(this, currentBookId, bmp)
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

    /** A small filled dog-ear in the top-right corner marking a bookmarked page (RR16). */
    private fun drawBookmarkCorner(canvas: Canvas) {
        val w = viewW.toFloat()
        val s = viewW * 0.045f
        val path = Path().apply {
            moveTo(w - s, 0f)
            lineTo(w, 0f)
            lineTo(w, s)
            close()
        }
        canvas.drawPath(path, bookmarkPaint)
    }

    /**
     * Decide whether a finger ACTION_DOWN is a deliberate navigation tap or a resting palm
     * (RR19/RR25). Reject obvious palms immediately — multi-touch, a large contact patch, a recent
     * or in-progress stylus, or an active stroke — and otherwise **defer** the tap by
     * [TAP_CONFIRM_MS]: any stylus event (incl. pen hover) inside that window cancels it, so the
     * common "rest the hand, then write" sequence never registers as a tap.
     */
    private fun maybeScheduleTap(e: MotionEvent) {
        val recentStylus = SystemClock.uptimeMillis() - lastStylusMs <= PALM_REJECT_MS
        val major = e.getTouchMajor(0)
        val vh = surfaceView.height
        val largeContact = vh > 0 && major >= vh * PALM_TOUCH_MAJOR_FRAC
        if (e.pointerCount != 1 || recentStylus || largeContact || strokeBuf.isNotEmpty()) {
            Log.i(
                TAG,
                "DIAG palm-reject down pc=${e.pointerCount} recent=$recentStylus " +
                    "major=$major large=$largeContact stroke=${strokeBuf.isNotEmpty()}",
            )
            return
        }
        pendingTapX = e.x
        pendingTapY = e.y
        mainHandler.removeCallbacks(pendingTap)
        mainHandler.postDelayed(pendingTap, TAP_CONFIRM_MS)
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
            else -> showBottomBar()
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

    /**
     * The reader's bottom control bar (RR16/RR25), KOReader-style: a thin panel **hugging the
     * bottom edge** — a page slider with a tappable page indicator, above a flat row of controls
     * (Home · Library · Bookmark · Marks · Contents · Open). Built programmatically, high-contrast
     * for e-ink; uses the cached page state so showing it needs no engine round-trip.
     */
    private fun showBottomBar() {
        if (docHandle == 0L) {
            openPicker()
            return
        }
        val total = pageCount.coerceAtLeast(1)
        val cur = currentPage.coerceIn(0, total - 1)
        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()

        val dialog = Dialog(this).apply { requestWindowFeature(Window.FEATURE_NO_TITLE) }
        val container = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Color.WHITE)
        }
        // Hairline top divider so the bar reads as a surface, not a floating box.
        container.addView(
            View(this).apply { setBackgroundColor(Color.parseColor("#33000000")) },
            LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(1).coerceAtLeast(1)),
        )

        // Page-slider row:  [N / Total]  ────────●────────
        val pageLabel = TextView(this).apply {
            text = "${cur + 1} / $total"
            setTextColor(Color.BLACK)
            textSize = 14f
            setPadding(0, 0, dp(14), 0)
            setOnClickListener { dialog.dismiss(); showPageEntry(total) }
        }
        val seek = SeekBar(this).apply {
            max = total - 1
            progress = cur
            setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
                override fun onProgressChanged(sb: SeekBar, p: Int, fromUser: Boolean) {
                    if (fromUser) pageLabel.text = "${p + 1} / $total"
                }
                override fun onStartTrackingTouch(sb: SeekBar) {}
                override fun onStopTrackingTouch(sb: SeekBar) { dialog.dismiss(); postJump(sb.progress) }
            })
        }
        container.addView(
            LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                setPadding(dp(16), dp(10), dp(16), dp(4))
                addView(pageLabel)
                addView(seek, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            },
        )

        // Control row: flat, evenly-weighted icon+label cells.
        val controls = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(dp(2), dp(2), dp(2), dp(10))
        }
        fun control(label: String, onClick: () -> Unit) {
            controls.addView(
                TextView(this).apply {
                    text = label
                    setTextColor(Color.BLACK)
                    textSize = 11f
                    gravity = Gravity.CENTER
                    setPadding(0, dp(8), 0, dp(8))
                    isClickable = true
                    setOnClickListener { dialog.dismiss(); onClick() }
                },
                LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f),
            )
        }
        val marked = bookmarks?.has(cur) == true
        control("🏠\nHome") { goHome() }
        control("📚\nLibrary") { showLibraryDialog() }
        control(if (marked) "🔖\nRemove" else "🔖\nBookmark") { toggleBookmark() }
        control("📑\nMarks") { showBookmarks() }
        control("🔍\nDefine") { enterSelectMode() }
        control("📄\nContents") { showContentsLazy() }
        control("📂\nOpen") { openPicker() }
        container.addView(controls)

        dialog.setContentView(container)
        dialog.window?.apply {
            setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
            setGravity(Gravity.BOTTOM)
            setBackgroundDrawable(android.graphics.drawable.ColorDrawable(Color.WHITE))
        }
        dialog.show()
    }

    /** A "go to page" text-entry dialog (RR11-FR1): type a 1-based page number to jump. */
    private fun showPageEntry(total: Int) {
        val input = EditText(this).apply {
            inputType = InputType.TYPE_CLASS_NUMBER
            hint = "1 – $total"
        }
        AlertDialog.Builder(this)
            .setTitle("Go to page")
            .setView(input)
            .setPositiveButton("Go") { _, _ ->
                val n = input.text.toString().toIntOrNull()
                if (n != null && n in 1..total) {
                    postJump(n - 1)
                } else {
                    Toast.makeText(this, "Enter a page from 1 to $total", Toast.LENGTH_SHORT).show()
                }
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    /** Toggle a bookmark on the current page (RR16); redraw so the dog-ear appears/disappears. */
    private fun toggleBookmark() {
        engine.execute {
            val bm = bookmarks ?: return@execute
            val page = currentPage
            val now = bm.toggle(page)
            runOnUiThread {
                val msg = if (now) "Bookmarked page ${page + 1}" else "Bookmark removed"
                Toast.makeText(this, msg, Toast.LENGTH_SHORT).show()
            }
            renderAndBlit()
            adapter.refreshFull()
        }
    }

    /** List the bookmarked pages (RR16); tap one to jump. */
    private fun showBookmarks() {
        engine.execute {
            val marks = bookmarks?.pages() ?: emptyList()
            runOnUiThread {
                if (marks.isEmpty()) {
                    Toast.makeText(this, "No bookmarks yet — tap Bookmark to add one", Toast.LENGTH_SHORT).show()
                    return@runOnUiThread
                }
                val labels = marks.map { "Page ${it + 1}" }.toTypedArray()
                AlertDialog.Builder(this)
                    .setTitle("Bookmarks")
                    .setItems(labels) { _, which -> postJump(marks[which]) }
                    .show()
            }
        }
    }

    /** Fetch the TOC on the engine thread, then show it (RR11-FR2). */
    private fun showContentsLazy() {
        engine.execute {
            if (docHandle == 0L) return@execute
            val toc = try {
                WireCodec.decodeToc(NativeBridge.nativeToc(docHandle))
            } catch (e: RuntimeException) {
                Log.e(TAG, "toc failed: ${e.message}")
                emptyList()
            }
            runOnUiThread {
                if (toc.isEmpty()) {
                    Toast.makeText(this, "No contents in this document", Toast.LENGTH_SHORT).show()
                } else {
                    showContents(toc)
                }
            }
        }
    }

    /** The document's table of contents (RR11-FR2), shown as a scrollable list from the popup. */
    private fun showContents(toc: List<TocItem>) {
        if (toc.isEmpty()) return
        val labels = toc.map { item ->
            val indent = "    ".repeat(item.depth)
            val page = item.targetPage?.let { "  ·  p${it + 1}" } ?: ""
            indent + item.title + page
        }.toTypedArray()
        AlertDialog.Builder(this)
            .setTitle("Contents")
            .setItems(labels) { _, which -> toc[which].targetPage?.let { postJump(it) } }
            .show()
    }

    /** The on-device library (RR17): pick a stored book to open it in place. */
    private fun showLibraryDialog() {
        val books = Books.list(this)
        if (books.isEmpty()) {
            Toast.makeText(this, "No books yet — open a PDF first", Toast.LENGTH_SHORT).show()
            return
        }
        val labels = books.map { Books.title(it) }.toTypedArray()
        AlertDialog.Builder(this)
            .setTitle("Library")
            .setItems(labels) { _, which -> openFromLibrary(books[which]) }
            .show()
    }

    /** Return to the home screen (RR16), leaving the reader. */
    private fun goHome() {
        startActivity(
            Intent(this, HomeActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP),
        )
        finish()
    }

    // ===== Dictionary (RR12 / ADR-INKREAD-0009 D4) =====

    /**
     * Enter "Define" mode: the firmware ink is released so the **stylus selects text** instead of
     * drawing — a stylus tap looks up the word, a drag highlights a phrase (RR11/RR12). Reliable
     * because stylus up/down is delivered (unlike the swallowed finger up that drives navigation).
     */
    private fun enterSelectMode() {
        selectMode = true
        ink.teardown() // stop firmware ink so the next stylus stroke is a selection, not ink
        Toast.makeText(this, "Tap a word (or drag over text) to look it up", Toast.LENGTH_SHORT).show()
    }

    /** Accumulate the selection stroke; finalize on UP (or a debounced pause if UP is swallowed). */
    private fun captureSelection(e: MotionEvent) {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                selBuf.clear()
                selBuf.add(e.x); selBuf.add(e.y)
                armSelectionTimeout()
            }
            MotionEvent.ACTION_MOVE -> {
                for (i in 0 until e.historySize) {
                    selBuf.add(e.getHistoricalX(i)); selBuf.add(e.getHistoricalY(i))
                }
                selBuf.add(e.x); selBuf.add(e.y)
                armSelectionTimeout()
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                selBuf.add(e.x); selBuf.add(e.y)
                mainHandler.removeCallbacks(selectionFinalize)
                finalizeSelection()
            }
        }
    }

    private fun armSelectionTimeout() {
        mainHandler.removeCallbacks(selectionFinalize)
        mainHandler.postDelayed(selectionFinalize, STROKE_PAUSE_MS)
    }

    /** Decide tap vs. drag, leave select mode, re-claim ink, and dispatch the lookup (UI thread). */
    private fun finalizeSelection() {
        if (selBuf.size < 2) { selBuf.clear(); return }
        val pts = selBuf.toFloatArray()
        selBuf.clear()
        val w = surfaceView.width.toFloat()
        val h = surfaceView.height.toFloat()
        selectMode = false
        ink.setup() // re-claim firmware ink for normal writing
        if (w <= 0f || h <= 0f) return

        var minX = pts[0]; var maxX = pts[0]; var minY = pts[1]; var maxY = pts[1]
        var i = 0
        while (i + 1 < pts.size) {
            minX = min(minX, pts[i]); maxX = max(maxX, pts[i])
            minY = min(minY, pts[i + 1]); maxY = max(maxY, pts[i + 1])
            i += 2
        }
        val dragged = (maxX - minX) > w * 0.03f || (maxY - minY) > h * 0.02f
        val page = currentPage
        if (dragged) {
            val r = floatArrayOf(minX / w, minY / h, maxX / w, maxY / h)
            engine.execute { defineRect(page, r) }
        } else {
            val nx = pts[0] / w
            val ny = pts[1] / h
            engine.execute { defineWord(page, nx, ny) }
        }
    }

    /** Resolve the word under a normalized point and look it up (engine thread). */
    private fun defineWord(page: Int, nx: Float, ny: Float) {
        if (docHandle == 0L) return
        val sel = try {
            WireCodec.decodeSelection(NativeBridge.nativeWordAt(docHandle, page, nx, ny))
        } catch (e: RuntimeException) {
            Log.e(TAG, "wordAt failed: ${e.message}"); return
        }
        if (sel.isEmpty) {
            runOnUiThread { Toast.makeText(this, "No word there", Toast.LENGTH_SHORT).show() }
            return
        }
        lookupAndShow(sel.text)
    }

    /** Resolve the text within a highlighted rect and look up its first word (engine thread). */
    private fun defineRect(page: Int, r: FloatArray) {
        if (docHandle == 0L) return
        val sel = try {
            WireCodec.decodeSelection(NativeBridge.nativeTextInRect(docHandle, page, r[0], r[1], r[2], r[3]))
        } catch (e: RuntimeException) {
            Log.e(TAG, "textInRect failed: ${e.message}"); return
        }
        val word = sel.text.split(Regex("\\s+")).firstOrNull().orEmpty()
        if (word.isBlank()) {
            runOnUiThread { Toast.makeText(this, "No text selected", Toast.LENGTH_SHORT).show() }
            return
        }
        lookupAndShow(word)
    }

    /** On-device lookup → online fallback (cached) → show the popup (engine thread). */
    private fun lookupAndShow(rawWord: String) {
        val word = rawWord.trim().trim { !it.isLetter() && it != '\'' && it != '-' }
        if (word.isEmpty()) return
        if (!ensureDictOpen()) {
            runOnUiThread { Toast.makeText(this, "Dictionary not available", Toast.LENGTH_SHORT).show() }
            return
        }
        var def = try {
            WireCodec.decodeDefinition(NativeBridge.nativeDefine(dictHandle, word, "en"))
        } catch (e: RuntimeException) {
            WordDefinition(false, "", "", emptyList(), emptyList())
        }
        if (!def.found) {
            onlineLookup(word)?.let { online ->
                try {
                    NativeBridge.nativeDictPut(dictHandle, online.lang, online.headword, online.senses.joinToString("\n"))
                } catch (e: RuntimeException) {
                    Log.w(TAG, "dict cache failed: ${e.message}")
                }
                def = online
            }
        }
        val result = def
        runOnUiThread {
            if (result.found) showDictPopup(word, result)
            else Toast.makeText(this, "\"$word\" not found", Toast.LENGTH_SHORT).show()
        }
    }

    /** Best-effort online lookup via Wiktionary's REST API (RR12; opt-in network). Engine thread. */
    private fun onlineLookup(word: String): WordDefinition? {
        return try {
            val url = URL("https://en.wiktionary.org/api/rest_v1/page/definition/${Uri.encode(word)}")
            val conn = (url.openConnection() as HttpURLConnection).apply {
                connectTimeout = 4000
                readTimeout = 6000
                setRequestProperty("User-Agent", "InkRead/0.1 (offline e-ink reader)")
            }
            if (conn.responseCode != 200) return null
            val body = conn.inputStream.bufferedReader().use { it.readText() }
            val root = JSONObject(body)
            val lang = if (root.has("en")) "en" else root.keys().asSequence().firstOrNull() ?: return null
            val arr = root.getJSONArray(lang)
            val senses = ArrayList<String>()
            outer@ for (i in 0 until arr.length()) {
                val group = arr.getJSONObject(i)
                val pos = group.optString("partOfSpeech", "")
                val defs = group.optJSONArray("definitions") ?: continue
                for (j in 0 until defs.length()) {
                    val d = stripHtmlTags(defs.getJSONObject(j).optString("definition", ""))
                    if (d.isNotBlank()) senses.add(if (pos.isNotEmpty()) "($pos) $d" else d)
                    if (senses.size >= 6) break@outer
                }
            }
            if (senses.isEmpty()) null else WordDefinition(true, word, lang, senses, emptyList())
        } catch (e: Exception) {
            Log.w(TAG, "online lookup failed: ${e.message}")
            null
        }
    }

    private fun stripHtmlTags(s: String): String =
        s.replace(Regex("<[^>]*>"), "").replace("&amp;", "&").replace("&#39;", "'").trim()

    /** Copy the bundled corpus out of assets (once) and open it; returns true if usable. */
    private fun ensureDictOpen(): Boolean {
        if (dictHandle != 0L) return true
        val dest = File(filesDir, "dict.db")
        if (!dest.exists() || dest.length() == 0L) {
            runOnUiThread { Toast.makeText(this, "Preparing dictionary…", Toast.LENGTH_SHORT).show() }
            try {
                assets.open("dict.db").use { input -> dest.outputStream().use { input.copyTo(it) } }
            } catch (e: Exception) {
                Log.e(TAG, "dict copy failed: ${e.message}")
                return false
            }
        }
        dictHandle = try {
            NativeBridge.nativeDictOpen(dest.absolutePath)
        } catch (e: RuntimeException) {
            Log.e(TAG, "dict open failed: ${e.message}"); 0L
        }
        return dictHandle != 0L
    }

    private fun closeDict() {
        val h = dictHandle
        dictHandle = 0L
        if (h != 0L) {
            try { NativeBridge.nativeDictClose(h) } catch (e: RuntimeException) { /* ignore */ }
        }
    }

    /** The Kindle-style definition card: headword + senses + thesaurus, anchored to the bottom. */
    private fun showDictPopup(word: String, def: WordDefinition) {
        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val dialog = Dialog(this).apply { requestWindowFeature(Window.FEATURE_NO_TITLE) }
        val col = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Color.WHITE)
            setPadding(dp(22), dp(16), dp(22), dp(18))
        }
        col.addView(
            TextView(this).apply {
                text = def.headword.ifEmpty { word }
                setTextColor(Color.BLACK)
                textSize = 24f
                typeface = Typeface.DEFAULT_BOLD
            },
        )
        if (def.lang.isNotEmpty() && def.lang != "en") {
            col.addView(
                TextView(this).apply {
                    text = def.lang
                    setTextColor(Color.DKGRAY)
                    textSize = 12f
                },
            )
        }
        for ((i, s) in def.senses.take(6).withIndex()) {
            col.addView(
                TextView(this).apply {
                    text = "${i + 1}.  $s"
                    setTextColor(Color.BLACK)
                    textSize = 15f
                    setPadding(0, dp(6), 0, 0)
                },
            )
        }
        if (def.synonyms.isNotEmpty()) {
            col.addView(
                TextView(this).apply {
                    text = "SYNONYMS"
                    setTextColor(Color.DKGRAY)
                    textSize = 11f
                    typeface = Typeface.DEFAULT_BOLD
                    setPadding(0, dp(14), 0, dp(2))
                },
            )
            col.addView(
                TextView(this).apply {
                    text = def.synonyms.take(12).joinToString(", ")
                    setTextColor(Color.BLACK)
                    textSize = 15f
                },
            )
        }
        dialog.setContentView(ScrollView(this).apply { addView(col) })
        dialog.window?.apply {
            setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
            setGravity(Gravity.BOTTOM)
            setBackgroundDrawable(ColorDrawable(Color.WHITE))
        }
        dialog.show()
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
        bookmarks = null // bookmarks are persisted on toggle; drop the per-book store
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

    companion object {
        const val TAG = "ReaderActivity"
        const val DPI = 226 // Supernote-class panel density (approx); refined per device.
        const val REQ_OPEN_DOC = 1 // startActivityForResult request code for the PDF picker.
        const val PREFS = "inkread"
        const val KEY_BOOK_PATH = "book_path" // stored PDF under app storage (RR27).
        const val KEY_BOOK_ID = "book_id" // stable per-book id (the stored file name).
        const val PALM_REJECT_MS = 1000L // a finger tap within this long of a stylus event = palm.
        const val STROKE_PAUSE_MS = 600L // commit a stroke after this pen-pause (swallowed-UP net).
        const val INK_STROKE_WIDTH = 6f // baked-ink line width (px) tuned to match the firmware pen.
        const val TAP_CONFIRM_MS = 300L // defer a finger tap; a stylus event in this window = palm.
        const val PALM_TOUCH_MAJOR_FRAC = 0.12f // contact major ≥ 12% of view height ⇒ a palm.

        // Launch extras from HomeActivity.
        const val EXTRA_PICK = "inkread.pick" // open the file picker on launch.
        const val EXTRA_BOOK_PATH = "inkread.book_path" // open this specific stored book…
        const val EXTRA_BOOK_ID = "inkread.book_id" // …with this stable id.
    }
}
