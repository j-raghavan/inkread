package dev.jraghavan.inkread.eink

import dev.jraghavan.inkread.DeviceCapabilities
import dev.jraghavan.inkread.RefreshCommand

/**
 * The device refresh adapter seam (RR2-FR3). v1 ships exactly ONE implementation —
 * [SupernoteEinkAdapter] — wired directly (no registry, no detection). The interface is
 * kept so a future `OnyxEinkAdapter` (Boox) is a drop-in (RR15-FR-future). The core knows
 * the adapter only through this interface; no vendor name appears in the core (IR-7).
 */
interface EinkAdapter {
    /** The capabilities this adapter advertises to the core at init (RR2-FR2). */
    fun capabilities(): DeviceCapabilities

    /**
     * Give the adapter the panel-owning [View] (e.g. the reader SurfaceView). Some devices
     * trigger an EPD refresh through the view's context (the Supernote routes via the system
     * "eink" service). Default no-op for adapters that don't need a view.
     */
    fun attachView(view: android.view.View?) {}

    /**
     * Enable/disable the device's system gesture service. On the Supernote that service
     * otherwise swallows the reader's touch-up events; disabling it gives the app full touch
     * streams (page turns, link taps, future pen input). Default no-op.
     */
    fun setSystemGesturesEnabled(enabled: Boolean) {}

    /** Execute one vendor-neutral [RefreshCommand] against the panel. */
    fun execute(command: RefreshCommand)

    /** Execute a whole command stream in order (the policy emits these as a batch). */
    fun executeAll(commands: List<RefreshCommand>) {
        for (c in commands) execute(c)
    }

    /**
     * Force a full-screen panel refresh after a blit that carries no command stream (the
     * initial open, a SAF import). On a full-only panel this is the same flash a page turn's
     * `Update{Full}` produces. Default no-op.
     */
    fun refreshFull() {}
}
