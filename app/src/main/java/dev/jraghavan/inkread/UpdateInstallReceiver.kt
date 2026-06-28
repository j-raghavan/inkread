package dev.jraghavan.inkread

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.util.Log
import java.io.File

/**
 * Receives the [PackageInstaller] session callback for a self-update (ADR-INKREAD-0014 UPD-FR5).
 *
 * The single unavoidable user step on a sideloaded app: when the session reaches
 * [PackageInstaller.STATUS_PENDING_USER_ACTION] the OS hands back a confirmation intent that we must
 * launch — that is Android's own "Install update?" dialog (no device-owner ⇒ no silent install).
 * Terminal statuses just log and tidy the cached APK.
 */
class UpdateInstallReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, Int.MIN_VALUE)) {
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                @Suppress("DEPRECATION")
                val confirm = intent.getParcelableExtra<Intent>(Intent.EXTRA_INTENT)
                if (confirm != null) {
                    confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                    context.startActivity(confirm)
                } else {
                    Log.w(TAG, "pending user action with no confirmation intent")
                }
            }

            PackageInstaller.STATUS_SUCCESS -> {
                Log.i(TAG, "update installed")
                clearCache(context)
            }

            else -> {
                val msg = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)
                Log.w(TAG, "update install failed (status=$status): $msg")
                clearCache(context)
            }
        }
    }

    /** Drop the staged APK once the session is terminal — never leave it in cache. */
    private fun clearCache(context: Context) {
        runCatching { File(context.cacheDir, UpdateController.UPDATE_DIR).deleteRecursively() }
    }

    private companion object {
        const val TAG = "UpdateInstall"
    }
}
