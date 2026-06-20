package dev.jraghavan.inkread.eink

import android.content.Context
import android.os.IBinder
import android.os.Parcel
import android.util.Log

/**
 * Supernote firmware HandWrite-ink client (RR19). Talks to the firmware's pen daemon over the
 * `service_myservice` Binder: the firmware paints stylus ink straight to the EPDC overlay at
 * sub-frame latency, so the app NEVER renders the live stroke — it only claims the pen, sets the
 * nib, and clears the overlay. Proven reachable from a sideloaded app (penspike Route 5, on a
 * Nomad). Reflection/binder throughout; never throws across the boundary (RR21-FR3).
 *
 * Clean-room (RR18): the contract is reproduced from plateaukao's AGPL-3 sources
 * (supernote_draw/SupernoteInk.kt and koreader pencil.koplugin/lib/supernote_ink.lua), which
 * reimplement Ratta's HandWriteClient. Both are AGPL-3, so this is license-clean; only the
 * documented binder contract (service name, interface token, tx codes, parcel layout) is
 * reproduced — no decompiled Ratta bytes. IR-7: this vendor code lives in the Kotlin shell.
 */
class SupernoteInk(private val context: Context) {

    private var cached: IBinder? = null
    private var active = false

    /** Resolve (and cache) the firmware binder via the hidden ServiceManager.getService. */
    private fun binder(): IBinder? {
        cached?.let { if (it.isBinderAlive) return it }
        cached = try {
            val sm = Class.forName("android.os.ServiceManager")
            val get = sm.getMethod("getService", String::class.java)
            SERVICE_NAMES.firstNotNullOfOrNull { name -> get.invoke(null, name) as? IBinder }
        } catch (t: Throwable) {
            Log.w(TAG, "ServiceManager.getService failed (hidden-API?): ${t.javaClass.simpleName}: ${t.message}")
            null
        }
        return cached
    }

    fun isAvailable(): Boolean = binder() != null

    /** Run a transaction: interface-token + app-name preamble, then [write] the per-call ints. */
    private fun send(code: Int, write: (Parcel) -> Unit) {
        val b = binder() ?: return
        val data = Parcel.obtain()
        val reply = Parcel.obtain()
        try {
            data.writeInterfaceToken(IFACE_TOKEN)
            data.writeString(APP_NAME)
            write(data)
            b.transact(code, data, reply, 0)
        } catch (t: Throwable) {
            Log.w(TAG, "transact(code=$code) failed: ${t.javaClass.simpleName}: ${t.message}")
        } finally {
            data.recycle()
            reply.recycle()
        }
    }

    /** Reflection: getSystemService("eink").enableFullUiAuto(boolean) — paint ink for our window. */
    private fun enableFullUiAuto(enable: Boolean) {
        try {
            val eink = context.getSystemService("eink") ?: return
            eink.javaClass.getMethod("enableFullUiAuto", Boolean::class.javaPrimitiveType)
                .invoke(eink, enable)
        } catch (t: Throwable) {
            Log.w(TAG, "enableFullUiAuto($enable) failed: ${t.javaClass.simpleName}: ${t.message}")
        }
    }

    /**
     * Claim the pen and turn on firmware ink for our window. Idempotent — safe to call on every
     * focus gain (the firmware resets ownership when another window takes focus). Returns whether
     * the firmware binder was reachable.
     */
    fun setup(): Boolean {
        if (binder() == null) return false
        send(TX_WRITE_APP_INFO) { it.writeInt(0); it.writeInt(0) }
        enableFullUiAuto(true)
        send(TX_DISABLE_AREA) { it.writeInt(0) } // no disabled areas
        send(TX_PEN) { it.writeInt(PEN_NEEDLE); it.writeInt(SIZE_EMR); it.writeInt(COLOR_BLACK) }
        active = true
        Log.i(TAG, "firmware ink claimed (pen=needle)")
        return true
    }

    /** Clear the firmware ink overlay (e.g. on page change so ink doesn't bleed to the next page). */
    fun clearAll() {
        if (!active) return
        send(TX_DRAW_BUFFER) { it.writeInt(255); it.writeInt(0) }
    }

    /**
     * Enable/disable firmware EMR ink painting over the whole window (RR19 / lasso UX). This is the
     * control Ratta's own lasso uses (`HandWriteClient.sendWritable`) to stop the EMR pen so it can
     * draw a dashed marquee itself — and it works for a sideloaded app because it rides the
     * reachable `service_myservice` binder, NOT the SELinux-gated `EinkManager.enableFullUiAuto`
     * (which is a silent no-op for an untrusted app — the reason toggling it never suppressed ink).
     *
     * It sends the "disable-area" transaction (same code as [TX_DISABLE_AREA]) with ONE sentinel
     * rect: `(0,0,18888,18888)` re-enables ink everywhere, `(0,0,19999,19999)` disables it
     * everywhere. Each rect is 5 ints `[left, top, width, height, 0]`.
     */
    fun setWritable(enable: Boolean) {
        if (!active) return
        val edge = if (enable) WRITABLE_ON_EDGE else WRITABLE_OFF_EDGE
        send(TX_DISABLE_AREA) {
            it.writeInt(1) // one rect
            it.writeInt(0); it.writeInt(0); it.writeInt(edge); it.writeInt(edge); it.writeInt(0)
        }
    }

    /** Release the firmware ink claim and clear the overlay. */
    fun teardown() {
        if (!active) return
        clearAll()
        enableFullUiAuto(false)
        active = false
        Log.i(TAG, "firmware ink released")
    }

    private companion object {
        const val TAG = "SupernoteInk"
        val SERVICE_NAMES = arrayOf("service_myservice", "service.myservice")
        const val IFACE_TOKEN = "android.demo.IMyService"
        const val APP_NAME = "inkread"

        const val TX_WRITE_APP_INFO = 0
        const val TX_DISABLE_AREA = 1
        const val TX_PEN = 2
        const val TX_DRAW_BUFFER = 6

        const val PEN_NEEDLE = 10
        const val COLOR_BLACK = 0
        const val SIZE_EMR = 1000

        // sendWritable sentinel rects (HandWriteClient): 18888 = ink on, 19999 = ink off.
        const val WRITABLE_ON_EDGE = 18888
        const val WRITABLE_OFF_EDGE = 19999
    }
}
