package dev.jraghavan.inkread

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.view.MotionEvent

/**
 * The zoom minimap (#60): the top-right card that shows the whole page, marks the visible window on
 * it, and carries the −/+ buttons.
 *
 * It exists because a zoomed e-ink page loses its context — you can see a column of text and no
 * longer know where on the page you are, and there is no scrollbar and no smooth scrolling to tell
 * you. The thumbnail is that answer.
 *
 * Extracted from `ReaderActivity` (which was 2,134 lines) because it is genuinely one thing: a
 * cached thumbnail, a geometry, a draw, and a touch handler that all agree with each other. That
 * agreement is the point — [geometry] is the single source of truth for where the card is, so
 * [draw] and [onTouch] cannot disagree about where the `+` button sits, which is the classic way a
 * panel like this ends up with an unhittable control.
 *
 * The thumbnail is captured **lazily**, once, when zoom is first engaged from fit — at that moment
 * `bitmap` still holds the fit render, because a pinch only transforms it on the surface and never
 * overwrites it. Re-capturing on every page flip would cost a scaled-bitmap allocation per turn for
 * a panel that is not even drawn at zoom 1.
 */
class MinimapController(private val host: Host) {

    /** What the minimap needs from the reader. */
    interface Host {
        /** Viewport width in pixels; `0` before the surface is sized. */
        val viewW: Int

        /** Viewport height in pixels; `0` before the surface is sized. */
        val viewH: Int

        /** Current zoom (`1f` = fit). */
        val zoom: Float

        /** Current pan in `[0,1]` over the off-screen overscan. */
        val panX: Float

        /** ...and vertically. */
        val panY: Float

        /** Move the viewport to `(x, y)`, both normalized `[0,1]`. */
        fun setPan(x: Float, y: Float)

        /** Push the current zoom/pan to the core and re-render. */
        fun applyZoom()

        /** Step the zoom by [factor], as the −/+ buttons do. */
        fun zoomBy(factor: Float)

        /** Run [block] only if the e-ink preview throttle allows it right now. */
        fun throttledPreview(block: () -> Unit)

        /** A chrome dimension in px. */
        fun dpInt(v: Int): Int
    }

    // ---- palette ----
    //
    // `by lazy` rather than eager: `android.graphics.Paint` is not mocked in host unit tests, so
    // constructing these in the constructor would put the geometry and pan maths — the part worth
    // testing — behind a device. They are also only ever needed once the reader zooms.
    private val bgPaint by lazy { Paint().apply { color = Color.WHITE; style = Paint.Style.FILL; isAntiAlias = true } }
    private val cardStroke by lazy { Paint().apply { color = Color.parseColor("#BDBDBD"); style = Paint.Style.STROKE; strokeWidth = 1.5f; isAntiAlias = true } }
    private val thumbStroke by lazy { Paint().apply { color = Color.parseColor("#E0E0E0"); style = Paint.Style.STROKE; strokeWidth = 1f; isAntiAlias = true } }
    private val viewportFill by lazy { Paint().apply { color = Color.parseColor("#22000000"); style = Paint.Style.FILL; isAntiAlias = true } }
    private val viewportPaint by lazy { Paint().apply { color = Color.BLACK; style = Paint.Style.STROKE; strokeWidth = 3f; isAntiAlias = true } }
    private val glyphPaint by lazy { Paint().apply { color = Color.BLACK; style = Paint.Style.STROKE; strokeWidth = 3f; strokeCap = Paint.Cap.ROUND; isAntiAlias = true } }

    private var thumb: Bitmap? = null

    /**
     * Whether the minimap is currently consuming touches.
     *
     * The reader reads this to clear the flags when the pen takes over mid-gesture: the finger's UP
     * never arrives in that case, so without an external reset the panel would swallow the next
     * finger gesture once the pen idled.
     */
    var active = false
        private set

    private var thumbDrag = false

    /**
     * Panel geometry: the thumbnail box plus the −/+ buttons under it. Deterministic from the
     * viewport, so [draw] and [onTouch] agree by construction. `null` until the view is sized.
     *
     * Plain floats, not `RectF`. Partly because a pure value is testable on the host — `RectF` is
     * one of the Android types the unit-test SDK stubs out — and partly because it keeps the
     * drawing type in the drawing code, where [draw] builds what it needs.
     */
    class Geometry(
        val left: Float,
        val top: Float,
        val tw: Float,
        val th: Float,
        /** Top of the −/+ button row, below the thumbnail. */
        val buttonTop: Float,
        /** Height of that row. */
        val buttonH: Float,
    ) {
        /** Where the − and + buttons meet: the row's vertical split. */
        val split get() = left + tw / 2f

        /** Bottom of the button row — the card's lowest content edge. */
        val buttonBottom get() = buttonTop + buttonH

        /** Whether `(x, y)` is on the thumbnail. */
        fun inThumb(x: Float, y: Float) = x >= left && x <= left + tw && y >= top && y <= top + th

        private fun inRow(y: Float) = y >= buttonTop && y <= buttonBottom

        /** Whether `(x, y)` is on the − button. */
        fun inMinus(x: Float, y: Float) = inRow(y) && x >= left && x <= split

        /** Whether `(x, y)` is on the + button. `split` belongs to −, so the two do not overlap. */
        fun inPlus(x: Float, y: Float) = inRow(y) && x > split && x <= left + tw
    }

    /** Refresh the cached thumbnail from a fit render [src]. */
    fun updateThumb(src: Bitmap) {
        val tw = host.viewW / 5
        val th = host.viewH / 5
        if (tw < 8 || th < 8) return
        val old = thumb
        thumb = Bitmap.createScaledBitmap(src, tw, th, true)
        if (old != null && old != thumb) old.recycle()
    }

    /** Snapshot [current] as the thumbnail, but only while still at fit — see the class note on why
     *  this is the moment the fit render is still the one on screen. */
    fun captureFitThumb(current: Bitmap?) {
        if (host.zoom <= 1f) current?.let { updateThumb(it) }
    }

    /** Drop the cached thumbnail: the page it showed is no longer the page being read. */
    fun invalidateThumb() {
        thumb = null
    }

    /** Forget any in-flight touch — for when the pen takes over and the finger's UP never lands. */
    fun cancelTouch() {
        active = false
        thumbDrag = false
    }

    /** The panel's geometry, or `null` if the view is not sized yet. */
    fun geometry(): Geometry? {
        if (host.viewW == 0 || host.viewH == 0) return null
        val tw = (host.viewW / 5).toFloat()
        val th = (host.viewH / 5).toFloat()
        val m = host.dpInt(8).toFloat()
        val left = host.viewW - tw - m
        val top = m
        val barTop = top + th + host.dpInt(6)
        val barH = host.dpInt(48).toFloat()
        return Geometry(left, top, tw, th, barTop, barH)
    }

    /**
     * The top-left corner of the visible window within the thumbnail, in thumbnail-normalized
     * `[0,1]`, together with its extent. Pure, and the inverse of [panFor].
     */
    fun window(zoom: Float, panX: Float, panY: Float): FloatArray {
        val vx0 = panX * (zoom - 1f) / zoom
        val vy0 = panY * (zoom - 1f) / zoom
        return floatArrayOf(vx0, vy0, 1f / zoom)
    }

    /**
     * The pan that centres the viewport on `(tnx, tny)` — a point in thumbnail-normalized `[0,1]`.
     * Pure, so the drag maths is testable without a device.
     */
    fun panFor(zoom: Float, tnx: Float, tny: Float): FloatArray {
        if (zoom <= 1f) return floatArrayOf(0f, 0f)
        return floatArrayOf(
            ((tnx.coerceIn(0f, 1f) * zoom - 0.5f) / (zoom - 1f)).coerceIn(0f, 1f),
            ((tny.coerceIn(0f, 1f) * zoom - 0.5f) / (zoom - 1f)).coerceIn(0f, 1f),
        )
    }

    /** Draw the card: the page thumb, the visible window, and the −/+ buttons below a divider. */
    fun draw(canvas: Canvas) {
        val thumb = this.thumb ?: return
        val g = geometry() ?: return
        val pad = host.dpInt(6).toFloat()
        val rad = host.dpInt(10).toFloat()
        val cardL = g.left - pad
        val cardT = g.top - pad
        val cardR = g.left + g.tw + pad
        val cardB = g.buttonBottom + pad
        // Rounded white card + subtle border.
        canvas.drawRoundRect(cardL, cardT, cardR, cardB, rad, rad, bgPaint)
        canvas.drawRoundRect(cardL, cardT, cardR, cardB, rad, rad, cardStroke)
        // Thumbnail with a light frame.
        canvas.drawBitmap(thumb, g.left, g.top, null)
        canvas.drawRect(g.left, g.top, g.left + g.tw, g.top + g.th, thumbStroke)
        // Visible-window rectangle: translucent fill + solid border = clear "you are here".
        val w = window(host.zoom, host.panX, host.panY)
        val vl = g.left + w[0] * g.tw
        val vt = g.top + w[1] * g.th
        val vr = g.left + (w[0] + w[2]) * g.tw
        val vb = g.top + (w[1] + w[2]) * g.th
        canvas.drawRect(vl, vt, vr, vb, viewportFill)
        canvas.drawRect(vl, vt, vr, vb, viewportPaint)
        // Divider above the button row, then a vertical split between − and +.
        canvas.drawLine(cardL + pad, g.buttonTop, cardR - pad, g.buttonTop, thumbStroke)
        canvas.drawLine(g.split, g.buttonTop + host.dpInt(4), g.split, g.buttonBottom - host.dpInt(4), thumbStroke)
        // − / + glyphs.
        val r = host.dpInt(8).toFloat()
        val cy = g.buttonTop + g.buttonH / 2f
        val minusCx = g.left + g.tw / 4f
        val plusCx = g.left + g.tw * 3f / 4f
        canvas.drawLine(minusCx - r, cy, minusCx + r, cy, glyphPaint)
        canvas.drawLine(plusCx - r, cy, plusCx + r, cy, glyphPaint)
        canvas.drawLine(plusCx, cy - r, plusCx, cy + r, glyphPaint)
    }

    /** Centre the viewport on a point tapped or dragged inside the thumbnail. */
    private fun navigate(x: Float, y: Float, g: Geometry) {
        if (host.zoom <= 1f) return
        val p = panFor(host.zoom, (x - g.left) / g.tw, (y - g.top) / g.th)
        host.setPan(p[0], p[1])
    }

    /** Handle a finger touch on the panel. `true` means it consumed the event, so the page's
     *  tap/pan/long-press logic is skipped. */
    fun onTouch(e: MotionEvent): Boolean {
        if (host.zoom <= 1f) return false
        val g = geometry() ?: return false
        val x = e.x
        val y = e.y
        val inThumb = g.inThumb(x, y)
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                when {
                    g.inMinus(x, y) -> { active = true; host.zoomBy(1f / ReaderActivity.ZOOM_STEP); return true }
                    g.inPlus(x, y) -> { active = true; host.zoomBy(ReaderActivity.ZOOM_STEP); return true }
                    inThumb -> { active = true; thumbDrag = true; navigate(x, y, g); host.applyZoom(); return true }
                }
            }
            MotionEvent.ACTION_MOVE -> if (thumbDrag && inThumb) {
                navigate(x, y, g); host.throttledPreview { host.applyZoom() }; return true
            } else if (active) return true
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> if (active) {
                if (thumbDrag) host.applyZoom()
                active = false; thumbDrag = false; return true
            }
        }
        return active
    }
}
