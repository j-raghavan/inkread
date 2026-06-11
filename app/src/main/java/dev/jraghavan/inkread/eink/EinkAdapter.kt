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

    /** Execute one vendor-neutral [RefreshCommand] against the panel. */
    fun execute(command: RefreshCommand)

    /** Execute a whole command stream in order (the policy emits these as a batch). */
    fun executeAll(commands: List<RefreshCommand>) {
        for (c in commands) execute(c)
    }
}
