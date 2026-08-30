package dev.jraghavan.inkread.eink

import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.View
import dev.jraghavan.inkread.DeviceCapabilities
import dev.jraghavan.inkread.RefreshCommand
import dev.jraghavan.inkread.RefreshIntent

/**
 * The Supernote (RK3566 EBC) refresh adapter (RR15).
 *
 * Maps each vendor-neutral [RefreshIntent] onto the panel mechanism this device actually exposes.
 * This is the ONLY vendor-named code in the project (IR-7); the core stays agnostic, and
 * `scripts/check-vendor-neutral.sh` keeps it that way.
 *
 * ## What this adapter does
 * It refreshes the panel, on every page. The RK3566 is a **full-only** panel and a blit to our
 * Surface does not repaint the EPD by itself — only the first window draw is auto-refreshed — so
 * every subsequent page explicitly asks for a frame via
 * `android.os.EinkManager.sendOneFullFrame()`, reached by reflection through the system `eink`
 * service. It also releases the firmware's global and stylus gesture grabs, without which the
 * vendor gesture layer eats touch events before the reader's window sees them. See the panel
 * mechanism note further down for the details.
 *
 * ## Full-screen quirk (RR2-FR4)
 * A refresh repaints the WHOLE screen regardless of the rect, so `refreshRegion` collapses to the
 * same full frame as `refreshFullScreen`. The [EbcMode] mapping in [mapIntent] is therefore
 * *descriptive* rather than load-bearing: it records what each intent would ask for on a panel that
 * let an app choose, and is staged for a device that does. [waitForLast] is genuinely a no-op —
 * this panel exposes no completion marker.
 *
 * ## What it deliberately does not do (RR15-FR3, settled by the RR19-FR4b spike)
 * Waveform selection and dirty-rect refresh are **not reachable from a sideloaded app** on this
 * SoC: `EinkManager.setMode` is a no-op for an untrusted window, `com.ratta.DrawService` returns a
 * null binder, and the `/dev/ebc` write path is unproven and reboot-risky. That is why
 * [capabilities] advertises `einkFull = false` — an honest declaration, not a placeholder, and the
 * core's policy degrades on it correctly. `docs/EINK-LIMITS.md` states the ceiling plainly.
 *
 * Live pen ink is likewise not this adapter's job and never will be: the firmware's own overlay
 * draws wet ink at sub-frame latency and inkread feeds it stroke geometry, keeping the Rust stroke
 * model underneath (spec amendment S-2, ADR-INKREAD-0004). Nothing an app-side refresh path could
 * add would beat it.
 */
class SupernoteEinkAdapter : EinkAdapter {

    /** EBC waveform modes (the vendor mechanism the core never names). */
    private enum class EbcMode { GC16, GL16, A2, DU, INIT }

    /** The panel-owning view; its context resolves the system "eink" service. */
    @Volatile private var view: View? = null

    /** Coalesces bursts of [refreshFull] into one panel frame (see [refreshFull]). */
    private val refreshHandler = Handler(Looper.getMainLooper())
    private val coalescedFrame = Runnable { sendOneFullFrame() }

    override fun capabilities(): DeviceCapabilities = DeviceCapabilities.supernoteBaseline()

    override fun attachView(view: View?) {
        this.view = view
        if (view == null) refreshHandler.removeCallbacks(coalescedFrame) // no panel to push to
    }

    /**
     * Request a full-screen panel refresh, **coalesced** (RR15 power). A GC16 full frame is the
     * most expensive operation on the EPD, and the shell fires `refreshFull()` after every UI-chrome
     * change (toolbar, palette, dialog dismiss, long-press lookup) — often several back-to-back. A
     * trailing-edge debounce collapses each burst into a single frame: every call reschedules the one
     * pending frame a short window out, so the last blit in the burst is what reaches the panel, and
     * accidental double-refreshes cost nothing. Policy-driven page turns go through
     * [execute]/[executeUpdate], not here, so their latency is unchanged.
     */
    override fun refreshFull() {
        refreshHandler.removeCallbacks(coalescedFrame)
        refreshHandler.postDelayed(coalescedFrame, COALESCE_WINDOW_MS)
    }

    override fun execute(command: RefreshCommand) {
        when (command) {
            is RefreshCommand.Update -> executeUpdate(command)
            RefreshCommand.WaitForLast -> waitForLast()
            RefreshCommand.EnterFastMode -> { /* advisory; no persistent fast region on EBC */ }
            RefreshCommand.LeaveFastMode -> { /* advisory */ }
        }
    }

    private fun executeUpdate(u: RefreshCommand.Update) {
        val mode = mapIntent(u.intent)
        // Rockchip quirk (RR2-FR4): FULL/Flash* ignore the rect → full-screen.
        val fullScreen = when (u.intent) {
            RefreshIntent.FULL, RefreshIntent.FLASH_UI, RefreshIntent.FLASH_PARTIAL -> true
            else -> false
        }
        if (fullScreen) {
            refreshFullScreen(mode)
        } else {
            refreshRegion(mode, u.x, u.y, u.w, u.h)
        }
    }

    /** Intent → EBC mode (RR15). Unsupported fast/regal degrade to the nearest mode. */
    private fun mapIntent(intent: RefreshIntent): EbcMode = when (intent) {
        RefreshIntent.FULL -> EbcMode.GC16          // high-fidelity flashing clear
        RefreshIntent.PARTIAL -> EbcMode.GL16        // anti-ghost content refresh
        RefreshIntent.FAST -> EbcMode.A2             // 1-bit fast (scroll/keyboard)
        RefreshIntent.UI -> EbcMode.GL16             // light UI update
        RefreshIntent.FLASH_UI -> EbcMode.GC16       // flashing UI clear
        RefreshIntent.FLASH_PARTIAL -> EbcMode.GC16  // flashing partial clear
    }

    // ---- panel mechanism ----
    //
    // The Supernote (RK3566) is a **full-only** panel: a blit to our Surface does NOT refresh
    // the EPD on its own — only the very first window draw is auto-refreshed by the firmware.
    // Every subsequent page must explicitly ask the panel to repaint. The working mechanism
    // (used by KOReader on this SoC) is `android.os.EinkManager.sendOneFullFrame()` reached
    // via the system "eink" service on the view's context. There is no usable partial/region
    // path from an untrusted window, so both full and region intents collapse to one full
    // frame here (RR2-FR4 Rockchip quirk). This is the only vendor-specific call (IR-7).

    private fun refreshFullScreen(mode: EbcMode) {
        Log.d(TAG, "panel full-screen refresh: $mode")
        sendOneFullFrame()
    }

    private fun refreshRegion(mode: EbcMode, x: Int, y: Int, w: Int, h: Int) {
        // Full-only panel: a region request still triggers a whole-screen frame.
        Log.d(TAG, "panel region refresh -> full: $mode @($x,$y) ${w}x$h")
        sendOneFullFrame()
    }

    private fun waitForLast() {
        // No exposed completion marker on this panel; the full-frame call is synchronous enough.
        Log.d(TAG, "wait-for-last (no-op on full-only panel)")
    }

    /**
     * Ask the panel to push one full frame from the current window contents to the EPD, via
     * `android.os.EinkManager.sendOneFullFrame()` (system "eink" service). Reflection because
     * the class is a hidden framework API; failures are logged, never thrown (RR21-FR3).
     */
    private fun sendOneFullFrame(): Boolean {
        val v = view ?: run {
            Log.w(TAG, "no view attached; cannot refresh panel")
            return false
        }
        return try {
            val einkManagerClass = Class.forName("android.os.EinkManager")
            val einkManager = v.context.getSystemService("eink")
                ?: run {
                    Log.w(TAG, "no 'eink' system service on this device")
                    return false
                }
            einkManagerClass.getDeclaredMethod("sendOneFullFrame").invoke(einkManager)
            true
        } catch (e: Exception) {
            Log.e(TAG, "sendOneFullFrame failed: $e")
            false
        }
    }

    override fun setSystemGesturesEnabled(enabled: Boolean) {
        // The Supernote's system gesture service (GMX) otherwise intercepts touch-up events
        // before the app's window sees them, breaking tap detection. Releasing global + stylus
        // gestures hands full touch streams to the reader. Reflection; never throws (RR21-FR3).
        val v = view ?: return
        val mgr = v.context.getSystemService("eink") ?: return
        val cls = Class.forName("android.os.EinkManager")
        for (method in arrayOf("setGlobalGuestureEnabled", "setStylusGuestureEnabled")) {
            runCatching {
                cls.getDeclaredMethod(method, Boolean::class.javaPrimitiveType)
                    .invoke(mgr, enabled)
            }.onFailure { Log.w(TAG, "$method($enabled) failed: $it") }
        }
        Log.i(TAG, "system gestures enabled=$enabled")
    }

    private companion object {
        const val TAG = "SupernoteEinkAdapter"

        /** Trailing-edge debounce window for [refreshFull] coalescing — small next to a GC16 full
         *  refresh (hundreds of ms), so a single refresh feels immediate while bursts collapse. */
        const val COALESCE_WINDOW_MS = 32L
    }
}
