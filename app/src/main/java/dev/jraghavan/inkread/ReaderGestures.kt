package dev.jraghavan.inkread

import kotlin.math.abs

/**
 * The reader's finger-gesture **decisions** (RR25-FR3, RR11-FR3, #49/#52/#54), extracted from
 * [ReaderActivity] in the same spirit as [PalmFilter]: the arithmetic that decides what a touch
 * *means* lives here and is unit-tested on the host, while the Activity keeps only the `MotionEvent`
 * plumbing and the side effects.
 *
 * The split matters because these rules are the ones a reader feels and nobody can test by reading:
 * how far a flick must travel before it turns a page, how much more horizontal than vertical it must
 * be, where the tap thirds fall, and — the subtle one — that a *stricter* pair of thresholds can
 * rescue a page-turn that palm rejection wrongly latched. Getting any of those slightly wrong
 * produces a reader that feels unreliable rather than one that visibly breaks, which is precisely
 * the failure a test catches and a device session does not.
 */
object ReaderGestures {

    /** Where along the width a tap landed. The page is split into equal thirds (RR25-FR3). */
    enum class Zone {
        /** Left third — previous page. */
        PREV,

        /** Middle third — the menu, or a double-tap zoom. */
        CENTRE,

        /** Right third — next page. */
        NEXT,
    }

    /** The third `x` falls in, for a `viewW`-wide panel. A zero width reports [Zone.CENTRE]. */
    fun zoneFor(x: Float, viewW: Float): Zone {
        if (viewW <= 0f) return Zone.CENTRE
        val third = viewW / 3f
        return when {
            x < third -> Zone.PREV
            x > 2f * third -> Zone.NEXT
            else -> Zone.CENTRE
        }
    }

    /**
     * Whether `(x, y)` is in the bookmark dog-ear's corner (the top [zoneH] of the rightmost
     * [zoneW], as panel fractions so it holds on any device).
     */
    fun isBookmarkCorner(
        x: Float,
        y: Float,
        viewW: Float,
        viewH: Float,
        zoneW: Float = ReaderActivity.BOOKMARK_ZONE_W,
        zoneH: Float = ReaderActivity.BOOKMARK_ZONE_H,
    ): Boolean = viewW > 0f && viewH > 0f && x > viewW * (1f - zoneW) && y < viewH * zoneH

    /**
     * The page delta a finger travel of `(dx, dy)` means, or `null` if it is not a page-turning
     * swipe. Swiping **left** goes forward, matching the direction the page moves.
     *
     * Two threshold pairs, and the difference between them is the whole point:
     *
     * - [SWIPE_FRAC] / [SWIPE_RATIO] — an ordinary flick. Deliberately short (about a tenth of the
     *   panel) because a page turn should not require a screen-width drag.
     * - [PALM_SWIPE_FRAC] / [PALM_SWIPE_RATIO] — used only to *rescue* a travel that palm rejection
     *   already latched (#49). A flat fingertip can read palm-sized, so a clearly deliberate swipe
     *   should still turn the page; but a genuine resting palm barely moves, so these are stricter
     *   on both axes. Loosening them would reintroduce "my hand turned the page while I was
     *   writing", which is the bug the palm latch exists to prevent.
     */
    fun swipeDelta(dx: Float, dy: Float, viewW: Float, strict: Boolean = false): Int? {
        val frac = if (strict) PALM_SWIPE_FRAC else SWIPE_FRAC
        val floor = if (strict) PALM_SWIPE_MIN_PX else SWIPE_MIN_PX
        val ratio = if (strict) PALM_SWIPE_RATIO else SWIPE_RATIO
        val minDist = (viewW * frac).coerceAtLeast(floor)
        if (abs(dx) <= minDist || abs(dx) <= abs(dy) * ratio) return null
        return if (dx < 0f) +1 else -1
    }

    /**
     * The page a net `delta` from `current` lands on, snapped to `[0, pageCount - 1]`, or `null` if
     * that is where the reader already is.
     *
     * Coalescing (RR25) is why this is a function rather than an increment: mashing the right edge
     * used to enqueue one render and one full EPD flash *per tap*, running N slow cycles serially.
     * The reader accumulates the net delta instead and issues one jump, so ten fast taps cost one or
     * two renders. `null` is the at-the-edge no-op — without it the last page re-renders on every
     * further tap, which on e-ink is a visible flash for nothing.
     */
    fun jumpTarget(current: Int, delta: Int, pageCount: Int): Int? {
        if (delta == 0) return null
        val last = pageCount.coerceAtLeast(1) - 1
        val target = (current + delta).coerceIn(0, last)
        return if (target == current) null else target
    }

    /**
     * Whether a tap at `(x, y)` continues a recent tap into a double-tap: within [doubleTapMs] and
     * [slopPx] of the previous one. Pure, so the caller owns the "last tap" state — see
     * [ReaderActivity.isCentreDoubleTap], which consumes the record so a third tap does not chain.
     */
    fun isDoubleTap(
        x: Float,
        y: Float,
        lastX: Float,
        lastY: Float,
        msSinceLast: Long,
        doubleTapMs: Long = ReaderActivity.DOUBLE_TAP_MS,
        slopPx: Float = ReaderActivity.DOUBLE_TAP_SLOP_PX,
    ): Boolean = msSinceLast <= doubleTapMs && kotlin.math.hypot(x - lastX, y - lastY) < slopPx

    // ---- thresholds ----

    /** An ordinary swipe must travel this fraction of the panel width... */
    const val SWIPE_FRAC = 0.06f

    /** ...or this many pixels, whichever is larger — a comfortable flick, not a screen-width drag. */
    const val SWIPE_MIN_PX = 90f

    /** ...and be this many times more horizontal than vertical. */
    const val SWIPE_RATIO = 1.2f

    /** A palm-latched travel must clear this much larger fraction to be rescued as a swipe (#49). */
    const val PALM_SWIPE_FRAC = 0.10f

    /** ...with this pixel floor... */
    const val PALM_SWIPE_MIN_PX = 140f

    /** ...and this much stronger horizontal bias. A resting palm barely moves; this is what keeps
     *  the rescue from re-admitting the writing hand. */
    const val PALM_SWIPE_RATIO = 2.0f
}
