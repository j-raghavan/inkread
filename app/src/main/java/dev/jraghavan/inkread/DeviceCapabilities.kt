package dev.jraghavan.inkread

/**
 * The capability flags an e-ink adapter advertises to the core (RR2-FR2).
 *
 * Field ORDER is load-bearing: [flags] must list the flags in the same declaration order as
 * `DeviceCapabilities` in `device-eink/src/capabilities.rs`, because the Fork-3 caps codec
 * serializes them positionally. Keep the two in sync.
 */
data class DeviceCapabilities(
    val eink: Boolean,
    val einkFull: Boolean,
    val regal: Boolean,
    val fastMode: Boolean,
    val regionalUpdate: Boolean,
    val hwInvert: Boolean,
    val hwDither: Boolean,
    val kaleidoWfm: Boolean,
    val colorScreen: Boolean,
    val swipeAnimation: Boolean,
    val penLowLatency: Boolean,
    val needsRefreshAfterResume: Boolean,
) {
    /** Flags in declaration order (= caps serialization order, Fork 3). */
    fun flags(): BooleanArray = booleanArrayOf(
        eink, einkFull, regal, fastMode, regionalUpdate, hwInvert,
        hwDither, kaleidoWfm, colorScreen, swipeAnimation,
        penLowLatency, needsRefreshAfterResume,
    )

    companion object {
        /**
         * An ordinary backlit display — no e-ink anywhere (#220).
         *
         * `eink = false` is the whole point: the core's refresh policy exists to hide EPD ghosting
         * and flash cost, and applying it to an LCD produces full-screen refreshes and a
         * refresh-after-resume that a panel redrawing at 60Hz has no use for. `colorScreen` is the
         * other honest difference; the rest of the flags describe e-ink hardware features and are
         * meaningless without a panel to apply them to.
         */
        fun genericDisplay(): DeviceCapabilities = DeviceCapabilities(
            eink = false,
            einkFull = false,
            regal = false,
            fastMode = false,
            regionalUpdate = false,
            hwInvert = false,
            hwDither = false,
            kaleidoWfm = false,
            colorScreen = true,
            swipeAnimation = true,
            penLowLatency = false,
            needsRefreshAfterResume = false,
        )

        /**
         * The honest Supernote profile: an e-ink panel **without** refresh control
         * (`einkFull = false`), so the core's policy collapses to periodic full-screen refreshes
         * (RR2-FR2 / RR3-AC3).
         *
         * This is a settled fact about the platform, not a placeholder waiting on a spike. The
         * RR19-FR4b spike ran and concluded that waveform selection and dirty-rect refresh are
         * unreachable from a sideloaded app on this SoC — see `docs/EINK-LIMITS.md` and
         * [dev.jraghavan.inkread.eink.SupernoteEinkAdapter]. Advertising anything fuller here would
         * make the policy emit commands the adapter cannot honour.
         */
        fun supernoteBaseline(): DeviceCapabilities = DeviceCapabilities(
            eink = true,
            einkFull = false,
            regal = false,
            fastMode = false,
            regionalUpdate = false,
            hwInvert = false,
            hwDither = false,
            kaleidoWfm = false,
            colorScreen = false,
            swipeAnimation = false,
            penLowLatency = false,
            needsRefreshAfterResume = true,
        )
    }
}
