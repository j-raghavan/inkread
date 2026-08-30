package dev.jraghavan.inkread

import android.graphics.Canvas
import android.graphics.Color
import android.graphics.DashPathEffect
import android.graphics.Paint
import android.graphics.Path

/**
 * The chrome painted **onto the rendered page bitmap**, before it is blitted (RR16, RR11,
 * ADR-INKREAD-0010): the bookmark ribbon, the lasso selection box, and the search-hit highlight.
 *
 * These belong together because of *where* they are drawn rather than what they mean. On this panel
 * an Android overlay view only refreshes when it is added to the window — moving one in place leaves
 * it invisible — so anything that has to survive a page turn is drawn on the SurfaceView's own
 * canvas instead. That constraint, not a UI taxonomy, is what puts a bookmark ribbon and a selection
 * box in the same class.
 *
 * Everything here is a pure draw: it reads the state it is handed, touches no reader state, and the
 * geometry is exposed as plain values so it can be tested on the host without a `Canvas`.
 */
class PageOverlays(private val host: Host) {

    /** The page↔view transform the overlays draw through. */
    interface Host {
        /** Page-normalized x → view pixels, at the current zoom and pan. */
        fun nToVx(nx: Float): Float

        /** ...and y. */
        fun nToVy(ny: Float): Float

        /** Viewport width in pixels. */
        val viewW: Int
    }

    // Lazy for the same reason as MinimapController's: `android.graphics.Paint` is stubbed in host
    // unit tests, and the geometry below is worth testing without a device.
    private val bookmarkPaint by lazy { Paint().apply { color = Color.BLACK; style = Paint.Style.FILL; isAntiAlias = true } }
    private val bookmarkOutlinePaint by lazy { Paint().apply { color = Color.parseColor("#9E9E9E"); style = Paint.Style.STROKE; strokeWidth = 2f; isAntiAlias = true } }

    /** White halo drawn under the ribbon so it stays visible over a dark page region (e.g. a black
     *  title band) — without it a black/gray ribbon vanishes on dark backgrounds. */
    private val bookmarkHaloPaint by lazy { Paint().apply { color = Color.WHITE; style = Paint.Style.STROKE; strokeWidth = 5f; isAntiAlias = true } }

    /** Dashed box around the active lasso selection (ADR-INKREAD-0010). */
    private val selectionPaint by lazy {
        Paint().apply {
            color = Color.BLACK
            style = Paint.Style.STROKE
            strokeWidth = 2f
            isAntiAlias = true
            pathEffect = DashPathEffect(floatArrayOf(12f, 8f), 0f)
        }
    }

    /** Filled square handles at the selection box corners (NeoReader frame 132). */
    private val selectionHandlePaint by lazy { Paint().apply { color = Color.BLACK; style = Paint.Style.FILL; isAntiAlias = true } }

    /** Search-hit highlight: a light translucent fill so the matched text stays readable on e-ink. */
    private val searchFillPaint by lazy { Paint().apply { color = Color.parseColor("#33000000"); style = Paint.Style.FILL; isAntiAlias = true } }

    /** A crisp outline around the active search hit (the one the reader is parked on). */
    private val searchBoxPaint by lazy { Paint().apply { color = Color.BLACK; style = Paint.Style.STROKE; strokeWidth = 2f; isAntiAlias = true } }

    /**
     * The bookmark ribbon's outline: a swallowtail pennant hanging from the top edge, near the
     * right. Returned as the five points that make the path, in order, so both the renderer and the
     * tap zone can be reasoned about — and tested — without constructing a `Path`.
     *
     * Derived entirely from [viewW] so it scales with the panel rather than being pinned to one
     * device's pixels.
     */
    class Ribbon(val left: Float, val right: Float, val length: Float, val notch: Float) {
        /** The point of the swallowtail notch, between the two tails. */
        val notchY get() = length - notch

        /** Horizontal centre — where the notch sits. */
        val centreX get() = (left + right) / 2f

        /** Ribbon width. */
        val width get() = right - left
    }

    /** Where the ribbon falls for a `viewW`-wide panel. Pure. */
    fun ribbon(viewW: Int = host.viewW): Ribbon {
        val w = viewW.toFloat()
        val rw = w * 0.035f // ribbon width
        val right = w - rw * 1.4f // inset from the right edge
        return Ribbon(left = right - rw, right = right, length = rw * 2.1f, notch = rw * 0.45f)
    }

    /**
     * Top-right **ribbon bookmark** (swallowtail): a faint outline always — that outline *is* the
     * affordance telling the reader the corner is tappable — filled solid when [marked].
     */
    fun drawBookmark(canvas: Canvas, marked: Boolean) {
        val r = ribbon()
        val path = Path().apply {
            moveTo(r.left, 0f)
            lineTo(r.right, 0f)
            lineTo(r.right, r.length)
            lineTo(r.centreX, r.notchY) // swallowtail
            lineTo(r.left, r.length)
            close()
        }
        // A white halo first so the ribbon reads on any background (e.g. a black title band).
        canvas.drawPath(path, bookmarkHaloPaint)
        canvas.drawPath(path, if (marked) bookmarkPaint else bookmarkOutlinePaint)
    }

    /** The active lasso selection's dashed bounding box + square corner handles (frame 132).
     *  [bounds] is `[x0, y0, x1, y1]`, page-normalized. */
    fun drawSelectionBox(canvas: Canvas, bounds: FloatArray) {
        if (bounds.size != 4) return
        val l = host.nToVx(bounds[0])
        val t = host.nToVy(bounds[1])
        val r = host.nToVx(bounds[2])
        val b = host.nToVy(bounds[3])
        canvas.drawRect(l, t, r, b, selectionPaint)
        val hs = SELECTION_HANDLE_PX
        for (cx in floatArrayOf(l, r)) {
            for (cy in floatArrayOf(t, b)) {
                canvas.drawRect(cx - hs, cy - hs, cx + hs, cy + hs, selectionHandlePaint)
            }
        }
    }

    /** The search hit's highlight [boxes] (a light fill + crisp outline). The active boxes for the
     *  current page come from [SearchController.highlightForPage]. */
    fun drawSearchHighlight(canvas: Canvas, boxes: List<SelBox>) {
        for (b in boxes) {
            val l = host.nToVx(b.x0)
            val t = host.nToVy(b.y0)
            val r = host.nToVx(b.x1)
            val btm = host.nToVy(b.y1)
            canvas.drawRect(l, t, r, btm, searchFillPaint)
            canvas.drawRect(l, t, r, btm, searchBoxPaint)
        }
    }

    private companion object {
        /** Half-size of the square corner handles on the selection box. */
        const val SELECTION_HANDLE_PX = 8f
    }
}
