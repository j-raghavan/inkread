package dev.jraghavan.inkread

/**
 * The rules for *changing* zoom (RR5-FR3), extracted from [ReaderActivity] alongside
 * [ViewTransform], which owns the map at a given zoom. This owns how you get from one zoom to the
 * next: the clamp, the snap back to fit, and the double-tap toggle.
 *
 * Small, but the two behaviours here are ones a reader notices immediately when they are wrong. The
 * deadband is what stops a page sitting at 1.003x — visually fit, but with the reader still on the
 * zoomed code path, still drawing a minimap, and still refusing centre taps. And the "leaving fit"
 * edge is the moment the minimap has to grab its thumbnail, because the fit render is on screen
 * then and not a moment later.
 */
object ZoomPolicy {

    /**
     * Anything at or below this counts as fit and snaps exactly to `1f`.
     *
     * Floating-point zoom arrives from a pinch as an arbitrary scale, so it lands near 1 without
     * ever being 1. Without the snap, `zoom > 1f` stays true forever after the first pinch and the
     * reader is permanently in its zoomed branch — minimap drawn, edge taps turning pages instead of
     * opening the menu, pan state accumulating — on a page that looks exactly like fit.
     */
    const val FIT_DEADBAND = 1.01f

    /** Whether `zoom` is fit (or close enough to be treated as it). */
    fun isFit(zoom: Float): Boolean = zoom <= 1f

    /**
     * The zoom after multiplying by `factor`, clamped to `[1, max]` and snapped to fit inside the
     * deadband. `max` is the reader's UI ceiling, which matches the core's own clamp.
     */
    fun stepped(current: Float, factor: Float, max: Float): Float {
        val next = (current * factor).coerceIn(1f, max)
        return if (next <= FIT_DEADBAND) 1f else next
    }

    /**
     * The zoom a double-tap produces: a toggle. From fit it jumps to `jumpTo`; from anywhere else it
     * returns to fit, so the same gesture always undoes itself.
     */
    fun doubleTapTarget(current: Float, jumpTo: Float, max: Float): Float =
        if (current > 1f) 1f else jumpTo.coerceIn(1f, max)

    /**
     * Whether this change crosses *out of* fit — the instant the minimap must capture its
     * thumbnail, while the fit render is still the one on screen.
     */
    fun leavingFit(from: Float, to: Float): Boolean = from <= 1f && to > 1f

    /** Whether a zoom change is a no-op, so the caller can skip a render and its EPD flash. */
    fun isNoChange(from: Float, to: Float): Boolean = from == to
}
