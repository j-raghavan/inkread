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
}
