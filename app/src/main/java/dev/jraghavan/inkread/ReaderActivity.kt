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
import android.os.Environment
import android.provider.Settings
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
import android.widget.FrameLayout
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

    // ---- handwriting (RR6 / ADR-INKREAD-0010) ----
    // Strokes live in the Rust core now (persisted to a `.inkread` sidecar); the shell captures
    // input, feeds the native ink seam, and bakes the core's strokes onto each rendered page.
    /** This page's strokes, decoded from the core's draw-wire for baking; engine-thread only. */
    private var pageStrokes: List<InkStrokeDraw> = emptyList()
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

    // ---- tool model (ADR-INKREAD-0010) ----
    /** The active annotation tool. [Tool.PEN] inks via firmware; the rest capture the stylus. */
    @Volatile private var tool: Tool = Tool.PEN
    /** The floating tool puck/palette overlay; created in onCreate. */
    private lateinit var toolPalette: ToolPalette
    /** In-progress selection stroke as interleaved view-px x,y; UI-thread only. */
    private val selBuf = ArrayList<Float>()
    /** Net for a swallowed stylus UP during selection (mirrors [strokeFinalize]). */
    private val selectionFinalize = Runnable { finalizeSelection() }
    /** In-progress eraser path as interleaved view-px x,y; UI-thread only. */
    private val eraseBuf = ArrayList<Float>()
    /** Net for a swallowed stylus UP during erasing (mirrors [strokeFinalize]). */
    private val eraseFinalize = Runnable { finalizeErase() }

    // ---- lasso (ADR-INKREAD-0010) ----
    /** The floating selection toolbar; created in onCreate. */
    private lateinit var selectionToolbar: SelectionToolbar
    private lateinit var colorPalette: ColorPalette
    /** Persistent Lasso discoverability banner (shown while Lasso is active with no selection). */
    private var lassoHint: TextView? = null
    /** In-progress lasso loop as interleaved view-px x,y; UI-thread only. */
    private val lassoBuf = ArrayList<Float>()
    /** Net for a swallowed stylus UP during the lasso loop. */
    private val lassoFinalize = Runnable { finalizeLasso() }
    /** 0=Smart, 1=Freehand lasso (NeoReader's two modes). */
    @Volatile private var lassoMode = 0
    /** The current selection's stroke ids (empty = no selection); read on both threads. */
    @Volatile private var selectedIds = IntArray(0)
    /** The selection's normalized bounds [x0,y0,x1,y1] for the box + toolbar anchor; empty = none. */
    @Volatile private var selectionBounds = FloatArray(0)
    /** When dragging the selection to move it: the down point (view px) and whether a move began. */
    private var moveStartX = 0f
    private var moveStartY = 0f
    private var movingSelection = false
    /** The in-progress stroke as interleaved view-px x,y; UI-thread only. */
    private val strokeBuf = ArrayList<Float>()
    private val mainHandler = Handler(Looper.getMainLooper())
    /** Safety net for a swallowed stylus ACTION_UP: commit the stroke after a brief pen pause. */
    private val strokeFinalize = Runnable { finalizeStroke() }

    // ---- stylus long-press → instant word lookup (natural "hold a word to define it") ----
    private var lpDownX = 0f
    private var lpDownY = 0f
    private var lpMoved = false
    /** Fires when the pen has been held ~still on a word: look it up, cancelling the nascent stroke. */
    private val longPress = Runnable {
        mainHandler.removeCallbacks(strokeFinalize) // this hold is a lookup, not a stroke
        strokeBuf.clear()
        val w = surfaceView.width.toFloat(); val h = surfaceView.height.toFloat()
        if (w <= 0f || h <= 0f) return@Runnable
        val nx = lpDownX / w; val ny = lpDownY / h
        val page = currentPage
        Log.i(TAG, "DIAG long-press lookup @($nx,$ny) page=$page")
        engine.execute {
            clearFirmwareInk(); renderAndBlit(); adapter.refreshFull() // wipe the pen dot the hold left
            defineWord(page, nx, ny)
        }
    }

    // ---- finger gestures: the panel DOES deliver finger UP (action=1) and a continuous stationary
    //      MOVE stream while held, so a tap (quick DOWN→UP) and a long-press (MOVEs past the
    //      threshold) are distinguishable. Tap → page nav fires on UP; a 500ms hold → word lookup.
    //      Palm rejection (forward-looking: a stylus event cancels) is preserved. UI-thread only. ----
    private var fingerDownX = 0f
    private var fingerDownY = 0f
    private var fingerMoved = false
    private var fingerLookupFired = false
    private var lastFingerMoveMs = 0L
    private val fingerLongPress = Runnable {
        // A genuine 500ms hold (UP cancels this for a tap; a beyond-slop MOVE cancels it for a
        // swipe). Mark it a long-press FIRST so the eventual UP never falls through to a page flip —
        // even if the lookup finds no word. (No "recent MOVE" gate: the held-finger MOVE stream has
        // gaps, and finger UP is reliable here, so the gate only caused false page flips.)
        if (fingerMoved) return@Runnable
        fingerLookupFired = true // suppresses the tap/page-flip on the upcoming UP
        if (SystemClock.uptimeMillis() - lastStylusMs <= PALM_REJECT_MS || strokeBuf.isNotEmpty()) return@Runnable
        lookupWordAtView(fingerDownX, fingerDownY)
    }

    /** Look up the word under a view-pixel point (shared by stylus + finger long-press). */
    private fun lookupWordAtView(vx: Float, vy: Float) {
        val w = surfaceView.width.toFloat(); val h = surfaceView.height.toFloat()
        if (w <= 0f || h <= 0f) return
        val nx = vx / w; val ny = vy / h; val page = currentPage
        Log.i(TAG, "DIAG long-press lookup @($nx,$ny) page=$page")
        engine.execute { defineWord(page, nx, ny) }
    }

    // ---- launch intent (from HomeActivity), read on the UI thread, consumed on the engine thread ----
    @Volatile private var requestPick = false
    @Volatile private var requestedPath: String? = null
    /** Filesystem path of the open document, for PDF export (ADR-INKREAD-0005). */
    @Volatile private var currentDocPath: String? = null
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
    /** Dashed box around the active lasso selection (ADR-INKREAD-0010). */
    private val selectionPaint = Paint().apply {
        color = Color.BLACK
        style = Paint.Style.STROKE
        strokeWidth = 2f
        isAntiAlias = true
        pathEffect = android.graphics.DashPathEffect(floatArrayOf(12f, 8f), 0f)
    }
    /** Filled square handles at the selection box corners (NeoReader frame 132). */
    private val selectionHandlePaint = Paint().apply { color = Color.BLACK; style = Paint.Style.FILL; isAntiAlias = true }
    /** Current pen / highlighter colour (index into the palettes); re-tapping a tool cycles it. */
    private var penColorIdx = 0
    private var hlColorIdx = 0
    private fun penColor() = PEN_COLORS[penColorIdx]
    private fun highlightColor() = HIGHLIGHT_COLORS[hlColorIdx]
    private val hlLivePaint = Paint().apply {
        style = Paint.Style.STROKE; strokeCap = Paint.Cap.ROUND; strokeJoin = Paint.Join.ROUND; isAntiAlias = true
    }
    /** Live highlighter band paint, coloured + sized to the current shade. */
    private fun highlighterLivePaint(): Paint {
        val c = highlightColor()
        hlLivePaint.color = Color.argb(c and 0xFF, (c ushr 24) and 0xFF, (c ushr 16) and 0xFF, (c ushr 8) and 0xFF)
        hlLivePaint.strokeWidth = HIGHLIGHT_WIDTH_PX
        return hlLivePaint
    }
    /** Dashed marching-ants line for the in-progress lasso loop (mirrors the firmware's own
     *  AreaSelectionView dashPaint — DashPathEffect{6,6} on a normal canvas). */
    private val lassoPaint = Paint().apply {
        color = Color.BLACK
        style = Paint.Style.STROKE
        strokeWidth = 2f
        isAntiAlias = true
        pathEffect = android.graphics.DashPathEffect(floatArrayOf(8f, 6f), 0f)
    }

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
            val toolType = event.getToolType(0)
            if (toolType == MotionEvent.TOOL_TYPE_STYLUS || toolType == MotionEvent.TOOL_TYPE_ERASER) {
                // The firmware paints the live ink (PEN); the app captures the same points to bake +
                // persist them (RR19). The app never navigates on the pen — and a stylus event
                // cancels any pending finger tap (that finger was a resting palm). The active tool
                // decides what the stylus does (ADR-INKREAD-0010).
                lastStylusMs = SystemClock.uptimeMillis()
                mainHandler.removeCallbacks(fingerLongPress) // a stylus event ⇒ that finger was a palm
                val a = event.actionMasked
                if (a == MotionEvent.ACTION_DOWN || a == MotionEvent.ACTION_UP) {
                    Log.i(TAG, "DIAG stylus action=$a tool=$tool type=$toolType hist=${event.historySize}")
                }
                when (tool) {
                    Tool.DEFINE -> captureSelection(event)
                    Tool.ERASER -> captureErase(event)
                    Tool.LASSO -> captureLasso(event)
                    else -> captureStylus(event) // PEN (Highlighter is still P2)
                }
            } else if (toolType == MotionEvent.TOOL_TYPE_FINGER) {
                when (event.actionMasked) {
                    MotionEvent.ACTION_DOWN -> onFingerDown(event)
                    MotionEvent.ACTION_MOVE -> onFingerMove(event)
                    MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> onFingerUp(event)
                }
            }
            true
        }
        // The panel refresh is routed through the view's context (Supernote "eink" service).
        adapter.attachView(surfaceView)
        // Host the surface + the docked tool toolbar (ADR-INKREAD-0010) in a FrameLayout overlay.
        val root = FrameLayout(this)
        root.addView(
            surfaceView,
            FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT,
            ),
        )
        toolPalette = ToolPalette(
            this,
            root,
            onToolSelected = { chosen -> onToolChosen(chosen) },
            // After the pill is moved/collapsed, repaint the page + force a panel refresh so the
            // EPD reflects its new position (the earlier puck "vanished" for lack of this refresh).
            onChrome = { engine.execute { renderAndBlit(); adapter.refreshFull() } },
            onUndo = { inkUndo() },
            onRedo = { inkRedo() },
        )
        selectionToolbar = SelectionToolbar(this, root) { action -> onSelectionAction(action) }
        colorPalette = ColorPalette(this, root)
        // Persistent affordance for Lasso (discoverability): a slim top banner shown while the
        // Lasso tool is active and nothing is selected. Tells the user the loop gesture; hidden
        // once a selection exists or another tool is chosen.
        lassoHint = TextView(this).apply {
            text = "Lasso — draw a loop around your writing to select"
            textSize = 14f
            setTextColor(Color.WHITE)
            setBackgroundColor(Color.BLACK)
            setPadding(dpInt(16), dpInt(8), dpInt(16), dpInt(8))
            visibility = View.GONE
        }
        root.addView(
            lassoHint,
            FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            ).apply { gravity = Gravity.TOP or Gravity.CENTER_HORIZONTAL; topMargin = dpInt(8) },
        )
        setContentView(root)
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
        // Re-apply the firmware-ink state for the active tool on every focus gain — the firmware
        // releases pen ownership when another window (e.g. the picker) takes focus (RR19). PEN
        // re-claims ink; a non-pen tool keeps it released so the stylus still selects/erases.
        if (hasFocus) applyToolInkState("focus")
    }

    override fun onResume() {
        super.onResume()
        // Re-apply ink state as soon as we're foreground. Belt-and-suspenders with
        // onWindowFocusChanged: the Supernote's window-focus events are flaky (the window can go
        // "Gone" right after launch), so onResume is the reliable foreground signal.
        applyToolInkState("resume")
    }

    override fun onPause() {
        super.onPause()
        if (::toolPalette.isInitialized) toolPalette.dismiss() // close any open palette popup
        if (::selectionToolbar.isInitialized) selectionToolbar.dismiss()
        if (::colorPalette.isInitialized) colorPalette.dismiss()
        mainHandler.removeCallbacks(fingerLongPress) // drop any pending finger gesture on leaving
        mainHandler.removeCallbacks(longPress)
        ink.teardown() // release the firmware ink claim + clear the overlay
        // Persist the reading position + flush ink when backgrounded (RR27/RR20) — engine thread.
        engine.execute {
            if (docHandle != 0L) {
                try { NativeBridge.nativeInkSave(docHandle) } catch (e: RuntimeException) { Log.e(TAG, "ink flush failed: ${e.message}") }
            }
            savePosition()
        }
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
            currentDocPath = path // remember for PDF export (ADR-INKREAD-0005)
            // Ink now lives in the Rust core, persisted to a `.inkread` sidecar next to the doc
            // (RR6/RR10 / ADR-INKREAD-0010). Attach the store so strokes save + reload.
            try {
                NativeBridge.nativeAttachInkStore(docHandle, path)
                Log.i(TAG, "DIAG ink store attached for $path")
            } catch (e: RuntimeException) {
                Log.e(TAG, "attach ink store failed: ${e.message}")
            }
            // Bookmarks remain a Kotlin sidecar (RR16), keyed by the book id.
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
        // Bake the CORE's strokes for this page onto the rendered page (RR6) before blitting.
        pageStrokes = try {
            WireCodec.decodeStrokes(NativeBridge.nativeInkStrokesForDraw(handle, currentPage))
        } catch (e: RuntimeException) {
            Log.e(TAG, "ink fetch failed: ${e.message}"); emptyList()
        }
        Log.i(TAG, "DIAG baked ${pageStrokes.size} core strokes on page $currentPage")
        val cv = Canvas(bmp)
        for (s in pageStrokes) drawStroke(cv, s)
        // The active lasso selection's bounding box (ADR-INKREAD-0010).
        if (selectedIds.isNotEmpty() && selectionBounds.size == 4) drawSelectionBox(cv)
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
                // Arm long-press → word lookup in Pen (reading) mode: hold the pen on a word to
                // define it, no tool switch needed. (Other tools have their own hold semantics.)
                if (tool == Tool.PEN) {
                    lpDownX = e.x; lpDownY = e.y; lpMoved = false
                    mainHandler.postDelayed(longPress, LONG_PRESS_MS)
                }
            }
            MotionEvent.ACTION_MOVE -> {
                for (i in 0 until e.historySize) {
                    strokeBuf.add(e.getHistoricalX(i)); strokeBuf.add(e.getHistoricalY(i))
                }
                strokeBuf.add(e.x); strokeBuf.add(e.y)
                armStrokeTimeout()
                // Any real movement means this is a stroke, not a hold → cancel the pending lookup.
                if (!lpMoved && kotlin.math.hypot(e.x - lpDownX, e.y - lpDownY) > LONG_PRESS_SLOP_PX) {
                    lpMoved = true; mainHandler.removeCallbacks(longPress)
                }
                // Pen rides the fast firmware overlay; Highlighter's is suppressed, so draw its band.
                if (tool == Tool.HIGHLIGHTER) drawLivePath(strokeBuf, highlighterLivePaint())
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                mainHandler.removeCallbacks(longPress) // lifted before the hold fired → normal stroke
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
        Log.i(TAG, "DIAG finalizeStroke buf=${strokeBuf.size / 2} pts")
        if (strokeBuf.size < 2) { strokeBuf.clear(); return }
        val raw = strokeBuf.toFloatArray()
        strokeBuf.clear()
        engine.execute { commitStroke(raw) }
    }

    /** Feed the captured pen stroke to the core (begin→points→end → autosave). Engine thread. */
    private fun commitStroke(raw: FloatArray) {
        val w = viewW; val h = viewH
        if (docHandle == 0L || w == 0 || h == 0) return
        // Highlighter = a wide, translucent band (its own core tool + colour); Pen = thin black.
        val isHl = tool == Tool.HIGHLIGHTER
        val coreTool = if (isHl) CORE_TOOL_HIGHLIGHTER else CORE_TOOL_PEN
        val widthNorm = (if (isHl) HIGHLIGHT_WIDTH_PX else INK_STROKE_WIDTH) / w
        val color = if (isHl) highlightColor() else penColor()
        try {
            NativeBridge.nativeInkBeginStroke(docHandle, coreTool, color, widthNorm, System.currentTimeMillis())
            var i = 0
            while (i + 1 < raw.size) {
                val nx = (raw[i] / w).coerceIn(0f, 1f)
                val ny = (raw[i + 1] / h).coerceIn(0f, 1f)
                NativeBridge.nativeInkAddPoint(docHandle, nx, ny, 1.0f, Float.NaN, Float.NaN, 0)
                i += 2
            }
            NativeBridge.nativeInkEndStroke(docHandle)
            Log.i(TAG, "DIAG commitStroke OK ${raw.size / 2} pts tool=$tool → core page $currentPage")
        } catch (e: RuntimeException) {
            Log.e(TAG, "ink commit failed: ${e.message}")
        }
        // Highlighter's firmware EMR ink is suppressed (we drew the live band ourselves), so bake it
        // from the core now. Pen rides the firmware overlay and bakes on the next full render.
        if (isHl) { clearFirmwareInk(); renderAndBlit(); adapter.refreshFull(); return }
        // The firmware overlay already shows this stroke live; it bakes from the core on the next
        // full render (page turn / revisit), so no immediate re-blit is needed here.
    }

    /** Draw one core stroke (normalized points + tool/color/width) onto [canvas]. */
    private fun drawStroke(canvas: Canvas, s: InkStrokeDraw) {
        val w = viewW.toFloat(); val h = viewH.toFloat()
        val norm = s.points
        if (norm.isEmpty()) return
        inkPaint.color = Color.argb(s.a, s.r, s.g, s.b)
        inkPaint.strokeWidth = (s.width * w).coerceAtLeast(1f)
        if (norm.size == 2) {
            inkDotPaint.color = inkPaint.color
            canvas.drawCircle(norm[0] * w, norm[1] * h, inkPaint.strokeWidth / 2f, inkDotPaint)
            return
        }
        val path = Path()
        path.moveTo(norm[0] * w, norm[1] * h)
        var i = 2
        while (i + 1 < norm.size) { path.lineTo(norm[i] * w, norm[i + 1] * h); i += 2 }
        canvas.drawPath(path, inkPaint)
    }

    /** Draw the active lasso selection's dashed bounding box + square corner handles (frame 132). */
    private fun drawSelectionBox(canvas: Canvas) {
        val b = selectionBounds
        val w = viewW.toFloat(); val h = viewH.toFloat()
        val l = b[0] * w; val t = b[1] * h; val r = b[2] * w; val btm = b[3] * h
        canvas.drawRect(l, t, r, btm, selectionPaint)
        val hs = SELECTION_HANDLE_PX
        for (cx in floatArrayOf(l, r)) for (cy in floatArrayOf(t, btm)) {
            canvas.drawRect(cx - hs, cy - hs, cx + hs, cy + hs, selectionHandlePaint)
        }
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
     * Finger DOWN: reject obvious palms (multi-touch, large contact, a recent/in-progress stylus, an
     * active stroke); otherwise arm the long-press → lookup timer. The tap itself is decided on UP
     * (the panel delivers finger UP reliably), so "rest the hand, then write" never turns a page.
     */
    private fun onFingerDown(e: MotionEvent) {
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
            fingerMoved = true // neutralise any later MOVE/UP from this rejected touch
            return
        }
        fingerDownX = e.x; fingerDownY = e.y
        fingerMoved = false; fingerLookupFired = false
        lastFingerMoveMs = SystemClock.uptimeMillis()
        mainHandler.removeCallbacks(fingerLongPress)
        mainHandler.postDelayed(fingerLongPress, FINGER_LONG_PRESS_MS)
    }

    /** Finger MOVE: track liveness; beyond the slop it's a swipe/scroll, not a tap or hold. */
    private fun onFingerMove(e: MotionEvent) {
        lastFingerMoveMs = SystemClock.uptimeMillis()
        if (!fingerMoved && kotlin.math.hypot(e.x - fingerDownX, e.y - fingerDownY) > FINGER_MOVE_SLOP_PX) {
            fingerMoved = true
            mainHandler.removeCallbacks(fingerLongPress)
        }
    }

    /** Finger UP: a quick, still press is a navigation tap (page zones / link / TOC). */
    private fun onFingerUp(e: MotionEvent) {
        mainHandler.removeCallbacks(fingerLongPress)
        if (fingerLookupFired) { fingerLookupFired = false; return } // the hold already looked up
        if (fingerMoved) return // a swipe or rejected palm — not a tap
        if (SystemClock.uptimeMillis() - lastStylusMs > PALM_REJECT_MS && strokeBuf.isEmpty()) {
            handleTap(fingerDownX, fingerDownY)
        } else {
            Log.i(TAG, "DIAG tap suppressed (stylus active → palm)")
        }
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
            dropSelectionForPageChange()
            renderAndBlit()
            savePosition() // persist position per jump so an abrupt kill still reopens here (RR27)
            adapter.executeAll(WireCodec.decodeCommands(commandBytes))
        }
    }

    /** Drop any lasso selection when the page changes — the ids belong to the old page (engine). */
    private fun dropSelectionForPageChange() {
        if (selectedIds.isEmpty()) return
        selectedIds = IntArray(0)
        selectionBounds = FloatArray(0)
        runOnUiThread { selectionToolbar.dismiss() }
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
            dropSelectionForPageChange()
            renderAndBlit()
            savePosition() // persist position per turn so an abrupt kill still reopens here (RR27)
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
        control("📄\nContents") { showContentsLazy() }
        control("💾\nExport") { showExportDialog() }
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

    /**
     * Export the annotations into the PDF (ADR-INKREAD-0005). Lets the user pick editable PDF
     * annotations vs. flattened (baked-in) content, then writes it back in place. Writing modifies
     * the original file, so confirm first; the heavy lifting is on the engine thread.
     */
    private fun showExportDialog() {
        val path = currentDocPath
        if (path == null || docHandle == 0L) {
            Toast.makeText(this, "No open document to export", Toast.LENGTH_SHORT).show()
            return
        }
        // inkread reads a PRIVATE copy of the PDF; to make the export visible on the desktop it must
        // land in a Partner-synced PUBLIC folder, which needs all-files access (Android 11+).
        if (!Environment.isExternalStorageManager()) {
            AlertDialog.Builder(this)
                .setTitle("Allow file access to export")
                .setMessage("To save annotated PDFs into your synced $EXPORT_DIR_NAME folder (so they appear on your computer), inkread needs \"All files access\". Grant it on the next screen, then export again.")
                .setPositiveButton("Open settings") { _, _ ->
                    val uri = Uri.parse("package:$packageName")
                    runCatching {
                        startActivity(Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, uri))
                    }.onFailure {
                        startActivity(Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION))
                    }
                }
                .setNegativeButton("Cancel", null)
                .show()
            return
        }
        // NOTE: AlertDialog shows EITHER a message OR an items list, not both — choices go in labels.
        AlertDialog.Builder(this)
            .setTitle("Export annotated PDF to $EXPORT_DIR_NAME")
            .setItems(
                arrayOf(
                    "Editable annotations (Adobe / Preview)",
                    "Flatten — shows everywhere (incl. Partner app)",
                ),
            ) { _, which ->
                val flatten = which == 1
                Toast.makeText(this, "Exporting…", Toast.LENGTH_SHORT).show()
                engine.execute { runExport(path, flatten) }
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    /**
     * Write the annotated PDF to public storage (engine thread). inkread only holds a private copy
     * of the source (opened via the picker), so it can't overwrite the original in place — instead
     * it saves a `-annotated.pdf` **beside the original** if that file can be found in the synced
     * folders, else into [EXPORT_DIR_NAME]. Either way it lands in a Partner-synced location.
     */
    private fun runExport(srcPath: String, flatten: Boolean) {
        val srcName = File(srcPath).name
        val baseName = srcName.removeSuffix(".pdf").removeSuffix(".PDF")
        val outDir = findOriginalParent(srcName)
            ?: File(Environment.getExternalStorageDirectory(), EXPORT_DIR_NAME)
        outDir.mkdirs()
        val outFile = File(outDir, "$baseName-annotated.pdf")
        val ok = try {
            NativeBridge.nativeExportPdf(docHandle, outFile.absolutePath, flatten)
            Log.i(TAG, "DIAG export OK → ${outFile.absolutePath} (flatten=$flatten)")
            true
        } catch (e: RuntimeException) {
            Log.e(TAG, "export failed: ${e.message}"); false
        }
        val rel = outFile.absolutePath
            .removePrefix(Environment.getExternalStorageDirectory().absolutePath + "/")
        runOnUiThread {
            Toast.makeText(
                this,
                if (ok) "Saved to $rel — sync to see it" else "Export failed",
                Toast.LENGTH_LONG,
            ).show()
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

    // ===== Tool model (ADR-INKREAD-0010) =====

    /**
     * Switch the active annotation tool (from the floating palette). [Tool.PEN] re-claims firmware
     * ink; every other tool releases it so the stylus selects/erases instead (the firmware-ink
     * toggle IS the mode). Highlighter/Lasso are P2 — not yet wired, so they're vetoed (return
     * false) and the active tool is unchanged. Returns true when the switch is committed.
     */
    private fun onToolChosen(chosen: Tool): Boolean {
        if (chosen.phase2) {
            Toast.makeText(this, "${chosen.label} is coming soon", Toast.LENGTH_SHORT).show()
            return false
        }
        // Re-tapping the active Lasso toggles its sub-mode (NeoReader: Smart ↔ Freehand).
        if (chosen == Tool.LASSO && tool == Tool.LASSO) {
            lassoMode = if (lassoMode == 0) 1 else 0
            val name = if (lassoMode == 0) "Smart lasso" else "Freehand lasso"
            Toast.makeText(this, "$name (tap Lasso again to switch)", Toast.LENGTH_SHORT).show()
            return true
        }
        // Re-tapping the active Highlighter / Pen opens its colour palette (stored true; grey on mono).
        if (chosen == Tool.HIGHLIGHTER && tool == Tool.HIGHLIGHTER) {
            colorPalette.show("Highlighter colour", HIGHLIGHT_COLORS, HIGHLIGHT_COLOR_NAMES, hlColorIdx) { idx ->
                hlColorIdx = idx
                Toast.makeText(this, "Highlighter: ${HIGHLIGHT_COLOR_NAMES[idx]}", Toast.LENGTH_SHORT).show()
            }
            return true
        }
        if (chosen == Tool.PEN && tool == Tool.PEN) {
            colorPalette.show("Pen colour", PEN_COLORS, PEN_COLOR_NAMES, penColorIdx) { idx ->
                penColorIdx = idx
                Toast.makeText(this, "Pen: ${PEN_COLOR_NAMES[idx]}", Toast.LENGTH_SHORT).show()
            }
            return true
        }
        if (chosen == tool) return true
        tool = chosen
        applyToolInkState("tool")
        // A tool switch ends any lasso selection (it's page- and tool-specific).
        selectedIds = IntArray(0)
        selectionBounds = FloatArray(0)
        selectionToolbar.dismiss()
        // Switching to a non-pen tool: wipe the firmware pen overlay so it doesn't sit on top of
        // the page while you lasso/erase/define (the real strokes are baked from the core).
        engine.execute {
            if (chosen != Tool.PEN) clearFirmwareInk()
            renderAndBlit()
            adapter.refreshFull()
        }
        val hint = when (chosen) {
            Tool.PEN -> "Pen — write with the stylus"
            Tool.HIGHLIGHTER -> "Highlighter — drag over text; tap again to change shade"
            Tool.ERASER -> "Eraser — drag the stylus over ink to remove it"
            Tool.DEFINE -> "Define — tap a word (or drag over text) to look it up"
            Tool.LASSO -> "Lasso — circle strokes to select; tap Lasso again for Freehand"
            else -> chosen.label
        }
        Toast.makeText(this, hint, Toast.LENGTH_SHORT).show()
        updateLassoHint()
        return true
    }

    /**
     * Keep the firmware ink **claimed in every mode** (ADR-INKREAD-0010). On this firmware the EMR
     * pen paints regardless of our claim, and `clearAll()` only works while claimed — so to wipe the
     * transient ink a non-pen gesture leaves behind, we must stay claimed and clear it afterwards
     * (see [clearFirmwareInk]). Pen keeps its live ink; non-pen tools clear theirs post-gesture.
     */
    private fun dpInt(v: Int) = (v * resources.displayMetrics.density).toInt()

    /** Show the Lasso hint banner only while Lasso is active and nothing is selected (UI thread). */
    private fun updateLassoHint() {
        val show = tool == Tool.LASSO && selectedIds.isEmpty()
        runOnUiThread { lassoHint?.visibility = if (show) View.VISIBLE else View.GONE }
    }

    private fun applyToolInkState(reason: String) {
        val ok = ink.setup()
        // Only the Pen (and Eraser) want the firmware EMR pen painting the live stroke. Lasso,
        // Define and Highlighter draw their OWN overlay (dashed loop / dashed select line / wide
        // band), so suppress the firmware ink for them — else it paints a solid black stroke on top.
        // setup() re-enables the writable area each call (incl. on focus regain), so re-assert here
        // for every reason. setWritable rides the service_myservice binder (works for a sideloaded
        // app); enableFullUiAuto is SELinux-blocked.
        val inkWritable = tool == Tool.PEN || tool == Tool.ERASER
        ink.setWritable(inkWritable)
        Log.i(TAG, "ink claimed ($reason) for $tool: available=$ok writable=$inkWritable")
    }

    /** Wipe the firmware ink overlay (engine thread) — used after a non-pen gesture so its transient
     *  ink doesn't linger over the page. Safe: real strokes are baked from the core on re-render. */
    private fun clearFirmwareInk() {
        ink.clearAll()
    }

    /** Accumulate the eraser path; finalize on UP (or a debounced pause if UP is swallowed). */
    private fun captureErase(e: MotionEvent) {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                eraseBuf.clear()
                eraseBuf.add(e.x); eraseBuf.add(e.y)
                armEraseTimeout()
            }
            MotionEvent.ACTION_MOVE -> {
                for (i in 0 until e.historySize) {
                    eraseBuf.add(e.getHistoricalX(i)); eraseBuf.add(e.getHistoricalY(i))
                }
                eraseBuf.add(e.x); eraseBuf.add(e.y)
                armEraseTimeout()
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                eraseBuf.add(e.x); eraseBuf.add(e.y)
                mainHandler.removeCallbacks(eraseFinalize)
                finalizeErase()
            }
        }
    }

    private fun armEraseTimeout() {
        mainHandler.removeCallbacks(eraseFinalize)
        mainHandler.postDelayed(eraseFinalize, STROKE_PAUSE_MS)
    }

    /** Hand the eraser path to the engine thread to remove crossed strokes (UI thread). */
    private fun finalizeErase() {
        if (eraseBuf.size < 2) { eraseBuf.clear(); return }
        val pts = eraseBuf.toFloatArray()
        eraseBuf.clear()
        engine.execute { commitErase(pts) }
    }

    /** Feed the eraser path to the core (Eraser stroke removes crossed strokes); re-render (engine). */
    private fun commitErase(viewPts: FloatArray) {
        val w = viewW; val h = viewH
        if (docHandle == 0L || w == 0 || h == 0) return
        val radiusNorm = ERASE_RADIUS_PX / w
        try {
            NativeBridge.nativeInkBeginStroke(docHandle, CORE_TOOL_ERASER, INK_COLOR_BLACK, radiusNorm, System.currentTimeMillis())
            var i = 0
            while (i + 1 < viewPts.size) {
                val nx = (viewPts[i] / w).coerceIn(0f, 1f)
                val ny = (viewPts[i + 1] / h).coerceIn(0f, 1f)
                NativeBridge.nativeInkAddPoint(docHandle, nx, ny, 1.0f, Float.NaN, Float.NaN, 0)
                i += 2
            }
            NativeBridge.nativeInkEndStroke(docHandle)
        } catch (e: RuntimeException) {
            Log.e(TAG, "erase failed: ${e.message}"); return
        }
        clearFirmwareInk() // wipe the firmware ink left by the eraser drag
        renderAndBlit()
        adapter.refreshFull()
    }

    // ===== Lasso selection (ADR-INKREAD-0010) =====

    /**
     * Capture the lasso stylus gesture. If the down lands **inside** an active selection, the gesture
     * MOVES that selection (NeoReader: drag the selection); otherwise it draws a new lasso loop.
     */
    private fun captureLasso(e: MotionEvent) {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                if (selectedIds.isNotEmpty() && pointInSelection(e.x, e.y)) {
                    movingSelection = true
                    moveStartX = e.x; moveStartY = e.y
                } else {
                    movingSelection = false
                    lassoBuf.clear()
                    lassoBuf.add(e.x); lassoBuf.add(e.y)
                    armLassoTimeout()
                }
            }
            MotionEvent.ACTION_MOVE -> {
                if (movingSelection) return // the move is applied once, on UP (one e-ink refresh)
                for (i in 0 until e.historySize) {
                    lassoBuf.add(e.getHistoricalX(i)); lassoBuf.add(e.getHistoricalY(i))
                }
                lassoBuf.add(e.x); lassoBuf.add(e.y)
                armLassoTimeout()
                drawLassoLoopLive() // we own the loop pixels now (firmware EMR ink suppressed)
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                if (movingSelection) {
                    movingSelection = false
                    applySelectionMove(e.x - moveStartX, e.y - moveStartY)
                } else {
                    lassoBuf.add(e.x); lassoBuf.add(e.y)
                    mainHandler.removeCallbacks(lassoFinalize)
                    finalizeLasso()
                }
            }
        }
    }

    private fun armLassoTimeout() {
        mainHandler.removeCallbacks(lassoFinalize)
        mainHandler.postDelayed(lassoFinalize, STROKE_PAUSE_MS)
    }

    /**
     * Draw the in-progress lasso loop as a dashed line over the cached page (UI thread). The
     * firmware EMR pen is suppressed in lasso mode ([applyToolInkState]), so WE render the loop —
     * a dashed marching-ants path, like Ratta's own AreaSelectionView. Reuses the cached page
     * [bitmap] (no core re-render); the active-stylus touch lets the firmware's auto fast-refresh
     * show it. lassoBuf holds view-px coords, matching the view-sized bitmap.
     */
    private fun drawLassoLoopLive() = drawLivePath(lassoBuf, lassoPaint)

    /**
     * Draw an in-progress gesture path (view-px [buf]) over the cached page (UI thread), for the
     * tools whose firmware EMR ink is suppressed: Lasso (dashed loop), Define (dashed select line),
     * Highlighter (wide translucent band). Reuses the cached page [bitmap] (no core re-render); the
     * active-stylus touch lets the firmware's auto fast-refresh show it.
     */
    private fun drawLivePath(buf: ArrayList<Float>, paint: Paint) {
        val bmp = bitmap ?: return
        if (buf.size < 4) return
        blit { canvas ->
            canvas.drawBitmap(bmp, 0f, 0f, null)
            val path = Path()
            path.moveTo(buf[0], buf[1])
            var i = 2
            while (i + 1 < buf.size) { path.lineTo(buf[i], buf[i + 1]); i += 2 }
            canvas.drawPath(path, paint)
        }
    }

    /** Whether a view-px point falls inside the current selection's bounds. */
    private fun pointInSelection(x: Float, y: Float): Boolean {
        val b = selectionBounds
        if (b.size != 4 || viewW == 0 || viewH == 0) return false
        val nx = x / viewW; val ny = y / viewH
        return nx in b[0]..b[2] && ny in b[1]..b[3]
    }

    /** Close the loop and ask the core which strokes it selects (engine thread). */
    private fun finalizeLasso() {
        Log.i(TAG, "DIAG finalizeLasso buf=${lassoBuf.size / 2} pts mode=$lassoMode")
        if (lassoBuf.size < 6) { // need ≥3 points for a polygon
            lassoBuf.clear()
            engine.execute { clearFirmwareInk(); renderAndBlit(); adapter.refreshFull() }
            return
        }
        val raw = lassoBuf.toFloatArray()
        lassoBuf.clear()
        val w = viewW; val h = viewH
        if (w == 0 || h == 0) return
        val poly = FloatArray(raw.size)
        var i = 0
        while (i + 1 < raw.size) {
            poly[i] = (raw[i] / w).coerceIn(0f, 1f)
            poly[i + 1] = (raw[i + 1] / h).coerceIn(0f, 1f)
            i += 2
        }
        engine.execute {
            if (docHandle == 0L) return@execute
            val ids = try {
                NativeBridge.nativeInkSelectInPolygon(docHandle, poly, lassoMode)
            } catch (e: RuntimeException) {
                Log.e(TAG, "lasso select failed: ${e.message}"); return@execute
            }
            Log.i(TAG, "DIAG lasso selected ${ids.size} strokes from ${poly.size / 2}-pt loop")
            setSelection(ids)
        }
    }

    /** Adopt `ids` as the selection, refresh the box, and show/update the selection toolbar (engine). */
    private fun setSelection(ids: IntArray) {
        selectedIds = ids
        selectionBounds = if (ids.isEmpty()) FloatArray(0) else try {
            NativeBridge.nativeInkSelectionBounds(docHandle, ids)
        } catch (e: RuntimeException) {
            FloatArray(0)
        }
        clearFirmwareInk() // wipe the firmware ink left by drawing the lasso loop
        renderAndBlit()
        adapter.refreshFull()
        updateLassoHint() // hide the hint once something is selected; re-show if selection emptied
        runOnUiThread {
            if (selectedIds.isEmpty()) {
                selectionToolbar.dismiss()
                if (tool == Tool.LASSO) Toast.makeText(this, "Nothing selected — circle around your writing", Toast.LENGTH_SHORT).show()
            } else {
                showSelectionToolbar()
            }
        }
    }

    /** Position the selection toolbar over the selection's pixel bounds (UI thread). */
    private fun showSelectionToolbar() {
        val b = selectionBounds
        if (b.size != 4) return
        val rect = android.graphics.RectF(b[0] * viewW, b[1] * viewH, b[2] * viewW, b[3] * viewH)
        val canPaste = try { NativeBridge.nativeInkHasClipboard(docHandle) } catch (e: RuntimeException) { false }
        selectionToolbar.show(rect, canPaste)
    }

    /** Apply a drag-move of the selection by a view-px delta (engine thread + autosave). */
    private fun applySelectionMove(dxPx: Float, dyPx: Float) {
        val ids = selectedIds
        if (ids.isEmpty() || viewW == 0 || viewH == 0) return
        val dx = dxPx / viewW; val dy = dyPx / viewH
        engine.execute {
            val changed = try {
                NativeBridge.nativeInkMoveSelection(docHandle, ids, dx, dy)
            } catch (e: RuntimeException) {
                Log.e(TAG, "move failed: ${e.message}"); false
            }
            if (changed) setSelection(ids) // recompute bounds + re-show toolbar at the new spot
        }
    }

    /** Handle a tap on the floating selection toolbar (UI thread → engine). */
    private fun onSelectionAction(action: SelAction) {
        val ids = selectedIds
        when (action) {
            SelAction.DONE -> clearSelection()
            SelAction.SELECT_ALL -> engine.execute {
                val all = try { NativeBridge.nativeInkSelectAll(docHandle) } catch (e: RuntimeException) { IntArray(0) }
                setSelection(all)
            }
            SelAction.DELETE -> if (ids.isNotEmpty()) engine.execute {
                try { NativeBridge.nativeInkDeleteSelection(docHandle, ids) } catch (e: RuntimeException) {}
                clearSelectionAndRender()
            }
            SelAction.CUT -> if (ids.isNotEmpty()) engine.execute {
                try { NativeBridge.nativeInkCutSelection(docHandle, ids) } catch (e: RuntimeException) {}
                clearSelectionAndRender()
            }
            SelAction.COPY -> if (ids.isNotEmpty()) engine.execute {
                try { NativeBridge.nativeInkCopySelection(docHandle, ids) } catch (e: RuntimeException) {}
                runOnUiThread { showSelectionToolbar() } // refresh Paste-enabled state
            }
            SelAction.PASTE -> engine.execute {
                val newIds = try { NativeBridge.nativeInkPaste(docHandle, PASTE_OFFSET, PASTE_OFFSET) } catch (e: RuntimeException) { IntArray(0) }
                if (newIds.isNotEmpty()) setSelection(newIds) else runOnUiThread { showSelectionToolbar() }
            }
        }
    }

    /** Undo the last ink edit (from the tool pill). Global — refreshes any active selection too. */
    private fun inkUndo() = engine.execute {
        try { NativeBridge.nativeInkUndo(docHandle) } catch (e: RuntimeException) {}
        refreshSelectionAfterHistory()
    }

    /** Redo the last undone ink edit (from the tool pill). */
    private fun inkRedo() = engine.execute {
        try { NativeBridge.nativeInkRedo(docHandle) } catch (e: RuntimeException) {}
        refreshSelectionAfterHistory()
    }

    /** After undo/redo, the selected strokes may have changed; re-render and re-anchor the toolbar. */
    private fun refreshSelectionAfterHistory() {
        if (selectedIds.isEmpty()) { clearSelectionAndRender(); return }
        setSelection(selectedIds)
    }

    /** Clear the selection (UI-triggered), then re-render to drop the box (engine). */
    private fun clearSelection() {
        engine.execute { clearSelectionAndRender() }
    }

    /** Drop the selection + toolbar and re-render the page (engine thread). */
    private fun clearSelectionAndRender() {
        selectedIds = IntArray(0)
        selectionBounds = FloatArray(0)
        renderAndBlit()
        adapter.refreshFull()
        updateLassoHint() // re-show the hint if still on the Lasso tool with nothing selected
        runOnUiThread { selectionToolbar.dismiss() }
    }

    // ===== Dictionary (RR12 / ADR-INKREAD-0009 D4) =====

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
                drawLivePath(selBuf, lassoPaint) // dashed select line (firmware EMR ink suppressed)
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

    /** Decide tap vs. drag and dispatch the lookup; stays in the (sticky) Define tool (UI thread). */
    private fun finalizeSelection() {
        if (selBuf.size < 2) { selBuf.clear(); return }
        val pts = selBuf.toFloatArray()
        selBuf.clear()
        val w = surfaceView.width.toFloat()
        val h = surfaceView.height.toFloat()
        // Define is a sticky tool (ADR-INKREAD-0010): stay in select mode + keep firmware ink
        // released until the user picks another tool from the palette.
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
        // Wipe the firmware ink the define gesture left behind (it never becomes an annotation).
        engine.execute { clearFirmwareInk(); renderAndBlit(); adapter.refreshFull() }
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
        bookmarks = null // bookmarks are persisted on toggle; drop the per-book store
        selectedIds = IntArray(0) // ink is persisted by the core to its sidecar
        selectionBounds = FloatArray(0)
        val h = docHandle
        docHandle = 0L // zero BEFORE the call so a re-entrant close is a no-op (Amendment 2)
        if (h == 0L) return
        try {
            NativeBridge.nativeInkSave(h) // flush any pending ink before teardown (RR20)
            NativeBridge.nativeSavePosition(h) // last-chance save before teardown (RR27)
        } catch (e: RuntimeException) {
            Log.e(TAG, "save on close failed: ${e.message}")
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
        // Public, Partner-synced folder the annotated PDF export is written to (Android external
        // storage root + this) so it reaches the desktop. "Document" is in the Supernote sync set.
        const val EXPORT_DIR_NAME = "Document"
        // Supernote folders the Partner app syncs — searched to place the export beside the original.
        val SYNCED_DIRS = arrayOf("Document", "EXPORT", "Note", "INBOX", "MyStyle", "Download")
        const val SELECTION_HANDLE_PX = 8f // half-size of the square corner handles on the selection box.
        const val STROKE_PAUSE_MS = 600L // commit a stroke after this pen-pause (swallowed-UP net).
        const val LONG_PRESS_MS = 500L // hold the pen this long (≈still) on a word → look it up.
        const val LONG_PRESS_SLOP_PX = 16f // movement beyond this cancels the long-press (it's a stroke).
        const val INK_STROKE_WIDTH = 6f // baked-ink line width (px) tuned to match the firmware pen.
        const val ERASE_RADIUS_PX = 22f // eraser hit radius (px): a stroke within this of the path goes.

        // Core ink seam constants (ADR-INKREAD-0010). Tool codes mirror `inkread_ink::Tool::code`.
        const val CORE_TOOL_PEN = 0
        const val CORE_TOOL_HIGHLIGHTER = 1
        const val CORE_TOOL_ERASER = 2
        const val INK_COLOR_BLACK = 0x000000FF // packed (r<<24|g<<16|b<<8|a): opaque black.
        const val HIGHLIGHT_WIDTH_PX = 30f // wide marker band (vs INK_STROKE_WIDTH for the pen).
        // REAL colours are stored per stroke (packed r<<24|g<<16|b<<8|a) and persisted in the
        // .inkbin sidecar, so a colour device / a future PDF-annotation export shows true colour.
        // On the MONOCHROME Supernote they just render as greys. Re-tap a tool to cycle its colour.
        val HIGHLIGHT_COLORS = intArrayOf(
            0xFFEB3B80.toInt(), // Yellow (translucent — keeps text readable)
            0x9CCC6580.toInt(), // Green
            0xF0629280.toInt(), // Pink
            0x4FC3F780.toInt(), // Blue
            0xFFB74D80.toInt(), // Orange
        )
        val HIGHLIGHT_COLOR_NAMES = arrayOf("Yellow", "Green", "Pink", "Blue", "Orange")
        val PEN_COLORS = intArrayOf(
            0x000000FF.toInt(), // Black
            0x1565C0FF.toInt(), // Blue
            0xC62828FF.toInt(), // Red
            0x2E7D32FF.toInt(), // Green
        )
        val PEN_COLOR_NAMES = arrayOf("Black", "Blue", "Red", "Green")
        val INK_COLOR_GRAY = 0x808080FF.toInt() // opaque mid-gray (visible on the 16-level panel).
        const val PASTE_OFFSET = 0.03f // normalized offset so a paste lands just beside the source.
        const val FINGER_LONG_PRESS_MS = 500L // finger held ~still this long on a word → look it up.
        const val FINGER_MOVE_SLOP_PX = 24f // finger travel beyond this = a swipe, not a tap/hold.
        const val PALM_TOUCH_MAJOR_FRAC = 0.12f // contact major ≥ 12% of view height ⇒ a palm.

        // Launch extras from HomeActivity.
        const val EXTRA_PICK = "inkread.pick" // open the file picker on launch.
        const val EXTRA_BOOK_PATH = "inkread.book_path" // open this specific stored book…
        const val EXTRA_BOOK_ID = "inkread.book_id" // …with this stable id.
    }
}
