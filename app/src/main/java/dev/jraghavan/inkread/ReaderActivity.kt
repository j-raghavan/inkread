package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.content.pm.ActivityInfo
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

    /** True while the EMR pen is in proximity (hover enter/move seen, no exit yet). The hand rests as
     *  the pen approaches, so any finger that lands while the pen hovers is a palm — this rejects the
     *  FIRST palm at the start of writing, before the pen has actually touched (RR19). */
    @Volatile private var penHovering = false

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
    /** 0-based page the strokes are keyed to; set on the engine thread after each render, read on the
     *  UI thread (slider, coalesced page turns) — so `@Volatile`. */
    @Volatile private var currentPage = 0
    /** Per-book bookmarks (RR16); engine-thread only. */
    private var bookmarks: Bookmarks? = null
    /** Total pages in the open doc; cached so the bottom-bar slider can read it on the UI thread. */
    @Volatile private var pageCount = 0
    /** Stable id of the open book (its file name); keys thumbnails + the bookmarks file. */
    @Volatile private var currentBookId = ""
    /** Whether PDF reflow mode is on (ADR-INKREAD-0011). Session-scoped: defaults off on each open
     *  (the fixed page is the faithful view; reflow is an opt-in toggle on the Page tab). */
    @Volatile private var reflowOn = false

    // ---- dictionary (RR12 / D4) — owns the corpus handle + lookup/define/manage UI (SRP) ----
    private val dict = DictController(object : DictController.Host {
        override val activity get() = this@ReaderActivity
        override val docHandle get() = this@ReaderActivity.docHandle
        override fun engineExecute(block: () -> Unit) { engine.execute(block) }
    })

    // ---- PDF annotation export (ADR-INKREAD-0005) — owns the chooser + engine-thread write (SRP) ----
    private val export = ExportController(object : ExportController.Host {
        override val activity get() = this@ReaderActivity
        override val docHandle get() = this@ReaderActivity.docHandle
        override val currentDocPath get() = this@ReaderActivity.currentDocPath
        override fun engineExecute(block: () -> Unit) { engine.execute(block) }
    })

    // ---- Supernote Digest write-through (ADR-INKREAD-0010) — saves a lasso selection into the
    //      firmware Digest app via its "Knowledge" provider; owns the vendor surface (IR-7). ----
    private val digest = DigestController(object : DigestController.Host {
        override val activity get() = this@ReaderActivity
        override val docHandle get() = this@ReaderActivity.docHandle
        override val currentDocPath get() = this@ReaderActivity.currentDocPath
        override fun engineExecute(block: () -> Unit) { engine.execute(block) }
    })

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
    /** In-document search (RR2) — owns its own query/hit state + dialogs (SRP). The shell only
     *  draws the active hit's highlight (see [drawSearchHighlight]) for the current page. */
    private val search = SearchController(object : SearchController.Host {
        override val activity get() = this@ReaderActivity
        override val docHandle get() = this@ReaderActivity.docHandle
        override val pageCount get() = this@ReaderActivity.pageCount
        override fun engineExecute(block: () -> Unit) { engine.execute(block) }
        override fun jumpToPage(page: Int) = postJump(page)
        override fun repaintPanel() = this@ReaderActivity.repaintPanel()
        override fun openPicker() = this@ReaderActivity.openPicker()
    })

    /** The in-progress stroke as interleaved view-px x,y; UI-thread only. */
    private val strokeBuf = ArrayList<Float>()
    private val mainHandler = Handler(Looper.getMainLooper())
    /** Safety net for a swallowed stylus ACTION_UP: commit the stroke after a brief pen pause. */
    private val strokeFinalize = Runnable { finalizeStroke() }

    /** Trailing-edge flush of deferred ink (RR20): coalesces the per-stroke fsync into one write a
     *  short while after the pen goes idle. onPause/teardown flush immediately and cancel this. */
    private val inkFlush = Runnable {
        val h = docHandle
        if (h != 0L) engine.execute {
            try { NativeBridge.nativeInkSave(h) } catch (e: RuntimeException) { Log.e(TAG, "ink flush failed: ${e.message}") }
        }
    }

    /** (Re)arm the trailing-edge ink flush after an edit; resets the timer on each new stroke. */
    private fun scheduleInkFlush() {
        mainHandler.removeCallbacks(inkFlush)
        mainHandler.postDelayed(inkFlush, INK_FLUSH_MS)
    }

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
        diag { "DIAG long-press lookup @($nx,$ny) page=$page" }
        engine.execute {
            clearFirmwareInk(); repaintPanel() // wipe the pen dot the hold left
            dict.defineWord(page, nx, ny)
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
    /** Latched for the whole finger gesture once it reads as a palm (at DOWN, or grown into one on a
     *  later MOVE). Distinct from [fingerMoved]: the zoomed-in pan path treats `fingerMoved` as "the
     *  user dragged" and would otherwise pan on a rejected palm's UP. MOVE/UP bail while this is set;
     *  cleared on the next DOWN / UP (#49). */
    private var fingerIsPalm = false
    /** Latched once a gesture has been multi-pointer (a pinch). A pointer lifting from 2→1 fingers
     *  arrives as ACTION_POINTER_UP at pointerCount==2, which the single-finger dispatch skips, so the
     *  surviving finger's trailing MOVEs would otherwise pan from the ORIGINAL down's stale origin and
     *  jump the page. While set, single-finger pan/tap is suppressed until a fresh DOWN starts a clean
     *  gesture; reset on ACTION_DOWN (#49). */
    private var gestureWasMultiTouch = false
    private val fingerLongPress = Runnable {
        // A genuine 500ms hold (UP cancels this for a tap; a beyond-slop MOVE cancels it for a
        // swipe). Mark it a long-press FIRST so the eventual UP never falls through to a page flip —
        // even if the lookup finds no word. (No "recent MOVE" gate: the held-finger MOVE stream has
        // gaps, and finger UP is reliable here, so the gate only caused false page flips.)
        // A pinch-zoom in flight is never a word lookup, even if a finger sat still long enough.
        if (fingerMoved || scaleDetector.isInProgress) return@Runnable
        fingerLookupFired = true // suppresses the tap/page-flip on the upcoming UP
        if (SystemClock.uptimeMillis() - lastStylusMs <= PALM_REJECT_MS || strokeBuf.isNotEmpty()) return@Runnable
        lookupWordAtView(fingerDownX, fingerDownY)
    }

    /** Look up the word under a view-pixel point (shared by stylus + finger long-press). */
    private fun lookupWordAtView(vx: Float, vy: Float) {
        if (viewW == 0 || viewH == 0) return
        val nx = vToNx(vx); val ny = vToNy(vy); val page = currentPage
        diag { "DIAG long-press lookup @($nx,$ny) page=$page" }
        engine.execute { dict.defineWord(page, nx, ny) }
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
    /** White halo drawn under the ribbon so it stays visible over a dark page region (e.g. a black
     *  title band) — without it a black/gray ribbon vanishes on dark backgrounds. */
    private val bookmarkHaloPaint = Paint().apply { color = Color.WHITE; style = Paint.Style.STROKE; strokeWidth = 5f; isAntiAlias = true }
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
    /** Search-hit highlight: a light translucent fill so the matched text stays readable on e-ink. */
    private val searchFillPaint = Paint().apply { color = Color.parseColor("#33000000"); style = Paint.Style.FILL; isAntiAlias = true }
    /** A crisp outline around the active search hit (the one the reader is parked on). */
    private val searchBoxPaint = Paint().apply { color = Color.BLACK; style = Paint.Style.STROKE; strokeWidth = 2f; isAntiAlias = true }
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
        // Re-apply the saved page rotation (RR4) before the surface is created so the first render
        // is at the right orientation. configChanges=orientation keeps us from recreating.
        requestedOrientation = orientationPref()
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
                    diag { "DIAG stylus action=$a tool=$tool type=$toolType hist=${event.historySize}" }
                }
                when (tool) {
                    Tool.DEFINE -> captureSelection(event)
                    Tool.ERASER -> captureErase(event)
                    Tool.LASSO -> captureLasso(event)
                    else -> captureStylus(event) // PEN (Highlighter is still P2)
                }
            } else if (toolType == MotionEvent.TOOL_TYPE_FINGER) {
                // A fresh primary DOWN starts a clean gesture: clear both per-gesture latches. This
                // also rescues a latch stranded by a gesture whose terminal UP was consumed elsewhere
                // (minimap, or a 1→2→1 transition that bypassed onFingerUp), which would otherwise kill
                // the next gesture (#49).
                if (event.actionMasked == MotionEvent.ACTION_DOWN) {
                    fingerIsPalm = false; gestureWasMultiTouch = false
                }
                // Latch once this gesture goes multi-pointer: after a pinch, a 2→1 lift leaves the
                // surviving finger with the original down's stale origin, so its trailing pan must be
                // suppressed until a fresh DOWN (#49). Set in BOTH branches below via this single check.
                if (event.pointerCount > 1) gestureWasMultiTouch = true
                // Pinch-zoom must not fire while writing: the resting hand registers as a 2-finger
                // contact and the ScaleGestureDetector would zoom the page out from under the pen.
                // Gate on pen-proximity ONLY: on this hardware a firm pinch fingertip reports a
                // contact-major as large as a palm (160–240px on a 2560px panel), so a contact-size
                // term here suppressed genuine two-finger pinches — pen activity (hover / stroke /
                // within PALM_REJECT_MS) is the reliable discriminator for the writing hand (#49,
                // device-confirmed on Nomad). Single-finger palm rejection still uses size, where
                // taps (≈80–128px) and palms (≈160–240px) separate cleanly.
                if (penActiveForPinch()) {
                    // The pen is active: this is the writing hand, not a deliberate pinch. Reject it
                    // BEFORE the ScaleGestureDetector sees it.
                    // Unconditionally feed the detector a CANCEL: it reverts any in-flight scale to
                    // the committed zoom, AND closes a buffered pointer-down from a fast-settling palm
                    // that landed before the detector crossed its minSpan (so isInProgress was still
                    // false) — gating the cancel on isInProgress would strand that pointer. CANCEL on
                    // an idle detector is a documented no-op, so always sending it is safe.
                    liveScale = 1f
                    val cancel = MotionEvent.obtain(event).apply { action = MotionEvent.ACTION_CANCEL }
                    scaleDetector.onTouchEvent(cancel)
                    cancel.recycle()
                    // Likewise drop any in-flight minimap interaction: its latches are reset ONLY
                    // inside handleMinimapTouch's UP path, which we bypass here — leaving them stuck
                    // would make handleMinimapTouch swallow the next finger gesture once the pen idles.
                    minimapActive = false; minimapThumbDrag = false
                    mainHandler.removeCallbacks(fingerLongPress) // the writing hand, not a tap
                    fingerMoved = true
                } else {
                    scaleDetector.onTouchEvent(event)
                    // A second finger means a pinch-zoom, not a tap/long-press. The single-finger DOWN
                    // armed the word-lookup timer; cancel it the instant a 2nd pointer appears (or the
                    // scale gesture engages) and neutralise this gesture, so a held pinch never triggers
                    // a Dict lookup. (onFingerMove/Up can't do this — they're gated to pointerCount==1.)
                    if (event.pointerCount > 1 || scaleDetector.isInProgress) {
                        mainHandler.removeCallbacks(fingerLongPress)
                        fingerMoved = true
                    }
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
            onChrome = { engine.execute { repaintPanel() } },
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
            // Track pen proximity: HOVER_ENTER/MOVE ⇒ near the glass, HOVER_EXIT ⇒ lifted away. A
            // finger that lands while the pen hovers is the accompanying palm (rejected immediately,
            // without waiting for the PALM_REJECT_MS window to be primed by a touch).
            penHovering = event.actionMasked != MotionEvent.ACTION_HOVER_EXIT
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
        mainHandler.removeCallbacks(inkFlush) // the explicit flush below supersedes the debounce
        ink.teardown() // release the firmware ink claim + clear the overlay
        // Persist the reading position + flush ink when backgrounded (RR27/RR20) — engine thread.
        engine.execute {
            if (docHandle != 0L) {
                try { NativeBridge.nativeInkSave(docHandle) } catch (e: RuntimeException) { Log.e(TAG, "ink flush failed: ${e.message}") }
            }
            savePosition()
        }
    }

    /**
     * Shed bounded native caches under platform memory pressure (RR24-FR3). Posted to the engine
     * thread because the session — and [docHandle] — are engine-thread-only and the render path
     * mutates the cache. `RUNNING_CRITICAL` and any backgrounded/hidden level map to *critical*
     * (drop all caches); lighter running pressure maps to *moderate* (drop the least-critical).
     */
    override fun onTrimMemory(level: Int) {
        super.onTrimMemory(level)
        val code = if (level >= android.content.ComponentCallbacks2.TRIM_MEMORY_RUNNING_CRITICAL) 1 else 0
        engine.execute {
            if (docHandle != 0L) {
                try {
                    NativeBridge.nativeOnTrimMemory(docHandle, code)
                } catch (e: RuntimeException) {
                    Log.e(TAG, "trim memory failed: ${e.message}")
                }
            }
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
            dict.close()
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
        val wasOpen = docHandle != 0L
        openDocumentIfNeeded()
        // A resize/rotation of an ALREADY-open doc: tell the core the new viewport so it renders at
        // the new size (else the render is size-mismatched → the rotated smear). RR21-FR4.
        if (wasOpen && docHandle != 0L) {
            try { NativeBridge.nativeSetViewport(docHandle, width, height, DPI) } catch (e: RuntimeException) {
                Log.e(TAG, "setViewport failed: ${e.message}")
            }
        }
        repaintPanel() // first page carries no command stream → refresh the panel (RR2-FR4)
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
                diag { "DIAG ink store attached for $path" }
                // Defer the per-stroke fsync: edits mark the page dirty and we flush on a trailing
                // debounce (scheduleInkFlush) + on pause/teardown, instead of fsyncing the sidecar
                // on every stroke-end — saves flash wear + energy on long note sessions. The core
                // still flushes on page-change/export, so nothing is lost on navigation.
                NativeBridge.nativeInkSetDeferredAutosave(docHandle, true)
            } catch (e: RuntimeException) {
                Log.e(TAG, "attach ink store failed: ${e.message}")
            }
            // Bookmarks remain a Kotlin sidecar (RR16), keyed by the book id.
            bookmarks = Bookmarks(File(filesDir, "bookmarks/${bookId.hashCode()}.json")).also { it.load() }
            currentBookId = bookId
            reflowOn = false // a fresh document opens in fixed-layout view (ADR-INKREAD-0011)
            // Re-apply the saved reflow text scale (EPUB); a no-op (-1) on fixed-layout PDF.
            val savedScale = textScalePref()
            if (savedScale != 1.0f) {
                val np = try { NativeBridge.nativeSetTextScale(docHandle, savedScale) } catch (e: RuntimeException) { -1 }
                if (np >= 0) Log.i(TAG, "applied text scale $savedScale → page $np")
            }
            // Re-apply the saved display contrast (RR4); 0 = off (a no-op in the core).
            try { NativeBridge.nativeSetContrast(docHandle, contrastPref()) } catch (e: RuntimeException) {}
            // Re-apply the saved page fit mode (RR4); default Page/contain.
            try { NativeBridge.nativeSetFit(docHandle, fitPref()) } catch (e: RuntimeException) {}
            // Re-apply the saved auto-crop + margin (RR4); default off.
            try { NativeBridge.nativeSetCrop(docHandle, if (cropAutoPref()) 1 else 0, cropMarginPref()) } catch (e: RuntimeException) {}
            // Re-apply the saved render quality (RR4); default 1.
            try { NativeBridge.nativeSetRenderQuality(docHandle, renderQualityPref()) } catch (e: RuntimeException) {}
            // Re-apply saved reflow line-spacing + alignment (RR4; EPUB only — PDF returns -1).
            if (lineSpacingPref() != 1) {
                try { NativeBridge.nativeSetLineSpacing(docHandle, LINE_SPACINGS[lineSpacingPref()]) } catch (e: RuntimeException) {}
            }
            if (alignmentPref() != 0) {
                try { NativeBridge.nativeSetAlignment(docHandle, alignmentPref()) } catch (e: RuntimeException) {}
            }
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
        repaintPanel() // the new book's first page has no command stream → refresh
    }

    /** Open a Library book in place (invoked from the reader popup; engine thread). */
    private fun openFromLibrary(file: File) {
        engine.execute { openSwap(file.absolutePath, file.name) }
    }

    /**
     * Render the current page and blit it. [deferLinks] skips the per-page link fetch so the page-turn
     * path (postJump) can flash the panel first and fetch links *after* — links are only needed for
     * the next tap, not before the page is visible.
     */
    private fun renderAndBlit(deferLinks: Boolean = false) {
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
        diag { "DIAG baked ${pageStrokes.size} core strokes on page $currentPage" }
        val cv = Canvas(bmp)
        for (s in pageStrokes) drawStroke(cv, s)
        // The active lasso selection's bounding box (ADR-INKREAD-0010).
        if (selectedIds.isNotEmpty() && selectionBounds.size == 4) drawSelectionBox(cv)
        // The active in-document search hit's highlight boxes (RR2), if it lives on this page.
        val searchHl = search.highlightForPage(currentPage)
        if (searchHl.isNotEmpty()) drawSearchHighlight(cv, searchHl)
        // Zoom minimap (top-right): full page + the current viewport window (RR5-FR3). The fit
        // thumbnail it draws is captured lazily when zoom is first engaged (captureFitThumb), not on
        // every fit-page turn — so ordinary reading pays no per-flip scale + alloc.
        if (zoom > 1f) drawZoomMinimap(cv)
        // A top-right dog-ear: faint outline (tap-to-bookmark affordance) / solid when bookmarked.
        drawBookmarkCorner(cv)
        // Cache the first page as the book's thumbnail, once (RR17-FR5).
        if (currentPage == 0 && currentBookId.isNotEmpty() && !Books.thumbFile(this, currentBookId).exists()) {
            Books.saveThumbnail(this, currentBookId, bmp)
        }
        blit { canvas -> canvas.drawBitmap(bmp, 0f, 0f, null) }
        if (!deferLinks) refreshCurrentLinks()
    }

    /** Cache the current page's links for tap hit-testing (RR11-FR3). Off the page-turn critical path
     *  (postJump calls this after the flash); links are only needed for the next tap. */
    private fun refreshCurrentLinks() {
        val handle = docHandle
        if (handle == 0L) return
        currentLinks = try {
            WireCodec.decodeLinks(NativeBridge.nativePageLinks(handle, currentPage))
        } catch (e: RuntimeException) {
            Log.e(TAG, "links fetch failed: ${e.message}")
            emptyList()
        }
        diag { "DIAG page $currentPage: ${currentLinks.size} links ${currentLinks.take(3).map { it.targetPage ?: it.uri }}" }
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

    /** Verbose diagnostic log, gated by [DIAG]. Inline + lambda so the message is not even built
     *  when tracing is off (these run on render/stroke/tap paths). */
    private inline fun diag(msg: () -> String) {
        if (DIAG) Log.i(TAG, msg())
    }

    // ---- panel repaint (RR2-FR4 / RR15): the single choke point for pushing to the EPD ----

    /**
     * Request a full-screen panel refresh — the ONE place a full EPD refresh is asked for outside
     * the policy's page-turn command stream. Routing every chrome/dialog/selection refresh through
     * here gives a single audit + extension point: the adapter coalesces bursts
     * (see [dev.jraghavan.inkread.eink.EinkAdapter.refreshFull]), and future partial-refresh logic
     * lands here, not at ~two dozen call sites.
     */
    private fun refreshPanel() {
        adapter.refreshFull()
    }

    /** Re-render the current page into the surface, then refresh the panel — the common "something
     *  changed, show it" path (engine thread). Page turns instead drive the policy's command
     *  stream via [dev.jraghavan.inkread.eink.EinkAdapter.executeAll]. */
    private fun repaintPanel() {
        renderAndBlit()
        refreshPanel()
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
        diag { "DIAG finalizeStroke buf=${strokeBuf.size / 2} pts" }
        if (strokeBuf.size < 2) { strokeBuf.clear(); return }
        val raw = strokeBuf.toFloatArray()
        strokeBuf.clear()
        engine.execute { commitStroke(raw) }
    }

    /** Map packed view-space `[x,y,…]` to packed page-normalized `[x,y,…]` for [NativeBridge.nativeInkAddPoints]. */
    private fun toNormPoints(view: FloatArray): FloatArray {
        val out = FloatArray(view.size)
        var i = 0
        while (i + 1 < view.size) {
            out[i] = vToNx(view[i]); out[i + 1] = vToNy(view[i + 1]); i += 2
        }
        return out
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
            NativeBridge.nativeInkAddPoints(docHandle, toNormPoints(raw))
            NativeBridge.nativeInkEndStroke(docHandle)
            scheduleInkFlush() // deferred autosave: persist on a trailing debounce, not this fsync
            diag { "DIAG commitStroke OK ${raw.size / 2} pts tool=$tool → core page $currentPage" }
        } catch (e: RuntimeException) {
            Log.e(TAG, "ink commit failed: ${e.message}")
        }
        // Highlighter's firmware EMR ink is suppressed (we drew the live band ourselves), so bake it
        // from the core now. Pen rides the firmware overlay and bakes on the next full render.
        if (isHl) { clearFirmwareInk(); repaintPanel(); return }
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

    /** Snapshot the current fit page for the zoom minimap — called once when zoom is engaged from
     *  fit (not on every flip). At that point [bitmap] still holds the fit render (a pinch only
     *  transforms it on the surface, never overwrites it). */
    private fun captureFitThumb() {
        if (zoom <= 1f) bitmap?.let { updateFitThumb(it) }
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
        // A white halo first so the ribbon reads on any background (e.g. a black title band).
        canvas.drawPath(path, bookmarkHaloPaint)
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
    /**
     * Heuristic palm / stray-touch test shared by the reading surface AND the chrome dialogs
     * (RR19 palm rejection): a **finger** touch is treated as a palm when it is multi-pointer, lands
     * within [PALM_REJECT_MS] of pen activity, arrives mid-stroke, or has a large contact major
     * (≥ [PALM_TOUCH_MAJOR_FRAC] of the panel height). A stylus/eraser touch is never a palm.
     */
    private fun isPalmTouch(e: MotionEvent): Boolean {
        val toolType = e.getToolType(0)
        return PalmFilter.isPalm(
            isStylusTool = toolType == MotionEvent.TOOL_TYPE_STYLUS || toolType == MotionEvent.TOOL_TYPE_ERASER,
            pointerCount = e.pointerCount,
            penHovering = penHovering,
            strokeInProgress = strokeBuf.isNotEmpty(),
            msSinceStylus = SystemClock.uptimeMillis() - lastStylusMs,
            palmRejectMs = PALM_REJECT_MS,
            touchMajorPx = e.getTouchMajor(0),
            viewHeightPx = surfaceView.height,
            touchMajorFrac = PALM_TOUCH_MAJOR_FRAC,
        )
    }

    /**
     * True while the pen is active (hovering, mid-stroke, or lifted within [PALM_REJECT_MS]) — so a
     * concurrent finger contact is the writing hand. Gates the pinch-zoom detector: the resting palm
     * during writing otherwise reads as a two-finger pinch and zooms the page out from under the pen
     * (RR19 palm rejection extended to the pincher). Mirrors [isPalmTouch]'s pen-proximity test,
     * minus the multi-pointer check (a pinch IS multi-pointer; pen proximity is the discriminator).
     */
    private fun penActiveForPinch(): Boolean = PalmFilter.isPenActive(
        penHovering = penHovering,
        strokeInProgress = strokeBuf.isNotEmpty(),
        msSinceStylus = SystemClock.uptimeMillis() - lastStylusMs,
        palmRejectMs = PALM_REJECT_MS,
    )

    /**
     * Wrap a chrome view (bottom bar, sheets) so a resting palm can't press its controls — the
     * single biggest palm-rejection gap, since dialog buttons bypass the reading surface's filter.
     * A palm-like DOWN is intercepted (swallowed) before it reaches any child; a real finger/stylus
     * tap passes straight through.
     */
    private fun palmGuard(content: View): View =
        object : FrameLayout(this) {
            override fun onInterceptTouchEvent(ev: MotionEvent): Boolean {
                if (ev.actionMasked == MotionEvent.ACTION_DOWN && isPalmTouch(ev)) {
                    diag { "DIAG chrome palm-reject major=${ev.getTouchMajor(0)} pc=${ev.pointerCount}" }
                    return true // consume here; children (buttons) never see it
                }
                return super.onInterceptTouchEvent(ev)
            }
        }.apply {
            addView(content, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
        }

    private fun onFingerDown(e: MotionEvent) {
        if (isPalmTouch(e)) {
            diag { "DIAG palm-reject down pc=${e.pointerCount} major=${e.getTouchMajor(0)}" }
            fingerIsPalm = true // latch: MOVE/UP bail, so a rejected palm never pans (esp. zoomed in)
            fingerMoved = true // also neutralise the tap/long-press paths
            return
        }
        fingerIsPalm = false
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
        // Bail (and latch) if already a palm, or if the pen engages mid-gesture (the writing hand
        // settling while the finger is still down). Re-validate pen-proximity only — NOT contact
        // size: a fast/flat legit swipe can momentarily spike touch-major past the palm fraction, and
        // re-checking size every MOVE would make one spurious sample kill the swipe (#49 review).
        if (fingerIsPalm || penActiveForPinch()) {
            fingerIsPalm = true
            return
        }
        // After a pinch, the surviving finger's origin is stale — don't pan until a fresh gesture.
        if (gestureWasMultiTouch) return
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
        // A palm commits nothing — no pan, no tap. Bail if latched, or if the pen is active (a finger
        // lift within PALM_REJECT_MS of pen activity is the writing hand; same RR19 rule the tap path
        // already applies below, here extended to the zoomed-in pan commit). This is the core of the
        // "resting hand pans/zooms the page" fix (#49).
        if (fingerIsPalm || penActiveForPinch()) { fingerIsPalm = false; return }
        // After a pinch, a 2→1 lift leaves a stale origin: commit no pan/tap for the trailing finger
        // (a fresh DOWN clears this latch and restarts clean panning) (#49).
        if (gestureWasMultiTouch) return
        // Zoomed in: a drag pans the page; a still tap in the L/R edge zone turns the page while
        // KEEPING the zoom (#52), so a zoomed reader advances without zooming out and back. The core
        // preserves zoom + column and resets to the top of the new page on a turn; mirror that
        // top-reset locally (panY = 0) so the shell's pan stays in sync (no native read-back). Center
        // stays a no-op while zoomed (it's content, not the menu).
        if (zoom > 1f) {
            if (fingerMoved) {
                val overX = viewW * (zoom - 1f); val overY = viewH * (zoom - 1f)
                if (overX > 0f) panX = (panX - (e.x - fingerDownX) / overX).coerceIn(0f, 1f)
                if (overY > 0f) panY = (panY - (e.y - fingerDownY) / overY).coerceIn(0f, 1f)
                applyZoom()
            } else {
                val w = surfaceView.width.toFloat()
                if (w > 0f) {
                    val third = w / 3f
                    if (fingerDownX < third) { panY = 0f; queuePageTurn(-1) }
                    else if (fingerDownX > 2f * third) { panY = 0f; queuePageTurn(+1) }
                }
            }
            return
        }
        if (fingerLookupFired) { fingerLookupFired = false; return } // the hold already looked up
        if (fingerMoved) return // a swipe or rejected palm — not a tap
        if (SystemClock.uptimeMillis() - lastStylusMs > PALM_REJECT_MS && strokeBuf.isEmpty()) {
            handleTap(fingerDownX, fingerDownY)
        } else {
            diag { "DIAG tap suppressed (stylus active → palm)" }
        }
    }

    private fun handleTap(x: Float, y: Float) {
        val w = surfaceView.width.toFloat()
        val h = surfaceView.height.toFloat()
        if (w > 0f && h > 0f) {
            val link = currentLinks.firstOrNull { it.contains(vToNx(x), vToNy(y)) }
            if (link != null) {
                diag { "DIAG handleTap link hit -> ${link.targetPage ?: link.uri}" }
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
        diag { "DIAG handleTap x=$x w=$w -> $zone (${currentLinks.size} links, no hit)" }
        when (zone) {
            "PREV" -> queuePageTurn(-1)
            "NEXT" -> queuePageTurn(+1)
            else -> showBottomBar()
        }
    }

    // ---- coalesced page turns (RR25) -----------------------------------------------------------
    // Each edge tap used to enqueue its own render + full EPD refresh on the (serial) engine thread,
    // so holding/mashing the right edge ran N slow cycles back-to-back. Instead we accumulate the net
    // page delta and issue ONE jump: the first tap fires immediately, and any taps that land while
    // that render is in flight are batched into a single follow-up jump. 10 fast taps → 1–2 renders.

    /** Pending net page delta from edge taps; UI-thread only. */
    private var pendingPageDelta = 0
    /** True while a coalesced jump is rendering; taps accumulate instead of enqueuing more (UI thread). */
    private var turnInFlight = false

    private fun queuePageTurn(delta: Int) {
        pendingPageDelta += delta
        flushPageTurns()
    }

    /** Apply the accumulated delta as a single jump, unless one is already rendering (then it drains
     *  on completion). Snaps to the document bounds; a no-op at the edges. */
    private fun flushPageTurns() {
        if (turnInFlight || pendingPageDelta == 0) return
        val last = pageCount.coerceAtLeast(1) - 1
        val target = (currentPage + pendingPageDelta).coerceIn(0, last)
        pendingPageDelta = 0
        if (target == currentPage) return
        turnInFlight = true
        postJump(target) { runOnUiThread { turnInFlight = false; flushPageTurns() } }
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
    private fun postJump(page: Int, onDone: (() -> Unit)? = null) {
        engine.execute {
            try {
                if (docHandle == 0L) return@execute
                val commandBytes = try {
                    NativeBridge.nativeJumpToPage(docHandle, page)
                } catch (e: RuntimeException) {
                    Log.e(TAG, "jump failed: ${e.message}")
                    return@execute
                }
                ink.clearAll() // wipe the firmware ink overlay so it doesn't bleed onto the new page
                dropSelectionForPageChange()
                renderAndBlit(deferLinks = true)
                // Flash the panel FIRST so the new page is visible with no persistence/links work
                // in front of it; then do the off-critical-path bookkeeping (RR27 position + links).
                adapter.executeAll(WireCodec.decodeCommands(commandBytes))
                savePosition() // persist position per jump so an abrupt kill still reopens here (RR27)
                refreshCurrentLinks()
            } finally {
                onDone?.invoke() // release the coalescing latch even on early-out / error
            }
        }
    }

    /** Drop any lasso selection when the page changes — the ids belong to the old page (engine). */
    private fun dropSelectionForPageChange() {
        if (selectedIds.isEmpty()) return
        selectedIds = IntArray(0)
        selectionBounds = FloatArray(0)
        runOnUiThread { selectionToolbar.dismiss() }
    }


    // ---- in-document search (RR2) ----

    /** Draw the search hit's highlight [boxes] on the page (a light fill + crisp outline). The
     *  active boxes for the current page come from [SearchController.highlightForPage]. */
    private fun drawSearchHighlight(canvas: Canvas, boxes: List<SelBox>) {
        for (b in boxes) {
            val l = nToVx(b.x0); val t = nToVy(b.y0); val r = nToVx(b.x1); val btm = nToVy(b.y1)
            canvas.drawRect(l, t, r, btm, searchFillPaint)
            canvas.drawRect(l, t, r, btm, searchBoxPaint)
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
            setBackgroundColor(Ink.paper)
        }
        // A crisp black keyline up top so the bar reads as a docked surface, not a floating box.
        container.addView(
            View(this).apply { setBackgroundColor(Ink.ink) },
            LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, Ink.hair()),
        )

        // Page-slider row:  [N / Total]  ────────●────────  (grayscale, tap the chip to type a page)
        val pageLabel = TextView(this).apply {
            text = "${cur + 1} / $total"
            setTextColor(Ink.ink)
            textSize = 12f
            typeface = Ink.mono
            letterSpacing = 0.04f
            gravity = Gravity.CENTER
            setPadding(dp(12), dp(6), dp(12), dp(6))
            background = GradientDrawable().apply {
                setColor(Ink.fill)
                cornerRadius = Ink.dpf(40)
            }
            setOnClickListener { dialog.dismiss(); showPageEntry(total) }
        }
        // A refined, thin grayscale track + small round thumb (the default SeekBar reads clunky).
        val trackH = dp(3).coerceAtLeast(2)
        fun bar(c: Int) = GradientDrawable().apply { setColor(c); cornerRadius = trackH.toFloat(); setSize(0, trackH) }
        val track = android.graphics.drawable.LayerDrawable(
            arrayOf(
                bar(Ink.hairline),
                android.graphics.drawable.ClipDrawable(bar(Ink.ink), Gravity.START, android.graphics.drawable.ClipDrawable.HORIZONTAL),
            ),
        ).apply { setId(0, android.R.id.background); setId(1, android.R.id.progress) }
        val knob = GradientDrawable().apply { shape = GradientDrawable.OVAL; setColor(Ink.ink); setSize(dp(16), dp(16)) }
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
                    setImageResource(iconRes); setColorFilter(Ink.ink)
                },
                LinearLayout.LayoutParams(dp(39), dp(39)),
            )
            cell.addView(TextView(this).apply {
                text = label; setTextColor(Ink.inkSoft); textSize = 11f
                typeface = Ink.mono; letterSpacing = 0.02f
                gravity = Gravity.CENTER; setPadding(0, dp(5), 0, 0)
            })
            controls.addView(cell, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        }
        // "Home" already opens the library home, so a separate Library item here is redundant.
        control(R.drawable.ic_menu_home, "Home") { goHome() }
        // (Bookmark toggle moved to the top-right corner dog-ear; "Marks" lists them.)
        control(R.drawable.ic_menu_marks, "Marks") { showBookmarks() }
        control(R.drawable.ic_menu_contents, "Contents") { showContentsLazy() }
        control(R.drawable.ic_menu_search, "Search") { search.showSearchDialog() }
        // Quick zoom (circle −/+ icons — not magnifiers, which are reserved for Search). Also in Adjust → Zoom.
        control(R.drawable.ic_menu_zoom_out, "Zoom −") { zoomBy(1f / ZOOM_STEP) }
        control(R.drawable.ic_menu_zoom_in, "Zoom +") { zoomBy(ZOOM_STEP) }
        control(R.drawable.ic_menu_export, "Export") { export.showExportDialog() }
        control(R.drawable.ic_menu_dict, "Dicts") { dict.showDictionariesDialog() }
        // Document controls consolidated into one KOReader-style tabbed sheet (Rotate/Fit/Font/Display).
        control(R.drawable.ic_menu_adjust, "Adjust") { showAdjustSheet() }
        control(R.drawable.ic_menu_open, "Open") { openPicker() }
        container.addView(controls)

        // Palm guard: a hand resting on the bottom-anchored bar must not press a control (esp. Home).
        dialog.setContentView(palmGuard(container))
        dialog.window?.apply {
            setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
            setGravity(Gravity.BOTTOM)
            setBackgroundDrawable(android.graphics.drawable.ColorDrawable(Ink.paper))
        }
        dialog.show()
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
            repaintPanel()
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
            setPadding(dp(24), dp(22), dp(24), dp(14))
        }
        outer.addView(Ink.eyebrow(this, "Contents"))
        outer.addView(Ink.gap(this, 10))
        outer.addView(Ink.rule(this))
        outer.addView(Ink.gap(this, 4))
        val list = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        toc.forEachIndexed { i, item ->
            if (i > 0) list.addView(View(this).apply { setBackgroundColor(Ink.hairline) },
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, Ink.hair()))
            list.addView(LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                setPadding(dp(4) + item.depth * dp(18), dp(14), dp(4), dp(14))
                isClickable = true
                setOnClickListener { dialog.dismiss(); item.targetPage?.let { postJump(it) } }
                addView(TextView(this@ReaderActivity).apply {
                    text = item.title
                    setTextColor(if (item.targetPage != null) Ink.ink else Ink.muted)
                    textSize = if (item.depth == 0) 17f else 15f
                    typeface = if (item.depth == 0) Ink.serifBold else Ink.serif
                    maxLines = 2
                }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
                item.targetPage?.let { p ->
                    addView(TextView(this@ReaderActivity).apply {
                        text = "${p + 1}"; setTextColor(Ink.muted); textSize = 12f; typeface = Ink.mono
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
            setBackgroundDrawable(Ink.cardBg())
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
        // Re-tapping the active Pen/Highlighter shows (or restyles, in place) its colour column — an
        // in-window view (see ColorPalette), so it never steals focus and the firmware keeps the
        // live-ink overlay (the only thing that displays committed strokes on the current page). The
        // column is PERSISTENT: it is never collapsed/removed while the tool stays active, because
        // removing the overlay view disturbs that firmware overlay and a sideloaded app cannot force
        // a same-page refresh to repaint it (verified on-device — only page turns refresh). It closes
        // only on a tool switch (a deliberate context change). No Toast on pick: a Toast is a separate
        // window that steals focus and drops the overlay — the ringed swatch is the feedback.
        if (chosen == Tool.HIGHLIGHTER && tool == Tool.HIGHLIGHTER) {
            if (colorPalette.isShowing()) collapseColorPalette()
            else openColorColumn("Highlighter", HIGHLIGHT_COLORS, HIGHLIGHT_COLOR_NAMES, hlColorIdx) { hlColorIdx = it }
            return true
        }
        if (chosen == Tool.PEN && tool == Tool.PEN) {
            if (colorPalette.isShowing()) collapseColorPalette()
            else openColorColumn("Pen", PEN_COLORS, PEN_COLOR_NAMES, penColorIdx) { penColorIdx = it }
            return true
        }
        if (chosen == tool) return true
        colorPalette.dismiss() // close the colour column when switching tools
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
            repaintPanel()
        }
        val hint = when (chosen) {
            Tool.PEN -> "Pen — write with the stylus"
            Tool.HIGHLIGHTER -> "Highlighter — drag over text; tap again to change shade"
            Tool.ERASER -> "Eraser — drag the stylus over ink to remove it"
            Tool.DEFINE -> "Define — tap a word to look it up; drag over text to select it"
            Tool.LASSO -> "Lasso — circle strokes to select; tap Lasso again for Freehand"
            else -> chosen.label
        }
        Toast.makeText(this, hint, Toast.LENGTH_SHORT).show()
        updateLassoHint()
        return true
    }

    /**
     * Collapse the colour column. Removing the overlay view disturbs the firmware ink overlay, so we
     * must repaint the page afterwards — and the ONLY repaint this firmware honours on the current
     * page is the policy page-render path (the same one a page turn uses, proven to produce a real
     * EPD frame-done). [postJump] of the current page runs clearAll + renderAndBlit(baked ink) +
     * executeAll(refresh command stream), which re-displays the committed strokes.
     */
    private fun collapseColorPalette() {
        colorPalette.dismiss()
        postJump(currentPage)
    }

    /**
     * Mount the colour column, then repaint the current page — the SHOW-side mirror of
     * [collapseColorPalette]. Adding the overlay view triggers the firmware's full auto-refresh, which
     * repaints the page from the app surface and wipes the live-ink overlay; strokes drawn since the
     * last page render live ONLY on that overlay, so without this repaint they vanish until a page turn
     * re-bakes them (the "tap Pen → annotations disappear" report, #50). [postJump] of the current page
     * re-bakes the committed strokes (renderAndBlit) and drives a real EPD frame, restoring them.
     */
    private fun openColorColumn(title: String, colors: IntArray, names: Array<String>, sel: Int, onPick: (Int) -> Unit) {
        colorPalette.show(title, colors, names, sel, onPick)
        postJump(currentPage)
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
            repaintPanel()
        }
    }

    /** Multiply the zoom (clamped); snap back to fit at ~1. Used by the +/- buttons and pinch-end. */
    private fun zoomBy(factor: Float) {
        val next = (zoom * factor).coerceIn(1f, MAX_ZOOM_UI)
        if (zoom <= 1f && next > 1f) captureFitThumb() // grab the fit thumb before leaving fit
        zoom = next
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
                if (gestureStartZoom <= 1f && newZoom > 1f) captureFitThumb() // zoom field still ≤1 here
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
            NativeBridge.nativeInkAddPoints(docHandle, toNormPoints(viewPts))
            NativeBridge.nativeInkEndStroke(docHandle)
            scheduleInkFlush() // deferred autosave: persist on a trailing debounce, not this fsync
        } catch (e: RuntimeException) {
            Log.e(TAG, "erase failed: ${e.message}"); return
        }
        clearFirmwareInk() // wipe the firmware ink left by the eraser drag
        repaintPanel()
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
        diag { "DIAG finalizeLasso buf=${lassoBuf.size / 2} pts mode=$lassoMode" }
        if (lassoBuf.size < 6) { // need ≥3 points for a polygon
            lassoBuf.clear()
            engine.execute { clearFirmwareInk(); repaintPanel() }
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
            diag { "DIAG lasso selected ${ids.size} strokes from ${poly.size / 2}-pt loop" }
            // No ink under the loop → fall back to selecting the PRINTED words inside it (the user
            // circled book text, not handwriting). Lasso thus selects ink OR text — circle anything.
            if (ids.isEmpty()) selectTextInLoop(poly) else setSelection(ids)
        }
    }

    /**
     * Lasso text fallback (engine thread): the gesture found no ink, so select printed text. An
     * **open diagonal drag** across lines (start far from lift, spanning >1 line) is a reading-order
     * line span — start line through the line before the lift taken whole, the lift line clipped to
     * its word, gaps filled ([NativeBridge.nativeTextLineSpan]). A **closed loop** around a few words
     * uses the polygon's bounding box ([NativeBridge.nativeTextInRect]). Then offer the actions.
     */
    private fun selectTextInLoop(poly: FloatArray) {
        if (docHandle == 0L || poly.size < 6) {
            runOnUiThread { Toast.makeText(this, "Nothing under the loop", Toast.LENGTH_SHORT).show() }
            return
        }
        val sx = poly[0]; val sy = poly[1]
        val ex = poly[poly.size - 2]; val ey = poly[poly.size - 1]
        var x0 = Float.MAX_VALUE; var y0 = Float.MAX_VALUE; var x1 = -Float.MAX_VALUE; var y1 = -Float.MAX_VALUE
        var i = 0
        while (i + 1 < poly.size) {
            x0 = minOf(x0, poly[i]); x1 = maxOf(x1, poly[i])
            y0 = minOf(y0, poly[i + 1]); y1 = maxOf(y1, poly[i + 1])
            i += 2
        }
        // Open drag (lift far from start) spanning multiple lines → reading-order line span; a closed
        // loop (lift returns near the start) → the bounding box of what was circled.
        val openDrag = kotlin.math.hypot(ex - sx, ey - sy) > OPEN_DRAG_FRAC
        val multiLine = (y1 - y0) > MULTILINE_DRAG_FRAC
        if (openDrag && multiLine) {
            presentLineSpanSelection(sx, sy, ex, ey, "No text under the selection")
        } else {
            presentTextSelection(x0, y0, x1, y1, "Nothing under the loop — circle ink or printed words")
        }
    }

    /**
     * Select the printed text in a normalized rect, shade the caught boxes, and offer
     * Define / Copy / Highlight (engine thread). Shared by the lasso text fallback and a Define-tool
     * drag. [emptyMsg] is toasted when the rect holds no text. A drag is a *selection*, never an
     * auto-lookup — the user picks Define from the action sheet if they want a definition.
     */
    private fun presentTextSelection(x0: Float, y0: Float, x1: Float, y1: Float, emptyMsg: String) {
        if (docHandle == 0L) return
        val sel = try {
            WireCodec.decodeSelection(NativeBridge.nativeTextInRect(docHandle, currentPage, x0, y0, x1, y1))
        } catch (e: RuntimeException) {
            Log.e(TAG, "text-in-rect failed: ${e.message}"); Selection("", emptyList())
        }
        showSelectionResult(sel, emptyMsg)
    }

    /**
     * Multi-line drag (engine thread): the reading-order selection the core sweeps from the drag's
     * start point to its lift point — whole lines through to the line before the lift, the lift line
     * clipped to the word under it, inter-line gaps filled (see [NativeBridge.nativeTextLineSpan]).
     */
    private fun presentLineSpanSelection(sx: Float, sy: Float, ex: Float, ey: Float, emptyMsg: String) {
        if (docHandle == 0L) return
        diag { "DIAG lineSpan start=(%.3f,%.3f) lift=(%.3f,%.3f) page=$currentPage".format(sx, sy, ex, ey) }
        val sel = try {
            WireCodec.decodeSelection(NativeBridge.nativeTextLineSpan(docHandle, currentPage, sx, sy, ex, ey))
        } catch (e: RuntimeException) {
            Log.e(TAG, "text-line-span failed: ${e.message}"); Selection("", emptyList())
        }
        showSelectionResult(sel, emptyMsg)
    }

    /** Render the caught selection's boxes and offer the action sheet — shared by the bbox and
     *  line-span selection paths (engine thread). A drag is a *selection*, never an auto-lookup. */
    private fun showSelectionResult(sel: Selection, emptyMsg: String) {
        diag { "DIAG text selection: '${sel.text.take(60)}' boxes=${sel.boxes.size}" }
        clearFirmwareInk() // wipe the firmware ink the select gesture left behind
        renderAndBlit()
        if (sel.isEmpty) {
            refreshPanel()
            runOnUiThread { Toast.makeText(this, emptyMsg, Toast.LENGTH_SHORT).show() }
            return
        }
        drawTextSelectionBoxes(sel.boxes) // show what was caught, then offer actions
        refreshPanel()
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
        // Define is a per-word action — it makes no sense for a multi-line selection, so a multi-line
        // catch (more than one line box) offers only Copy + Highlight.
        val items = if (sel.boxes.size > 1) arrayOf("Copy", "Highlight", "Add to Digest")
        else arrayOf("Define", "Copy", "Highlight", "Add to Digest")
        AlertDialog.Builder(this, R.style.InkDialog)
            .setTitle(if (snippet.length > 42) snippet.take(42) + "…" else snippet)
            .setItems(items) { _, which ->
                when (items[which]) {
                    "Define" -> dict.defineSelectionText(snippet)
                    "Copy" -> copyTextToClipboard(snippet)
                    "Highlight" -> engine.execute { highlightTextBoxes(sel) }
                    "Add to Digest" -> digest.addDigestText(currentPage, sel.text)
                }
            }
            // Any dismissal (action chosen or cancelled) clears the box overlay; a Highlight redraws
            // it with the real annotation, a Define opens the dict card over the cleared page.
            .setOnDismissListener { engine.execute { repaintPanel() } }
            .show()
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
            scheduleInkFlush() // deferred autosave: persist the baked bands on the trailing debounce
            diag { "DIAG highlighted ${sel.boxes.size} text boxes" }
        } catch (e: RuntimeException) {
            Log.e(TAG, "text highlight failed: ${e.message}")
        }
        clearFirmwareInk(); repaintPanel()
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
        repaintPanel()
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
            // Save the PDF text under the selection into the Supernote Digest; keep the selection up.
            SelAction.DIGEST -> if (ids.isNotEmpty()) digest.addDigest(currentPage, selectionBounds.copyOf())
        }
    }

    /** Undo the last ink edit (from the tool pill). Global — refreshes any active selection too. */
    private fun inkUndo() = engine.execute {
        try { NativeBridge.nativeInkUndo(docHandle); scheduleInkFlush() } catch (e: RuntimeException) {}
        refreshSelectionAfterHistory()
    }

    /** Redo the last undone ink edit (from the tool pill). */
    private fun inkRedo() = engine.execute {
        try { NativeBridge.nativeInkRedo(docHandle); scheduleInkFlush() } catch (e: RuntimeException) {}
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
        repaintPanel()
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
        if (dragged) {
            // A drag is a text *selection*, not a one-word lookup: show the caught text + the
            // Copy/Highlight (and Define for one line) sheet, never an auto dict card.
            val multiLine = (maxY - minY) > h * MULTILINE_DRAG_FRAC
            if (multiLine) {
                // The core sweeps from the drag's start point to its lift point: whole lines through
                // to the line before the lift, the lift line clipped to its word, gaps filled.
                val sx = vToNx(pts[0]); val sy = vToNy(pts[1])
                val ex = vToNx(pts[pts.size - 2]); val ey = vToNy(pts[pts.size - 1])
                engine.execute { presentLineSpanSelection(sx, sy, ex, ey, "No text under the selection") }
            } else {
                // Single-line drag: the dragged horizontal span on that one line.
                val r = floatArrayOf(vToNx(minX), vToNy(minY), vToNx(maxX), vToNy(maxY))
                engine.execute { presentTextSelection(r[0], r[1], r[2], r[3], "No text under the selection") }
            }
        } else {
            // A single still tap is a word lookup (the dict card).
            val page = currentPage
            engine.execute { dict.defineWord(page, vToNx(pts[0]), vToNy(pts[1])) }
            // Wipe the firmware ink the define gesture left behind (it never becomes an annotation).
            engine.execute { clearFirmwareInk(); repaintPanel() }
        }
    }

    private fun contrastPref(): Int =
        getSharedPreferences("display", MODE_PRIVATE).getInt("contrast", 0).coerceIn(0, CONTRAST_MAX)

    private fun setContrastPref(step: Int) =
        getSharedPreferences("display", MODE_PRIVATE).edit().putInt("contrast", step).apply()

    // ---- Document settings sheet (RR4 — KOReader-style tabbed controls) ----

    /**
     * A KOReader-style tabbed settings sheet that consolidates the document controls
     * (Rotate / Fit / Font / Display) behind one bottom-bar entry — matching KOReader's bottom
     * sheet structure. Each tab swaps an inline control panel; changes apply live + persist.
     */
    private fun showAdjustSheet() {
        if (docHandle == 0L) { openPicker(); return }
        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val dialog = Dialog(this).apply { requestWindowFeature(Window.FEATURE_NO_TITLE) }
        val content = android.widget.FrameLayout(this)

        val panels: List<Triple<String, Int, () -> View>> = listOf(
            Triple("Rotate", R.drawable.ic_menu_rotate) { rotationPanel() },
            Triple("Crop", R.drawable.ic_menu_crop) { cropPanel() },
            Triple("Zoom", R.drawable.ic_menu_fit) { zoomPanel() },
            Triple("Page", R.drawable.ic_menu_page) { pagePanel() },
            Triple("Font", R.drawable.ic_menu_font) { fontPanel() },
            Triple("Display", R.drawable.ic_menu_display) { displayPanel() },
        )
        val cells = ArrayList<LinearLayout>()
        fun select(i: Int) {
            content.removeAllViews()
            content.addView(panels[i].third())
            cells.forEachIndexed { j, c ->
                // Active tab: white, boxed (connected to the panel) + bold label. Inactive: flat gray.
                if (j == i) {
                    c.background = GradientDrawable().apply {
                        setColor(Ink.paper); setStroke(Ink.keyline(), Ink.ink)
                    }
                } else {
                    c.background = null
                    c.setBackgroundColor(Ink.fill)
                }
                val tab = c.getChildAt(1) as? TextView
                tab?.setTypeface(Ink.mono, if (j == i) android.graphics.Typeface.BOLD else android.graphics.Typeface.NORMAL)
                tab?.setTextColor(if (j == i) Ink.ink else Ink.inkSoft)
            }
        }
        val tabRow = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setBackgroundColor(Ink.fill)
        }
        panels.forEachIndexed { i, (label, icon, _) ->
            val cell = LinearLayout(this).apply {
                orientation = LinearLayout.VERTICAL; gravity = Gravity.CENTER
                setPadding(dp(4), dp(10), dp(4), dp(10)); isClickable = true
                setOnClickListener { select(i) }
                addView(
                    ImageView(this@ReaderActivity).apply { setImageResource(icon); setColorFilter(Ink.ink) },
                    LinearLayout.LayoutParams(dp(24), dp(24)),
                )
                addView(TextView(this@ReaderActivity).apply {
                    text = label; textSize = 10f; setTextColor(Ink.inkSoft); typeface = Ink.mono
                    gravity = Gravity.CENTER; setPadding(0, dp(3), 0, 0)
                })
            }
            cells.add(cell)
            tabRow.addView(cell, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        }
        val container = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Ink.paper)
            // Black keyline up top so the sheet reads as a docked surface (bottom-bar template).
            addView(
                View(this@ReaderActivity).apply { setBackgroundColor(Ink.ink) },
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, Ink.hair()),
            )
            // Active tab's panel — WRAP_CONTENT so the sheet GROWS UP per tab (KOReader-style),
            // bottom-anchored, instead of a fixed box with dead space.
            addView(
                content,
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT),
            )
            addView(
                View(this@ReaderActivity).apply { setBackgroundColor(Ink.hairline) },
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, Ink.hair()),
            )
            addView(tabRow)
        }
        select(0)
        dialog.setContentView(palmGuard(container)) // same palm guard as the bottom bar
        dialog.window?.apply {
            setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
            setGravity(Gravity.BOTTOM)
            setBackgroundDrawable(android.graphics.drawable.ColorDrawable(Ink.paper))
        }
        dialog.show()
    }

    /** A KOReader-style **segmented control**: a rounded pill of [options] with the [selected]
     *  segment filled dark. Updates its own highlight on tap, then calls [onSelect]. */
    private fun segmented(options: List<String>, selected: Int, onSelect: (Int) -> Unit): View {
        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val radius = dp(20).toFloat()
        var sel = selected
        val segs = ArrayList<TextView>()
        fun style(tv: TextView, on: Boolean) {
            if (on) {
                tv.setTextColor(Ink.paper)
                tv.setTypeface(null, android.graphics.Typeface.BOLD)
                tv.background = GradientDrawable().apply { setColor(Ink.ink); cornerRadius = radius }
            } else {
                tv.setTextColor(Ink.ink)
                tv.setTypeface(null, android.graphics.Typeface.NORMAL)
                tv.background = null
            }
        }
        return LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            background = GradientDrawable().apply {
                setColor(Ink.paper); cornerRadius = radius
                setStroke(Ink.hair(), Ink.ringSoft)
            }
            val p = dp(3); setPadding(p, p, p, p)
            options.forEachIndexed { i, opt ->
                val tv = TextView(this@ReaderActivity).apply {
                    text = opt; textSize = 15f; gravity = Gravity.CENTER
                    setPadding(dp(6), dp(10), dp(6), dp(10)); isClickable = true
                    setOnClickListener { sel = i; segs.forEachIndexed { j, t -> style(t, j == sel) }; onSelect(i) }
                }
                style(tv, i == sel)
                segs.add(tv)
                addView(tv, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            }
        }
    }

    /** A KOReader-style **cell bar**: [count] boxes filled up to the current level; tapping box i
     *  sets level i+1 (tapping the current top cell turns it off → 0). Repaints on tap. */
    private fun cellBar(count: Int, initial: Int, onSet: (Int) -> Unit): View {
        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        var filled = initial
        val draws = ArrayList<GradientDrawable>()
        fun repaint() = draws.forEachIndexed { i, g ->
            g.setColor(if (i < filled) Ink.ink else Ink.paper)
        }
        return LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
            for (i in 0 until count) {
                val g = GradientDrawable().apply { setStroke(Ink.hair(), Ink.ringSoft) }
                draws.add(g)
                addView(View(this@ReaderActivity).apply {
                    background = g; isClickable = true
                    setOnClickListener {
                        filled = if (i + 1 == filled) 0 else i + 1
                        repaint(); invalidate()
                        onSet(filled)
                    }
                }, LinearLayout.LayoutParams(0, dp(30), 1f).apply { val m = dp(2); setMargins(m, 0, m, 0) })
            }
            repaint()
        }
    }

    /** A KOReader-style settings row: a right-aligned [label] on the left, the [control] on the right. */
    private fun settingRow(label: String, control: View): View {
        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        return LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(16), dp(14), dp(16), dp(14))
            addView(TextView(this@ReaderActivity).apply {
                text = label; textSize = 16f; setTextColor(Color.BLACK); gravity = Gravity.END
            }, LinearLayout.LayoutParams(dp(96), ViewGroup.LayoutParams.WRAP_CONTENT))
            addView(control, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
                marginStart = dp(14)
            })
        }
    }

    private fun rotationPanel(): View {
        val orients = intArrayOf(
            ActivityInfo.SCREEN_ORIENTATION_PORTRAIT,
            ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE,
            ActivityInfo.SCREEN_ORIENTATION_REVERSE_PORTRAIT,
            ActivityInfo.SCREEN_ORIENTATION_REVERSE_LANDSCAPE,
        )
        val sel = orients.indexOf(orientationPref()).coerceAtLeast(0)
        return settingRow("Rotation", segmented(listOf("0°", "90°", "180°", "270°"), sel) { which ->
            diag { "DIAG rotation -> $which" }
            applyOrientation(orients[which])
        })
    }

    private fun fitPanel(): View {
        val sel = fitPref().coerceIn(0, 2) // index = core FitMode code
        return settingRow("Fit", segmented(listOf("Full", "Width", "Height"), sel) { which ->
            setFitPref(which)
            diag { "DIAG fit -> mode=$which" }
            engine.execute {
                try { NativeBridge.nativeSetFit(docHandle, which) } catch (e: RuntimeException) {}
                repaintPanel()
            }
        })
    }

    /** The "Crop" tab: Page Crop (None/Auto) + a Margin cell bar (margin kept around the content). */
    private fun cropPanel(): View {
        val container = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        container.addView(settingRow("Page Crop", segmented(listOf("None", "Auto"), if (cropAutoPref()) 1 else 0) { which ->
            setCropAutoPref(which == 1)
            diag { "DIAG crop auto=${which == 1}" }
            engine.execute {
                try { NativeBridge.nativeSetCrop(docHandle, which, cropMarginPref()) } catch (e: RuntimeException) {}
                repaintPanel()
            }
        }))
        container.addView(settingRow("Margin", cellBar(8, cropMarginPref()) { level ->
            setCropMarginPref(level)
            diag { "DIAG crop margin=$level" }
            engine.execute {
                try { NativeBridge.nativeSetCrop(docHandle, if (cropAutoPref()) 1 else 0, level) } catch (e: RuntimeException) {}
                repaintPanel()
            }
        }))
        return container
    }

    private fun cropAutoPref(): Boolean =
        getSharedPreferences("display", MODE_PRIVATE).getBoolean("crop_auto", false)

    private fun setCropAutoPref(v: Boolean) =
        getSharedPreferences("display", MODE_PRIVATE).edit().putBoolean("crop_auto", v).apply()

    private fun cropMarginPref(): Int =
        getSharedPreferences("display", MODE_PRIVATE).getInt("crop_margin", 1).coerceIn(0, 8)

    private fun setCropMarginPref(v: Int) =
        getSharedPreferences("display", MODE_PRIVATE).edit().putInt("crop_margin", v).apply()

    /** The "Page" tab: reflow Line Spacing + Alignment (EPUB; a toast on fixed-layout PDF). */
    private fun pagePanel(): View {
        fun applyReflow(call: () -> Int) {
            engine.execute {
                val np = try { call() } catch (e: RuntimeException) { -1 }
                if (np >= 0) {
                    pageCount = NativeBridge.nativePageCount(docHandle)
                    repaintPanel()
                } else {
                    runOnUiThread { Toast.makeText(this, "Page layout adjusts reflowable books (EPUB)", Toast.LENGTH_SHORT).show() }
                }
            }
        }
        val container = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        // Reflow toggle — only for a text-layer PDF (EPUB is always reflowable; a scanned PDF can't).
        // It gates whether the Line Spacing / Alignment / Font Size controls take effect on a PDF.
        val supportsReflow = try { NativeBridge.nativeSupportsReflow(docHandle) } catch (e: RuntimeException) { false }
        if (supportsReflow) {
            container.addView(settingRow("Reflow", segmented(listOf("Off", "On"), if (reflowOn) 1 else 0) { which ->
                diag { "DIAG reflow=${which == 1}" }
                setReflowMode(which == 1)
            }))
        }
        container.addView(settingRow("Line Spacing", segmented(listOf("Small", "Medium", "Large"), lineSpacingPref()) { which ->
            setLineSpacingPref(which)
            diag { "DIAG line spacing=$which" }
            applyReflow { NativeBridge.nativeSetLineSpacing(docHandle, LINE_SPACINGS[which]) }
        }))
        container.addView(settingRow("Alignment", segmented(listOf("Left", "Justify", "Center", "Right"), alignmentPref()) { which ->
            setAlignmentPref(which)
            diag { "DIAG alignment=$which" }
            applyReflow { NativeBridge.nativeSetAlignment(docHandle, which) }
        }))
        return container
    }

    /** Toggle PDF reflow (ADR-INKREAD-0011). On enable, re-apply the saved typography so the
     *  reflowed PDF respects the user's font size / spacing / alignment; the page count and position
     *  change across the toggle, so refresh both. A `-1` means no text layer (scanned PDF). */
    private fun setReflowMode(on: Boolean) {
        engine.execute {
            val np = try { NativeBridge.nativeSetReflow(docHandle, on) } catch (e: RuntimeException) { -1 }
            if (np >= 0) {
                reflowOn = on
                if (on) {
                    if (textScalePref() != 1.0f) {
                        try { NativeBridge.nativeSetTextScale(docHandle, textScalePref()) } catch (e: RuntimeException) {}
                    }
                    if (lineSpacingPref() != 1) {
                        try { NativeBridge.nativeSetLineSpacing(docHandle, LINE_SPACINGS[lineSpacingPref()]) } catch (e: RuntimeException) {}
                    }
                    if (alignmentPref() != 0) {
                        try { NativeBridge.nativeSetAlignment(docHandle, alignmentPref()) } catch (e: RuntimeException) {}
                    }
                }
                pageCount = NativeBridge.nativePageCount(docHandle)
                repaintPanel()
            } else {
                runOnUiThread { Toast.makeText(this, "This PDF has no text layer to reflow", Toast.LENGTH_SHORT).show() }
            }
        }
    }

    private fun lineSpacingPref(): Int =
        getSharedPreferences("typography", MODE_PRIVATE).getInt("line_spacing", 1).coerceIn(0, 2)

    private fun setLineSpacingPref(i: Int) =
        getSharedPreferences("typography", MODE_PRIVATE).edit().putInt("line_spacing", i).apply()

    private fun alignmentPref(): Int =
        getSharedPreferences("typography", MODE_PRIVATE).getInt("alignment", 0).coerceIn(0, 3)

    private fun setAlignmentPref(i: Int) =
        getSharedPreferences("typography", MODE_PRIVATE).edit().putInt("alignment", i).apply()

    /** The "Zoom" tab: the Fit segmented row + a live zoom −/+ stepper (zoom moved off the bar). */
    private fun zoomPanel(): View {
        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val zlabel = TextView(this).apply {
            textSize = 16f; setTextColor(Color.BLACK); gravity = Gravity.CENTER; minWidth = dp(64)
        }
        fun refresh() { zlabel.text = "${(zoom * 100).toInt()}%" }
        refresh()
        fun pill(t: String, on: () -> Unit) = TextView(this).apply {
            text = t; textSize = 16f; gravity = Gravity.CENTER; setTextColor(Color.BLACK)
            setPadding(dp(18), dp(10), dp(18), dp(10)); isClickable = true
            background = GradientDrawable().apply {
                setColor(Color.WHITE); cornerRadius = dp(20).toFloat(); setStroke(maxOf(1, dp(1)), Color.parseColor("#9E9E9E"))
            }
            setOnClickListener { on() }
        }
        val zoomControl = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
            addView(pill("−") { zoomBy(1f / ZOOM_STEP); refresh() })
            addView(zlabel, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
                val m = dp(10); setMargins(m, 0, m, 0)
            })
            addView(pill("+") { zoomBy(ZOOM_STEP); refresh() })
        }
        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(fitPanel())
            addView(settingRow("Zoom", zoomControl))
        }
    }

    private fun displayPanel(): View {
        val container = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        container.addView(settingRow("Contrast", cellBar(CONTRAST_MAX, contrastPref()) { level ->
            setContrastPref(level)
            diag { "DIAG contrast step=$level" }
            engine.execute {
                try { NativeBridge.nativeSetContrast(docHandle, level) } catch (e: RuntimeException) {}
                repaintPanel()
            }
        }))
        container.addView(settingRow("Quality", segmented(listOf("Low", "Default", "High"), renderQualityPref()) { which ->
            setRenderQualityPref(which)
            diag { "DIAG render quality=$which" }
            engine.execute {
                try { NativeBridge.nativeSetRenderQuality(docHandle, which) } catch (e: RuntimeException) {}
                repaintPanel()
            }
        }))
        return container
    }

    private fun renderQualityPref(): Int =
        getSharedPreferences("display", MODE_PRIVATE).getInt("render_quality", 1).coerceIn(0, 2)

    private fun setRenderQualityPref(q: Int) =
        getSharedPreferences("display", MODE_PRIVATE).edit().putInt("render_quality", q).apply()

    private fun fontPanel(): View {
        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        var idx = nearestScaleIndex(textScalePref())
        val value = TextView(this).apply {
            textSize = 16f; setTextColor(Color.BLACK); gravity = Gravity.CENTER; minWidth = dp(64)
        }
        fun refresh() { value.text = "${(TEXT_SCALES[idx] * 100).toInt()}%" }
        refresh()
        fun apply() {
            setTextScalePref(TEXT_SCALES[idx]); refresh()
            engine.execute {
                val np = try { NativeBridge.nativeSetTextScale(docHandle, TEXT_SCALES[idx]) } catch (e: RuntimeException) { -1 }
                if (np >= 0) { pageCount = NativeBridge.nativePageCount(docHandle); repaintPanel() }
                else runOnUiThread { Toast.makeText(this, "Font size adjusts reflowable books (EPUB)", Toast.LENGTH_SHORT).show() }
            }
        }
        fun pill(t: String, on: () -> Unit) = TextView(this).apply {
            text = t; textSize = 16f; gravity = Gravity.CENTER; setTextColor(Color.BLACK)
            setPadding(dp(18), dp(10), dp(18), dp(10)); isClickable = true
            background = GradientDrawable().apply {
                setColor(Color.WHITE); cornerRadius = dp(20).toFloat(); setStroke(maxOf(1, dp(1)), Color.parseColor("#9E9E9E"))
            }
            setOnClickListener { on() }
        }
        val control = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
            addView(pill("A−") { if (idx > 0) { idx--; apply() } })
            addView(value, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
                val m = dp(10); setMargins(m, 0, m, 0)
            })
            addView(pill("A+") { if (idx < TEXT_SCALES.size - 1) { idx++; apply() } })
        }
        return settingRow("Font Size", control)
    }

    /** Set + persist the screen orientation; the resize re-renders the page (engine via surfaceChanged). */
    private fun applyOrientation(orientation: Int) {
        setOrientationPref(orientation)
        requestedOrientation = orientation
    }

    private fun fitPref(): Int =
        getSharedPreferences("display", MODE_PRIVATE).getInt("fit", 0)

    private fun setFitPref(mode: Int) =
        getSharedPreferences("display", MODE_PRIVATE).edit().putInt("fit", mode).apply()

    private fun orientationPref(): Int =
        getSharedPreferences("display", MODE_PRIVATE)
            .getInt("orientation", ActivityInfo.SCREEN_ORIENTATION_PORTRAIT)

    private fun setOrientationPref(orientation: Int) =
        getSharedPreferences("display", MODE_PRIVATE).edit().putInt("orientation", orientation).apply()

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

        /** Gate for verbose `DIAG` tracing. Off by default: these logs run on render/stroke/tap
         *  paths and can leak reading behavior to logcat on a shared device. Flip when debugging. */
        const val DIAG = false
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
        const val SELECTION_HANDLE_PX = 8f // half-size of the square corner handles on the selection box.
        const val MULTILINE_DRAG_FRAC = 0.045f // drag vertical span (frac of height) above which it's a multi-line → line-span select.
        const val OPEN_DRAG_FRAC = 0.08f // lasso: start-to-lift distance (normalized) above which the gesture is an open drag (vs a closed loop).
        const val CONTRAST_MAX = 8 // mirrors reader-core render::contrast::MAX_CONTRAST_STEP (RR4).
        val LINE_SPACINGS = floatArrayOf(1.2f, 1.4f, 1.7f) // Small / Medium / Large (RR4).
        const val HIGHLIGHT_WIDTH_PX = 30f // wide marker band (vs INK_STROKE_WIDTH for the pen).
        const val STROKE_PAUSE_MS = 600L // commit a stroke after this pen-pause (swallowed-UP net).
        const val INK_FLUSH_MS = 1500L // trailing-edge delay before the deferred ink autosave fsyncs.
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
        // Contact major ≥ this fraction of the panel height ⇒ a palm. Lowered from 0.12 to 0.06
        // (≈154px on a 2560px panel) per on-device PALMDIAG capture: resting palms reported
        // touch-major ≈ 140–240px, well under the old 307px gate, so the very first palm leaked
        // before any pen event primed the timing window. 0.06 catches those by size at the outset;
        // a fingertip tap reads well below it. Tunable.
        const val PALM_TOUCH_MAJOR_FRAC = 0.06f

        // Launch extras from HomeActivity.
        const val EXTRA_PICK = "inkread.pick" // open the file picker on launch.
        const val EXTRA_BOOK_PATH = "inkread.book_path" // open this specific stored book…
        const val EXTRA_BOOK_ID = "inkread.book_id" // …with this stable id.
    }
}
