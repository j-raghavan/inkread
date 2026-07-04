package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.graphics.Color
import android.graphics.Paint
import android.graphics.RectF
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.MotionEvent
import android.widget.Toast
import kotlin.math.max
import kotlin.math.min

/**
 * Lasso selection + Define-tool text selection (ADR-INKREAD-0010), extracted from
 * `ReaderActivity` (SRP). Owns the selection state (stroke ids + normalized bounds), the lasso
 * loop capture/finalize (ink OR printed-text fallback), selection move, the selection-toolbar
 * actions (delete/cut/copy/paste/select-all/digest/done), ink undo/redo, and the Define tool's tap-vs-drag
 * dispatch. The shell keeps the views (selection toolbar, hint banner), the page/zoom geometry,
 * and the draw primitives — all reached through [Host].
 *
 * Threading mirrors the original inline code: capture runs on the UI thread; the core calls run
 * on the engine thread via [Host.engineExecute]. [selectionBounds]/[selectedIds] stay volatile —
 * written on the engine thread, read by the UI render path.
 */
class LassoController(private val host: Host) {

    /** What the lasso needs from the reader shell. */
    interface Host {
        /** Context for dialogs/toasts/clipboard, `runOnUiThread`. */
        val activity: Activity

        /** The open document handle (`0` = none); read live per call. */
        val docHandle: Long

        val currentPage: Int

        /** View geometry (the zoom/pan model stays in the shell). */
        val viewW: Int

        val viewH: Int

        val zoom: Float

        val surfaceW: Int

        val surfaceH: Int

        /** The active annotation tool (for the hint + empty-selection toast). */
        val activeTool: Tool

        /** Current highlighter swatch (packed r<<24|g<<16|b<<8|a). */
        val highlightColor: Int

        // View↔normalized transforms (shell owns zoom/pan).
        fun vToNx(vx: Float): Float

        fun vToNy(vy: Float): Float

        fun nToVx(nx: Float): Float

        fun nToVy(ny: Float): Float

        /** Run [block] on the single engine thread (serializes native access). */
        fun engineExecute(block: () -> Unit)

        /** Draw an in-progress gesture path over the cached page (UI thread). */
        fun drawLivePath(buf: ArrayList<Float>, paint: Paint)

        /** Blit the cached page with [draw] painted over it; no-op without a cached page. */
        fun overlayOnPage(draw: (android.graphics.Canvas) -> Unit)

        /** Re-render the core page into the panel (engine thread). */
        fun renderAndBlit()

        /** Re-render + refresh (any thread). */
        fun repaintPanel()

        /** Force a panel refresh of what's already blitted (engine thread). */
        fun refreshPanel()

        /** Wipe the firmware ink overlay (engine thread). */
        fun clearFirmwareInk()

        /** (Re)arm the trailing-edge deferred ink autosave. */
        fun scheduleInkFlush()

        /** Show/anchor the floating selection toolbar (UI thread). */
        fun showSelectionToolbar(rect: RectF, canPaste: Boolean)

        fun dismissSelectionToolbar()

        /** Show/hide the Lasso discoverability banner. */
        fun setLassoHintVisible(show: Boolean)

        // Sibling-controller actions offered from the text-selection sheet.
        /** Engine thread: word lookup at a normalized point (Define tap). */
        fun defineWord(page: Int, nx: Float, ny: Float)

        fun defineSelectionText(text: String)

        fun addDigest(page: Int, boundsNorm: FloatArray)

        fun addDigestText(page: Int, text: String, boundsNorm: FloatArray?)

        /** Verbose diagnostic log, gated by the shell's `DIAG` flag. */
        fun diag(msg: () -> String)
    }

    private val activity: Activity get() = host.activity
    private fun runOnUiThread(block: () -> Unit) = activity.runOnUiThread(block)
    private val mainHandler = Handler(Looper.getMainLooper())

    /** In-progress lasso loop as interleaved view-px x,y; UI-thread only. */
    private val lassoBuf = ArrayList<Float>()

    /** Net for a swallowed stylus UP during the lasso loop. */
    private val lassoFinalize = Runnable { finalizeLasso() }

    /** 0=Smart, 1=Freehand lasso (NeoReader's two modes). */
    @Volatile private var lassoMode = 0

    /** The current selection's stroke ids (empty = no selection); read on both threads. */
    @Volatile private var selectedIds = IntArray(0)

    /** The selection's normalized bounds [x0,y0,x1,y1] for the box + toolbar anchor; empty = none. */
    @Volatile var selectionBounds = FloatArray(0)
        private set

    /** When dragging the selection to move it: the down point (view px) and whether a move began. */
    private var moveStartX = 0f
    private var moveStartY = 0f
    private var movingSelection = false

    /** In-progress selection stroke as interleaved view-px x,y; UI-thread only. */
    private val selBuf = ArrayList<Float>()

    /** Net for a swallowed stylus UP during selection (mirrors the pen path's strokeFinalize). */
    private val selectionFinalize = Runnable { finalizeSelection() }

    /** Dashed marching-ants line for the in-progress lasso loop (mirrors the firmware's own
     *  AreaSelectionView dashPaint — DashPathEffect{6,6} on a normal canvas). */
    private val lassoPaint = Paint().apply {
        color = Color.BLACK
        style = Paint.Style.STROKE
        strokeWidth = 2f
        isAntiAlias = true
        pathEffect = android.graphics.DashPathEffect(floatArrayOf(8f, 6f), 0f)
    }

    val hasSelection: Boolean get() = selectedIds.isNotEmpty()

    /** Toggle Smart ↔ Freehand; returns the new mode's display name (for the toast). */
    fun cycleLassoMode(): String {
        lassoMode = if (lassoMode == 0) 1 else 0
        return if (lassoMode == 0) "Smart lasso" else "Freehand lasso"
    }

    /** Show the Lasso banner while the tool is active with nothing selected. */
    fun updateLassoHint() {
        val show = host.activeTool == Tool.LASSO && selectedIds.isEmpty()
        runOnUiThread { host.setLassoHintVisible(show) }
    }

    /** Drop any lasso selection when the page changes — the ids belong to the old page (engine). */
    fun dropSelectionForPageChange() {
        if (selectedIds.isEmpty()) return
        selectedIds = IntArray(0)
        selectionBounds = FloatArray(0)
        runOnUiThread { host.dismissSelectionToolbar() }
    }

    /** A tool switch ends any lasso selection (it's page- and tool-specific); no repaint here —
     *  the switch itself repaints. */
    fun dropSelectionForToolChange() {
        selectedIds = IntArray(0)
        selectionBounds = FloatArray(0)
        host.dismissSelectionToolbar()
    }

    /** Forget the selection on document close (ink is persisted by the core to its sidecar). */
    fun reset() {
        selectedIds = IntArray(0)
        selectionBounds = FloatArray(0)
    }

    // ===== Lasso selection (ADR-INKREAD-0010) =====

    /**
     * Capture the lasso stylus gesture. If the down lands **inside** an active selection, the gesture
     * MOVES that selection (NeoReader: drag the selection); otherwise it draws a new lasso loop.
     */
    fun captureLasso(e: MotionEvent) {
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
                // We own the loop pixels (firmware EMR ink suppressed): dashed marching-ants line.
                host.drawLivePath(lassoBuf, lassoPaint)
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
        mainHandler.postDelayed(lassoFinalize, ReaderActivity.STROKE_PAUSE_MS)
    }

    /** Whether a view-px point falls inside the current selection's bounds. */
    private fun pointInSelection(x: Float, y: Float): Boolean {
        val b = selectionBounds
        if (b.size != 4 || host.viewW == 0 || host.viewH == 0) return false
        val nx = host.vToNx(x); val ny = host.vToNy(y)
        return nx in b[0]..b[2] && ny in b[1]..b[3]
    }

    /** Close the loop and ask the core which strokes it selects (engine thread). */
    private fun finalizeLasso() {
        host.diag { "DIAG finalizeLasso buf=${lassoBuf.size / 2} pts mode=$lassoMode" }
        if (lassoBuf.size < 6) { // need ≥3 points for a polygon
            lassoBuf.clear()
            host.engineExecute { host.clearFirmwareInk(); host.repaintPanel() }
            return
        }
        val raw = lassoBuf.toFloatArray()
        lassoBuf.clear()
        val w = host.viewW; val h = host.viewH
        if (w == 0 || h == 0) return
        val poly = FloatArray(raw.size)
        var i = 0
        while (i + 1 < raw.size) {
            poly[i] = host.vToNx(raw[i])
            poly[i + 1] = host.vToNy(raw[i + 1])
            i += 2
        }
        host.engineExecute {
            if (host.docHandle == 0L) return@engineExecute
            val ids = try {
                NativeBridge.nativeInkSelectInPolygon(host.docHandle, poly, lassoMode)
            } catch (e: RuntimeException) {
                Log.e(TAG, "lasso select failed: ${e.message}"); return@engineExecute
            }
            host.diag { "DIAG lasso selected ${ids.size} strokes from ${poly.size / 2}-pt loop" }
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
        if (host.docHandle == 0L || poly.size < 6) {
            runOnUiThread { Toast.makeText(activity, "Nothing under the loop", Toast.LENGTH_SHORT).show() }
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
        // A MULTI-LINE text selection always reads in reading order — intermediate lines whole, the
        // last line clipped — whether the gesture was an open drag or a closed loop (a geometric
        // bbox across lines would catch only the columns inside the loop, leaving intermediate lines
        // partial). Use the drag's start→lift when it's a directional open drag; for a closed loop
        // use its top-left→bottom-right corners so the span still reads top to bottom.
        val openDrag = kotlin.math.hypot(ex - sx, ey - sy) > OPEN_DRAG_FRAC
        val multiLine = (y1 - y0) > MULTILINE_DRAG_FRAC
        if (multiLine) {
            if (openDrag) presentLineSpanSelection(sx, sy, ex, ey, "No text under the selection")
            // Closed loop: corner→corner. The last line is clipped to the loop's rightmost extent
            // (x1), an approximation — for an irregular loop that may run a word or two past where the
            // user closed it on the bottom line. Acceptable for circling a region; the directional
            // open-drag path above clips precisely to the actual lift point.
            else presentLineSpanSelection(x0, y0, x1, y1, "No text under the selection")
        } else {
            // Single line → the dragged/circled span on that line (precise, horizontal).
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
        if (host.docHandle == 0L) return
        val sel = try {
            WireCodec.decodeSelection(NativeBridge.nativeTextInRect(host.docHandle, host.currentPage, x0, y0, x1, y1))
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
        if (host.docHandle == 0L) return
        host.diag { "DIAG lineSpan start=(%.3f,%.3f) lift=(%.3f,%.3f) page=${host.currentPage}".format(sx, sy, ex, ey) }
        val sel = try {
            WireCodec.decodeSelection(NativeBridge.nativeTextLineSpan(host.docHandle, host.currentPage, sx, sy, ex, ey))
        } catch (e: RuntimeException) {
            Log.e(TAG, "text-line-span failed: ${e.message}"); Selection("", emptyList())
        }
        showSelectionResult(sel, emptyMsg)
    }

    /** Render the caught selection's boxes and offer the action sheet — shared by the bbox and
     *  line-span selection paths (engine thread). A drag is a *selection*, never an auto-lookup. */
    private fun showSelectionResult(sel: Selection, emptyMsg: String) {
        host.diag { "DIAG text selection: '${sel.text.take(60)}' boxes=${sel.boxes.size}" }
        host.clearFirmwareInk() // wipe the firmware ink the select gesture left behind
        host.renderAndBlit()
        if (sel.isEmpty) {
            host.refreshPanel()
            runOnUiThread { Toast.makeText(activity, emptyMsg, Toast.LENGTH_SHORT).show() }
            return
        }
        drawTextSelectionBoxes(sel.boxes) // show what was caught, then offer actions
        host.refreshPanel()
        runOnUiThread { showTextSelectionActions(sel) }
    }

    /** Shade the selected printed-text boxes over the cached page (so the user sees the catch). */
    private fun drawTextSelectionBoxes(boxes: List<SelBox>) {
        val fill = Paint().apply { color = Color.argb(60, 0, 0, 0); style = Paint.Style.FILL }
        host.overlayOnPage { canvas ->
            for (b in boxes) {
                canvas.drawRect(host.nToVx(b.x0), host.nToVy(b.y0), host.nToVx(b.x1), host.nToVy(b.y1), fill)
            }
        }
    }

    /** Action sheet for circled printed text: Define · Copy · Highlight (UI thread). */
    private fun showTextSelectionActions(sel: Selection) {
        val snippet = sel.text.trim().replace(Regex("\\s+"), " ")
        // Define is a per-word action — it makes no sense for a multi-line selection, so a multi-line
        // catch (more than one line box) offers only Copy + Highlight.
        val items = if (sel.boxes.size > 1) arrayOf("Copy", "Highlight", "Add to Digest")
        else arrayOf("Define", "Copy", "Highlight", "Add to Digest")
        AlertDialog.Builder(activity, R.style.InkDialog)
            .setTitle(if (snippet.length > 42) snippet.take(42) + "…" else snippet)
            .setItems(items) { _, which ->
                when (items[which]) {
                    "Define" -> host.defineSelectionText(snippet)
                    "Copy" -> copyTextToClipboard(snippet)
                    "Highlight" -> host.engineExecute { highlightTextBoxes(sel) }
                    "Add to Digest" -> host.addDigestText(host.currentPage, sel.text, sel.boundsNorm())
                }
            }
            // Any dismissal (action chosen or cancelled) clears the box overlay; a Highlight redraws
            // it with the real annotation, a Define opens the dict card over the cleared page.
            .setOnDismissListener { host.engineExecute { host.repaintPanel() } }
            .show()
    }

    /** Copy printed-text selection to the system clipboard. */
    private fun copyTextToClipboard(text: String) {
        val cm = activity.getSystemService(android.content.Context.CLIPBOARD_SERVICE) as android.content.ClipboardManager
        cm.setPrimaryClip(android.content.ClipData.newPlainText("inkread", text))
        Toast.makeText(activity, "Copied", Toast.LENGTH_SHORT).show()
    }

    /**
     * Highlight circled printed text by laying one translucent highlighter stroke across each text
     * box (engine thread) — reusing the ink highlighter's persistence + PDF export path, so no new
     * annotation subsystem is needed. The band width matches the line height; colour follows the
     * highlighter's current swatch.
     */
    private fun highlightTextBoxes(sel: Selection) {
        if (host.docHandle == 0L || sel.boxes.isEmpty()) return
        val color = host.highlightColor
        try {
            for (b in sel.boxes) {
                val midY = (b.y0 + b.y1) / 2f
                val widthNorm = (b.y1 - b.y0) * host.viewH / host.viewW.coerceAtLeast(1) // line height as a page-space width
                NativeBridge.nativeInkBeginStroke(host.docHandle, ReaderActivity.CORE_TOOL_HIGHLIGHTER, color, widthNorm, System.currentTimeMillis())
                NativeBridge.nativeInkAddPoint(host.docHandle, b.x0, midY, 1.0f, Float.NaN, Float.NaN, 0)
                NativeBridge.nativeInkAddPoint(host.docHandle, b.x1, midY, 1.0f, Float.NaN, Float.NaN, 0)
                NativeBridge.nativeInkEndStroke(host.docHandle)
            }
            host.scheduleInkFlush() // deferred autosave: persist the baked bands on the trailing debounce
            host.diag { "DIAG highlighted ${sel.boxes.size} text boxes" }
        } catch (e: RuntimeException) {
            Log.e(TAG, "text highlight failed: ${e.message}")
        }
        host.clearFirmwareInk(); host.repaintPanel()
    }

    /** Adopt `ids` as the selection, refresh the box, and show/update the selection toolbar (engine). */
    private fun setSelection(ids: IntArray) {
        selectedIds = ids
        selectionBounds = if (ids.isEmpty()) FloatArray(0) else try {
            NativeBridge.nativeInkSelectionBounds(host.docHandle, ids)
        } catch (e: RuntimeException) {
            FloatArray(0)
        }
        host.clearFirmwareInk() // wipe the firmware ink left by drawing the lasso loop
        host.repaintPanel()
        updateLassoHint() // hide the hint once something is selected; re-show if selection emptied
        runOnUiThread {
            if (selectedIds.isEmpty()) {
                host.dismissSelectionToolbar()
                if (host.activeTool == Tool.LASSO) {
                    Toast.makeText(activity, "Nothing selected — circle around your writing", Toast.LENGTH_SHORT).show()
                }
            } else {
                showSelectionToolbar()
            }
        }
    }

    /** Position the selection toolbar over the selection's pixel bounds (UI thread). */
    private fun showSelectionToolbar() {
        val b = selectionBounds
        if (b.size != 4) return
        val rect = RectF(host.nToVx(b[0]), host.nToVy(b[1]), host.nToVx(b[2]), host.nToVy(b[3]))
        val canPaste = try { NativeBridge.nativeInkHasClipboard(host.docHandle) } catch (e: RuntimeException) { false }
        host.showSelectionToolbar(rect, canPaste)
    }

    /** Apply a drag-move of the selection by a view-px delta (engine thread + autosave). */
    private fun applySelectionMove(dxPx: Float, dyPx: Float) {
        val ids = selectedIds
        if (ids.isEmpty() || host.viewW == 0 || host.viewH == 0) return
        val dx = dxPx / (host.viewW * host.zoom); val dy = dyPx / (host.viewH * host.zoom)
        host.engineExecute {
            val changed = try {
                NativeBridge.nativeInkMoveSelection(host.docHandle, ids, dx, dy)
            } catch (e: RuntimeException) {
                Log.e(TAG, "move failed: ${e.message}"); false
            }
            if (changed) setSelection(ids) // recompute bounds + re-show toolbar at the new spot
        }
    }

    /** Handle a tap on the floating selection toolbar (UI thread → engine). */
    fun onSelectionAction(action: SelAction) {
        val ids = selectedIds
        when (action) {
            SelAction.DONE -> clearSelection()
            SelAction.SELECT_ALL -> host.engineExecute {
                val all = try { NativeBridge.nativeInkSelectAll(host.docHandle) } catch (e: RuntimeException) { IntArray(0) }
                setSelection(all)
            }
            SelAction.DELETE -> if (ids.isNotEmpty()) host.engineExecute {
                try { NativeBridge.nativeInkDeleteSelection(host.docHandle, ids) } catch (e: RuntimeException) {}
                clearSelectionAndRender()
            }
            SelAction.CUT -> if (ids.isNotEmpty()) host.engineExecute {
                try { NativeBridge.nativeInkCutSelection(host.docHandle, ids) } catch (e: RuntimeException) {}
                clearSelectionAndRender()
            }
            SelAction.COPY -> if (ids.isNotEmpty()) host.engineExecute {
                try { NativeBridge.nativeInkCopySelection(host.docHandle, ids) } catch (e: RuntimeException) {}
                runOnUiThread { showSelectionToolbar() } // refresh Paste-enabled state
            }
            SelAction.PASTE -> host.engineExecute {
                val newIds = try { NativeBridge.nativeInkPaste(host.docHandle, PASTE_OFFSET, PASTE_OFFSET) } catch (e: RuntimeException) { IntArray(0) }
                if (newIds.isNotEmpty()) setSelection(newIds) else runOnUiThread { showSelectionToolbar() }
            }
            // Save the PDF text under the selection into the Supernote Digest; keep the selection up.
            SelAction.DIGEST -> if (ids.isNotEmpty()) host.addDigest(host.currentPage, selectionBounds.copyOf())
        }
    }

    /** Undo the last ink edit (from the tool pill). Global — refreshes any active selection too. */
    fun inkUndo() = host.engineExecute {
        try { NativeBridge.nativeInkUndo(host.docHandle); host.scheduleInkFlush() } catch (e: RuntimeException) {}
        refreshSelectionAfterHistory()
    }

    /** Redo the last undone ink edit (from the tool pill). */
    fun inkRedo() = host.engineExecute {
        try { NativeBridge.nativeInkRedo(host.docHandle); host.scheduleInkFlush() } catch (e: RuntimeException) {}
        refreshSelectionAfterHistory()
    }

    /** After undo/redo, the selected strokes may have changed; re-render and re-anchor the toolbar. */
    private fun refreshSelectionAfterHistory() {
        if (selectedIds.isEmpty()) { clearSelectionAndRender(); return }
        setSelection(selectedIds)
    }

    /** Clear the selection (UI-triggered), then re-render to drop the box (engine). */
    private fun clearSelection() {
        host.engineExecute { clearSelectionAndRender() }
    }

    /** Drop the selection + toolbar and re-render the page (engine thread). */
    private fun clearSelectionAndRender() {
        selectedIds = IntArray(0)
        selectionBounds = FloatArray(0)
        host.repaintPanel()
        updateLassoHint() // re-show the hint if still on the Lasso tool with nothing selected
        runOnUiThread { host.dismissSelectionToolbar() }
    }

    // ===== Define-tool text selection (RR12 / ADR-INKREAD-0009 D4) =====

    /** Accumulate the selection stroke; finalize on UP (or a debounced pause if UP is swallowed). */
    fun captureSelection(e: MotionEvent) {
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
                host.drawLivePath(selBuf, lassoPaint) // dashed select line (firmware EMR ink suppressed)
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
        mainHandler.postDelayed(selectionFinalize, ReaderActivity.STROKE_PAUSE_MS)
    }

    /** Decide tap vs. drag and dispatch the lookup; stays in the (sticky) Define tool (UI thread). */
    private fun finalizeSelection() {
        if (selBuf.size < 2) { selBuf.clear(); return }
        val pts = selBuf.toFloatArray()
        selBuf.clear()
        val w = host.surfaceW.toFloat()
        val h = host.surfaceH.toFloat()
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
                val sx = host.vToNx(pts[0]); val sy = host.vToNy(pts[1])
                val ex = host.vToNx(pts[pts.size - 2]); val ey = host.vToNy(pts[pts.size - 1])
                host.engineExecute { presentLineSpanSelection(sx, sy, ex, ey, "No text under the selection") }
            } else {
                // Single-line drag: the dragged horizontal span on that one line.
                val r = floatArrayOf(host.vToNx(minX), host.vToNy(minY), host.vToNx(maxX), host.vToNy(maxY))
                host.engineExecute { presentTextSelection(r[0], r[1], r[2], r[3], "No text under the selection") }
            }
        } else {
            // A single still tap is a word lookup (the dict card).
            val page = host.currentPage
            host.engineExecute { host.defineWord(page, host.vToNx(pts[0]), host.vToNy(pts[1])) }
            // Wipe the firmware ink the define gesture left behind (it never becomes an annotation).
            host.engineExecute { host.clearFirmwareInk(); host.repaintPanel() }
        }
    }

    companion object {
        private const val TAG = "LassoController"

        /** Drag vertical span (frac of height) above which it's a multi-line → line-span select. */
        const val MULTILINE_DRAG_FRAC = 0.045f

        /** Lasso: start-to-lift distance (normalized) above which the gesture is an open drag
         *  (vs a closed loop). */
        const val OPEN_DRAG_FRAC = 0.08f

        /** Normalized offset so a paste lands just beside the source. */
        const val PASTE_OFFSET = 0.03f
    }
}
