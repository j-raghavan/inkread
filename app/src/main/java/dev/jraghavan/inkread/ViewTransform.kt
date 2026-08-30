package dev.jraghavan.inkread

/**
 * The page↔view coordinate map at a given zoom and pan (RR5-FR3), extracted from [ReaderActivity].
 *
 * `zoom == 1` is fit-to-viewport; above that the page is larger than the panel and `panX`/`panY`
 * say how far through the off-screen overscan the viewport sits, each in `[0, 1]`. At `zoom == 1`
 * both directions reduce to the plain `nx * viewW` / `x / viewW` mapping, which is why the reader
 * can use one path whether or not it is zoomed.
 *
 * Every coordinate the reader converts goes through here — ink capture, lasso hit-testing, link
 * hit-testing, selection boxes, search highlights, the minimap window. Before this was a type, each
 * consumer took four separate lambdas (`nToVx`, `nToVy`, `vToNx`, `vToNy`) wired individually
 * through its own `Host` interface, so the same four-function contract was restated five times and
 * nothing checked that the forward and inverse maps agreed.
 *
 * Immutable: the reader rebuilds one when zoom or pan changes, so a transform can never be half
 * updated while a gesture reads it.
 */
data class ViewTransform(
    /** Viewport width in pixels. */
    val viewW: Int,
    /** Viewport height in pixels. */
    val viewH: Int,
    /** `1f` = fit; larger magnifies. */
    val zoom: Float = 1f,
    /** Horizontal position through the overscan, `[0, 1]`. */
    val panX: Float = 0f,
    /** Vertical position through the overscan, `[0, 1]`. */
    val panY: Float = 0f,
) {
    /** Page-normalized x → view pixels. */
    fun nToVx(nx: Float): Float = nx * viewW * zoom - panX * viewW * (zoom - 1f)

    /** Page-normalized y → view pixels. */
    fun nToVy(ny: Float): Float = ny * viewH * zoom - panY * viewH * (zoom - 1f)

    /** View pixels → page-normalized x, clamped to the page. */
    fun vToNx(vx: Float): Float =
        if (viewW == 0 || zoom == 0f) 0f
        else ((vx + panX * viewW * (zoom - 1f)) / (viewW * zoom)).coerceIn(0f, 1f)

    /** View pixels → page-normalized y, clamped to the page. */
    fun vToNy(vy: Float): Float =
        if (viewH == 0 || zoom == 0f) 0f
        else ((vy + panY * viewH * (zoom - 1f)) / (viewH * zoom)).coerceIn(0f, 1f)

    /** An on-screen length in pixels → normalized page units at this zoom. */
    fun lenToNorm(px: Float): Float = if (viewW == 0 || zoom == 0f) 0f else px / (viewW * zoom)

    /** Off-screen overscan in pixels: how far the page extends beyond the viewport. */
    val overX: Float get() = viewW * (zoom - 1f)

    /** ...vertically. */
    val overY: Float get() = viewH * (zoom - 1f)

    /** Whether the page is magnified at all. */
    val isZoomed: Boolean get() = zoom > 1f

    /**
     * The pan after dragging by `(dx, dy)` view pixels. Dragging right moves the *viewport* left
     * over the page, so the delta subtracts. Clamped so a drag cannot push the page off its own
     * margin; a degenerate axis (no overscan) keeps its current value rather than dividing by zero.
     */
    fun panAfterDrag(dx: Float, dy: Float): Pair<Float, Float> {
        val nx = if (overX > 0f) (panX - dx / overX).coerceIn(0f, 1f) else panX
        val ny = if (overY > 0f) (panY - dy / overY).coerceIn(0f, 1f) else panY
        return nx to ny
    }

    /**
     * The pan that anchors page-normalized `(nx, ny)` under view point `(fx, fy)` — the focal maths
     * shared by pinch-end and double-tap-zoom, so a pinch and a double-tap put the same content
     * under the same finger.
     */
    fun panAnchoring(nx: Float, ny: Float, fx: Float, fy: Float): Pair<Float, Float> {
        val px = if (overX > 0f) ((nx * viewW * zoom - fx) / overX).coerceIn(0f, 1f) else 0f
        val py = if (overY > 0f) ((ny * viewH * zoom - fy) / overY).coerceIn(0f, 1f) else 0f
        return px to py
    }

    /** The same transform at a new zoom, keeping the pan. */
    fun withZoom(newZoom: Float): ViewTransform = copy(zoom = newZoom)

    /** The same transform at a new pan. */
    fun withPan(x: Float, y: Float): ViewTransform = copy(panX = x, panY = y)
}
