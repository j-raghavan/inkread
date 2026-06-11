package dev.jraghavan.inkread.penspike

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.IBinder
import android.util.Log

/**
 * Route 2 probe (RR19-FR3 step 2): bind the exported `com.ratta.DrawService`.
 *
 * Recon found `com.ratta.drawpath/.DrawService` exported via action "com.ratta.DrawService"
 * with no bind permission → bindable from a sideloaded app. Binding success alone is a
 * valuable reachability fact: it is the bridge to EinkManager-level control that a sideloaded
 * app otherwise can't reach.
 *
 * We do NOT guess AIDL transaction codes (that would be fragile and could destabilise the
 * panel). We report:
 *   - whether onServiceConnected fires (BINDS),
 *   - the binder's getInterfaceDescriptor() (so the AIDL can be developed later),
 *   - pingBinder()/isBinderAlive() liveness.
 * That is the honest, safe reachability result for this route.
 */
class DrawServiceProbe(private val ctx: Context) {

    private var bound = false
    private var lastReport: String = "ROUTE 2 (DrawService): not yet attempted"

    private val conn = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName?, service: IBinder?) {
            val descriptor = try {
                service?.interfaceDescriptor ?: "(null descriptor)"
            } catch (t: Throwable) {
                "(descriptor query threw: ${t.javaClass.simpleName})"
            }
            val alive = try {
                service?.isBinderAlive == true
            } catch (t: Throwable) {
                false
            }
            val ping = try {
                service?.pingBinder() == true
            } catch (t: Throwable) {
                false
            }
            lastReport = "ROUTE 2 (DrawService): BINDS (onServiceConnected) " +
                "descriptor='$descriptor' alive=$alive ping=$ping comp=$name"
            Log.i(TAG, lastReport)
        }

        override fun onServiceDisconnected(name: ComponentName?) {
            Log.i(TAG, "ROUTE 2 (DrawService): onServiceDisconnected $name")
        }

        override fun onNullBinding(name: ComponentName?) {
            lastReport = "ROUTE 2 (DrawService): onNullBinding (service returned null binder) $name"
            Log.w(TAG, lastReport)
        }
    }

    /** Attempt the bind. Returns the immediate bindService() boolean; the descriptor arrives async. */
    fun bind(): Boolean {
        val intent = Intent().apply {
            component = ComponentName("com.ratta.drawpath", "com.ratta.drawpath.DrawService")
            action = "com.ratta.DrawService"
        }
        return try {
            bound = ctx.bindService(intent, conn, Context.BIND_AUTO_CREATE)
            lastReport = if (bound) {
                "ROUTE 2 (DrawService): bindService()=true (awaiting onServiceConnected...)"
            } else {
                "ROUTE 2 (DrawService): FAILED bindService()=false (component not found / not bindable)"
            }
            Log.i(TAG, lastReport)
            bound
        } catch (t: Throwable) {
            lastReport = "ROUTE 2 (DrawService): FAILED ${t.javaClass.simpleName}: ${t.message}"
            Log.e(TAG, lastReport, t)
            false
        }
    }

    fun unbind() {
        if (bound) {
            runCatching { ctx.unbindService(conn) }
            bound = false
        }
    }

    fun report(): String = lastReport
}
