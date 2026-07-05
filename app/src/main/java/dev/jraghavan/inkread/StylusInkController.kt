package dev.jraghavan.inkread

import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.MotionEvent

/**
 * Stylus pen/highlighter + eraser capture and the ink commit path (RR19/RR20), extracted from
 * `ReaderActivity` (SRP). Owns the in-progress stroke/erase buffers, the pen long-press → word
 * lookup, the deferred autosave debounce, the pen colour selection, the ink paints, and
 * [drawStroke] (baking a core stroke onto the page). The shell keeps the firmware-ink object,
 * the coordinate transforms, and the shared `drawLivePath` primitive — all reached through [Host].
 *
 * Threading mirrors the original inline code: capture runs on the UI thread; commits and the
 * autosave flush run on the engine thread via [Host.engineExecute]. The timeout/long-press/flush
 * runnables live on this controller's own main-looper handler (nothing outside cancelled them
 * except onPause, which now routes through [cancelPendingOnPause]).
 */
class StylusInkController(private val host: Host) {

    /** What stylus capture needs from the reader shell. */
    interface Host {
        /** The open document handle (`0` = none); read live per call. */
        val docHandle: Long

        val currentPage: Int

        val viewW: Int

        val viewH: Int

        val zoom: Float

        /** The active annotation tool (Pen vs Highlighter decides the core tool + colour). */
        val activeTool: Tool

        // View↔normalized transforms + length scaling (shell owns zoom/pan).
        fun vToNx(vx: Float): Float

        fun vToNy(vy: Float): Float

        fun nToVx(nx: Float): Float

        fun nToVy(ny: Float): Float

        /** Map a view-px length to a page-normalized width (for stroke/eraser widths). */
        fun lenToNorm(px: Float): Float

        /** Run [block] on the single engine thread (serializes native access). */
        fun engineExecute(block: () -> Unit)

        /** Wipe the firmware ink overlay (engine thread). */
        fun clearFirmwareInk()

        /** Re-render + refresh the current page (any thread). */
        fun repaintPanel()

        /** Draw an in-progress gesture path over the cached page (UI thread) — the highlighter band. */
        fun drawLivePath(buf: ArrayList<Float>, paint: Paint)

        /** Engine thread: look up the word under a normalized point (pen long-press). */
        fun defineWord(page: Int, nx: Float, ny: Float)

        /** Verbose diagnostic log, gated by the shell's `DIAG` flag. */
        fun diag(msg: () -> String)
    }

    private val handler = Handler(Looper.getMainLooper())

    /** The in-progress stroke as interleaved view-px x,y; UI-thread only. */
    private val strokeBuf = ArrayList<Float>()

    /** Safety net for a swallowed stylus ACTION_UP: commit the stroke after a brief pen pause. */
    private val strokeFinalize = Runnable { finalizeStroke() }

    /** In-progress eraser path as interleaved view-px x,y; UI-thread only. */
    private val eraseBuf = ArrayList<Float>()

    /** Net for a swallowed stylus UP during erasing (mirrors [strokeFinalize]). */
    private val eraseFinalize = Runnable { finalizeErase() }

    /** Trailing-edge flush of deferred ink (RR20): coalesces the per-stroke fsync into one write a
     *  short while after the pen goes idle. onPause/teardown flush immediately and cancel this. */
    private val inkFlush = Runnable {
        val h = host.docHandle
        if (h != 0L) host.engineExecute {
            try { NativeBridge.nativeInkSave(h) } catch (e: RuntimeException) { Log.e(TAG, "ink flush failed: ${e.message}") }
        }
    }

    // ---- stylus long-press → instant word lookup (natural "hold a word to define it") ----
    private var lpDownX = 0f
    private var lpDownY = 0f
    private var lpMoved = false

    /** Fires when the pen has been held ~still on a word: look it up, cancelling the nascent stroke. */
    private val longPress = Runnable {
        handler.removeCallbacks(strokeFinalize) // this hold is a lookup, not a stroke
        strokeBuf.clear()
        if (host.viewW == 0 || host.viewH == 0) return@Runnable
        val nx = host.vToNx(lpDownX); val ny = host.vToNy(lpDownY)
        val page = host.currentPage
        host.diag { "DIAG long-press lookup @($nx,$ny) page=$page" }
        host.engineExecute {
            host.clearFirmwareInk(); host.repaintPanel() // wipe the pen dot the hold left
            host.defineWord(page, nx, ny)
        }
    }

    private val inkPaint = Paint().apply {
        color = Color.BLACK
        style = Paint.Style.STROKE
        strokeWidth = INK_STROKE_WIDTH // match the firmware needle (baked was thinner than live)
        strokeCap = Paint.Cap.ROUND
        strokeJoin = Paint.Join.ROUND
        isAntiAlias = true
    }
    private val inkDotPaint = Paint().apply { color = Color.BLACK; style = Paint.Style.FILL; isAntiAlias = true }

    private val hlLivePaint = Paint().apply {
        style = Paint.Style.STROKE; strokeCap = Paint.Cap.ROUND; strokeJoin = Paint.Join.ROUND; isAntiAlias = true
    }

    /** Current pen / highlighter colour (index into the palettes); re-tapping a tool cycles it. */
    var penColorIndex = 0
    var hlColorIndex = 0

    private fun penColor() = ReaderActivity.PEN_COLORS[penColorIndex]

    /** The active highlighter swatch (packed r<<24|g<<16|b<<8|a) — also read by the lasso text highlight. */
    val highlightColor: Int get() = ReaderActivity.HIGHLIGHT_COLORS[hlColorIndex]

    /** True while a stylus stroke is mid-capture (drives palette state + finger palm gating). */
    val strokeInProgress: Boolean get() = strokeBuf.isNotEmpty()

    /** Live highlighter band paint, coloured + sized to the current shade. */
    private fun highlighterLivePaint(): Paint {
        val c = highlightColor
        hlLivePaint.color = Color.argb(c and 0xFF, (c ushr 24) and 0xFF, (c ushr 16) and 0xFF, (c ushr 8) and 0xFF)
        hlLivePaint.strokeWidth = HIGHLIGHT_WIDTH_PX
        return hlLivePaint
    }

    /** (Re)arm the trailing-edge ink flush after an edit; resets the timer on each new stroke. */
    fun scheduleInkFlush() {
        handler.removeCallbacks(inkFlush)
        handler.postDelayed(inkFlush, INK_FLUSH_MS)
    }

    /** onPause: drop the pending word-lookup + deferred flush (the shell flushes explicitly after).
     *  Mirrors the original onPause, which cancelled exactly these two — an in-flight stroke's
     *  finalize is deliberately left armed so a stroke at pause time still commits. */
    fun cancelPendingOnPause() {
        handler.removeCallbacks(longPress)
        handler.removeCallbacks(inkFlush)
    }

    // ---- handwriting capture (RR19) ----

    /** Accumulate the stylus stroke; commit on UP (or a debounced pen-pause if UP is swallowed). */
    fun captureStylus(e: MotionEvent) {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                strokeBuf.clear()
                strokeBuf.add(e.x); strokeBuf.add(e.y)
                armStrokeTimeout()
                // Arm long-press → word lookup in Pen (reading) mode: hold the pen on a word to
                // define it, no tool switch needed. (Other tools have their own hold semantics.)
                if (host.activeTool == Tool.PEN) {
                    lpDownX = e.x; lpDownY = e.y; lpMoved = false
                    handler.postDelayed(longPress, LONG_PRESS_MS)
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
                    lpMoved = true; handler.removeCallbacks(longPress)
                }
                // Pen rides the fast firmware overlay; Highlighter's is suppressed, so draw its band.
                if (host.activeTool == Tool.HIGHLIGHTER) host.drawLivePath(strokeBuf, highlighterLivePaint())
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                handler.removeCallbacks(longPress) // lifted before the hold fired → normal stroke
                strokeBuf.add(e.x); strokeBuf.add(e.y)
                handler.removeCallbacks(strokeFinalize)
                finalizeStroke()
            }
        }
    }

    private fun armStrokeTimeout() {
        handler.removeCallbacks(strokeFinalize)
        handler.postDelayed(strokeFinalize, ReaderActivity.STROKE_PAUSE_MS)
    }

    /** Hand the captured stroke to the engine thread for persistence (UI thread). */
    private fun finalizeStroke() {
        host.diag { "DIAG finalizeStroke buf=${strokeBuf.size / 2} pts" }
        if (strokeBuf.size < 2) { strokeBuf.clear(); return }
        val raw = strokeBuf.toFloatArray()
        strokeBuf.clear()
        host.engineExecute { commitStroke(raw) }
    }

    /** Map packed view-space `[x,y,…]` to packed page-normalized `[x,y,…]` for [NativeBridge.nativeInkAddPoints]. */
    private fun toNormPoints(view: FloatArray): FloatArray {
        val out = FloatArray(view.size)
        var i = 0
        while (i + 1 < view.size) {
            out[i] = host.vToNx(view[i]); out[i + 1] = host.vToNy(view[i + 1]); i += 2
        }
        return out
    }

    /** Feed the captured pen stroke to the core (begin→points→end → autosave). Engine thread. */
    private fun commitStroke(raw: FloatArray) {
        val w = host.viewW; val h = host.viewH
        if (host.docHandle == 0L || w == 0 || h == 0) return
        // Highlighter = a wide, translucent band (its own core tool + colour); Pen = thin black.
        val isHl = host.activeTool == Tool.HIGHLIGHTER
        val coreTool = if (isHl) ReaderActivity.CORE_TOOL_HIGHLIGHTER else ReaderActivity.CORE_TOOL_PEN
        val widthNorm = host.lenToNorm(if (isHl) HIGHLIGHT_WIDTH_PX else INK_STROKE_WIDTH)
        val color = if (isHl) highlightColor else penColor()
        try {
            NativeBridge.nativeInkBeginStroke(host.docHandle, coreTool, color, widthNorm, System.currentTimeMillis())
            NativeBridge.nativeInkAddPoints(host.docHandle, toNormPoints(raw))
            NativeBridge.nativeInkEndStroke(host.docHandle)
            scheduleInkFlush() // deferred autosave: persist on a trailing debounce, not this fsync
            host.diag { "DIAG commitStroke OK ${raw.size / 2} pts tool=${host.activeTool} → core page ${host.currentPage}" }
        } catch (e: RuntimeException) {
            Log.e(TAG, "ink commit failed: ${e.message}")
        }
        // Highlighter's firmware EMR ink is suppressed (we drew the live band ourselves), so bake it
        // from the core now. Pen rides the firmware overlay and bakes on the next full render.
        if (isHl) { host.clearFirmwareInk(); host.repaintPanel(); return }
        // The firmware overlay already shows this stroke live; it bakes from the core on the next
        // full render (page turn / revisit), so no immediate re-blit is needed here.
    }

    /** Draw one core stroke (normalized points + tool/color/width) onto [canvas]. */
    fun drawStroke(canvas: Canvas, s: InkStrokeDraw) {
        val norm = s.points
        if (norm.isEmpty()) return
        inkPaint.color = Color.argb(s.a, s.r, s.g, s.b)
        inkPaint.strokeWidth = (s.width * host.viewW * host.zoom).coerceAtLeast(1f)
        if (norm.size == 2) {
            inkDotPaint.color = inkPaint.color
            canvas.drawCircle(host.nToVx(norm[0]), host.nToVy(norm[1]), inkPaint.strokeWidth / 2f, inkDotPaint)
            return
        }
        val path = Path()
        path.moveTo(host.nToVx(norm[0]), host.nToVy(norm[1]))
        var i = 2
        while (i + 1 < norm.size) { path.lineTo(host.nToVx(norm[i]), host.nToVy(norm[i + 1])); i += 2 }
        canvas.drawPath(path, inkPaint)
    }

    // ---- eraser (RR19) ----

    /** Accumulate the eraser path; finalize on UP (or a debounced pause if UP is swallowed). */
    fun captureErase(e: MotionEvent) {
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
                handler.removeCallbacks(eraseFinalize)
                finalizeErase()
            }
        }
    }

    private fun armEraseTimeout() {
        handler.removeCallbacks(eraseFinalize)
        handler.postDelayed(eraseFinalize, ReaderActivity.STROKE_PAUSE_MS)
    }

    /** Hand the eraser path to the engine thread to remove crossed strokes (UI thread). */
    private fun finalizeErase() {
        if (eraseBuf.size < 2) { eraseBuf.clear(); return }
        val pts = eraseBuf.toFloatArray()
        eraseBuf.clear()
        host.engineExecute { commitErase(pts) }
    }

    /** Feed the eraser path to the core (Eraser stroke removes crossed strokes); re-render (engine). */
    private fun commitErase(viewPts: FloatArray) {
        val w = host.viewW; val h = host.viewH
        if (host.docHandle == 0L || w == 0 || h == 0) return
        val radiusNorm = host.lenToNorm(ERASE_RADIUS_PX)
        try {
            NativeBridge.nativeInkBeginStroke(host.docHandle, ReaderActivity.CORE_TOOL_ERASER, ReaderActivity.INK_COLOR_BLACK, radiusNorm, System.currentTimeMillis())
            NativeBridge.nativeInkAddPoints(host.docHandle, toNormPoints(viewPts))
            NativeBridge.nativeInkEndStroke(host.docHandle)
            scheduleInkFlush() // deferred autosave: persist on a trailing debounce, not this fsync
        } catch (e: RuntimeException) {
            Log.e(TAG, "erase failed: ${e.message}"); return
        }
        host.clearFirmwareInk() // wipe the firmware ink left by the eraser drag
        host.repaintPanel()
    }

    companion object {
        private const val TAG = "StylusInk"

        // Ink-tuning constants used only by this controller (shared ones — STROKE_PAUSE_MS,
        // the CORE_TOOL_* codes, the colour palettes — stay in ReaderActivity).
        const val INK_STROKE_WIDTH = 6f // baked-ink line width (px) tuned to match the firmware pen.
        const val HIGHLIGHT_WIDTH_PX = 30f // wide marker band (vs INK_STROKE_WIDTH for the pen).
        const val ERASE_RADIUS_PX = 22f // eraser hit radius (px): a stroke within this of the path goes.
        const val INK_FLUSH_MS = 1500L // trailing-edge delay before the deferred ink autosave fsyncs.
        const val LONG_PRESS_MS = 500L // hold the pen this long (≈still) on a word → look it up.
        const val LONG_PRESS_SLOP_PX = 16f // movement beyond this cancels the long-press (it's a stroke).
    }
}
