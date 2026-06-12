package dev.jraghavan.inkread.eink

import android.util.Log
import android.view.View
import dev.jraghavan.inkread.DeviceCapabilities
import dev.jraghavan.inkread.RefreshCommand
import dev.jraghavan.inkread.RefreshIntent

/**
 * The Supernote (RK3566 EBC) refresh adapter (RR15).
 *
 * Maps each vendor-neutral [RefreshIntent] to an EBC waveform mode. This is the ONLY
 * vendor-named code in the project (IR-7); the core stays agnostic.
 *
 * M0 STATUS (device-verified 2026-06-11): the reader **displays on the panel via the device's
 * automatic refresh** — a PDF rendered on the Supernote with these methods as no-ops. So explicit
 * waveform control is intentionally not exercised at M0 (see the execution-path note below).
 *
 * ## Rockchip full-screen quirk (RR2-FR4)
 * On the RK3566 EBC a FULL/Flash refresh refreshes the WHOLE screen regardless of the rect
 * (coordinates ignored). So [RefreshIntent.FULL]/`FLASH_*` are treated as full-screen here;
 * only PARTIAL/FAST honor the per-update rect.
 *
 * ## Execution path (RR15-FR3 — settled by the RR19-FR4b spike)
 * The spike confirmed the **reader rides the device's automatic e-ink refresh**: a sideloaded
 * app draws to its Surface and the Supernote firmware's system-level einkhwc refreshes the
 * window — no app-side waveform call needed. (KOReader runs on the Supernote exactly this way:
 * it detects the device as non-eink and issues *zero* e-ink calls.) Explicit waveform control
 * from a sideload is **system/privilege-gated**: `android.os.EinkManager` is reachable but its
 * `setMode`/refresh calls are no-ops for an untrusted window; `com.ratta.DrawService` returns a
 * null binder; `/dev/ebc` opens (Ratta `ht_eink` 'HT' ioctl family — *not* stock ebc-dev) and
 * the FB is mmap-readable, but the write/refresh path is unproven and reboot-risky. Therefore
 * M0 advertises [DeviceCapabilities.supernoteBaseline] (`einkFull = false`) and the refresh
 * methods below are intentional **no-ops** — the device drives the panel. The low-latency A2
 * pen path (explicit refresh control) is deferred to **M1c handwriting**, where it needs the
 * privileged HandWriteClient route (RR19-FR3b) or a best-effort auto-path fallback. The
 * EBC-mode mapping below is staged for that future, vendor-named work.
 */
class SupernoteEinkAdapter : EinkAdapter {

    /** EBC waveform modes (the vendor mechanism the core never names). */
    private enum class EbcMode { GC16, GL16, A2, DU, INIT }

    /** The panel-owning view; its context resolves the system "eink" service. */
    @Volatile private var view: View? = null

    override fun capabilities(): DeviceCapabilities = DeviceCapabilities.supernoteBaseline()

    override fun attachView(view: View?) {
        this.view = view
    }

    override fun refreshFull() {
        sendOneFullFrame()
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

    private companion object {
        const val TAG = "SupernoteEinkAdapter"
    }
}
