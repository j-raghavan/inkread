package dev.jraghavan.inkread

import android.content.Context
import android.os.Environment
import java.io.File

/**
 * Discovery + install bookkeeping for user-installed StarDict dictionaries (KOReader-style, RR12 /
 * ADR-INKREAD-0009 D2). The user drops a StarDict folder (`*.ifo` + `*.idx` + `*.dict[.dz]`, optional
 * `*.syn`) into one of the [roots]; inkread lists them, and [NativeBridge.nativeDictImport] compiles
 * each into the writable `dict.db` the reader already opens. The Rust core owns parsing/compilation;
 * this object is pure file discovery + a tiny installed-set in SharedPreferences.
 */
object Dictionaries {
    /** A discovered StarDict bundle on disk (not necessarily installed yet). */
    data class Bundle(val name: String, val dir: File, val sourceTag: String, val installed: Boolean)

    /**
     * Scan roots, most natural first:
     *  - `…/inkread/dict/` — inkread's own drop-in folder (the documented home).
     *  - `…/MyStyle/SnDict/` — the Supernote dictionary plugin's folder, so dictionaries already on
     *    the device are offered without re-copying.
     */
    fun roots(): List<File> {
        val ext = Environment.getExternalStorageDirectory()
        return listOf(File(ext, "inkread/dict"), File(ext, "MyStyle/SnDict"))
    }

    /** inkread's own drop-in folder (created on demand) so the manage screen can name a real path. */
    fun homeRoot(): File = File(Environment.getExternalStorageDirectory(), "inkread/dict").apply { mkdirs() }

    /** Every StarDict bundle found across [roots], de-duplicated by source tag (home wins). */
    fun discover(context: Context): List<Bundle> {
        val installed = installedTags(context)
        val seen = HashSet<String>()
        val out = ArrayList<Bundle>()
        for (root in roots()) {
            val subdirs = root.listFiles { f -> f.isDirectory } ?: continue
            for (dir in subdirs.sortedBy { it.name.lowercase() }) {
                if (!isStarDict(dir)) continue
                val tag = sourceTag(dir)
                if (!seen.add(tag)) continue
                out.add(Bundle(displayName(dir), dir, tag, installed.contains(tag)))
            }
        }
        return out
    }

    /** A folder is a StarDict bundle when it holds both an `*.ifo` and an `*.idx`. */
    fun isStarDict(dir: File): Boolean {
        val files = dir.listFiles() ?: return false
        return files.any { it.name.endsWith(".ifo") } && files.any { it.name.endsWith(".idx") }
    }

    /**
     * The lang/source id stored with every entry (and used to uninstall). Derived from the folder
     * name so it is stable + human-meaningful; lowercased and sanitized for safe SQL/text use.
     */
    fun sourceTag(dir: File): String =
        dir.name.lowercase().replace(Regex("[^a-z0-9._-]"), "-").trim('-').ifEmpty { "dict" }

    /** Prefer the StarDict `bookname=` from the `.ifo`; fall back to the folder name. */
    fun displayName(dir: File): String {
        val ifo = dir.listFiles { f -> f.name.endsWith(".ifo") }?.firstOrNull() ?: return dir.name
        return try {
            ifo.readLines().firstNotNullOfOrNull { line ->
                line.substringAfter("bookname=", "").trim().ifEmpty { null }
            } ?: dir.name
        } catch (e: Exception) {
            dir.name
        }
    }

    // ---- installed set (SharedPreferences) ----

    private fun prefs(context: Context) = context.getSharedPreferences("dictionaries", Context.MODE_PRIVATE)

    private fun installedTags(context: Context): Set<String> =
        prefs(context).getStringSet(KEY_INSTALLED, emptySet()) ?: emptySet()

    fun isInstalled(context: Context, tag: String): Boolean = installedTags(context).contains(tag)

    fun markInstalled(context: Context, tag: String) {
        prefs(context).edit().putStringSet(KEY_INSTALLED, installedTags(context) + tag).apply()
    }

    fun markRemoved(context: Context, tag: String) {
        prefs(context).edit().putStringSet(KEY_INSTALLED, installedTags(context) - tag).apply()
    }

    private const val KEY_INSTALLED = "installed"
}
