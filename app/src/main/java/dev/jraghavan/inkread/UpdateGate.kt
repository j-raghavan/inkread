package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.widget.Toast
import java.util.concurrent.Executors

/**
 * The self-updater's UX policy (ADR-INKREAD-0014 UPD-FR6/FR7/FR8) — the calm, e-ink-first layer over
 * the mechanism in [UpdateController]. Decides *when* to check (throttled, on launch, off the UI
 * thread — never background polling) and *how* to surface a result: the in-app "Update available"
 * prompt, or, under the opt-in auto-update toggle, straight to the system installer.
 *
 * Kept off [HomeActivity] so the launch hook is a single call; all blocking work runs on a worker
 * thread and every UI touch is guarded against a finishing Activity.
 */
object UpdateGate {

    /** At most one launch check per window — a sideloaded reader is often offline, and the home
     *  screen should never wait on the network (UPD-FR6). */
    private const val THROTTLE_MS = 6L * 60 * 60 * 1000 // 6 hours

    /** One reusable daemon thread for all update I/O — never blocks the UI, never leaks a thread per
     *  check (and a daemon never holds the JVM alive). Update work is sequential, so one suffices. */
    private val io = Executors.newSingleThreadExecutor { r ->
        Thread(r, "inkread-update").apply { isDaemon = true }
    }

    /** On-launch check (from [HomeActivity.onResume]): honours the auto-check toggle + throttle. */
    fun maybeCheckOnLaunch(activity: Activity) {
        val ctx = activity.applicationContext
        if (!AppSettings.autoUpdateCheck(ctx)) return
        if (System.currentTimeMillis() - AppSettings.updateLastCheckMs(ctx) < THROTTLE_MS) return
        runCheck(activity, manual = false)
    }

    /** Manual "Check for updates" (from [SettingsActivity]): ignores the throttle and reports when
     *  there is nothing newer (UPD-FR8). */
    fun checkNow(activity: Activity) {
        Toast.makeText(activity, "Checking for updates…", Toast.LENGTH_SHORT).show()
        runCheck(activity, manual = true)
    }

    private fun runCheck(activity: Activity, manual: Boolean) {
        val ctx = activity.applicationContext
        val controller = UpdateController(ctx)
        io.execute {
            val result = controller.check()
            // Only a check that actually reached the server advances the throttle — an offline launch
            // must stay free to retry the moment connectivity returns (UPD-FR9).
            if (result !is UpdateController.CheckResult.Failed) {
                AppSettings.setUpdateLastCheckMs(ctx, System.currentTimeMillis())
            }
            onUi(activity) {
                when (result) {
                    is UpdateController.CheckResult.Failed ->
                        if (manual) toast(activity, "Couldn't check for updates")
                    is UpdateController.CheckResult.UpToDate ->
                        if (manual) toast(activity, "inkread is up to date")
                    is UpdateController.CheckResult.Update -> onUpdate(activity, controller, result.available, manual, ctx)
                }
            }
        }
    }

    /** Route a found update: a skipped version stays silent on an automatic check (a manual one
     *  always surfaces it); auto-install goes straight to download+install; otherwise prompt. */
    private fun onUpdate(
        activity: Activity,
        controller: UpdateController,
        a: UpdateController.Available,
        manual: Boolean,
        ctx: android.content.Context,
    ) {
        when {
            !manual && a.version == AppSettings.updateSkipVersion(ctx) -> Unit
            !manual && AppSettings.autoInstallEffective(ctx) && controller.canRequestInstall() ->
                downloadThenInstall(activity, controller, a)
            else -> showPrompt(activity, controller, a)
        }
    }

    /** The "Update available" prompt: Install / Skip this version / Later (UPD-FR6). */
    private fun showPrompt(activity: Activity, controller: UpdateController, a: UpdateController.Available) {
        AlertDialog.Builder(activity, R.style.InkDialog)
            .setTitle("Update available — v${a.version}")
            .setMessage(promptBody(a))
            .setPositiveButton("Install") { _, _ -> beginInstall(activity, controller, a) }
            .setNegativeButton("Skip this version") { _, _ ->
                AppSettings.setUpdateSkipVersion(activity.applicationContext, a.version)
            }
            .setNeutralButton("Later", null)
            .show()
    }

    /** Start the install flow from the prompt — route to the permission grant if it is not held. */
    private fun beginInstall(activity: Activity, controller: UpdateController, a: UpdateController.Available) {
        if (!controller.canRequestInstall()) {
            toast(activity, "Allow inkread to install apps, then tap Install again")
            runCatching { activity.startActivity(controller.unknownSourcesSettingsIntent()) }
            return
        }
        downloadThenInstall(activity, controller, a)
    }

    /** Download + verify off the UI thread, then hand the verified APK to the system installer. */
    private fun downloadThenInstall(activity: Activity, controller: UpdateController, a: UpdateController.Available) {
        toast(activity, "Downloading v${a.version}…")
        io.execute {
            val apk = controller.downloadAndVerify(a)
            onUi(activity) {
                if (apk == null) {
                    toast(activity, "Update download failed — will retry on next launch")
                } else {
                    runCatching { controller.install(apk) }
                        .onFailure { toast(activity, "Couldn't start the installer") }
                }
            }
        }
    }

    /** Version + a trimmed slice of the release notes — enough to decide, not a wall of text. */
    private fun promptBody(a: UpdateController.Available): String {
        val notes = a.notes.trim()
        if (notes.isEmpty()) return "A newer version of inkread is available."
        val trimmed = if (notes.length > NOTES_LIMIT) notes.take(NOTES_LIMIT).trimEnd() + "…" else notes
        return "What's new:\n\n$trimmed"
    }

    private fun onUi(activity: Activity, block: () -> Unit) {
        activity.runOnUiThread {
            if (!activity.isFinishing && !activity.isDestroyed) block()
        }
    }

    private fun toast(activity: Activity, msg: String) =
        Toast.makeText(activity, msg, Toast.LENGTH_SHORT).show()

    private const val NOTES_LIMIT = 600
}
