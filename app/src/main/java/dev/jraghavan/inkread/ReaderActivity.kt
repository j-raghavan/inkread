package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.drawable.GradientDrawable
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
import android.widget.ImageView
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
        if (viewW == 0 || viewH == 0) return@Runnable
        val nx = vToNx(lpDownX); val ny = vToNy(lpDownY)
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
        if (viewW == 0 || viewH == 0) return
        val nx = vToNx(vx); val ny = vToNy(vy); val page = currentPage
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
    private val bookmarkOutlinePaint = Paint().apply { color = Color.parseColor("#9E9E9E"); style = Paint.Style.STROKE; strokeWidth = 2f; isAntiAlias = true }
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
    /** Small full-page thumbnail (from the fit render) for the zoom minimap; null until first render. */
    private var fitThumb: Bitmap? = null
    private var minimapActive = false
    private var minimapThumbDrag = false
    private val minimapBgPaint = Paint().apply { color = Color.WHITE; style = Paint.Style.FILL; isAntiAlias = true }
    private val minimapCardStroke = Paint().apply { color = Color.parseColor("#BDBDBD"); style = Paint.Style.STROKE; strokeWidth = 1.5f; isAntiAlias = true }
    private val minimapThumbStroke = Paint().apply { color = Color.parseColor("#E0E0E0"); style = Paint.Style.STROKE; strokeWidth = 1f; isAntiAlias = true }
    private val minimapViewportFill = Paint().apply { color = Color.parseColor("#22000000"); style = Paint.Style.FILL; isAntiAlias = true }
    private val minimapViewportPaint = Paint().apply { color = Color.BLACK; style = Paint.Style.STROKE; strokeWidth = 3f; isAntiAlias = true }
    private val minimapGlyphPaint = Paint().apply { color = Color.BLACK; style = Paint.Style.STROKE; strokeWidth = 3f; strokeCap = Paint.Cap.ROUND; isAntiAlias = true }
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
                scaleDetector.onTouchEvent(event)
                // While a pinch is in progress (2 fingers), don't run tap/pan/long-press logic.
                if (!scaleDetector.isInProgress && event.pointerCount == 1) {
                    // The zoom minimap (when shown) is an interactive navigator + zoom control;
                    // it claims touches over its panel before the page gesture logic runs.
                    if (!handleMinimapTouch(event)) when (event.actionMasked) {
                        MotionEvent.ACTION_DOWN -> onFingerDown(event)
                        MotionEvent.ACTION_MOVE -> onFingerMove(event)
                        MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> onFingerUp(event)
                    }
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
        // PDF (fixed-layout) + EPUB (reflowable). The core dispatches by file extension; some
        // pickers tag .epub as octet-stream, so accept that too and let the core validate.
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = "*/*"
            putExtra(
                Intent.EXTRA_MIME_TYPES,
                arrayOf("application/pdf", "application/epub+zip", "application/octet-stream"),
            )
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
            // Re-apply the saved reflow text scale (EPUB); a no-op (-1) on fixed-layout PDF.
            val savedScale = textScalePref()
            if (savedScale != 1.0f) {
                val np = try { NativeBridge.nativeSetTextScale(docHandle, savedScale) } catch (e: RuntimeException) { -1 }
                if (np >= 0) Log.i(TAG, "applied text scale $savedScale → page $np")
            }
            // Re-apply the saved display contrast (RR4); 0 = off (a no-op in the core).
            try { NativeBridge.nativeSetContrast(docHandle, contrastPref()) } catch (e: RuntimeException) {}
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
        // Keep a small full-page thumbnail from the fit render to drive the zoom minimap.
        if (zoom <= 1f) updateFitThumb(bmp)
        // Zoom minimap (top-right): full page + the current viewport window (RR5-FR3).
        if (zoom > 1f) drawZoomMinimap(cv)
        // A top-right dog-ear: faint outline (tap-to-bookmark affordance) / solid when bookmarked.
        drawBookmarkCorner(cv)
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
        val widthNorm = lenToNorm(if (isHl) HIGHLIGHT_WIDTH_PX else INK_STROKE_WIDTH)
        val color = if (isHl) highlightColor() else penColor()
        try {
            NativeBridge.nativeInkBeginStroke(docHandle, coreTool, color, widthNorm, System.currentTimeMillis())
            var i = 0
            while (i + 1 < raw.size) {
                NativeBridge.nativeInkAddPoint(docHandle, vToNx(raw[i]), vToNy(raw[i + 1]), 1.0f, Float.NaN, Float.NaN, 0)
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
        val norm = s.points
        if (norm.isEmpty()) return
        inkPaint.color = Color.argb(s.a, s.r, s.g, s.b)
        inkPaint.strokeWidth = (s.width * viewW * zoom).coerceAtLeast(1f)
        if (norm.size == 2) {
            inkDotPaint.color = inkPaint.color
            canvas.drawCircle(nToVx(norm[0]), nToVy(norm[1]), inkPaint.strokeWidth / 2f, inkDotPaint)
            return
        }
        val path = Path()
        path.moveTo(nToVx(norm[0]), nToVy(norm[1]))
        var i = 2
        while (i + 1 < norm.size) { path.lineTo(nToVx(norm[i]), nToVy(norm[i + 1])); i += 2 }
        canvas.drawPath(path, inkPaint)
    }

    /** Refresh the cached full-page thumbnail from a fit render [src] (drives the zoom minimap). */
    private fun updateFitThumb(src: Bitmap) {
        val tw = viewW / 5; val th = viewH / 5
        if (tw < 8 || th < 8) return
        val old = fitThumb
        fitThumb = Bitmap.createScaledBitmap(src, tw, th, true)
        if (old != null && old != fitThumb) old.recycle()
    }

    /** Minimap panel geometry (thumbnail + the −/+ zoom buttons below it). Deterministic from the
     *  viewport so the renderer and the touch hit-test agree. Null if the view isn't sized yet. */
    private class MmGeom(
        val left: Float, val top: Float, val tw: Float, val th: Float,
        val minus: android.graphics.RectF, val plus: android.graphics.RectF,
    )
    private fun minimapGeometry(): MmGeom? {
        if (viewW == 0 || viewH == 0) return null
        val tw = (viewW / 5).toFloat(); val th = (viewH / 5).toFloat()
        val m = dpInt(8).toFloat()
        val left = viewW - tw - m; val top = m
        val barTop = top + th + dpInt(6)
        val barH = dpInt(48).toFloat(); val half = tw / 2f
        val minus = android.graphics.RectF(left, barTop, left + half, barTop + barH)
        val plus = android.graphics.RectF(left + half, barTop, left + tw, barTop + barH)
        return MmGeom(left, top, tw, th, minus, plus)
    }

    /** Draw the zoom minimap (top-right): a rounded card with the full-page thumb, the visible
     *  window highlighted, and clean −/+ zoom buttons below a divider. */
    private fun drawZoomMinimap(canvas: Canvas) {
        val thumb = fitThumb ?: return
        val g = minimapGeometry() ?: return
        val pad = dpInt(6).toFloat(); val rad = dpInt(10).toFloat()
        val cardL = g.left - pad; val cardT = g.top - pad
        val cardR = g.left + g.tw + pad; val cardB = g.plus.bottom + pad
        // Rounded white card + subtle border.
        canvas.drawRoundRect(cardL, cardT, cardR, cardB, rad, rad, minimapBgPaint)
        canvas.drawRoundRect(cardL, cardT, cardR, cardB, rad, rad, minimapCardStroke)
        // Thumbnail with a light frame.
        canvas.drawBitmap(thumb, g.left, g.top, null)
        canvas.drawRect(g.left, g.top, g.left + g.tw, g.top + g.th, minimapThumbStroke)
        // Visible-window rectangle: translucent fill + solid border = clear "you are here".
        val z = zoom
        val vx0 = panX * (z - 1f) / z; val vy0 = panY * (z - 1f) / z; val v = 1f / z
        val vl = g.left + vx0 * g.tw; val vt = g.top + vy0 * g.th
        val vr = g.left + (vx0 + v) * g.tw; val vb = g.top + (vy0 + v) * g.th
        canvas.drawRect(vl, vt, vr, vb, minimapViewportFill)
        canvas.drawRect(vl, vt, vr, vb, minimapViewportPaint)
        // Divider above the button row, then a vertical split between − and +.
        canvas.drawLine(cardL + pad, g.minus.top, cardR - pad, g.minus.top, minimapThumbStroke)
        canvas.drawLine(g.plus.left, g.minus.top + dpInt(4), g.plus.left, g.minus.bottom - dpInt(4), minimapThumbStroke)
        // − / + glyphs.
        val r = dpInt(8).toFloat()
        canvas.drawLine(g.minus.centerX() - r, g.minus.centerY(), g.minus.centerX() + r, g.minus.centerY(), minimapGlyphPaint)
        canvas.drawLine(g.plus.centerX() - r, g.plus.centerY(), g.plus.centerX() + r, g.plus.centerY(), minimapGlyphPaint)
        canvas.drawLine(g.plus.centerX(), g.plus.centerY() - r, g.plus.centerX(), g.plus.centerY() + r, minimapGlyphPaint)
    }

    /** Center the zoom viewport on a point tapped/dragged inside the minimap thumbnail. */
    private fun navigateMinimap(x: Float, y: Float, g: MmGeom) {
        val z = zoom
        if (z <= 1f) return
        val tnx = ((x - g.left) / g.tw).coerceIn(0f, 1f) // page-normalized target center
        val tny = ((y - g.top) / g.th).coerceIn(0f, 1f)
        panX = ((tnx * z - 0.5f) / (z - 1f)).coerceIn(0f, 1f)
        panY = ((tny * z - 0.5f) / (z - 1f)).coerceIn(0f, 1f)
    }

    /** Handle a finger touch on the minimap panel (navigate / zoom buttons). Returns true if it
     *  consumed the event so the page's tap/pan/long-press logic is skipped. */
    private fun handleMinimapTouch(e: MotionEvent): Boolean {
        if (zoom <= 1f) return false
        val g = minimapGeometry() ?: return false
        val x = e.x; val y = e.y
        val inThumb = x >= g.left && x <= g.left + g.tw && y >= g.top && y <= g.top + g.th
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                when {
                    g.minus.contains(x, y) -> { minimapActive = true; zoomBy(1f / ZOOM_STEP); return true }
                    g.plus.contains(x, y) -> { minimapActive = true; zoomBy(ZOOM_STEP); return true }
                    inThumb -> { minimapActive = true; minimapThumbDrag = true; navigateMinimap(x, y, g); applyZoom(); return true }
                }
            }
            MotionEvent.ACTION_MOVE -> if (minimapThumbDrag && inThumb) {
                navigateMinimap(x, y, g); throttledPreview { applyZoom() }; return true
            } else if (minimapActive) return true
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> if (minimapActive) {
                if (minimapThumbDrag) applyZoom()
                minimapActive = false; minimapThumbDrag = false; return true
            }
        }
        return minimapActive
    }

    /** Draw the active lasso selection's dashed bounding box + square corner handles (frame 132). */
    private fun drawSelectionBox(canvas: Canvas) {
        val b = selectionBounds
        val l = nToVx(b[0]); val t = nToVy(b[1]); val r = nToVx(b[2]); val btm = nToVy(b[3])
        canvas.drawRect(l, t, r, btm, selectionPaint)
        val hs = SELECTION_HANDLE_PX
        for (cx in floatArrayOf(l, r)) for (cy in floatArrayOf(t, btm)) {
            canvas.drawRect(cx - hs, cy - hs, cx + hs, cy + hs, selectionHandlePaint)
        }
    }

    /** A small filled dog-ear in the top-right corner marking a bookmarked page (RR16). */
    /** Top-right **ribbon bookmark** (swallowtail): a faint outline always (the tappable affordance)
     *  that fills solid when the page is bookmarked. Tapping the top-right corner toggles it. */
    private fun drawBookmarkCorner(canvas: Canvas) {
        val w = viewW.toFloat()
        val rw = viewW * 0.035f                 // ribbon width
        val len = rw * 2.1f                      // ribbon length
        val notch = rw * 0.45f                   // depth of the swallowtail notch
        val right = w - rw * 1.4f                // inset from the right edge
        val left = right - rw
        val path = Path().apply {
            moveTo(left, 0f)
            lineTo(right, 0f)
            lineTo(right, len)
            lineTo((left + right) / 2f, len - notch) // swallowtail
            lineTo(left, len)
            close()
        }
        if (bookmarks?.has(currentPage) == true) {
            canvas.drawPath(path, bookmarkPaint)
        } else {
            canvas.drawPath(path, bookmarkOutlinePaint)
        }
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
        // When zoomed in, a one-finger drag pans (handled on UP) — don't arm long-press lookup.
        if (zoom <= 1f) {
            mainHandler.removeCallbacks(fingerLongPress)
            mainHandler.postDelayed(fingerLongPress, FINGER_LONG_PRESS_MS)
        }
    }

    /** Finger MOVE: track liveness; beyond the slop it's a swipe/scroll, not a tap or hold. When
     *  zoomed, a drag live-previews the pan (cached bitmap translated); committed on UP. */
    private fun onFingerMove(e: MotionEvent) {
        lastFingerMoveMs = SystemClock.uptimeMillis()
        if (!fingerMoved && kotlin.math.hypot(e.x - fingerDownX, e.y - fingerDownY) > FINGER_MOVE_SLOP_PX) {
            fingerMoved = true
            mainHandler.removeCallbacks(fingerLongPress)
        }
        if (fingerMoved && zoom > 1f) {
            throttledPreview {
                previewBitmap(android.graphics.Matrix().apply { postTranslate(e.x - fingerDownX, e.y - fingerDownY) })
            }
        }
    }

    /** Finger UP: a quick, still press is a navigation tap (page zones / link / TOC). */
    private fun onFingerUp(e: MotionEvent) {
        mainHandler.removeCallbacks(fingerLongPress)
        // Zoomed in: a drag pans the page; a still tap does nothing (no page-turn while zoomed).
        if (zoom > 1f) {
            if (fingerMoved) {
                val overX = viewW * (zoom - 1f); val overY = viewH * (zoom - 1f)
                if (overX > 0f) panX = (panX - (e.x - fingerDownX) / overX).coerceIn(0f, 1f)
                if (overY > 0f) panY = (panY - (e.y - fingerDownY) / overY).coerceIn(0f, 1f)
                applyZoom()
            }
            return
        }
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
            val link = currentLinks.firstOrNull { it.contains(vToNx(x), vToNy(y)) }
            if (link != null) {
                Log.i(TAG, "DIAG handleTap link hit -> ${link.targetPage ?: link.uri}")
                followLink(link)
                return
            }
        }
        // Top-right corner → toggle the bookmark dog-ear (Kindle/KOReader convention).
        if (w > 0f && h > 0f && x > w * 0.86f && y < h * 0.08f) {
            toggleBookmark()
            return
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

        // Page-slider row:  [N / Total]  ────────●────────  (grayscale, tap the chip to type a page)
        val pageLabel = TextView(this).apply {
            text = "${cur + 1} / $total"
            setTextColor(Color.BLACK)
            textSize = 13f
            gravity = Gravity.CENTER
            setPadding(dp(12), dp(5), dp(12), dp(5))
            background = GradientDrawable().apply {
                setColor(Color.parseColor("#F2F2F2"))
                cornerRadius = dp(14).toFloat()
            }
            setOnClickListener { dialog.dismiss(); showPageEntry(total) }
        }
        // A refined, thin grayscale track + small round thumb (the default SeekBar reads clunky).
        val trackH = dp(3).coerceAtLeast(2)
        fun bar(c: Int) = GradientDrawable().apply { setColor(c); cornerRadius = trackH.toFloat(); setSize(0, trackH) }
        val track = android.graphics.drawable.LayerDrawable(
            arrayOf(
                bar(Color.parseColor("#D8D8D8")),
                android.graphics.drawable.ClipDrawable(bar(Color.BLACK), Gravity.START, android.graphics.drawable.ClipDrawable.HORIZONTAL),
            ),
        ).apply { setId(0, android.R.id.background); setId(1, android.R.id.progress) }
        val knob = GradientDrawable().apply { shape = GradientDrawable.OVAL; setColor(Color.BLACK); setSize(dp(16), dp(16)) }
        val seek = SeekBar(this).apply {
            max = total - 1
            progress = cur
            progressDrawable = track
            thumb = knob
            splitTrack = false
            setPadding(dp(10), dp(4), dp(10), dp(4))
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
                setPadding(dp(16), dp(12), dp(16), dp(6))
                addView(seek, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
                addView(pageLabel, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply { marginStart = dp(12) })
            },
        )

        // Control row: flat, evenly-weighted icon+label cells.
        val controls = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(dp(2), dp(6), dp(2), dp(12))
        }
        // One control = a line icon over a small label (Boox/NeoReader bottom-bar style, frame 069).
        fun control(iconRes: Int, label: String, onClick: () -> Unit) {
            val cell = LinearLayout(this).apply {
                orientation = LinearLayout.VERTICAL
                gravity = Gravity.CENTER
                setPadding(dp(2), dp(8), dp(2), dp(8))
                isClickable = true
                setOnClickListener { dialog.dismiss(); onClick() }
            }
            cell.addView(
                ImageView(this).apply {
                    setImageResource(iconRes); setColorFilter(Color.BLACK)
                },
                LinearLayout.LayoutParams(dp(26), dp(26)),
            )
            cell.addView(TextView(this).apply {
                text = label; setTextColor(Color.parseColor("#444444")); textSize = 10f
                gravity = Gravity.CENTER; setPadding(0, dp(4), 0, 0)
            })
            controls.addView(cell, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        }
        // "Home" already opens the library home, so a separate Library item here is redundant.
        control(R.drawable.ic_menu_home, "Home") { goHome() }
        // (Bookmark toggle moved to the top-right corner dog-ear; "Marks" lists them.)
        control(R.drawable.ic_menu_marks, "Marks") { showBookmarks() }
        control(R.drawable.ic_menu_contents, "Contents") { showContentsLazy() }
        control(R.drawable.ic_menu_zoom_out, "Zoom −") { zoomBy(1f / ZOOM_STEP) }
        control(R.drawable.ic_menu_zoom_in, "Zoom +") { zoomBy(ZOOM_STEP) }
        control(R.drawable.ic_menu_export, "Export") { showExportDialog() }
        control(R.drawable.ic_tool_define, "Dicts") { showDictionariesDialog() }
        control(R.drawable.ic_menu_font, "Font") { showTypographyDialog() }
        control(R.drawable.ic_menu_display, "Display") { showDisplayDialog() }
        control(R.drawable.ic_menu_open, "Open") { openPicker() }
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
            AlertDialog.Builder(this, R.style.InkDialog)
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
        AlertDialog.Builder(this, R.style.InkDialog)
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
        AlertDialog.Builder(this, R.style.InkDialog)
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
                AlertDialog.Builder(this, R.style.InkDialog)
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
        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val dialog = Dialog(this).apply { requestWindowFeature(Window.FEATURE_NO_TITLE) }

        val outer = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Color.WHITE)
            setPadding(dp(24), dp(20), dp(24), dp(12))
        }
        outer.addView(TextView(this).apply {
            text = "Contents"; setTextColor(Color.BLACK); textSize = 20f
            typeface = Typeface.DEFAULT_BOLD; setPadding(0, 0, 0, dp(12))
        })
        val list = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        toc.forEachIndexed { i, item ->
            if (i > 0) list.addView(View(this).apply { setBackgroundColor(Color.parseColor("#EEEEEE")) },
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, maxOf(1, dp(1))))
            list.addView(LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                setPadding(dp(4) + item.depth * dp(18), dp(14), dp(4), dp(14))
                isClickable = true
                setOnClickListener { dialog.dismiss(); item.targetPage?.let { postJump(it) } }
                addView(TextView(this@ReaderActivity).apply {
                    text = item.title
                    setTextColor(if (item.targetPage != null) Color.BLACK else Color.parseColor("#9E9E9E"))
                    textSize = if (item.depth == 0) 16f else 15f
                    if (item.depth == 0) typeface = Typeface.DEFAULT_BOLD
                    maxLines = 2
                }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
                item.targetPage?.let { p ->
                    addView(TextView(this@ReaderActivity).apply {
                        text = "${p + 1}"; setTextColor(Color.parseColor("#9E9E9E")); textSize = 13f
                        setPadding(dp(12), 0, 0, 0)
                    })
                }
            })
        }
        outer.addView(ScrollView(this).apply { addView(list) },
            LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, (resources.displayMetrics.heightPixels * 0.7f).toInt()))

        dialog.setContentView(outer)
        dialog.window?.apply {
            setLayout((resources.displayMetrics.widthPixels * 0.82f).toInt(), ViewGroup.LayoutParams.WRAP_CONTENT)
            setBackgroundDrawable(GradientDrawable().apply { setColor(Color.WHITE); cornerRadius = dp(12).toFloat() })
        }
        dialog.show()
    }

    /** The on-device library (RR17): pick a stored book to open it in place. */
    private fun showLibraryDialog() {
        val books = Books.list(this)
        if (books.isEmpty()) {
            Toast.makeText(this, "No books yet — open a PDF first", Toast.LENGTH_SHORT).show()
            return
        }
        val labels = books.map { Books.title(it) }.toTypedArray()
        AlertDialog.Builder(this, R.style.InkDialog)
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

    // ---- pinch-zoom transform (RR5-FR3). zoom=1 = fit; pan in [0,1] over the off-screen overscan.
    //      Every ink coord conversion goes through these so the overlay tracks the zoomed page;
    //      at zoom=1 they reduce to the old `x/viewW` / `nx*viewW` mapping. ----
    @Volatile private var zoom = 1f
    @Volatile private var panX = 0f
    @Volatile private var panY = 0f
    private fun nToVx(nx: Float) = nx * viewW * zoom - panX * viewW * (zoom - 1f)
    private fun nToVy(ny: Float) = ny * viewH * zoom - panY * viewH * (zoom - 1f)
    private fun vToNx(vx: Float) = ((vx + panX * viewW * (zoom - 1f)) / (viewW * zoom)).coerceIn(0f, 1f)
    private fun vToNy(vy: Float) = ((vy + panY * viewH * (zoom - 1f)) / (viewH * zoom)).coerceIn(0f, 1f)
    /** Convert an on-screen length (px) to normalized page units at the current zoom. */
    private fun lenToNorm(px: Float) = px / (viewW * zoom)
    /** Push the current zoom/pan to the core and re-render (engine thread). */
    private fun applyZoom() {
        engine.execute {
            if (docHandle != 0L) {
                try { NativeBridge.nativeSetZoom(docHandle, zoom, panX, panY) } catch (e: RuntimeException) {}
            }
            renderAndBlit()
            adapter.refreshFull()
        }
    }

    /** Multiply the zoom (clamped); snap back to fit at ~1. Used by the +/- buttons and pinch-end. */
    private fun zoomBy(factor: Float) {
        zoom = (zoom * factor).coerceIn(1f, MAX_ZOOM_UI)
        if (zoom <= 1.01f) { zoom = 1f; panX = 0f; panY = 0f }
        applyZoom()
    }

    // Live-preview state: during a pinch/pan we cheaply transform the CACHED page bitmap on the
    // canvas (no pdfium, no JNI) for instant feedback, then re-render crisp once on gesture end.
    private var gestureStartZoom = 1f
    private var liveScale = 1f
    private var focusX = 0f
    private var focusY = 0f
    private var lastPreviewMs = 0L
    private val previewPaint = Paint().apply { isFilterBitmap = true }

    /** Throttle live previews so e-ink isn't asked to refresh faster than it can. */
    private inline fun throttledPreview(block: () -> Unit) {
        val now = SystemClock.uptimeMillis()
        if (now - lastPreviewMs >= PREVIEW_MS) { lastPreviewMs = now; block() }
    }

    /** Blit the cached page bitmap transformed by [m] — instant zoom/pan feedback, no re-render. */
    private fun previewBitmap(m: android.graphics.Matrix) {
        val bmp = bitmap ?: return
        blit { c -> c.drawColor(Color.WHITE); c.drawBitmap(bmp, m, previewPaint) }
    }

    /** Pinch-to-zoom: live-preview the cached bitmap scaled around the focal point during the
     *  gesture; on end, commit the zoom with focal-anchored pan and do one crisp pdfium re-render. */
    private val scaleDetector by lazy {
        android.view.ScaleGestureDetector(this, object : android.view.ScaleGestureDetector.SimpleOnScaleGestureListener() {
            override fun onScaleBegin(d: android.view.ScaleGestureDetector): Boolean {
                gestureStartZoom = zoom; liveScale = 1f; focusX = d.focusX; focusY = d.focusY
                return true
            }
            override fun onScale(d: android.view.ScaleGestureDetector): Boolean {
                liveScale *= d.scaleFactor
                val eff = (gestureStartZoom * liveScale).coerceIn(1f, MAX_ZOOM_UI) / gestureStartZoom
                throttledPreview {
                    previewBitmap(android.graphics.Matrix().apply { postScale(eff, eff, focusX, focusY) })
                }
                return true
            }
            override fun onScaleEnd(d: android.view.ScaleGestureDetector) {
                val newZoom = (gestureStartZoom * liveScale).coerceIn(1f, MAX_ZOOM_UI)
                if (newZoom <= 1.01f) {
                    zoom = 1f; panX = 0f; panY = 0f
                } else {
                    // Anchor the pinched point: keep the content under the focal point fixed.
                    val nx = vToNx(focusX); val ny = vToNy(focusY) // uses the pre-zoom factor
                    zoom = newZoom
                    val overX = viewW * (zoom - 1f); val overY = viewH * (zoom - 1f)
                    panX = if (overX > 0f) ((nx * viewW * zoom - focusX) / overX).coerceIn(0f, 1f) else 0f
                    panY = if (overY > 0f) ((ny * viewH * zoom - focusY) / overY).coerceIn(0f, 1f) else 0f
                }
                applyZoom()
            }
        })
    }

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
        val radiusNorm = lenToNorm(ERASE_RADIUS_PX)
        try {
            NativeBridge.nativeInkBeginStroke(docHandle, CORE_TOOL_ERASER, INK_COLOR_BLACK, radiusNorm, System.currentTimeMillis())
            var i = 0
            while (i + 1 < viewPts.size) {
                NativeBridge.nativeInkAddPoint(docHandle, vToNx(viewPts[i]), vToNy(viewPts[i + 1]), 1.0f, Float.NaN, Float.NaN, 0)
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
        val nx = vToNx(x); val ny = vToNy(y)
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
            poly[i] = vToNx(raw[i])
            poly[i + 1] = vToNy(raw[i + 1])
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
            // No ink under the loop → fall back to selecting the PRINTED words inside it (the user
            // circled book text, not handwriting). Lasso thus selects ink OR text — circle anything.
            if (ids.isEmpty()) selectTextInLoop(poly) else setSelection(ids)
        }
    }

    /**
     * Lasso text fallback (engine thread): the loop found no ink, so select the printed text within
     * the loop's bounding box via the core's text seam ([NativeBridge.nativeTextInRect]) and offer
     * Define / Copy / Highlight. A polygon's bbox over a hand-drawn circle comfortably covers the
     * words the user meant; per-vertex containment is a future refinement.
     */
    private fun selectTextInLoop(poly: FloatArray) {
        if (docHandle == 0L || poly.size < 6) {
            runOnUiThread { Toast.makeText(this, "Nothing under the loop", Toast.LENGTH_SHORT).show() }
            return
        }
        var x0 = Float.MAX_VALUE; var y0 = Float.MAX_VALUE; var x1 = -Float.MAX_VALUE; var y1 = -Float.MAX_VALUE
        var i = 0
        while (i + 1 < poly.size) {
            x0 = minOf(x0, poly[i]); x1 = maxOf(x1, poly[i])
            y0 = minOf(y0, poly[i + 1]); y1 = maxOf(y1, poly[i + 1])
            i += 2
        }
        val sel = try {
            WireCodec.decodeSelection(NativeBridge.nativeTextInRect(docHandle, currentPage, x0, y0, x1, y1))
        } catch (e: RuntimeException) {
            Log.e(TAG, "lasso text-in-rect failed: ${e.message}"); Selection("", emptyList())
        }
        Log.i(TAG, "DIAG lasso text fallback: '${sel.text.take(40)}' (${sel.boxes.size} boxes)")
        clearFirmwareInk() // wipe the firmware ink left by drawing the lasso loop
        renderAndBlit()
        if (sel.isEmpty) {
            adapter.refreshFull()
            runOnUiThread {
                Toast.makeText(this, "Nothing under the loop — circle ink or printed words", Toast.LENGTH_SHORT).show()
            }
            return
        }
        drawTextSelectionBoxes(sel.boxes) // show what was caught, then offer actions
        adapter.refreshFull()
        runOnUiThread { showTextSelectionActions(sel) }
    }

    /** Shade the selected printed-text boxes over the cached page (so the user sees the catch). */
    private fun drawTextSelectionBoxes(boxes: List<SelBox>) {
        val bmp = bitmap ?: return
        val fill = Paint().apply { color = Color.argb(60, 0, 0, 0); style = Paint.Style.FILL }
        blit { canvas ->
            canvas.drawBitmap(bmp, 0f, 0f, null)
            for (b in boxes) canvas.drawRect(nToVx(b.x0), nToVy(b.y0), nToVx(b.x1), nToVy(b.y1), fill)
        }
    }

    /** Action sheet for circled printed text: Define · Copy · Highlight (UI thread). */
    private fun showTextSelectionActions(sel: Selection) {
        val snippet = sel.text.trim().replace(Regex("\\s+"), " ")
        AlertDialog.Builder(this, R.style.InkDialog)
            .setTitle(if (snippet.length > 42) snippet.take(42) + "…" else snippet)
            .setItems(arrayOf("Define", "Copy", "Highlight")) { _, which ->
                when (which) {
                    0 -> defineSelectionText(snippet)
                    1 -> copyTextToClipboard(snippet)
                    2 -> engine.execute { highlightTextBoxes(sel) }
                }
            }
            // Any dismissal (action chosen or cancelled) clears the box overlay; a Highlight redraws
            // it with the real annotation, a Define opens the dict card over the cleared page.
            .setOnDismissListener { engine.execute { renderAndBlit(); adapter.refreshFull() } }
            .show()
    }

    /** Define the first word-like token of a printed-text selection (lookup is per-word). */
    private fun defineSelectionText(text: String) {
        val word = text.split(Regex("\\s+")).firstOrNull { it.any(Char::isLetter) } ?: return
        engine.execute { lookupAndShow(word) }
    }

    /** Copy printed-text selection to the system clipboard. */
    private fun copyTextToClipboard(text: String) {
        val cm = getSystemService(android.content.Context.CLIPBOARD_SERVICE) as android.content.ClipboardManager
        cm.setPrimaryClip(android.content.ClipData.newPlainText("inkread", text))
        Toast.makeText(this, "Copied", Toast.LENGTH_SHORT).show()
    }

    /**
     * Highlight circled printed text by laying one translucent highlighter stroke across each text
     * box (engine thread) — reusing the ink highlighter's persistence + PDF export path, so no new
     * annotation subsystem is needed. The band width matches the line height; colour follows the
     * highlighter's current swatch.
     */
    private fun highlightTextBoxes(sel: Selection) {
        if (docHandle == 0L || sel.boxes.isEmpty()) return
        val color = highlightColor()
        try {
            for (b in sel.boxes) {
                val midY = (b.y0 + b.y1) / 2f
                val widthNorm = (b.y1 - b.y0) * viewH / viewW.coerceAtLeast(1) // line height as a page-space width
                NativeBridge.nativeInkBeginStroke(docHandle, CORE_TOOL_HIGHLIGHTER, color, widthNorm, System.currentTimeMillis())
                NativeBridge.nativeInkAddPoint(docHandle, b.x0, midY, 1.0f, Float.NaN, Float.NaN, 0)
                NativeBridge.nativeInkAddPoint(docHandle, b.x1, midY, 1.0f, Float.NaN, Float.NaN, 0)
                NativeBridge.nativeInkEndStroke(docHandle)
            }
            Log.i(TAG, "DIAG highlighted ${sel.boxes.size} text boxes")
        } catch (e: RuntimeException) {
            Log.e(TAG, "text highlight failed: ${e.message}")
        }
        clearFirmwareInk(); renderAndBlit(); adapter.refreshFull()
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
        val rect = android.graphics.RectF(nToVx(b[0]), nToVy(b[1]), nToVx(b[2]), nToVy(b[3]))
        val canPaste = try { NativeBridge.nativeInkHasClipboard(docHandle) } catch (e: RuntimeException) { false }
        selectionToolbar.show(rect, canPaste)
    }

    /** Apply a drag-move of the selection by a view-px delta (engine thread + autosave). */
    private fun applySelectionMove(dxPx: Float, dyPx: Float) {
        val ids = selectedIds
        if (ids.isEmpty() || viewW == 0 || viewH == 0) return
        val dx = dxPx / (viewW * zoom); val dy = dyPx / (viewH * zoom)
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
            val r = floatArrayOf(vToNx(minX), vToNy(minY), vToNx(maxX), vToNy(maxY))
            engine.execute { defineRect(page, r) }
        } else {
            engine.execute { defineWord(page, vToNx(pts[0]), vToNy(pts[1])) }
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

    /**
     * The definition card (RR12 / ADR-INKREAD-0009 D3) — a bottom sheet styled after the Supernote
     * dictionary plugin: the headword (with the *looked-up* word bracketed when it differs, e.g.
     * `run ⟨running⟩`), a **WordNet** source label, a **Definition / Thesaurus** toggle, and senses
     * grouped by part of speech with numbered glosses, examples in curly quotes, and per-sense
     * synonyms. WordNet ships no phonetics/audio, so none are shown (no faux IPA). On-device
     * results parse via [WordNet]; non-WordNet online hits fall back to plain numbered glosses.
     */
    private fun showDictPopup(word: String, def: WordDefinition) {
        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val grey = Color.parseColor("#6B6B6B")
        val faint = Color.parseColor("#9E9E9E")
        val serif = Typeface.create("serif", Typeface.NORMAL)
        val serifBold = Typeface.create("serif", Typeface.BOLD)

        val parsed = WordNet.parse(def.senses)
        val headword = def.headword.ifEmpty { word }
        // Thesaurus = the synonyms table plus every per-sense [syn:] set, deduped, headword removed.
        val thesaurus = (def.synonyms + parsed.senses.flatMap { it.synonyms })
            .map { it.trim() }.filter { it.isNotEmpty() && !it.equals(headword, ignoreCase = true) }
            .distinct()

        val dialog = Dialog(this).apply { requestWindowFeature(Window.FEATURE_NO_TITLE) }
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            background = GradientDrawable().apply {
                setColor(Color.WHITE)
                cornerRadii = floatArrayOf(dp(18).toFloat(), dp(18).toFloat(), dp(18).toFloat(), dp(18).toFloat(), 0f, 0f, 0f, 0f)
            }
            setPadding(dp(24), dp(12), dp(24), dp(20))
        }

        // ── grab handle (calm sheet affordance) ──────────────────────────────────
        root.addView(View(this).apply {
            background = GradientDrawable().apply { setColor(Color.parseColor("#D8D8D8")); cornerRadius = dp(2).toFloat() }
            layoutParams = LinearLayout.LayoutParams(dp(36), dp(4)).apply {
                gravity = Gravity.CENTER_HORIZONTAL; bottomMargin = dp(12)
            }
        })

        // ── header: headword + looked-up chip · WordNet source ───────────────────
        val header = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL }
        header.addView(TextView(this).apply {
            text = headword; setTextColor(Color.BLACK); textSize = 27f; typeface = serifBold
        })
        if (!word.equals(headword, ignoreCase = true) && word.isNotEmpty()) {
            header.addView(TextView(this).apply {
                text = "⟨ $word ⟩"; setTextColor(grey); textSize = 14f; typeface = serif
                setPadding(dp(10), dp(8), 0, 0)
            })
        }
        header.addView(View(this), LinearLayout.LayoutParams(0, 0, 1f)) // spacer
        header.addView(TextView(this).apply {
            text = if (def.lang.isNotEmpty() && def.lang != "en") "WordNet · ${def.lang}" else "WordNet"
            setTextColor(faint); textSize = 11f; letterSpacing = 0.06f
        })
        root.addView(header)

        // ── Definition / Thesaurus toggle ────────────────────────────────────────
        val body = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(0, dp(14), 0, 0) }
        lateinit var tabDef: TextView
        lateinit var tabThe: TextView
        fun styleTab(tab: TextView, active: Boolean) {
            tab.setTextColor(if (active) Color.BLACK else faint)
            tab.typeface = if (active) Typeface.DEFAULT_BOLD else Typeface.DEFAULT
            tab.paintFlags = if (active) tab.paintFlags or android.graphics.Paint.UNDERLINE_TEXT_FLAG
            else tab.paintFlags and android.graphics.Paint.UNDERLINE_TEXT_FLAG.inv()
        }
        fun renderDefinition() {
            body.removeAllViews()
            if (parsed.parseFailed) {
                for ((i, s) in def.senses.filter { it.isNotBlank() }.take(8).withIndex()) {
                    body.addView(senseRow(i + 1, s, dp(0)))
                }
                return
            }
            var lastPos: String? = "?" // sentinel so the first group always prints its badge
            for (sense in parsed.senses) {
                if (sense.pos != lastPos) {
                    lastPos = sense.pos
                    body.addView(posBadge(WordNet.labelForPos(sense.pos)))
                }
                body.addView(senseRow(sense.index, sense.definition, dp(2)))
                for (ex in sense.examples) {
                    body.addView(TextView(this).apply {
                        text = "“$ex”"; setTextColor(grey); textSize = 14f
                        typeface = Typeface.create(Typeface.DEFAULT, Typeface.ITALIC)
                        setPadding(dp(22), dp(3), 0, 0)
                    })
                }
                if (sense.synonyms.isNotEmpty()) {
                    body.addView(TextView(this).apply {
                        text = "≈ ${sense.synonyms.joinToString(", ")}"
                        setTextColor(faint); textSize = 13f; setPadding(dp(22), dp(3), 0, 0)
                    })
                }
            }
        }
        fun renderThesaurus() {
            body.removeAllViews()
            if (thesaurus.isEmpty()) {
                body.addView(TextView(this).apply {
                    text = "No thesaurus entries for this word."
                    setTextColor(grey); textSize = 15f; setPadding(0, dp(4), 0, 0)
                })
                return
            }
            body.addView(posBadge("synonyms"))
            body.addView(TextView(this).apply {
                text = thesaurus.joinToString(" · ")
                setTextColor(Color.BLACK); textSize = 16f; setLineSpacing(dp(4).toFloat(), 1f)
                setPadding(0, dp(4), 0, 0)
            })
        }
        val tabs = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; setPadding(0, dp(10), 0, 0) }
        tabDef = TextView(this).apply {
            text = "Definition"; textSize = 14f; setPadding(0, dp(4), dp(22), dp(4)); isClickable = true
            setOnClickListener { styleTab(tabDef, true); styleTab(tabThe, false); renderDefinition() }
        }
        tabThe = TextView(this).apply {
            text = "Thesaurus"; textSize = 14f; setPadding(0, dp(4), 0, dp(4)); isClickable = true
            setOnClickListener { styleTab(tabThe, true); styleTab(tabDef, false); renderThesaurus() }
        }
        tabs.addView(tabDef); tabs.addView(tabThe)
        root.addView(tabs)
        root.addView(View(this).apply {
            setBackgroundColor(Color.parseColor("#ECECEC"))
            layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, maxOf(1, dp(1))).apply {
                topMargin = dp(8)
            }
        })
        root.addView(ScrollView(this).apply {
            isVerticalScrollBarEnabled = false
            addView(body)
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                (resources.displayMetrics.heightPixels * 0.5f).toInt(),
            )
        })

        styleTab(tabDef, true); styleTab(tabThe, false); renderDefinition()
        dialog.setContentView(root)
        dialog.window?.apply {
            setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
            setGravity(Gravity.BOTTOM)
            setBackgroundDrawable(ColorDrawable(Color.TRANSPARENT))
        }
        dialog.show()
    }

    /** A part-of-speech badge (a small dark-outlined pill) heading a group of senses. */
    private fun posBadge(label: String): TextView {
        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        return TextView(this).apply {
            text = label; setTextColor(Color.BLACK); textSize = 12f; typeface = Typeface.DEFAULT_BOLD
            letterSpacing = 0.04f
            setPadding(dp(10), dp(3), dp(10), dp(4))
            background = GradientDrawable().apply {
                setColor(Color.parseColor("#F0F0F0"))
                setStroke(maxOf(1, dp(1)), Color.parseColor("#C9C9C9"))
                cornerRadius = dp(10).toFloat()
            }
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT,
            ).apply { topMargin = dp(14); bottomMargin = dp(2) }
        }
    }

    /** A numbered sense line: a fixed-width index gutter and the gloss filling the rest. */
    private fun senseRow(index: Int, text: String, topPad: Int): View {
        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        return LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(0, maxOf(topPad, dp(7)), 0, 0)
            addView(TextView(this@ReaderActivity).apply {
                this.text = "$index."; setTextColor(Color.parseColor("#6B6B6B")); textSize = 15f
                typeface = Typeface.DEFAULT_BOLD
                layoutParams = LinearLayout.LayoutParams(dp(22), ViewGroup.LayoutParams.WRAP_CONTENT)
            })
            addView(TextView(this@ReaderActivity).apply {
                this.text = text; setTextColor(Color.BLACK); textSize = 15f
                setLineSpacing(dp(3).toFloat(), 1f)
                layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
            })
        }
    }

    /**
     * Manage user-installed dictionaries (RR12 / ADR-INKREAD-0009 D2) — the KOReader-style "install
     * your own dictionary" surface. Lists StarDict folders found under [Dictionaries.roots] with an
     * Install / Remove action each; install compiles the bundle into the writable corpus via
     * [NativeBridge.nativeDictImport] on the engine thread. Reading the public folders needs
     * all-files access (same gate as export).
     */
    private fun showDictionariesDialog() {
        if (!Environment.isExternalStorageManager()) {
            AlertDialog.Builder(this, R.style.InkDialog)
                .setTitle("Allow file access for dictionaries")
                .setMessage("To find dictionaries you've copied to the device, inkread needs \"All files access\". Grant it on the next screen, then open Dicts again.")
                .setPositiveButton("Open settings") { _, _ ->
                    val uri = Uri.parse("package:$packageName")
                    runCatching {
                        startActivity(Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, uri))
                    }.onFailure { startActivity(Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION)) }
                }
                .setNegativeButton("Cancel", null)
                .show()
            return
        }
        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val home = Dictionaries.homeRoot()
        val list = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(dp(20), dp(8), dp(20), dp(8)) }

        val dialog = AlertDialog.Builder(this, R.style.InkDialog)
            .setTitle("Dictionaries")
            .setView(ScrollView(this).apply { addView(list) })
            .setPositiveButton("Done", null)
            .create()

        fun refresh() {
            list.removeAllViews()
            val bundles = Dictionaries.discover(this)
            if (bundles.isEmpty()) {
                list.addView(TextView(this).apply {
                    text = "No dictionaries found.\n\nCopy a StarDict folder (its .ifo, .idx and .dict/.dict.dz files) into:\n${home.absolutePath}\n\nthen reopen this screen."
                    setTextColor(Color.parseColor("#555555")); textSize = 14f; setLineSpacing(dp(3).toFloat(), 1f)
                })
                return
            }
            for (b in bundles) {
                val row = LinearLayout(this).apply {
                    orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
                    setPadding(0, dp(10), 0, dp(10))
                }
                row.addView(LinearLayout(this).apply {
                    orientation = LinearLayout.VERTICAL
                    layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
                    addView(TextView(this@ReaderActivity).apply {
                        text = b.name; setTextColor(Color.BLACK); textSize = 16f
                    })
                    addView(TextView(this@ReaderActivity).apply {
                        text = if (b.installed) "Installed" else "Not installed"
                        setTextColor(Color.parseColor("#9E9E9E")); textSize = 12f
                    })
                })
                row.addView(TextView(this).apply {
                    text = if (b.installed) "Remove" else "Install"
                    setTextColor(Color.BLACK); textSize = 14f; typeface = Typeface.DEFAULT_BOLD
                    setPadding(dp(14), dp(6), dp(14), dp(6))
                    background = GradientDrawable().apply {
                        setColor(Color.WHITE); setStroke(maxOf(1, dp(1)), Color.BLACK); cornerRadius = dp(16).toFloat()
                    }
                    isClickable = true
                    setOnClickListener {
                        if (b.installed) removeDictionary(b) { refresh() }
                        else installDictionary(b) { refresh() }
                    }
                })
                list.addView(row)
                list.addView(View(this).apply {
                    setBackgroundColor(Color.parseColor("#EEEEEE"))
                    layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, maxOf(1, dp(1)))
                })
            }
        }
        refresh()
        dialog.show()
    }

    /** Compile a StarDict bundle into the corpus on the engine thread, with a blocking progress note. */
    private fun installDictionary(b: Dictionaries.Bundle, onDone: () -> Unit) {
        val progress = AlertDialog.Builder(this, R.style.InkDialog)
            .setTitle("Installing ${b.name}")
            .setMessage("Large dictionaries can take a while. Please keep inkread open.")
            .setCancelable(false)
            .create()
        progress.show()
        engine.execute {
            val ok = ensureDictOpen()
            val result = if (ok) {
                try {
                    val n = NativeBridge.nativeDictImport(dictHandle, b.dir.absolutePath, b.sourceTag, false)
                    Dictionaries.markInstalled(this, b.sourceTag)
                    "Installed ${b.name} ($n entries)"
                } catch (e: RuntimeException) {
                    Log.e(TAG, "dict import failed: ${e.message}")
                    "Couldn't install ${b.name}"
                }
            } else {
                "Dictionary store unavailable"
            }
            runOnUiThread {
                progress.dismiss()
                Toast.makeText(this, result, Toast.LENGTH_SHORT).show()
                onDone()
            }
        }
    }

    /** Drop every entry for a user dictionary's source tag (the inverse of install). */
    private fun removeDictionary(b: Dictionaries.Bundle, onDone: () -> Unit) {
        engine.execute {
            if (ensureDictOpen()) {
                try {
                    NativeBridge.nativeDictForget(dictHandle, b.sourceTag)
                } catch (e: RuntimeException) {
                    Log.e(TAG, "dict forget failed: ${e.message}")
                }
            }
            Dictionaries.markRemoved(this, b.sourceTag)
            runOnUiThread {
                Toast.makeText(this, "Removed ${b.name}", Toast.LENGTH_SHORT).show()
                onDone()
            }
        }
    }

    /**
     * Reflow **font size** control (RR2-FR5) — A− / A+ over a set of scale steps, applied live via
     * [NativeBridge.nativeSetTextScale] (EPUB repaginates, preserving the chapter). The scale
     * persists and is re-applied on the next open. A no-op toast on fixed-layout PDF.
     */
    private fun showTypographyDialog() {
        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        var idx = nearestScaleIndex(textScalePref())
        val label = TextView(this).apply {
            textSize = 18f; setTextColor(Color.BLACK); gravity = Gravity.CENTER
            minWidth = dp(96)
        }
        fun refreshLabel() { label.text = "${(TEXT_SCALES[idx] * 100).toInt()}%" }
        refreshLabel()
        fun apply() {
            val scale = TEXT_SCALES[idx]
            setTextScalePref(scale)
            refreshLabel()
            engine.execute {
                val np = try { NativeBridge.nativeSetTextScale(docHandle, scale) } catch (e: RuntimeException) { -1 }
                if (np >= 0) {
                    pageCount = NativeBridge.nativePageCount(docHandle)
                    renderAndBlit(); adapter.refreshFull()
                } else {
                    runOnUiThread {
                        Toast.makeText(this, "Font size adjusts reflowable books (EPUB)", Toast.LENGTH_SHORT).show()
                    }
                }
            }
        }
        fun stepButton(text: String, onTap: () -> Unit) = TextView(this).apply {
            this.text = text; textSize = 22f; setTextColor(Color.BLACK); gravity = Gravity.CENTER
            setPadding(dp(22), dp(6), dp(22), dp(10))
            background = GradientDrawable().apply {
                setColor(Color.WHITE); setStroke(maxOf(1, dp(1)), Color.BLACK); cornerRadius = dp(20).toFloat()
            }
            isClickable = true; setOnClickListener { onTap() }
        }
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER
            setPadding(dp(24), dp(22), dp(24), dp(10))
            addView(stepButton("A−") { if (idx > 0) { idx--; apply() } })
            addView(label, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
                marginStart = dp(18); marginEnd = dp(18)
            })
            addView(stepButton("A+") { if (idx < TEXT_SCALES.size - 1) { idx++; apply() } })
        }
        AlertDialog.Builder(this, R.style.InkDialog)
            .setTitle("Font size")
            .setView(row)
            .setPositiveButton("Done", null)
            .show()
    }

    /**
     * Display settings (RR4 — KOReader's "Contrast" tab). A contrast stepper (0 = off … MAX) that
     * applies live and persists per the app (re-applied on open). The matching native control is a
     * post-render pixel remap, so it works on PDF and EPUB and needs only a re-render.
     */
    private fun showDisplayDialog() {
        if (docHandle == 0L) { openPicker(); return }
        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        var step = contrastPref()

        val label = TextView(this).apply {
            textSize = 18f; setTextColor(Color.BLACK); gravity = Gravity.CENTER; minWidth = dp(72)
        }
        fun refresh() { label.text = if (step == 0) "Off" else step.toString() }
        refresh()
        fun apply() {
            setContrastPref(step)
            refresh()
            engine.execute {
                try { NativeBridge.nativeSetContrast(docHandle, step) } catch (e: RuntimeException) {}
                renderAndBlit(); adapter.refreshFull()
            }
        }
        fun button(txt: String, onTap: () -> Unit) = TextView(this).apply {
            text = txt; textSize = 22f; gravity = Gravity.CENTER
            setTextColor(Color.BLACK); setPadding(dp(20), dp(6), dp(20), dp(6)); isClickable = true
            setOnClickListener { onTap() }
        }
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(20), dp(8), dp(20), dp(8))
            addView(button("−") { if (step > 0) { step--; apply() } })
            addView(label)
            addView(button("+") { if (step < CONTRAST_MAX) { step++; apply() } })
        }
        val container = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(TextView(this@ReaderActivity).apply {
                text = "Contrast"; textSize = 13f; setTextColor(Color.parseColor("#666666"))
                setPadding(dp(20), dp(14), dp(20), 0)
            })
            addView(row)
        }
        AlertDialog.Builder(this).setTitle("Display").setView(container)
            .setPositiveButton("Done", null).show()
    }

    private fun contrastPref(): Int =
        getSharedPreferences("display", MODE_PRIVATE).getInt("contrast", 0).coerceIn(0, CONTRAST_MAX)

    private fun setContrastPref(step: Int) =
        getSharedPreferences("display", MODE_PRIVATE).edit().putInt("contrast", step).apply()

    private fun textScalePref(): Float =
        getSharedPreferences("typography", MODE_PRIVATE).getFloat("scale", 1.0f)

    private fun setTextScalePref(scale: Float) =
        getSharedPreferences("typography", MODE_PRIVATE).edit().putFloat("scale", scale).apply()

    private fun nearestScaleIndex(scale: Float): Int {
        var best = 0
        var bestDist = Float.MAX_VALUE
        TEXT_SCALES.forEachIndexed { i, v ->
            val dist = kotlin.math.abs(v - scale)
            if (dist < bestDist) { bestDist = dist; best = i }
        }
        return best
    }

    /** Persist the current reading position (RR12-FR3 / RR27); store-less / closed = no-op. */
    private fun savePosition() {
        if (docHandle == 0L) return
        try {
            NativeBridge.nativeSavePosition(docHandle)
        } catch (e: RuntimeException) {
            Log.e(TAG, "save position failed: ${e.message}")
        }
        // Record read progress for the home shelf (RR16/RR17).
        val total = pageCount
        if (total > 0 && currentBookId.isNotEmpty()) {
            Books.setProgress(this, currentBookId, ((currentPage + 1) * 100) / total)
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
        const val MAX_ZOOM_UI = 5f // matches the core's MAX_ZOOM clamp (RR5-FR3).
        const val PREVIEW_MS = 50L // min interval between live zoom/pan preview blits (e-ink cadence).
        const val ZOOM_STEP = 1.4f // +/- button zoom multiplier.
        const val EXPORT_DIR_NAME = "Document"
        // Supernote folders the Partner app syncs — searched to place the export beside the original.
        val SYNCED_DIRS = arrayOf("Document", "EXPORT", "Note", "INBOX", "MyStyle", "Download")
        const val SELECTION_HANDLE_PX = 8f // half-size of the square corner handles on the selection box.
        const val CONTRAST_MAX = 8 // mirrors reader-core render::contrast::MAX_CONTRAST_STEP (RR4).
        const val STROKE_PAUSE_MS = 600L // commit a stroke after this pen-pause (swallowed-UP net).
        const val LONG_PRESS_MS = 500L // hold the pen this long (≈still) on a word → look it up.
        const val LONG_PRESS_SLOP_PX = 16f // movement beyond this cancels the long-press (it's a stroke).
        const val INK_STROKE_WIDTH = 6f // baked-ink line width (px) tuned to match the firmware pen.
        const val ERASE_RADIUS_PX = 22f // eraser hit radius (px): a stroke within this of the path goes.

        // Core ink seam constants (ADR-INKREAD-0010). Tool codes mirror `inkread_ink::Tool::code`.
        /** Reflow font-size steps (multiples of the core's base body size); 1.0 = default. */
        val TEXT_SCALES = floatArrayOf(0.6f, 0.7f, 0.8f, 0.9f, 1.0f, 1.15f, 1.3f, 1.5f, 1.75f, 2.0f, 2.5f)
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
