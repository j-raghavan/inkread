package dev.jraghavan.inkread

import android.content.Context

/**
 * App-level preferences (distinct from the reader's per-document display settings). A thin,
 * SharedPreferences-backed value store surfaced by [SettingsActivity] and read by the features that
 * the preference governs. Named `AppSettings` to avoid colliding with `android.provider.Settings`.
 */
object AppSettings {
    private const val PREFS = "settings"
    private const val KEY_OVERWRITE_ON_EXPORT = "overwrite_on_export"
    private const val KEY_ONLINE_LOOKUP = "online_lookup"
    private const val KEY_AUTO_UPDATE_CHECK = "auto_update_check"
    private const val KEY_AUTO_INSTALL_UPDATES = "auto_install_updates"
    private const val KEY_UPDATE_SKIP_VERSION = "update_skip_version"
    private const val KEY_UPDATE_LAST_CHECK_MS = "update_last_check_ms"
    private const val KEY_TOOLBAR_HORIZONTAL = "toolbar_horizontal"
    private const val KEY_OPDS_URL = "opds_url"
    private const val KEY_OPDS_USER = "opds_user"
    private const val KEY_OPDS_PASSWORD = "opds_password"

    private fun prefs(c: Context) = c.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /**
     * When true, exporting annotations replaces the original document instead of writing a separate
     * `-annotated` copy beside it. Off by default — overwriting is destructive and irreversible
     * (ADR-INKREAD-0005), so the safe copy is the floor.
     */
    fun overwriteOnExport(c: Context): Boolean = prefs(c).getBoolean(KEY_OVERWRITE_ON_EXPORT, false)

    fun setOverwriteOnExport(c: Context, value: Boolean) =
        prefs(c).edit().putBoolean(KEY_OVERWRITE_ON_EXPORT, value).apply()

    /**
     * When true, a dictionary miss falls back to an online (Wiktionary) lookup, waking the radio.
     * Off by default — inkread is offline-first, and a definitive opt-in keeps looked-up words from
     * leaving the device without the reader's say-so (the review's privacy/power note).
     */
    fun onlineLookup(c: Context): Boolean = prefs(c).getBoolean(KEY_ONLINE_LOOKUP, false)

    fun setOnlineLookup(c: Context, value: Boolean) =
        prefs(c).edit().putBoolean(KEY_ONLINE_LOOKUP, value).apply()

    /**
     * When true the floating tool palette runs **across the top** instead of down the right edge
     * (#200).
     *
     * Off by default, because the vertical pill is what everyone has been using. The horizontal form
     * exists because the two forms cost different things: the strip is a fixed, finger-sized width
     * while the page margin is a fraction of the panel, so on a narrower device the pill is wider
     * than the margin and clips the end of every line it spans — around twenty of them. Across the
     * top it clips part of a single line instead, and the smaller the screen the bigger that
     * difference is.
     */
    fun toolbarHorizontal(c: Context): Boolean = prefs(c).getBoolean(KEY_TOOLBAR_HORIZONTAL, false)

    fun setToolbarHorizontal(c: Context, value: Boolean) =
        prefs(c).edit().putBoolean(KEY_TOOLBAR_HORIZONTAL, value).apply()

    // ── Self-update (ADR-INKREAD-0014) ────────────────────────────────────────────────────────────

    /**
     * When true, inkread checks GitHub for a newer release on launch and prompts to install it
     * (UPD-FR6/FR8). On by default — a sideloaded build has no store to deliver fixes, so surfacing
     * them is the floor; the check is a single on-launch request, never background polling.
     */
    fun autoUpdateCheck(c: Context): Boolean = prefs(c).getBoolean(KEY_AUTO_UPDATE_CHECK, true)

    fun setAutoUpdateCheck(c: Context, value: Boolean) =
        prefs(c).edit().putBoolean(KEY_AUTO_UPDATE_CHECK, value).apply()

    /**
     * When true, a detected update is downloaded + verified and sent straight to the system
     * installer without the in-app prompt (UPD-FR7). Off by default — auto-installing without an
     * explicit opt-in is surprising and uses data. Only effective while [autoUpdateCheck] is on
     * ([autoInstallEffective]); Android still shows its own final install confirmation.
     */
    fun autoInstallUpdates(c: Context): Boolean = prefs(c).getBoolean(KEY_AUTO_INSTALL_UPDATES, false)

    fun setAutoInstallUpdates(c: Context, value: Boolean) =
        prefs(c).edit().putBoolean(KEY_AUTO_INSTALL_UPDATES, value).apply()

    /** Auto-install acts only when both it and the launch check are enabled (UPD-FR8). */
    fun autoInstallEffective(c: Context): Boolean = autoUpdateCheck(c) && autoInstallUpdates(c)

    /**
     * The release version the reader chose to skip (UPD-FR6 "Skip this version"), or "" for none.
     * A matching candidate is suppressed; any newer version clears past it.
     */
    fun updateSkipVersion(c: Context): String =
        prefs(c).getString(KEY_UPDATE_SKIP_VERSION, "").orEmpty()

    fun setUpdateSkipVersion(c: Context, version: String) =
        prefs(c).edit().putString(KEY_UPDATE_SKIP_VERSION, version).apply()

    /** Epoch-ms of the last completed update check (UPD-FR6 throttle); 0 if never. */
    fun updateLastCheckMs(c: Context): Long = prefs(c).getLong(KEY_UPDATE_LAST_CHECK_MS, 0L)

    fun setUpdateLastCheckMs(c: Context, ms: Long) =
        prefs(c).edit().putLong(KEY_UPDATE_LAST_CHECK_MS, ms).apply()

    // ---- OPDS library: calibre content server / Calibre-Web (ADR-INKREAD-0016, #175) ----

    /**
     * The catalog server's address, as the reader typed it ("" = no library configured, which is
     * what hides the Home entry). [OpdsController.catalogRoot] normalizes it before use, so a bare
     * host is fine here.
     */
    fun opdsUrl(c: Context): String = prefs(c).getString(KEY_OPDS_URL, "").orEmpty()

    fun setOpdsUrl(c: Context, value: String) =
        prefs(c).edit().putString(KEY_OPDS_URL, value.trim()).apply()

    /** Basic-auth username; "" when the server allows anonymous access (calibre's default). */
    fun opdsUser(c: Context): String = prefs(c).getString(KEY_OPDS_USER, "").orEmpty()

    fun setOpdsUser(c: Context, value: String) =
        prefs(c).edit().putString(KEY_OPDS_USER, value.trim()).apply()

    /**
     * Basic-auth password, stored **in plain text** like every other preference here.
     *
     * That is a deliberate, disclosed choice rather than an oversight (ADR-INKREAD-0016 Decision 4):
     * on a sideloaded app that already holds MANAGE_EXTERNAL_STORAGE on a single-user device, a
     * keystore wrapper would raise the effort of an attack who is already inside by approximately
     * nothing. The Settings screen says so plainly instead of implying a protection that is not there.
     */
    fun opdsPassword(c: Context): String = prefs(c).getString(KEY_OPDS_PASSWORD, "").orEmpty()

    fun setOpdsPassword(c: Context, value: String) =
        prefs(c).edit().putString(KEY_OPDS_PASSWORD, value).apply()

    /** Whether a library is configured at all — gates the Home entry point. */
    fun opdsConfigured(c: Context): Boolean = opdsUrl(c).isNotBlank()
}
