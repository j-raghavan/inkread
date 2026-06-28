package dev.jraghavan.inkread

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInfo
import android.content.pm.PackageInstaller
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.provider.Settings
import android.util.Log
import org.json.JSONObject
import java.io.File

/**
 * The Android shell's half of the in-app self-updater (ADR-INKREAD-0014). The network and the
 * install live here (IR-7 — the Rust core stays IO-free and only *decides*): this fetches the
 * project's GitHub `releases/latest` payload, hands it to [NativeBridge.nativeUpdateDecide] for the
 * semver decision, then — on the reader's say-so or under auto-update — downloads the APK, verifies
 * it ([UpdateVerify]: published SHA-256 + signer pin), and installs it via the [PackageInstaller]
 * session API.
 *
 * Every method here blocks on I/O; callers invoke them off the UI thread (see [HomeActivity]). The
 * controller is mechanism only — the skip/throttle/auto-install *policy* lives in the UX layer.
 */
class UpdateController(private val context: Context) {

    /** A newer release the core surfaced — the parsed [NativeBridge.nativeUpdateDecide] decision. */
    data class Available(
        val version: String,
        val notes: String,
        val apkUrl: String,
        val sha256Url: String,
    )

    /**
     * The outcome of a [check]: a newer release, nothing newer, or the check never reached the
     * server. Distinguishing [Failed] from [UpToDate] lets the UX avoid burning the throttle on an
     * offline launch and report "couldn't check" vs "up to date" honestly (UPD-FR9).
     */
    sealed interface CheckResult {
        data class Update(val available: Available) : CheckResult
        object UpToDate : CheckResult
        object Failed : CheckResult
    }

    /** The installed `versionName` (the basis for the semver comparison), or "" if unavailable. */
    fun installedVersion(): String =
        runCatching { context.packageManager.getPackageInfo(context.packageName, 0).versionName }
            .getOrNull()
            .orEmpty()

    /**
     * Fetch `releases/latest` and ask the core whether it is newer. [CheckResult.Failed] means the
     * server was never reached (offline / rate-limited / bad local version) — the caller leaves the
     * throttle untouched; [CheckResult.UpToDate] means the server answered with nothing installable.
     * Blocking — call off the UI thread.
     */
    fun check(): CheckResult {
        val installed = installedVersion()
        if (installed.isEmpty()) return CheckResult.Failed
        val json = HttpFetch.getText(LATEST_RELEASE_URL, USER_AGENT, ACCEPT_GITHUB, TIMEOUT_MS, MAX_TEXT_BYTES)
            ?: return CheckResult.Failed
        val decision = runCatching { JSONObject(NativeBridge.nativeUpdateDecide(installed, json)) }
            .getOrNull() ?: return CheckResult.Failed
        if (!decision.optBoolean("updateAvailable", false)) return CheckResult.UpToDate
        val apkUrl = decision.optString("apkUrl")
        if (apkUrl.isEmpty()) return CheckResult.UpToDate
        return CheckResult.Update(
            Available(
                version = decision.optString("version"),
                notes = decision.optString("notes"),
                apkUrl = apkUrl,
                sha256Url = decision.optString("sha256Url"),
            ),
        )
    }

    /** Whether the OS will let this app launch an install (the "install unknown apps" grant). */
    fun canRequestInstall(): Boolean =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            context.packageManager.canRequestPackageInstalls()
        } else {
            true // pre-O relies on the global "unknown sources" toggle; the installer surfaces it.
        }

    /** Intent to the per-app "install unknown apps" screen, so the reader can grant it once. */
    fun unknownSourcesSettingsIntent(): Intent =
        Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${context.packageName}"))

    /**
     * Download [a]'s APK to private cache and verify it: the published SHA-256 (when present) MUST
     * match, and the signer set MUST equal the installed app's (UPD-FR3/FR4). Returns the verified
     * file, or `null` on any download/verify failure (the partial file is discarded). Blocking.
     */
    fun downloadAndVerify(a: Available): File? {
        val dir = File(context.cacheDir, UPDATE_DIR).apply { mkdirs() }
        // One slot, overwritten each attempt — never accumulate stale APKs.
        val apk = File(dir, "update.apk")
        if (!HttpFetch.download(a.apkUrl, apk, USER_AGENT, TIMEOUT_MS, MAX_APK_BYTES)) {
            apk.delete()
            return null
        }

        val apkBytes = runCatching { apk.readBytes() }.getOrNull()
        if (apkBytes == null) {
            apk.delete()
            return null
        }

        // Checksum gate: a *published* digest must match; an *absent* one falls back to signer-pin
        // alone (UPD-FR3) — distinguish the two so a missing file is not silently treated as a pass.
        val checksumText = if (a.sha256Url.isNotEmpty()) {
            HttpFetch.getText(a.sha256Url, USER_AGENT, null, TIMEOUT_MS, MAX_TEXT_BYTES)
        } else {
            null
        }
        if (UpdateVerify.expectedShaFrom(checksumText) != null &&
            !UpdateVerify.matchesPublishedSha(apkBytes, checksumText)
        ) {
            Log.w(TAG, "update APK checksum mismatch — discarding")
            apk.delete()
            return null
        }

        // Signer-pin gate: the download must be signed by this app's key, else the OS would reject
        // it anyway (and a swapped asset is refused before the installer is ever invoked).
        if (!UpdateVerify.certsMatch(installedSignerCerts(), archiveSignerCerts(apk))) {
            Log.w(TAG, "update APK signer mismatch — discarding")
            apk.delete()
            return null
        }
        return apk
    }

    /**
     * Hand [apk] to the system installer via a [PackageInstaller] session (streams the bytes — no
     * FileProvider needed). The OS shows its own final "install?" confirmation; the outcome is
     * delivered to [UpdateInstallReceiver]. Throws only on a session-open failure the caller logs.
     */
    fun install(apk: File) {
        val installer = context.packageManager.packageInstaller
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
        params.setAppPackageName(context.packageName)
        val sessionId = installer.createSession(params)
        installer.openSession(sessionId).use { session ->
            session.openWrite("inkread", 0, apk.length()).use { out ->
                apk.inputStream().use { it.copyTo(out) }
                session.fsync(out)
            }
            val intent = Intent(context, UpdateInstallReceiver::class.java).setAction(ACTION_INSTALLED)
            val flags = PendingIntent.FLAG_UPDATE_CURRENT or
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_MUTABLE else 0
            val pending = PendingIntent.getBroadcast(context, sessionId, intent, flags)
            session.commit(pending.intentSender)
        }
    }

    // ── signer extraction (signing-cert API moved at API 28) ───────────────────────────────────────

    private fun installedSignerCerts(): List<ByteArray> = runCatching {
        certBytesOf(context.packageManager.getPackageInfo(context.packageName, signingFlag()))
    }.getOrDefault(emptyList())

    private fun archiveSignerCerts(apk: File): List<ByteArray> = runCatching {
        context.packageManager.getPackageArchiveInfo(apk.absolutePath, signingFlag())
            ?.let { certBytesOf(it) }
            .orEmpty()
    }.getOrDefault(emptyList())

    @Suppress("DEPRECATION") // GET_SIGNATURES is the only option below API 28 (minSdk 24).
    private fun signingFlag(): Int =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            PackageManager.GET_SIGNING_CERTIFICATES
        } else {
            PackageManager.GET_SIGNATURES
        }

    @Suppress("DEPRECATION")
    private fun certBytesOf(info: PackageInfo): List<ByteArray> =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            // apkContentsSigners = the certs the package is CURRENTLY signed by, symmetric for an
            // installed app and a downloaded archive. signingCertificateHistory would add the
            // on-device rotation lineage the archive lacks, which certsMatch's size-equality would
            // then falsely reject (a legit update after any future key rotation).
            info.signingInfo?.apkContentsSigners?.map { it.toByteArray() }.orEmpty()
        } else {
            info.signatures?.map { it.toByteArray() }.orEmpty()
        }

    companion object {
        private const val TAG = "UpdateController"

        /** The project's GitHub releases endpoint (the only place the host is named — IR-7 keeps the
         *  core unaware; the shell owns the URL). `/latest` already excludes drafts + prereleases. */
        const val LATEST_RELEASE_URL = "https://api.github.com/repos/j-raghavan/inkread/releases/latest"
        private const val ACCEPT_GITHUB = "application/vnd.github+json"
        private const val USER_AGENT = "inkread-updater"
        private const val ACTION_INSTALLED = "dev.jraghavan.inkread.UPDATE_INSTALLED"

        /** Private cache subdir holding the single staged APK — shared with [UpdateInstallReceiver]
         *  so its post-install cleanup targets the same directory. */
        const val UPDATE_DIR = "updates"
        private const val TIMEOUT_MS = 15_000
        private const val MAX_TEXT_BYTES = 512 * 1024 // release JSON / checksum line
        private const val MAX_APK_BYTES = 200L * 1024 * 1024 // APK ceiling (guards a hostile stream)
    }
}
