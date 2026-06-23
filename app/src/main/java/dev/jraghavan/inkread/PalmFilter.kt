package dev.jraghavan.inkread

/**
 * Pure palm / stray-touch decision (RR19 palm rejection), extracted from [ReaderActivity] so it can
 * be unit-tested on the host JVM with the real contact metrics captured on-device (no emulator can
 * simulate the Supernote's EMR pen + EPD, so device-shaped logic is validated here instead).
 *
 * A **finger** touch is treated as a resting palm — and therefore ignored for navigation and the
 * word-lookup gestures — when ANY of these hold:
 *  - it is multi-pointer (a second contact while writing), or
 *  - the EMR pen is currently **in proximity** (hovering), or
 *  - it landed within [palmRejectMs] of the last pen event (touch OR hover), or
 *  - a stroke is in progress, or
 *  - its contact major is a large fraction ([touchMajorFrac]) of the panel height.
 *
 * The "at the outset" gap this addresses: palm rejection used to be bootstrapped only by *recent*
 * pen activity, so the very first palm that lands BEFORE the pen has hovered/touched leaked through
 * (the panel reports a fairly small touch-major even for a palm). Proximity (hover) tracking plus a
 * tightened size threshold catch that first contact without waiting for the pen to engage.
 */
object PalmFilter {

    /**
     * @param isStylusTool   the touch's tool is a stylus/eraser — never a palm.
     * @param pointerCount   number of active pointers in the event.
     * @param penHovering    the EMR pen is currently in proximity (hover enter/move seen, no exit).
     * @param strokeInProgress an ink stroke is being captured right now.
     * @param msSinceStylus  elapsed ms since the last pen touch/hover event.
     * @param palmRejectMs   reject a finger within this long of pen activity.
     * @param touchMajorPx   the contact's major axis in pixels (panel-reported).
     * @param viewHeightPx   panel height in pixels (0 ⇒ unknown, size test skipped).
     * @param touchMajorFrac contact-major ≥ this fraction of [viewHeightPx] ⇒ a palm.
     */
    fun isPalm(
        isStylusTool: Boolean,
        pointerCount: Int,
        penHovering: Boolean,
        strokeInProgress: Boolean,
        msSinceStylus: Long,
        palmRejectMs: Long,
        touchMajorPx: Float,
        viewHeightPx: Int,
        touchMajorFrac: Float,
    ): Boolean {
        if (isStylusTool) return false
        if (pointerCount > 1) return true
        if (penHovering) return true
        if (msSinceStylus <= palmRejectMs) return true
        if (strokeInProgress) return true
        return viewHeightPx > 0 && touchMajorPx >= viewHeightPx * touchMajorFrac
    }
}
