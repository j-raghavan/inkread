package dev.jraghavan.inkread.eink

import android.util.Log
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

    override fun capabilities(): DeviceCapabilities = DeviceCapabilities.supernoteBaseline()

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

    // ---- panel mechanism (stubbed for M0; wired to einkhwc/`/dev/ebc` post-spike) ----

    private fun refreshFullScreen(mode: EbcMode) {
        Log.d(TAG, "EBC full-screen refresh: $mode (Rockchip quirk)")
        // TODO(device): cooperate with einkhwc / ioctl `/dev/ebc` (RR15-FR3). DEVICE-UNVERIFIED.
    }

    private fun refreshRegion(mode: EbcMode, x: Int, y: Int, w: Int, h: Int) {
        Log.d(TAG, "EBC region refresh: $mode @($x,$y) ${w}x$h")
        // TODO(device): regional partial update (RR15-FR3). DEVICE-UNVERIFIED.
    }

    private fun waitForLast() {
        Log.d(TAG, "EBC wait-for-last (sync barrier before flash)")
        // TODO(device): block on the prior update marker (RR3-FR8). DEVICE-UNVERIFIED.
    }

    private companion object {
        const val TAG = "SupernoteEinkAdapter"
    }
}
