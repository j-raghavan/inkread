package dev.jraghavan.inkread.eink

import android.content.Context
import dev.jraghavan.inkread.DeviceCapabilities
import dev.jraghavan.inkread.RefreshCommand

/**
 * The adapter for a device with an ordinary backlit display (#220).
 *
 * Every method is a no-op because there is nothing to do: an LCD redraws itself. The value is not in
 * what it does but in what it *advertises* — `eink = false`, so the core stops applying a refresh
 * policy written for a panel that ghosts.
 */
class LcdAdapter : EinkAdapter {
    override fun capabilities(): DeviceCapabilities = DeviceCapabilities.genericDisplay()

    override fun execute(command: RefreshCommand) {
        // Nothing to execute: with `eink = false` the core emits no refresh commands, and a stray
        // one would still describe work an LCD does not need.
    }
}

/**
 * Picks the display adapter for the device this build is running on (#220).
 *
 * The reader previously constructed [SupernoteEinkAdapter] unconditionally, so any other device was
 * told it had an e-ink panel and got the ghosting cadence, full-screen flashes and
 * refresh-after-resume meant for one. Nothing crashed — every vendor call already fails closed —
 * but the behaviour was wrong.
 *
 * The probe stays in the shell. The core must name no vendor (IR-7); it only ever sees the
 * resulting [DeviceCapabilities].
 */
object DisplayAdapters {

    /**
     * True when this device exposes the Supernote's `eink` system service.
     *
     * The same signal [SupernoteEinkAdapter] already uses for every refresh, so selection cannot
     * disagree with execution: if this is false, every call that adapter makes would have no-opped
     * anyway. Deliberately **not** a `Build.MANUFACTURER` check — a model string is a guess about
     * hardware, whereas the service either answers or it does not.
     */
    fun hasEinkService(context: Context): Boolean =
        runCatching { context.getSystemService("eink") != null }.getOrDefault(false)

    /** The adapter for `context`'s device. */
    fun forDevice(context: Context): EinkAdapter =
        if (hasEinkService(context)) SupernoteEinkAdapter() else LcdAdapter()
}
