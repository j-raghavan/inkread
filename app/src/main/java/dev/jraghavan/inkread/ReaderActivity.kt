package dev.jraghavan.inkread

import android.app.Activity
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
import kotlin.math.ceil
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.widget.Toast
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.Window
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.TextView
import dev.jraghavan.inkread.eink.EinkAdapter
import java.net.HttpURLConnection
import java.net.URL
import org.json.JSONObject
import dev.jraghavan.inkread.eink.DisplayAdapters
import dev.jraghavan.inkread.eink.SupernoteInk
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.Executors

/**
 * The reader Activity (RR1-FR2, RR21) — device-verified on the Supernote family.
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
    // Chosen by capability probe, not hardcoded (#220): a device with no e-ink service is told it
    // has no e-ink panel, rather than being given the Supernote's refresh policy by default.
    private val adapter: EinkAdapter by lazy { DisplayAdapters.forDevice(this) }

    /** Periodic full-refresh cadence (#99): fires a full EPD flash every Nth page-turn to clear
     *  ghosting. Interval comes from [DisplayPrefs.fullRefreshEvery] (set in onCreate + on change);
     *  the counter is touched only on the engine thread from [postJump]. */
    private val refreshCadence = RefreshCadence(0)

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
    /** Chapters as (start page, title) from the top-level resolved TOC, sorted — drives chapter
     *  prev/next + the current-chapter label in the reading bar (1.7/1.8). */
    @Volatile private var chapters: List<Pair<Int, String>> = emptyList()
    /** Stable id of the open book (its file name); keys thumbnails + the bookmarks file. */
    @Volatile private var currentBookId = ""
    /** Foreground reading-session start (elapsed ms) + the page it began on, for ReadingStats. */
    private var sessionStartMs = 0L
    private var sessionStartPage = 0
    /** Whether PDF reflow mode is on (ADR-INKREAD-0011). Session-scoped: defaults off on each open
     *  (the fixed page is the faithful view; reflow is an opt-in toggle on the Page tab). */
    @Volatile private var reflowOn = false
    /** Whether the current view honors zoom — a fixed-layout page that is not reflowed (#61, mirrors
     *  the core's `is_magnifiable`). Gates every zoom entry point so a pinch/double-tap on a
     *  reflowable view (EPUB, or a reflowed PDF) can't strand the shell's zoom. Refreshed on open and
     *  on a reflow toggle. */
    @Volatile private var magnifiable = false
    /** True while a reflow toggle's full-document repagination is running — guards a re-toggle.
     *  Large PDFs take seconds (#55). */
    @Volatile private var reflowInProgress = false
    /** The "Reflowing…" notice for the reflow toggle; [ReflowProgress] owns showing and dismissing it. */
    private var reflowProgress: ReflowProgress? = null

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

    /** Whether the reader has asked for the tool palette to run across the top (#200). */
    private val paletteHorizontal: Boolean get() = AppSettings.toolbarHorizontal(this)

    /**
     * The vertical pill's last parked spot (#200), or null when it has never been moved.
     *
     * Only the vertical form remembers a position. The horizontal bar docks to a corner chosen in
     * Settings and opens there every time, so there is nothing free-form to store.
     */
    private fun parkedPalettePosition(): ToolPalette.Position? =
        ToolPalette.Position.of(
            prefs.getFloat(KEY_PALETTE_X, Float.NaN),
            prefs.getFloat(KEY_PALETTE_Y, Float.NaN),
        )

    // ---- lasso (ADR-INKREAD-0010) ----
    /** The floating selection toolbar; created in onCreate. */
    private lateinit var selectionToolbar: SelectionToolbar
    private lateinit var toolOptions: ToolOptions
    /** Persistent Lasso discoverability banner (shown while Lasso is active with no selection). */
    private var lassoHint: TextView? = null
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

    /** Persisted display + typography settings (RR4); shared by openBook and the Adjust sheet. */
    private val displayPrefs = DisplayPrefs(this)

    /** Document-settings sheet (RR4) — owns the tabbed panels + widgets + presets (SRP). The
     *  shell keeps the page geometry: reflow toggle + zoom mutate through the Host. */
    private val adjust = AdjustSheetController(object : AdjustSheetController.Host {
        override val activity get() = this@ReaderActivity
        override val docHandle get() = this@ReaderActivity.docHandle
        override val prefs get() = displayPrefs
        override val reflowOn get() = this@ReaderActivity.reflowOn
        override val zoomPercent get() = (zoom * 100).toInt()
        override fun engineExecute(block: () -> Unit) { engine.execute(block) }
        override fun repaintPanel() = this@ReaderActivity.repaintPanel()
        override fun refreshPageCount() { pageCount = NativeBridge.nativePageCount(docHandle) }
        override fun applyFullRefreshInterval(n: Int) {
            refreshCadence.interval = n // @Volatile — safe from this UI-thread setter
            engine.execute { refreshCadence.reset() } // restart the count on the engine thread (#99)
        }
        override fun refreshToolPalette() {
            if (::toolPalette.isInitialized) toolPalette.refreshChrome()
        }
        override fun setReflowMode(on: Boolean) = this@ReaderActivity.setReflowMode(on)
        override fun zoomIn() = zoomBy(ZOOM_STEP)
        override fun zoomOut() = zoomBy(1f / ZOOM_STEP)
        override fun openPicker() = this@ReaderActivity.openPicker()
        override fun openFontPicker() = this@ReaderActivity.openFontPicker()
        override fun palmGuard(content: View) = this@ReaderActivity.palmGuard(content)
        override fun diag(msg: () -> String) = this@ReaderActivity.diag(msg)
    })

    /** Bottom control bar + the nav panels it opens (RR16/RR25) — page slider, chapter jumps,
     *  bookmarks, Contents, annotations list, library, go-home (SRP). */
    private val bottomBar = BottomBarController(object : BottomBarController.Host {
        override val activity get() = this@ReaderActivity
        override val docHandle get() = this@ReaderActivity.docHandle
        override val pageCount get() = this@ReaderActivity.pageCount
        override val currentPage get() = this@ReaderActivity.currentPage
        override val chapters get() = this@ReaderActivity.chapters
        override val bookmarks get() = this@ReaderActivity.bookmarks
        override val requestedPath get() = this@ReaderActivity.requestedPath
        override fun engineExecute(block: () -> Unit) { engine.execute(block) }
        override fun postJump(page: Int) = this@ReaderActivity.postJump(page)
        override fun repaintPanel() = this@ReaderActivity.repaintPanel()
        override fun openPicker() = this@ReaderActivity.openPicker()
        override fun palmGuard(content: View) = this@ReaderActivity.palmGuard(content)
        override fun zoomIn() = zoomBy(ZOOM_STEP)
        override fun zoomOut() = zoomBy(1f / ZOOM_STEP)
        override val magnifiable get() = this@ReaderActivity.magnifiable
        override fun textLarger() = stepTextScale(+1)
        override fun textSmaller() = stepTextScale(-1)
        override fun openSearch() = search.showSearchDialog()
        override fun openExport() = export.showExportDialog()
        override fun openDicts() = dict.showDictionariesDialog()
        override fun openAdjust() = adjust.show()
        override fun refreshNow() { engine.execute { refreshCadence.reset(); refreshPanel() } } // #99
    })

    /** Lasso selection + Define-tool text selection (ADR-INKREAD-0010) — owns the selection
     *  state and the toolbar actions (SRP). The shell keeps the views, geometry, and draw
     *  primitives behind the Host. */
    private val lasso = LassoController(object : LassoController.Host {
        override val activity get() = this@ReaderActivity
        override val docHandle get() = this@ReaderActivity.docHandle
        override val currentPage get() = this@ReaderActivity.currentPage
        override val viewW get() = this@ReaderActivity.viewW
        override val viewH get() = this@ReaderActivity.viewH
        override val zoom get() = this@ReaderActivity.zoom
        override val surfaceW get() = surfaceView.width
        override val surfaceH get() = surfaceView.height
        override val activeTool get() = tool
        override val highlightColor get() = stylus.highlightColor
        override fun vToNx(vx: Float) = this@ReaderActivity.vToNx(vx)
        override fun vToNy(vy: Float) = this@ReaderActivity.vToNy(vy)
        override fun nToVx(nx: Float) = this@ReaderActivity.nToVx(nx)
        override fun nToVy(ny: Float) = this@ReaderActivity.nToVy(ny)
        override fun engineExecute(block: () -> Unit) { engine.execute(block) }
        override fun drawLivePath(buf: ArrayList<Float>, paint: Paint) = this@ReaderActivity.drawLivePath(buf, paint)
        override fun overlayOnPage(draw: (Canvas) -> Unit) {
            val bmp = bitmap ?: return
            blit { c -> c.drawBitmap(bmp, 0f, 0f, null); draw(c) }
        }
        override fun renderAndBlit() = this@ReaderActivity.renderAndBlit()
        override fun repaintPanel() = this@ReaderActivity.repaintPanel()
        override fun refreshPanel() = this@ReaderActivity.refreshPanel()
        override fun clearFirmwareInk() = this@ReaderActivity.clearFirmwareInk()
        override fun scheduleInkFlush() = stylus.scheduleInkFlush()
        override fun showSelectionToolbar(rect: android.graphics.RectF, canPaste: Boolean) =
            selectionToolbar.show(rect, canPaste)
        override fun dismissSelectionToolbar() = selectionToolbar.dismiss()
        override fun setLassoHintVisible(show: Boolean) {
            lassoHint?.visibility = if (show) View.VISIBLE else View.GONE
        }
        override fun defineWord(page: Int, nx: Float, ny: Float) = dict.defineWord(page, nx, ny)
        override fun defineSelectionText(text: String) = dict.defineSelectionText(text)
        override fun addDigest(page: Int, boundsNorm: FloatArray) = digest.addDigest(page, boundsNorm)
        override fun addDigestText(page: Int, text: String, boundsNorm: FloatArray?) =
            digest.addDigestText(page, text, boundsNorm)
        override fun diag(msg: () -> String) = this@ReaderActivity.diag(msg)
    })

    private val mainHandler = Handler(Looper.getMainLooper())

    /** Stylus pen/highlighter + eraser capture and the ink commit path (RR19/RR20) — owns the
     *  stroke buffers, pen colours, deferred autosave, and stroke baking (SRP). The shell keeps
     *  the firmware-ink object, the transforms, and drawLivePath behind the Host. */
    private val stylus = StylusInkController(object : StylusInkController.Host {
        override val docHandle get() = this@ReaderActivity.docHandle
        override val currentPage get() = this@ReaderActivity.currentPage
        override val viewW get() = this@ReaderActivity.viewW
        override val viewH get() = this@ReaderActivity.viewH
        override val zoom get() = this@ReaderActivity.zoom
        override val activeTool get() = tool
        override fun vToNx(vx: Float) = this@ReaderActivity.vToNx(vx)
        override fun vToNy(vy: Float) = this@ReaderActivity.vToNy(vy)
        override fun nToVx(nx: Float) = this@ReaderActivity.nToVx(nx)
        override fun nToVy(ny: Float) = this@ReaderActivity.nToVy(ny)
        override fun lenToNorm(px: Float) = this@ReaderActivity.lenToNorm(px)
        override fun engineExecute(block: () -> Unit) { engine.execute(block) }
        override fun clearFirmwareInk() = this@ReaderActivity.clearFirmwareInk()
        override fun repaintPanel() = this@ReaderActivity.repaintPanel()
        override fun drawLivePath(buf: ArrayList<Float>, paint: Paint) = this@ReaderActivity.drawLivePath(buf, paint)
        override fun defineWord(page: Int, nx: Float, ny: Float) = dict.defineWord(page, nx, ny)
        override fun diag(msg: () -> String) = this@ReaderActivity.diag(msg)
    })

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
    /** Last centre tap (uptime ms + view px) for double-tap-to-zoom detection (#54). */
    private var lastCentreTapMs = 0L
    private var lastCentreTapX = 0f
    private var lastCentreTapY = 0f
    /** A centre single-tap's action (open the bottom bar), deferred [DOUBLE_TAP_MS] so it can be
     *  cancelled if a second tap turns it into a double-tap-zoom. Edge taps stay immediate (#54). */
    private val pendingCentreMenu = Runnable { bottomBar.showBottomBar() }
    private val fingerLongPress = Runnable {
        // A genuine 500ms hold (UP cancels this for a tap; a beyond-slop MOVE cancels it for a
        // swipe). Mark it a long-press FIRST so the eventual UP never falls through to a page flip —
        // even if the lookup finds no word. (No "recent MOVE" gate: the held-finger MOVE stream has
        // gaps, and finger UP is reliable here, so the gate only caused false page flips.)
        // A pinch-zoom in flight is never a word lookup, even if a finger sat still long enough.
        if (fingerMoved || scaleDetector.isInProgress) return@Runnable
        fingerLookupFired = true // suppresses the tap/page-flip on the upcoming UP
        if (SystemClock.uptimeMillis() - lastStylusMs <= PALM_REJECT_MS || stylus.strokeInProgress) return@Runnable
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

    /** Chrome painted onto the page bitmap: bookmark ribbon, selection box, search highlight. */
    private val overlays = PageOverlays(object : PageOverlays.Host {
        override fun nToVx(nx: Float) = this@ReaderActivity.nToVx(nx)
        override fun nToVy(ny: Float) = this@ReaderActivity.nToVy(ny)
        override val viewW get() = this@ReaderActivity.viewW
    })

    /** The zoom minimap (#60) — the top-right page thumbnail and its −/+ buttons. */
    private val minimap = MinimapController(object : MinimapController.Host {
        override val viewW get() = this@ReaderActivity.viewW
        override val viewH get() = this@ReaderActivity.viewH
        override val zoom get() = this@ReaderActivity.zoom
        override val panX get() = this@ReaderActivity.panX
        override val panY get() = this@ReaderActivity.panY
        // Qualified: bare `panX` would resolve to this object's own override, not the reader's.
        override fun setPan(x: Float, y: Float) {
            this@ReaderActivity.panX = x
            this@ReaderActivity.panY = y
        }
        override fun applyZoom() = this@ReaderActivity.applyZoom()
        override fun zoomBy(factor: Float) = this@ReaderActivity.zoomBy(factor)
        override fun throttledPreview(block: () -> Unit) = this@ReaderActivity.throttledPreview(block)
        override fun dpInt(v: Int) = this@ReaderActivity.dpInt(v)
    })
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
        // Apply the saved menu-size preference before any chrome is built (#133), so the bottom bar
        // and sheets lay out at the user's scale from the first frame.
        Ink.uiScale = displayPrefs.uiScale
        refreshCadence.interval = displayPrefs.fullRefreshEvery // periodic full-refresh cadence (#99)
        // Re-apply the saved page rotation (RR4) before the surface is created so the first render
        // is at the right orientation. configChanges=orientation keeps us from recreating.
        requestedOrientation = displayPrefs.orientation
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
                mainHandler.removeCallbacks(pendingCentreMenu) // ...and don't pop the bar mid-stroke (#54)
                val a = event.actionMasked
                if (a == MotionEvent.ACTION_DOWN || a == MotionEvent.ACTION_UP) {
                    diag { "DIAG stylus action=$a tool=$tool type=$toolType hist=${event.historySize}" }
                }
                // An inverted pen erases regardless of the palette (#158) — see [Tool.forStylus].
                when (Tool.forStylus(tool, toolType)) {
                    Tool.DEFINE -> lasso.captureSelection(event)
                    Tool.ERASER -> stylus.captureErase(event)
                    Tool.LASSO -> lasso.captureLasso(event)
                    else -> stylus.captureStylus(event) // PEN (Highlighter is still P2)
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
                    // inside MinimapController's UP path, which we bypass here — leaving them stuck
                    // would make the minimap swallow the next finger gesture once the pen idles.
                    minimap.cancelTouch()
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
                        if (!minimap.onTouch(event)) when (event.actionMasked) {
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
            onUndo = { lasso.inkUndo() },
            onRedo = { lasso.inkRedo() },
            // Reopen the toolbar where the reader last parked it (#200) rather than at the default
            // dock — repositioning it on every document open was the standing annoyance.
            orientation = if (paletteHorizontal) {
                ToolPalette.Orientation.HORIZONTAL
            } else {
                ToolPalette.Orientation.VERTICAL
            },
            side = if (AppSettings.toolbarOnLeft(this)) {
                ToolPalette.Anchor.START
            } else {
                ToolPalette.Anchor.END
            },
            // Docked into the top-right corner the strip lands on the bookmark ribbon — and, worse,
            // inside its tap target, swallowing the touch that toggles a bookmark. Hold it clear.
            dockClearance = if (AppSettings.toolbarOnLeft(this)) {
                0
            } else {
                // Rounded up: truncating leaves the strip a fraction of a pixel inside the zone.
                ceil(resources.displayMetrics.widthPixels * BOOKMARK_ZONE_W).toInt()
            },
            // The horizontal bar is corner-docked, and the corner *is* its position: every book
            // opens it collapsed there. It can still be dragged aside mid-read, but that is a
            // this-book-only move and is deliberately not carried into the next one.
            savedPosition = if (paletteHorizontal) null else parkedPalettePosition(),
            startExpanded = prefs.getBoolean(KEY_PALETTE_EXPANDED, false),
            onExpandedChanged = { open ->
                prefs.edit().putBoolean(KEY_PALETTE_EXPANDED, open).apply()
            },
            onMoved = { at ->
                if (!paletteHorizontal) {
                    prefs.edit()
                        .putFloat(KEY_PALETTE_X, at.x)
                        .putFloat(KEY_PALETTE_Y, at.y)
                        .apply()
                }
            },
        )
        selectionToolbar = SelectionToolbar(this, root) { action -> lasso.onSelectionAction(action) }
        // The column opens beside the pill wherever it is parked, so docking the bar left or on
        // top no longer strands the colours on the right of the page (#200). Read through lambdas
        // rather than captured once: the pill is draggable, so its position is only true at the
        // moment the column is shown.
        toolOptions = ToolOptions(this, root, toolbar = { toolPalette.placement() })
        // Restore the saved pen thickness (#199) before any stroke can be committed, so the first
        // stroke after a relaunch is the width the reader chose rather than the default.
        stylus.penWidthIndex = prefs
            .getInt(KEY_PEN_WIDTH, StylusInkController.DEFAULT_PEN_WIDTH_INDEX)
            .coerceIn(0, StylusInkController.PEN_WIDTHS.size - 1)
        // Persistent affordance for Lasso (discoverability): a slim top banner shown while the
        // Lasso tool is active and nothing is selected. Tells the user the loop gesture; hidden
        // once a selection exists or another tool is chosen.
        lassoHint = TextView(this).apply {
            text = "Lasso — draw a loop around your writing to select"
            textSize = Ink.sp(14f)
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

    /** Keeps a repeat `surfaceChanged` from re-rendering the page it just drew (#186). */
    private val renderGate = SurfaceRenderGate()

    override fun surfaceCreated(holder: SurfaceHolder) {
        renderGate.onSurfaceCreated()
        // Paint the surface white the instant it exists, on this thread. The size arrives in
        // surfaceChanged, whose work is handed to the engine thread — so the "Loading…" frame can be
        // queued behind whatever that thread is already doing. Until something is pushed, a
        // SurfaceView shows black, and on this panel that is a full black refresh. One white frame
        // here closes that window regardless of how busy the engine is.
        blit { canvas -> canvas.drawColor(Color.WHITE) }
    }

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
        // Start timing this foreground reading session (ReadingStats — streak + weekly chrome).
        sessionStartMs = SystemClock.elapsedRealtime()
        sessionStartPage = currentPage
    }

    override fun onPause() {
        super.onPause()
        // Record the finished reading session (time read + pages advanced) before tearing down.
        if (docHandle != 0L && sessionStartMs > 0L && currentBookId.isNotEmpty()) {
            val minutes = ((SystemClock.elapsedRealtime() - sessionStartMs) / 60_000L).toInt()
            ReadingStats.record(this, minutes, (currentPage - sessionStartPage).coerceAtLeast(0))
        }
        sessionStartMs = 0L
        // The tool palette is deliberately NOT collapsed here. Whether the tools are out is the
        // reader's working posture and survives leaving the app, the way the chosen tool does.
        if (::selectionToolbar.isInitialized) selectionToolbar.dismiss()
        if (::toolOptions.isInitialized) toolOptions.dismiss()
        mainHandler.removeCallbacks(fingerLongPress) // drop any pending finger gesture on leaving
        mainHandler.removeCallbacks(pendingCentreMenu) // don't pop the bar on a paused/finishing activity (#54)
        dismissReflowProgress() // don't leak the "Reflowing…" dialog if backgrounded mid-reflow (#55)
        stylus.cancelPendingOnPause() // drop the pending pen-lookup + deferred flush (explicit save below)
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

    /** Launch the system file picker for a document (RR22). */
    private fun openPicker() {
        // Every format the core opens: PDF (fixed-layout), EPUB (reflowable), CBZ (comics), and
        // plain text. The core content-sniffs first, then falls back to the extension; some pickers
        // tag these as octet-stream, so accept that too and let the core validate.
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = "*/*"
            putExtra(
                Intent.EXTRA_MIME_TYPES,
                arrayOf(
                    "application/pdf",
                    "application/epub+zip",
                    "application/vnd.comicbook+zip",
                    "application/x-cbz",
                    "text/plain",
                    "application/octet-stream",
                ),
            )
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        try {
            startActivityForResult(intent, REQ_OPEN_DOC)
        } catch (e: android.content.ActivityNotFoundException) {
            Toast.makeText(this, "No file picker available", Toast.LENGTH_SHORT).show()
        }
    }

    /** Launch the system file picker for a font file to import (RR28-FR3). */
    fun openFontPicker() {
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            // Pickers tag fonts inconsistently (font/ttf, application/x-font-ttf, octet-stream), so
            // accept anything and let the core reject what it cannot parse.
            type = "*/*"
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        try {
            startActivityForResult(intent, REQ_OPEN_FONT)
        } catch (e: android.content.ActivityNotFoundException) {
            Toast.makeText(this, "No file picker available", Toast.LENGTH_SHORT).show()
        }
    }

    /** Copy a picked font into `fonts/` and re-register, then report what happened. */
    private fun importFont(uri: Uri) {
        val stored = UserFonts.import(this, uri, suggestedName = null)
        // No need to re-apply the reader's face here: an import renumbers the registry, but the
        // open document holds a built `AbFont` that owns its bytes, so it is unaffected — and the
        // saved choice is a name, which resolves the same before and after (#169).
        runOnUiThread {
            val message = if (stored == null) {
                "That file isn't a font inkread can use"
            } else {
                "Added ${UserFonts.displayName(stored)} — pick it under Typeface"
            }
            Toast.makeText(this, message, Toast.LENGTH_SHORT).show()
        }
    }

    @Deprecated("startActivityForResult is fine for this single-Activity shell (no AndroidX).")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == REQ_OPEN_FONT) {
            val uri = data?.data
            if (resultCode == RESULT_OK && uri != null) {
                engine.execute { importFont(uri) }
            }
            return
        }
        if (requestCode != REQ_OPEN_DOC) return
        if (resultCode != RESULT_OK) {
            // Picker cancelled. With no document open (e.g. launched straight into the picker from
            // Home's "Open a Document"), there's nothing to show and the only tap action would
            // re-open the picker — return to Home instead of stranding the reader on a blank page
            // (#123). With a document already open (the "Open" bottom-bar control), stay on it.
            if (docHandle == 0L) finish()
            return
        }
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
        // Android delivers surfaceChanged more than once per surface, at the same size. Rendering
        // each one drew the opening page twice — a full page layout and an EPD refresh thrown away
        // on the slowest path there is (#186). A recreated surface still renders: see
        // [SurfaceRenderGate].
        if (!renderGate.needsRender(width, height, docHandle != 0L)) {
            diag { "DIAG surfaceChanged ${width}x$height ignored (already rendered)" }
            return
        }
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
        // System fallback faces (CJK etc.) must be in the core's chain before the document builds
        // its reflow view — fonts registered later only affect documents opened after them.
        FallbackFonts.ensureRegistered()
        // The reader's own imported faces, registered in the same breath so the picker lists them
        // from the first document open (RR28-FR3).
        UserFonts.register(this)
        // The saved typeface is a name, but installs predating that stored an index. Convert it
        // here: the registry has just been rebuilt and the font directory has not changed since the
        // index was written, so it still names the face the reader chose (#169).
        val faces = UserFonts.faceNames()
        displayPrefs.migrateFontIdToName(faces)
        val capsBytes = WireCodec.encodeCapabilities(adapter.capabilities())
        NativeBridge.nativeInit(capsBytes)
        val dbPath = File(filesDir, "reader.db").absolutePath
        docHandle = try {
            NativeBridge.nativeOpenDocumentWithStore(
                path, capsBytes, viewW, viewH, DPI, dbPath, bookId,
                displayPrefs.textScale, displayPrefs.fontId(faces),
                displayPrefs.lineSpacingMult, displayPrefs.alignment, displayPrefs.columns,
                displayPrefs.marginPct,
            )
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
            // A fixed-layout PDF magnifies; EPUB (always reflowed) does not (#61, RR25-FR3).
            magnifiable = try { NativeBridge.nativeIsMagnifiable(docHandle) } catch (e: RuntimeException) { false }
            // Reflow typography (text scale, face, line spacing, alignment) was applied by
            // nativeOpenDocumentWithStore above, so the book is paginated once, at the settings it
            // will actually be read at (#161/#162).
            // Re-apply the saved display contrast (RR4); 0 = off (a no-op in the core).
            try { NativeBridge.nativeSetContrast(docHandle, displayPrefs.contrast) } catch (e: RuntimeException) {}
            // Re-apply night mode (invert); default off (RR4 / style presets).
            try { NativeBridge.nativeSetNight(docHandle, displayPrefs.night) } catch (e: RuntimeException) {}
            // Re-apply the saved page fit mode (RR4); default Page/contain.
            try { NativeBridge.nativeSetFit(docHandle, displayPrefs.fit) } catch (e: RuntimeException) {}
            // Re-apply the saved auto-crop + margin (RR4); default off.
            try { NativeBridge.nativeSetCrop(docHandle, if (displayPrefs.cropAuto) 1 else 0, displayPrefs.cropMargin) } catch (e: RuntimeException) {}
            // Re-apply the saved render quality (RR4); default 1.
            try { NativeBridge.nativeSetRenderQuality(docHandle, displayPrefs.renderQuality) } catch (e: RuntimeException) {}
            // (line spacing + alignment were restored above, in the same nativeSetTypography call)
            pageCount = NativeBridge.nativePageCount(docHandle)
            Books.pushRecent(this, bookId, path)
            Books.setLastOpened(this, bookId) // orders the shelf past the recents cut (#227)
            // Capture the real document metadata so the library shows the actual title/author + page
            // position instead of the filename (the home redesign's real-data path).
            try {
                val t = NativeBridge.nativeDocTitle(docHandle)
                val a = NativeBridge.nativeDocAuthor(docHandle)
                Books.setMeta(this, bookId, t, a, pageCount)
            } catch (e: RuntimeException) {
                Log.e(TAG, "doc metadata failed: ${e.message}")
            }
            Log.i(
                TAG,
                "opened $bookId: $pageCount pages, resumed at page ${NativeBridge.nativeCurrentPage(docHandle)}",
            )
            // Decode the TOC once: it drives both chapter prev/next (1.7) and the Daily per-article
            // jump. Chapter starts = top-level resolved targets (fall back to all targets for a flat
            // TOC), de-duped + sorted by page.
            val toc = try {
                WireCodec.decodeToc(NativeBridge.nativeToc(docHandle))
            } catch (e: RuntimeException) {
                Log.e(TAG, "toc failed: ${e.message}"); emptyList()
            }
            val tops = toc.filter { it.depth == 0 && it.targetPage != null }
            chapters = (if (tops.isNotEmpty()) tops else toc.filter { it.targetPage != null })
                .map { it.targetPage!! to it.title }
                .distinctBy { it.first }
                .sortedBy { it.first }
            // Daily: a tapped headline opens the issue AT that article. The issue's TOC is
            // [Cover, article0, article1, …], so article N is TOC entry N+1.
            val dailyArticle = intent.getIntExtra(EXTRA_DAILY_ARTICLE, -1)
            if (dailyArticle >= 0) {
                toc.getOrNull(dailyArticle + 1)?.targetPage?.let { page ->
                    try {
                        NativeBridge.nativeJumpToPage(docHandle, page)
                        currentPage = NativeBridge.nativeCurrentPage(docHandle)
                        Log.i(TAG, "daily: jumped to article $dailyArticle → page $page")
                    } catch (e: RuntimeException) {
                        Log.e(TAG, "daily article jump failed: ${e.message}")
                    }
                }
            }
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

        // Render-path timing (Rendering M1 observability): core render → copy → composite → blit.
        // Cheap to measure; per-render detail is gated behind diag, and any render that approaches
        // the e-ink budget is surfaced at I-level so slowness shows up without a debug build.
        val tStart = SystemClock.elapsedRealtime()
        try {
            buf.clear()
            NativeBridge.nativeRenderPage(handle, buf)
        } catch (e: RuntimeException) {
            Log.e(TAG, "render failed: ${e.message}")
            return
        }
        val tCore = SystemClock.elapsedRealtime()
        buf.rewind()
        bmp.copyPixelsFromBuffer(buf)
        val tCopy = SystemClock.elapsedRealtime()
        currentPage = NativeBridge.nativeCurrentPage(handle)
        // Bake the CORE's strokes for this page onto the rendered page (RR6) before blitting.
        pageStrokes = try {
            WireCodec.decodeStrokes(NativeBridge.nativeInkStrokesForDraw(handle, currentPage))
        } catch (e: RuntimeException) {
            Log.e(TAG, "ink fetch failed: ${e.message}"); emptyList()
        }
        diag { "DIAG baked ${pageStrokes.size} core strokes on page $currentPage" }
        val cv = Canvas(bmp)
        for (s in pageStrokes) stylus.drawStroke(cv, s)
        // The active lasso selection's bounding box (ADR-INKREAD-0010).
        if (lasso.hasSelection) overlays.drawSelectionBox(cv, lasso.selectionBounds)
        // The active in-document search hit's highlight boxes (RR2), if it lives on this page.
        val searchHl = search.highlightForPage(currentPage)
        if (searchHl.isNotEmpty()) overlays.drawSearchHighlight(cv, searchHl)
        // Zoom minimap (top-right): full page + the current viewport window (RR5-FR3). The fit
        // thumbnail it draws is captured lazily when zoom is first engaged (captureFitThumb), not on
        // every fit-page turn — so ordinary reading pays no per-flip scale + alloc.
        if (zoom > 1f) minimap.draw(cv)
        // A top-right dog-ear: faint outline (tap-to-bookmark affordance) / solid when bookmarked.
        overlays.drawBookmark(cv, marked = bookmarks?.has(currentPage) == true)
        // Cache the first page as the book's thumbnail, once (RR17-FR5).
        if (currentPage == 0 && currentBookId.isNotEmpty() && !Books.thumbFile(this, currentBookId).exists()) {
            Books.saveThumbnail(this, currentBookId, bmp)
        }
        val tComposite = SystemClock.elapsedRealtime()
        blit { canvas -> canvas.drawBitmap(bmp, 0f, 0f, null) }
        val tBlit = SystemClock.elapsedRealtime()
        val core = tCore - tStart; val copy = tCopy - tCore
        val composite = tComposite - tCopy; val blitMs = tBlit - tComposite; val total = tBlit - tStart
        // One concise line per render (renders are low-frequency — once per page turn/settle). This is
        // the on-app cost; the panel's async EPD waveform refresh isn't observable from here. A render
        // at/over the e-ink budget is flagged SLOW inline.
        val slow = if (total >= SLOW_RENDER_MS) " SLOW" else ""
        Log.i(TAG, "render p=$currentPage: core=$core copy=$copy composite=$composite blit=$blitMs total=${total}ms$slow")
        // Read-ahead: warm the next page (in the direction of travel) into the core's render cache,
        // off the critical path — the next turn then hits the cache (core≈0), attacking the render's
        // biggest cost. Deduped so chrome repaints of the same page don't re-enqueue.
        val ahead = PrefetchPolicy.nextPage(currentPage, lastTurnDir, pageCount, zoom, lastPrefetchedPage)
        if (ahead != null) {
            lastPrefetchedPage = ahead
            engine.execute {
                val h = docHandle
                if (h != 0L) runCatching { NativeBridge.nativePrefetchPage(h, ahead) }
            }
        }
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
            strokeInProgress = stylus.strokeInProgress,
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
        strokeInProgress = stylus.strokeInProgress,
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

    /**
     * Finger DOWN: reject obvious palms (multi-touch, large contact, a recent/in-progress stylus, an
     * active stroke); otherwise arm the long-press → lookup timer. The tap itself is decided on UP
     * (the panel delivers finger UP reliably), so "rest the hand, then write" never turns a page.
     */
    private fun onFingerDown(e: MotionEvent) {
        if (isPalmTouch(e)) {
            diag { "DIAG palm-reject down pc=${e.pointerCount} major=${e.getTouchMajor(0)}" }
            fingerIsPalm = true // latch: MOVE/UP bail, so a rejected palm never pans (esp. zoomed in)
            fingerMoved = true // also neutralise the tap/long-press paths
            // Record the origin anyway: a flat fingertip swipe can read as palm-sized, so UP recovers
            // a clearly-horizontal travel with the pen away as a page-turn (see onFingerUp).
            fingerDownX = e.x; fingerDownY = e.y
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
        // Recover a swipe mis-flagged as a palm: a single-finger contact that travelled clearly
        // horizontally while the pen stayed away is a deliberate page-turn, not a resting palm (palms
        // accompany pen writing). Thresholds are STRICTER than a normal swipe (longer, more strongly
        // horizontal) so a real resting palm — which barely moves — never turns the page.
        if (fingerIsPalm && !penActiveForPinch() && !gestureWasMultiTouch && zoom <= 1f) {
            val dx = e.x - fingerDownX
            val dy = e.y - fingerDownY
            val dir = ReaderGestures.swipeDelta(dx, dy, surfaceView.width.toFloat(), strict = true)
            if (dir != null) {
                fingerIsPalm = false; minimap.invalidateThumb()
                diag { "DIAG palm-swipe recovered dx=$dx dy=$dy" }
                queuePageTurn(dir)
                return
            }
        }
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
                val (px, py) = transform.panAfterDrag(e.x - fingerDownX, e.y - fingerDownY)
                panX = px; panY = py
                applyZoom()
            } else {
                val w = surfaceView.width.toFloat()
                if (w > 0f) {
                    // Follow a tapped link first (links are zoom/pan-aware via vToNx/vToNy) so a link
                    // in the edge zone isn't swallowed as a page turn while zoomed (#52 review).
                    val link = currentLinks.firstOrNull { it.contains(vToNx(fingerDownX), vToNy(fingerDownY)) }
                    if (link != null) { followLink(link); return }
                    // The minimap thumbnail is the OLD page's; drop it so the turn doesn't leave the
                    // wrong page on screen (it re-captures on the next return to fit) (#52 review).
                    when (ReaderGestures.zoneFor(fingerDownX, w)) {
                        ReaderGestures.Zone.PREV ->
                            { panY = 0f; minimap.invalidateThumb(); queuePageTurn(-1) }
                        ReaderGestures.Zone.NEXT ->
                            { panY = 0f; minimap.invalidateThumb(); queuePageTurn(+1) }
                        // Centre double-tap while zoomed → restore fit (#54); a single centre tap
                        // records for double-tap detection but otherwise does nothing while zoomed.
                        ReaderGestures.Zone.CENTRE ->
                            if (isCentreDoubleTap(fingerDownX, fingerDownY)) {
                                doubleTapZoom(fingerDownX, fingerDownY)
                            }
                    }
                }
            }
            return
        }
        if (fingerLookupFired) { fingerLookupFired = false; return } // the hold already looked up
        if (fingerMoved) {
            // Not zoomed: a horizontal swipe turns the page (swipe left → next, right → previous),
            // a more-vertical or short drag does nothing. Tap zones still work for precise turns.
            val dx = e.x - fingerDownX
            val dy = e.y - fingerDownY
            val dir = ReaderGestures.swipeDelta(dx, dy, surfaceView.width.toFloat())
            diag { "DIAG finger swipe dx=$dx dy=$dy dir=$dir" }
            if (dir != null) {
                minimap.invalidateThumb()
                queuePageTurn(dir)
            }
            return // a swipe (handled above) or rejected palm — not a tap
        }
        if (SystemClock.uptimeMillis() - lastStylusMs > PALM_REJECT_MS && !stylus.strokeInProgress) {
            handleTap(fingerDownX, fingerDownY)
        } else {
            diag { "DIAG tap suppressed (stylus active → palm)" }
        }
    }

    /**
     * Route a tap: a tapped link wins (RR11-FR3), else tap zones (RR25-FR3 — left third = prev,
     * right third = next, center = contents). The page fills the viewport (stretched render), so
     * the hit-test is the normalized tap `(x/w, y/h)` against the link rects.
     */
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
        if (ReaderGestures.isBookmarkCorner(x, y, w, h)) {
            bottomBar.toggleBookmark()
            return
        }
        val zone = ReaderGestures.zoneFor(x, w)
        diag { "DIAG handleTap x=$x w=$w -> $zone (${currentLinks.size} links, no hit)" }
        // An edge tap breaks any centre double-tap chain (so centre→edge→centre within the window
        // isn't read as a double-tap-zoom) (#54).
        if (zone != ReaderGestures.Zone.CENTRE) lastCentreTapMs = 0L
        when (zone) {
            ReaderGestures.Zone.PREV -> queuePageTurn(-1)
            ReaderGestures.Zone.NEXT -> queuePageTurn(+1)
            ReaderGestures.Zone.CENTRE -> {
                // Centre: a double-tap zooms toward the point (#54); a single tap opens the menu,
                // deferred [DOUBLE_TAP_MS] so the first tap of a double-tap doesn't flash the bar open.
                // (Edge page turns above stay immediate — no double-tap latency on navigation.)
                mainHandler.removeCallbacks(pendingCentreMenu)
                if (isCentreDoubleTap(x, y)) doubleTapZoom(x, y)
                else mainHandler.postDelayed(pendingCentreMenu, DOUBLE_TAP_MS)
            }
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
    /** Direction of travel (+1 forward / -1 back); biases read-ahead. Forward by default. */
    @Volatile private var lastTurnDir = 1
    /** Last page handed to read-ahead — dedupes prefetch enqueues across chrome repaints of one page. */
    private var lastPrefetchedPage = -1

    private fun queuePageTurn(delta: Int) {
        if (delta != 0) lastTurnDir = if (delta < 0) -1 else 1
        pendingPageDelta += delta
        flushPageTurns()
    }

    /** Apply the accumulated delta as a single jump, unless one is already rendering (then it drains
     *  on completion). Snaps to the document bounds; a no-op at the edges. */
    private fun flushPageTurns() {
        if (turnInFlight || pendingPageDelta == 0) return
        val target = ReaderGestures.jumpTarget(currentPage, pendingPageDelta, pageCount)
        pendingPageDelta = 0
        if (target == null) return
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
            // A real page-turn changes the page; a re-render (postJump(currentPage)) does not and
            // must not advance the full-refresh cadence (#99). Read currentPage here, on the engine
            // thread that mutates it, so the check sees the actual pre-jump page.
            val isTurn = page != currentPage
            try {
                if (docHandle == 0L) return@execute
                val commandBytes = try {
                    NativeBridge.nativeJumpToPage(docHandle, page)
                } catch (e: RuntimeException) {
                    Log.e(TAG, "jump failed: ${e.message}")
                    return@execute
                }
                ink.clearAll() // wipe the firmware ink overlay so it doesn't bleed onto the new page
                lasso.dropSelectionForPageChange()
                renderAndBlit(deferLinks = true)
                // Flash the panel FIRST so the new page is visible with no persistence/links work
                // in front of it; then do the off-critical-path bookkeeping (RR27 position + links).
                adapter.executeAll(WireCodec.decodeCommands(commandBytes))
                // Every Nth page-turn, clear accumulated ghosting with a full flash (#99).
                if (isTurn && refreshCadence.onPageTurn()) refreshPanel()
                savePosition() // persist position per jump so an abrupt kill still reopens here (RR27)
                refreshCurrentLinks()
            } finally {
                onDone?.invoke() // release the coalescing latch even on early-out / error
            }
        }
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
            val name = lasso.cycleLassoMode()
            Toast.makeText(this, "$name (tap Lasso again to switch)", Toast.LENGTH_SHORT).show()
            return true
        }
        // Re-tapping the active Pen/Highlighter shows (or restyles, in place) its colour column — an
        // in-window view (see ToolOptions), so it never steals focus and the firmware keeps the
        // live-ink overlay (the only thing that displays committed strokes on the current page). The
        // column is PERSISTENT: it is never collapsed/removed while the tool stays active, because
        // removing the overlay view disturbs that firmware overlay and a sideloaded app cannot force
        // a same-page refresh to repaint it (verified on-device — only page turns refresh). It closes
        // only on a tool switch (a deliberate context change). No Toast on pick: a Toast is a separate
        // window that steals focus and drops the overlay — the ringed swatch is the feedback.
        if (chosen == Tool.HIGHLIGHTER && tool == Tool.HIGHLIGHTER) {
            if (toolOptions.isShowing()) collapseToolOptions()
            else openToolOptions("Highlighter", HIGHLIGHT_COLORS, HIGHLIGHT_COLOR_NAMES, stylus.hlColorIndex) { stylus.hlColorIndex = it }
            return true
        }
        if (chosen == Tool.PEN && tool == Tool.PEN) {
            if (toolOptions.isShowing()) collapseToolOptions()
            else openPenOptions()
            return true
        }
        if (chosen == tool) return true
        toolOptions.dismiss() // close the colour column when switching tools
        tool = chosen
        applyToolInkState("tool")
        // A tool switch ends any lasso selection (it's page- and tool-specific).
        lasso.dropSelectionForToolChange()
        // Switching to a non-pen tool: wipe the firmware pen overlay so it doesn't sit on top of
        // the page while you lasso/erase/define (the real strokes are baked from the core).
        engine.execute {
            if (chosen != Tool.PEN) clearFirmwareInk()
            repaintPanel()
        }
        val hint = when (chosen) {
            Tool.PEN -> "Pen — write with the stylus"
            Tool.HIGHLIGHTER -> "Highlighter — drag over text; tap again to change shade"
            Tool.ERASER -> "Eraser — drag over ink to remove it, or flip the pen from any tool"
            Tool.DEFINE -> "Define — tap a word to look it up; drag over text to select it"
            Tool.LASSO -> "Lasso — circle strokes to select; tap Lasso again for Freehand"
            else -> chosen.label
        }
        Toast.makeText(this, hint, Toast.LENGTH_SHORT).show()
        lasso.updateLassoHint()
        return true
    }

    /**
     * Collapse the colour column. Removing the overlay view disturbs the firmware ink overlay, so we
     * must repaint the page afterwards — and the ONLY repaint this firmware honours on the current
     * page is the policy page-render path (the same one a page turn uses, proven to produce a real
     * EPD frame-done). [postJump] of the current page runs clearAll + renderAndBlit(baked ink) +
     * executeAll(refresh command stream), which re-displays the committed strokes.
     */
    private fun collapseToolOptions() {
        toolOptions.dismiss()
        postJump(currentPage)
    }

    /**
     * Mount the colour column, then repaint the current page — the SHOW-side mirror of
     * [collapseToolOptions]. Adding the overlay view triggers the firmware's full auto-refresh, which
     * repaints the page from the app surface and wipes the live-ink overlay; strokes drawn since the
     * last page render live ONLY on that overlay, so without this repaint they vanish until a page turn
     * re-bakes them (the "tap Pen → annotations disappear" report, #50). [postJump] of the current page
     * re-bakes the committed strokes (renderAndBlit) and drives a real EPD frame, restoring them.
     */
    private fun openToolOptions(title: String, colors: IntArray, names: Array<String>, sel: Int, onPick: (Int) -> Unit) {
        toolOptions.show(title, colors, names, sel, onPick)
        postJump(currentPage)
    }

    /**
     * The pen's options: colour plus line thickness (#199). The thickness is persisted, so it
     * survives a relaunch the way the reader's other typography choices do; strokes already written
     * keep the width they were drawn at, since the core stores width per stroke.
     */
    private fun openPenOptions() {
        toolOptions.show(
            "Pen",
            PEN_COLORS,
            PEN_COLOR_NAMES,
            stylus.penColorIndex,
            StylusInkController.PEN_WIDTHS,
            StylusInkController.PEN_WIDTH_NAMES,
            stylus.penWidthIndex,
            { stylus.penColorIndex = it },
            {
                stylus.penWidthIndex = it
                prefs.edit().putInt(KEY_PEN_WIDTH, it).apply()
            },
        )
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
    /** The current page↔view map (RR5-FR3). Rebuilt from the live zoom/pan on every read, so a
     *  gesture can never see it half-updated; the maths itself lives in [ViewTransform]. */
    private val transform: ViewTransform get() = ViewTransform(viewW, viewH, zoom, panX, panY)

    private fun nToVx(nx: Float) = transform.nToVx(nx)
    private fun nToVy(ny: Float) = transform.nToVy(ny)
    private fun vToNx(vx: Float) = transform.vToNx(vx)
    private fun vToNy(vy: Float) = transform.vToNy(vy)

    /** Convert an on-screen length (px) to normalized page units at the current zoom. */
    private fun lenToNorm(px: Float) = transform.lenToNorm(px)
    /** Push the current zoom/pan to the core and re-render (engine thread). */
    private fun applyZoom() {
        // Any zoom/pan change (pinch, +/- buttons, double-tap, pan) cancels a deferred centre menu so
        // it can't pop the bar open after a zoom (#54). UI-thread; safe before the engine post.
        mainHandler.removeCallbacks(pendingCentreMenu)
        engine.execute {
            if (docHandle != 0L) {
                try { NativeBridge.nativeSetZoom(docHandle, zoom, panX, panY) } catch (e: RuntimeException) {}
            }
            repaintPanel()
        }
    }

    /**
     * Step the reflow text size one preset (#212) — the reflowable counterpart of [zoomBy].
     *
     * Routed through the same `applyReflowScale` a pinch uses, so the button and the gesture cannot
     * drift apart. At either end this is a no-op rather than a repagination that changes nothing;
     * the toast still fires so a tap is never silent, which is the whole complaint behind #212.
     */
    private fun stepTextScale(by: Int) {
        val cur = DisplayPrefs.nearestScaleIndex(displayPrefs.textScale)
        val next = DisplayPrefs.steppedScaleIndex(displayPrefs.textScale, by)
        if (next == cur) {
            val pct = (DisplayPrefs.TEXT_SCALES[cur] * 100).toInt()
            val end = if (by > 0) "largest" else "smallest"
            Log.i(TAG, "text scale: at the $end preset ($pct%), no change")
            Toast.makeText(this, "Text $pct% — already the $end", Toast.LENGTH_SHORT).show()
            return
        }
        Log.i(
            TAG,
            "text scale: ${DisplayPrefs.TEXT_SCALES[cur]} → ${DisplayPrefs.TEXT_SCALES[next]}" +
                " (index $cur → $next)",
        )
        adjust.applyReflowScale(next, announce = true)
    }

    /** Multiply the zoom (clamped); snap back to fit at ~1. Used by the +/- buttons and pinch-end. */
    private fun zoomBy(factor: Float) {
        if (!magnifiable) {
            // The silent case behind #212: a fixed-layout control on a reflowed view. Logged rather
            // than dropped, so "the button does nothing" is answerable from a logcat.
            Log.i(TAG, "zoom ignored: this document is not magnifiable (reflowed view)")
            return
        }
        val from = zoom
        val next = ZoomPolicy.stepped(zoom, factor, MAX_ZOOM_UI)
        // The fit thumb must be grabbed while the fit render is still on screen (see ZoomPolicy).
        if (ZoomPolicy.leavingFit(from, next)) minimap.captureFitThumb(bitmap)
        zoom = next
        if (ZoomPolicy.isFit(zoom)) { panX = 0f; panY = 0f }
        Log.i(TAG, "zoom: $from → $zoom" + if (from == zoom) " (at the limit)" else "")
        applyZoom()
    }

    /**
     * Double-tap zoom toggle (#54): from fit, zoom toward the tapped point ([DOUBLE_TAP_ZOOM]),
     * anchoring the content under the tap with the same focal math as pinch-end; while zoomed, a
     * double-tap restores fit. `(fx, fy)` is the tap in view pixels.
     */
    private fun doubleTapZoom(fx: Float, fy: Float) {
        if (!magnifiable) return // reflowable view: a double-tap can't magnify (#61)
        if (zoom > 1f) {
            zoom = 1f; panX = 0f; panY = 0f
            applyZoom()
            return
        }
        minimap.captureFitThumb(bitmap) // grab the fit thumb before leaving fit (for the zoom minimap)
        val nx = vToNx(fx); val ny = vToNy(fy) // page point under the tap, at the current (fit) factor
        zoom = ZoomPolicy.doubleTapTarget(zoom, DOUBLE_TAP_ZOOM, MAX_ZOOM_UI)
        // The same anchoring a pinch-end uses, so a double-tap and a pinch put the same content
        // under the same finger — one implementation, in [ViewTransform.panAnchoring].
        val (px, py) = transform.panAnchoring(nx, ny, fx, fy)
        panX = px; panY = py
        applyZoom()
    }

    /** True if `(x, y)` continues a recent tap into a double-tap (within [DOUBLE_TAP_MS] and
     *  [DOUBLE_TAP_SLOP_PX] of the last centre tap). Records the tap as the potential first of a pair. */
    private fun isCentreDoubleTap(x: Float, y: Float): Boolean {
        val now = SystemClock.uptimeMillis()
        if (ReaderGestures.isDoubleTap(x, y, lastCentreTapX, lastCentreTapY, now - lastCentreTapMs)) {
            lastCentreTapMs = 0L // consume — a third tap doesn't chain
            return true
        }
        lastCentreTapMs = now; lastCentreTapX = x; lastCentreTapY = y
        return false
    }

    // Live-preview state: during a pinch/pan we cheaply transform the CACHED page bitmap on the
    // canvas (no pdfium, no JNI) for instant feedback, then re-render crisp once on gesture end.
    private var gestureStartZoom = 1f
    private var pinchFont = false // this pinch adjusts font size (reflowable) vs. magnifies (fixed)
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
                // Reflowable views can't magnify (#61, RR25-FR3); instead the pinch resizes the font
                // (pinch out = larger, in = smaller), like KOReader. Fixed-layout views magnify.
                pinchFont = !magnifiable
                gestureStartZoom = zoom; liveScale = 1f; focusX = d.focusX; focusY = d.focusY
                return true
            }
            override fun onScale(d: android.view.ScaleGestureDetector): Boolean {
                liveScale *= d.scaleFactor
                if (pinchFont) return true // font resize repaginates — too costly per frame; apply on end
                val eff = (gestureStartZoom * liveScale).coerceIn(1f, MAX_ZOOM_UI) / gestureStartZoom
                throttledPreview {
                    previewBitmap(android.graphics.Matrix().apply { postScale(eff, eff, focusX, focusY) })
                }
                return true
            }
            override fun onScaleEnd(d: android.view.ScaleGestureDetector) {
                if (pinchFont) {
                    // Map the pinch ratio onto the font scale: pinch out 1.3x ≈ font 1.3x, snapped to
                    // the nearest preset. A small/accidental pinch leaves the size unchanged.
                    val cur = DisplayPrefs.nearestScaleIndex(displayPrefs.textScale)
                    val target = DisplayPrefs.nearestScaleIndex(
                        (displayPrefs.textScale * liveScale).coerceIn(DisplayPrefs.TEXT_SCALES.first(), DisplayPrefs.TEXT_SCALES.last())
                    )
                    if (target != cur) adjust.applyReflowScale(target, announce = true)
                    return
                }
                val newZoom = ZoomPolicy.stepped(gestureStartZoom, liveScale, MAX_ZOOM_UI)
                // `zoom` is still the pre-gesture value here, so the fit thumb is still valid.
                if (ZoomPolicy.leavingFit(gestureStartZoom, newZoom)) minimap.captureFitThumb(bitmap)
                if (ZoomPolicy.isFit(newZoom)) {
                    zoom = 1f; panX = 0f; panY = 0f
                } else {
                    // Anchor the pinched point: keep the content under the focal point fixed.
                    val nx = vToNx(focusX); val ny = vToNy(focusY) // uses the pre-zoom factor
                    zoom = newZoom
                    val (px, py) = transform.panAnchoring(nx, ny, focusX, focusY)
                    panX = px; panY = py
                }
                applyZoom()
            }
        })
    }

    private fun applyToolInkState(reason: String) {
        val ok = ink.setup()
        // Only the Pen wants the firmware EMR pen painting the live stroke. Lasso, Define,
        // Highlighter and Eraser draw their OWN overlay (dashed loop / dashed select line / wide
        // band / swept band), so suppress the firmware ink for them — else it paints a solid black
        // stroke on top. The Eraser used to be grouped with the Pen here, which had the firmware
        // ink a real eraser sweep onto the panel; wiping it afterwards was a race the eraser
        // sometimes lost, leaving a scribble over the page (#158).
        // setup() re-enables the writable area each call (incl. on focus regain), so re-assert here
        // for every reason. setWritable rides the service_myservice binder (works for a sideloaded
        // app); enableFullUiAuto is SELinux-blocked.
        val inkWritable = tool == Tool.PEN
        ink.setWritable(inkWritable)
        Log.i(TAG, "ink claimed ($reason) for $tool: available=$ok writable=$inkWritable")
    }

    /** Wipe the firmware ink overlay (engine thread) — used after a non-pen gesture so its transient
     *  ink doesn't linger over the page. Safe: real strokes are baked from the core on re-render. */
    private fun clearFirmwareInk() {
        ink.clearAll()
    }

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

    /** Toggle PDF reflow (ADR-INKREAD-0011). On enable, re-apply the saved typography so the
     *  reflowed PDF respects the user's font size / spacing / alignment; the page count and position
     *  change across the toggle, so refresh both. A `-1` means no text layer (scanned PDF). */
    private fun setReflowMode(on: Boolean) {
        if (reflowInProgress) return // ignore a re-toggle while a reflow build is running
        reflowInProgress = true
        // Enabling reflow on a PDF re-extracts the text layer and repaginates the WHOLE document — on
        // a large book that's several seconds. Show a "Reflowing…" notice if it doesn't finish
        // quickly so the toggle doesn't look frozen (#55). Not cancellable: this path rebuilds the
        // reflow view rather than driving the core's pagination, so there is no progress to report
        // and nothing to fall back to. The work itself is already off the UI thread (engine).
        reflowProgress = ReflowProgress(this, cancellable = false).also { it.begin() }
        engine.execute {
            val np = try { NativeBridge.nativeSetReflow(docHandle, on) } catch (e: RuntimeException) { -1 }
            if (np >= 0) {
                reflowOn = on
                // The reflow toggle changes magnifiability; the core resets its zoom on reflow-on, so
                // drop any stranded shell zoom/pan to match and re-fit (#61).
                magnifiable = try { NativeBridge.nativeIsMagnifiable(docHandle) } catch (e: RuntimeException) { !on }
                if (!magnifiable && zoom != 1f) { zoom = 1f; panX = 0f; panY = 0f }
                if (on) {
                    // Same one-call restore as the open path — a reflowed PDF gets the reader's
                    // saved typeface too, which the per-setting restore here used to skip.
                    try {
                        NativeBridge.nativeSetTypography(
                            docHandle,
                            displayPrefs.textScale,
                            displayPrefs.fontId(UserFonts.faceNames()),
                            displayPrefs.lineSpacingMult,
                            displayPrefs.alignment,
                            displayPrefs.columns,
                            displayPrefs.marginPct,
                        )
                    } catch (e: RuntimeException) {}
                }
                pageCount = NativeBridge.nativePageCount(docHandle)
                repaintPanel()
            } else {
                runOnUiThread { Toast.makeText(this, "This PDF has no text layer to reflow", Toast.LENGTH_SHORT).show() }
            }
            runOnUiThread { dismissReflowProgress() }
        }
    }

    /** Dismiss the reflow "Reflowing…" notice (and disarm a not-yet-shown one); clears the guard. */
    private fun dismissReflowProgress() {
        reflowProgress?.end()
        reflowProgress = null
        reflowInProgress = false
    }

    /** Persist the current reading position (RR12-FR3 / RR27); store-less / closed = no-op. */
    private fun savePosition() {
        if (docHandle == 0L) return
        try {
            NativeBridge.nativeSavePosition(docHandle)
        } catch (e: RuntimeException) {
            Log.e(TAG, "save position failed: ${e.message}")
        }
        // Record read progress + page position for the home shelf (RR16/RR17).
        val total = pageCount
        if (total > 0 && currentBookId.isNotEmpty()) {
            Books.setProgress(this, currentBookId, ((currentPage + 1) * 100) / total)
            Books.setPage(this, currentBookId, currentPage)
        }
    }

    private fun closeDocument() {
        bookmarks = null // bookmarks are persisted on toggle; drop the per-book store
        lasso.reset() // ink is persisted by the core to its sidecar
        chapters = emptyList() // recomputed on the next open

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
        const val REQ_OPEN_FONT = 2 // ...and for the font-import picker (RR28-FR3).
        const val PREFS = "inkread"
        const val KEY_BOOK_PATH = "book_path" // stored PDF under app storage (RR27).
        const val KEY_BOOK_ID = "book_id" // stable per-book id (the stored file name).
        const val KEY_PEN_WIDTH = "pen_width" // selected pen thickness, an index into PEN_WIDTHS (#199).
        /**
         * The bookmark dog-ear's tap target: the top [BOOKMARK_ZONE_H] of the rightmost
         * [BOOKMARK_ZONE_W] of the page, as fractions so it holds on any panel.
         *
         * Named because two things need it and they must not drift apart: the tap handler that
         * toggles the bookmark, and the tool palette, which has to keep out of it (#200).
         */
        const val BOOKMARK_ZONE_W = 0.14f
        const val BOOKMARK_ZONE_H = 0.08f

        const val KEY_PALETTE_EXPANDED = "palette_expanded" // tools out or put away (#200).
        const val KEY_PALETTE_X = "palette_x" // parked tool-palette corner, host fractions (#200).
        const val KEY_PALETTE_Y = "palette_y"
        const val PALM_REJECT_MS = 1000L // a finger tap within this long of a stylus event = palm.
        // Public, Partner-synced folder the annotated PDF export is written to (Android external
        // storage root + this) so it reaches the desktop. "Document" is in the Supernote sync set.
        const val MAX_ZOOM_UI = 5f // matches the core's MAX_ZOOM clamp (RR5-FR3).
        const val PREVIEW_MS = 50L // min interval between live zoom/pan preview blits (e-ink cadence).
        const val ZOOM_STEP = 1.4f // +/- button zoom multiplier.
        const val DOUBLE_TAP_ZOOM = 2.0f // zoom level a double-tap jumps to from fit (#54).
        const val DOUBLE_TAP_MS = 280L // max gap between the two taps of a double-tap (#54).
        const val DOUBLE_TAP_SLOP_PX = 60f // max distance between the two taps to count as a double-tap.
        const val SELECTION_HANDLE_PX = 8f // half-size of the square corner handles on the selection box.
        const val STROKE_PAUSE_MS = 600L // commit a stroke after this pen-pause (swallowed-UP net); shared with the lasso net.

        // Core ink seam constants (ADR-INKREAD-0010). Tool codes mirror `inkread_ink::Tool::code`.
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
        const val FINGER_LONG_PRESS_MS = 500L // finger held ~still this long on a word → look it up.
        const val FINGER_MOVE_SLOP_PX = 24f // finger travel beyond this = a swipe, not a tap/hold.
        const val SLOW_RENDER_MS = 250L // a render+blit at/over this approaches the e-ink budget — log it.
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
        const val EXTRA_DAILY_ARTICLE = "inkread.daily_article" // open a Daily issue at this article index.
    }
}
