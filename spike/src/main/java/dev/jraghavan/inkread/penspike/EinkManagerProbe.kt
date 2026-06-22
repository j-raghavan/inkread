package dev.jraghavan.inkread.penspike

import android.content.Context
import android.util.Log

/**
 * Route 0 probe (RR19-FR3 step 0) — the **EinkManager reflection** path.
 *
 * Clean-room (RR18) from KOReader's AGPL `android-luajit-launcher`
 * (`device/epd/rockchip/RK35xxEPDController`): the RK3566 SoC — the Supernote's — is refreshed
 * by KOReader via `android.os.EinkManager`, NOT `/dev/ebc`:
 *   `getSystemService("eink")` → `sendOneFullFrame()`  (full-screen; `setMode("12")`=A2 left off).
 *
 * This probe answers, safely, whether that same path is open to OUR sideloaded app:
 *   1. resolve `getSystemService("eink")` and report the concrete class,
 *   2. dump EVERY method of that class — the REAL API surface on Ratta's build (recon only
 *      guessed at `setEinkUpdateMode`/`setEinkA2Gate`/`SendHWCCmd`); introspection is ground truth,
 *   3. invoke the ONE call KOReader ships to thousands of users — `sendOneFullFrame()` — a safe,
 *      recoverable full-screen refresh,
 *   4. report PRESENCE (not invocation) of the mode setters, so the next call is built from real
 *      signatures rather than guessed ones.
 *
 * Safety: only the no-arg `sendOneFullFrame()` is invoked. Mode setters (`setMode`,
 * `setEinkUpdateMode`, …) change global panel state on unknown signatures, so they are listed
 * but NOT called here — that's a deliberate, separate decision once the signatures are known.
 */
object EinkManagerProbe {

    private const val TAG = "PenSpike-eink"

    /** Refresh/mode methods worth calling next, if present (reported here, not invoked). */
    private val INTERESTING = listOf(
        "sendOneFullFrame", "sendOneFrame", "fullRefresh", "partialRefresh",
        "setMode", "setEinkUpdateMode", "setEinkMode", "setEinkA2Gate",
        "sendHWCCmd", "setScreenMode", "requestEpdMode", "setByEinkUpdateMode",
    )

    /**
     * Run the probe. Returns a multi-line report whose FIRST line is a one-line verdict
     * (suitable for the consolidated SELFTEST VERDICT block); the rest is the method dump.
     */
    fun run(ctx: Context): String {
        val body = StringBuilder()
        fun line(s: String) { body.append(s).append('\n'); Log.i(TAG, s) }

        val svc: Any? = try {
            ctx.getSystemService("eink")
        } catch (t: Throwable) {
            line("  getSystemService(\"eink\") THREW ${t.javaClass.simpleName}: ${t.message}")
            null
        }

        if (svc == null) {
            val exists = try { Class.forName("android.os.EinkManager"); true } catch (t: Throwable) { false }
            line("  getSystemService(\"eink\") = null (class exists=$exists) => EinkManager NOT usable by this app")
            return "ROUTE 0 (EinkManager): reachable=false (null service; class exists=$exists)\n$body"
        }

        val cls = svc.javaClass
        line("  service = ${cls.name}")

        val methods = try { cls.methods } catch (t: Throwable) { emptyArray() }
        line("  --- ${cls.name} public methods (${methods.size}) ---")
        methods.sortedBy { it.name }.forEach { m ->
            val params = m.parameterTypes.joinToString(",") { it.simpleName }
            line("    ${m.returnType.simpleName} ${m.name}($params)")
        }

        val present = INTERESTING.filter { name -> methods.any { it.name == name } }
        line("  interesting present: $present")

        // Invoke the ONE safe, KOReader-proven call: sendOneFullFrame().
        var fullFrameOk: Boolean? = null
        val full = methods.firstOrNull { it.name == "sendOneFullFrame" && it.parameterTypes.isEmpty() }
        if (full != null) {
            fullFrameOk = try {
                full.invoke(svc)
                line("  sendOneFullFrame() INVOKED ok => watch the panel for a full refresh")
                true
            } catch (t: Throwable) {
                line("  sendOneFullFrame() invoke THREW ${t.javaClass.simpleName}: ${t.message}")
                false
            }
        } else {
            line("  sendOneFullFrame() NOT present — see method dump for the real refresh call")
        }
        line("  (mode setters listed but NOT invoked — build the next call from real signatures)")

        val verdict = "ROUTE 0 (EinkManager): reachable=true class=${cls.simpleName} " +
            "methods=${methods.size} sendOneFullFrame=${fullFrameOk ?: "absent"} present=$present"
        return "$verdict\n$body"
    }

    private fun resolve(ctx: Context, log: (String) -> Unit): Pair<Any, Class<*>>? {
        val svc = try { ctx.getSystemService("eink") } catch (t: Throwable) { null } ?: return null
        return svc to svc.javaClass
    }

    /**
     * Best-effort full-screen EPD refresh via the proven `sendOneFullFrame()` — used to push the
     * spike's own SurfaceView UI (legend/instructions) onto the panel, since a sideloaded
     * SurfaceView's posts don't reliably trigger the einkhwc to refresh on their own.
     */
    fun fullRefresh(ctx: Context): Boolean {
        return try {
            val svc = ctx.getSystemService("eink") ?: return false
            svc.javaClass.getMethod("sendOneFullFrame").invoke(svc)
            Log.i(TAG, "fullRefresh: sendOneFullFrame invoked")
            true
        } catch (t: Throwable) {
            Log.w(TAG, "fullRefresh failed: ${t.javaClass.simpleName}: ${t.message}")
            false
        }
    }

    /**
     * Disambiguate `getSeqNum(0)` and `setMode`/`getMode` semantics — rigorously, so we don't
     * mistake a read-incrementing counter for a refresh signal (the +1-per-read trap).
     *
     * Reports:
     *   - back-to-back getSeqNum with NO action  → is the READ itself incrementing it?
     *   - getSeqNum drift over 250 ms with NO refresh call → baseline drift (free-running?)
     *   - getSeqNum delta over 250 ms WITH sendOneFullFrame → only a *refresh signal* if it
     *     clearly exceeds the baseline.
     *   - setMode("12") then getMode() → does the GLOBAL mode change? (may be a per-window mode
     *     getMode doesn't report — confirmed separately in foreground via [foregroundModeTest]).
     */
    fun confirmRefresh(ctx: Context): String {
        val body = StringBuilder()
        fun line(s: String) { body.append(s).append('\n'); Log.i(TAG, s) }
        val (svc, cls) = resolve(ctx, ::line) ?: return "EM-CONFIRM: service=null\n$body"

        fun call(name: String, sig: Array<Class<*>>, vararg a: Any?): Any? =
            try { cls.getMethod(name, *sig).invoke(svc, *a) }
            catch (t: Throwable) { line("    $name() THREW ${t.javaClass.simpleName}: ${t.message}"); null }
        val intT = Int::class.javaPrimitiveType!!
        fun seq(): Int = (call("getSeqNum", arrayOf(intT), 0) as? Int) ?: -1
        fun mode(): String = (call("getMode", emptyArray()) as? String) ?: "?"
        fun sleep(ms: Long) { try { Thread.sleep(ms) } catch (_: InterruptedException) {} }

        line("EM-CONFIRM: getSeqNum/setMode semantics ====")
        line("  isValid=${call("isValid", emptyArray())} einkEnabled=${call("getEinkEnabled", emptyArray())}")

        // (0) Does READING getSeqNum increment it? (back-to-back, no sleep, no refresh)
        val rA = seq(); val rB = seq()
        val readIncrements = rB == rA + 1
        line("  getSeqNum back-to-back (no action): $rA,$rB -> readIncrements=$readIncrements")

        // (1) Baseline drift over 250 ms with NO refresh call.
        val d0 = seq(); sleep(250); val d1 = seq()
        val baseline = d1 - d0
        line("  getSeqNum baseline /250ms (NO refresh): $d0 -> $d1 (drift=$baseline)")

        // (2) Delta over 250 ms WITH sendOneFullFrame — refresh signal only if >> baseline.
        val s0 = seq(); call("sendOneFullFrame", emptyArray()); sleep(250); val s1 = seq()
        val fullDelta = s1 - s0
        val seqIsRefreshSignal = fullDelta > baseline + 1
        line("  getSeqNum /250ms WITH sendOneFullFrame: $s0 -> $s1 (delta=$fullDelta vs baseline=$baseline) " +
            "seqIsRefreshSignal=$seqIsRefreshSignal")

        // (3) setMode round-trip at GLOBAL level (foreground retry is separate).
        val origMode = mode()
        call("setMode", arrayOf(String::class.java), "12"); sleep(50)
        val a2Mode = mode()
        call("setMode", arrayOf(String::class.java), origMode)
        val stuck = a2Mode != origMode
        line("  setMode global: orig=$origMode afterSetMode(12)=$a2Mode stuck=$stuck")

        val verdict = "EM-CONFIRM: reachable=true setModeStuckGlobal=$stuck " +
            "seqReadIncrements=$readIncrements seqIsRefreshSignal=$seqIsRefreshSignal " +
            "(origMode=$origMode fullDelta=$fullDelta baseline=$baseline)"
        line(verdict)
        return "$verdict\n$body"
    }

    /**
     * Retry `setMode("12")` while our window is FOREGROUND/focused (called from onResume) — the
     * system likely honors a per-app eink mode only for the active window, which is why the
     * onCreate-time attempt didn't stick. Emits MARKER lines so a wide logcat
     * (`-s PenSpike PenSpike-eink EinkManager eink SurfaceFlinger`) can correlate the system
     * hwcomposer's reaction. Leaves A2 set ~400 ms (visible as a mode change) then restores.
     */
    fun foregroundModeTest(ctx: Context): String {
        val body = StringBuilder()
        fun line(s: String) { body.append(s).append('\n'); Log.i(TAG, s) }
        val (svc, cls) = resolve(ctx, ::line) ?: return "EM-FG: service=null\n$body"

        fun call(name: String, sig: Array<Class<*>>, vararg a: Any?): Any? =
            try { cls.getMethod(name, *sig).invoke(svc, *a) }
            catch (t: Throwable) { line("    $name() THREW ${t.javaClass.simpleName}: ${t.message}"); null }
        fun mode(): String = (call("getMode", emptyArray()) as? String) ?: "?"
        fun sleep(ms: Long) { try { Thread.sleep(ms) } catch (_: InterruptedException) {} }

        line("EM-FG: setMode while FOREGROUND ==== (watch system eink/EinkManager logs)")
        val orig = mode()
        line("  MARKER-1 before setMode; getMode=$orig")
        call("setMode", arrayOf(String::class.java), "12"); sleep(50)
        val after = mode()
        line("  MARKER-2 after setMode(12); getMode=$after stuck=${after != orig}")
        call("sendOneFullFrame", emptyArray()); sleep(400)
        line("  MARKER-3 after sendOneFullFrame; getMode=${mode()}")
        call("setMode", arrayOf(String::class.java), orig)
        line("  MARKER-4 restored setMode($orig); getMode=${mode()}")

        return "EM-FG: origMode=$orig setModeStuck=${after != orig} (see MARKER lines + system logs)\n$body"
    }
}
