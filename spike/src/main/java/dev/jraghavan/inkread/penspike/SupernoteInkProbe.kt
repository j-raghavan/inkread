package dev.jraghavan.inkread.penspike

import android.content.Context
import android.os.IBinder
import android.os.Parcel
import android.util.Log

/**
 * Route 5 probe — the **Supernote firmware HandWrite binder** (`service_myservice`).
 *
 * This is the route the other four spikes never tried, and it is the one plateaukao's working
 * Supernote KOReader fork ships on. Unlike R1–R4, the app NEVER renders the live stroke: the
 * firmware's pen daemon paints ink straight to the EPDC overlay at sub-frame latency. The app
 * only (a) claims pen ownership, (b) sets the pen, (c) tells the firmware to paint ink for our
 * window (enableFullUiAuto), and (d) clears the overlay once a finished stroke is baked into a
 * durable buffer. So this route's latency is the *firmware's*, not ours.
 *
 * Clean-room (RR18): the protocol is reimplemented from plateaukao's AGPL-3 sources
 *   - Kotlin original: plateaukao/supernote_draw  app/.../SupernoteInk.kt
 *   - Lua/JNI port:    plateaukao/koreader (tag supernote-eink-v1)
 *                      plugins/pencil.koplugin/lib/supernote_ink.lua
 * which themselves reimplement Ratta's `com.ratta.supernote.eventlibrary.HandWriteClient`.
 * Both that fork and inkread are AGPL-3, so this is license-clean. No decompiled Ratta bytes are
 * copied — only the documented binder contract (service name, interface token, tx codes, parcel
 * layout) is reproduced. This module is device-specific; the spike is exempt from IR-7.
 *
 * THE GATING UNKNOWN this probe answers: `android.os.ServiceManager.getService` is a hidden API.
 * On targetSdk >= 28 the Java-reflection lookup below may be blocked by the hidden-API blocklist.
 *   - If it returns a live binder  => GREEN: a plain sideloaded app reaches the firmware ink path.
 *   - If reflection is blocked      => the production path moves the getService lookup into the
 *     existing C native helper (JNI is NOT subject to hidden-API enforcement — which is exactly
 *     how the koreader Lua port reaches it). The probe reports which case we are in.
 */
class SupernoteInkProbe(private val ctx: Context) {

    companion object {
        private const val TAG = "PenSpike-sn"

        // Current firmware registers "service_myservice"; older code paths use the legacy alias.
        private val SERVICE_NAMES = arrayOf("service_myservice", "service.myservice")
        private const val IFACE_TOKEN = "android.demo.IMyService"
        private const val APP_NAME = "inkread-penspike"

        // Firmware transaction codes (from HandWriteClient).
        private const val TX_WRITE_APP_INFO = 0
        private const val TX_DISABLE_AREA = 1
        private const val TX_PEN = 2
        private const val TX_DRAW_BUFFER = 6

        // Pen codes for Nomad (deviceType==3 / A5X2): Needle/Ink/Mark/Calligraphy.
        private const val PEN_NEEDLE = 10
        private const val COLOR_BLACK = 0
        // EMR nib size — a mid-range spike default; the real value is tuned later in the InkModel.
        private const val SIZE_EMR = 1000
    }

    private var cached: IBinder? = null
    private var lastReport: String = "ROUTE 5 (service_myservice): not yet attempted"
    private var active = false

    /** Resolve (and cache) the firmware binder via the hidden ServiceManager.getService. */
    private fun binder(): IBinder? {
        cached?.let { if (it.isBinderAlive) return it }
        cached = try {
            val sm = Class.forName("android.os.ServiceManager")
            val get = sm.getMethod("getService", String::class.java)
            var found: IBinder? = null
            for (name in SERVICE_NAMES) {
                val b = get.invoke(null, name) as? IBinder
                if (b != null) {
                    Log.i(TAG, "found binder for \"$name\" alive=${b.isBinderAlive} desc=${descriptorOf(b)}")
                    found = b
                    break
                }
            }
            found
        } catch (t: Throwable) {
            // A NoSuchMethodException / blocklist denial here is the hidden-API verdict, not a bug.
            Log.w(TAG, "ServiceManager.getService reflection failed (hidden-API?): ${t.javaClass.simpleName}: ${t.message}")
            null
        }
        return cached
    }

    private fun descriptorOf(b: IBinder): String =
        try { b.interfaceDescriptor ?: "(null)" } catch (t: Throwable) { "(threw ${t.javaClass.simpleName})" }

    /** One-shot reachability check for the consolidated SELFTEST VERDICT block. */
    fun probe(): String {
        val b = binder()
        lastReport = if (b != null) {
            "ROUTE 5 (service_myservice): reachable=true alive=${b.isBinderAlive} desc='${descriptorOf(b)}' " +
                "(firmware ink path open to a sideloaded app)"
        } else {
            val classExists = try { Class.forName("android.os.ServiceManager"); true } catch (t: Throwable) { false }
            "ROUTE 5 (service_myservice): reachable=false (getService returned null or hidden-API blocked; " +
                "ServiceManager class exists=$classExists) — production path = getService via JNI native helper"
        }
        Log.i(TAG, lastReport)
        return lastReport
    }

    /** Run a transaction: token + app-name preamble, then [write] the per-call ints. */
    private fun send(code: Int, write: (Parcel) -> Unit) {
        val b = binder() ?: run { Log.w(TAG, "binder null; skip tx=$code"); return }
        val data = Parcel.obtain()
        val reply = Parcel.obtain()
        try {
            data.writeInterfaceToken(IFACE_TOKEN)
            data.writeString(APP_NAME)
            write(data)
            b.transact(code, data, reply, 0)
            Log.d(TAG, "tx=$code ok reply='${runCatching { reply.readString() }.getOrNull()}'")
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
            val eink = ctx.getSystemService("eink") ?: run {
                Log.w(TAG, "getSystemService(\"eink\") null — cannot enableFullUiAuto"); return
            }
            eink.javaClass.getMethod("enableFullUiAuto", Boolean::class.javaPrimitiveType)
                .invoke(eink, enable)
            Log.i(TAG, "enableFullUiAuto($enable) ok")
        } catch (t: Throwable) {
            Log.w(TAG, "enableFullUiAuto($enable) failed: ${t.javaClass.simpleName}: ${t.message}")
        }
    }

    /**
     * Enter the route: claim pen ownership, turn on firmware ink for our window, clear any disable
     * areas, and set a black needle pen. After this, drawing with the stylus should leave firmware
     * ink under the nib with no per-sample app work — that is the GREEN signal to watch for.
     */
    fun setup(): Boolean {
        if (binder() == null) { Log.w(TAG, "setup skipped — binder unreachable"); return false }
        send(TX_WRITE_APP_INFO) { it.writeInt(0); it.writeInt(0) }
        enableFullUiAuto(true)
        send(TX_DISABLE_AREA) { it.writeInt(0) } // clear disable areas
        send(TX_PEN) { it.writeInt(PEN_NEEDLE); it.writeInt(SIZE_EMR); it.writeInt(COLOR_BLACK) }
        active = true
        Log.i(TAG, "ROUTE 5 setup done — draw with the stylus; firmware should ink under the nib")
        return true
    }

    /** Leave the route: release the firmware ink claim and clear the overlay. */
    fun teardown() {
        if (!active) return
        send(TX_DRAW_BUFFER) { it.writeInt(255); it.writeInt(0) } // clearAll
        enableFullUiAuto(false)
        active = false
        Log.i(TAG, "ROUTE 5 teardown done — firmware ink released")
    }

    fun isActive(): Boolean = active

    fun report(): String = lastReport
}
