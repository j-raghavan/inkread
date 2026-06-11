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
         * The honest Supernote M0 baseline: an e-ink panel without full refresh control
         * (`einkFull = false`) — the core collapses to periodic full-screen refreshes
         * (RR2-FR2 / RR3-AC3). M0 advertises this until the RR19-FR4b spike proves the
         * fast path, at which point the adapter can advertise the fuller profile.
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
