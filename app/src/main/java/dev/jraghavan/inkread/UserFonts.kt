package dev.jraghavan.inkread

import android.content.Context
import android.net.Uri
import android.util.Log
import java.io.File

/**
 * The reader's own imported reading faces (RR28-FR3), kept in `filesDir/fonts` (the `fonts/`
 * directory of RR22-FR1).
 *
 * The shell owns the directory and hands the core raw **bytes**, exactly as [FallbackFonts] does
 * for system faces — the core never learns a filesystem path (IR-7). Font ids are positional: the
 * core numbers user faces after the bundled families in registration order, so registration is
 * always the directory sorted by name, and the whole registry is rebuilt after an import or a
 * removal rather than appended to.
 */
object UserFonts {
    private const val TAG = "UserFonts"

    /** What the core's parser accepts. A TrueType collection needs a face index, so it is not here. */
    private val SUPPORTED = setOf("ttf", "otf")

    /** Largest font we will copy in — a guard against a mis-picked file, not a format limit. Well
     *  under [Books]' document cap: no real typeface is anywhere near this. */
    private const val MAX_BYTES = 32L * 1024 * 1024

    /** The import directory, created on demand. */
    fun dir(context: Context): File = File(context.filesDir, "fonts").apply { mkdirs() }

    /** The imported font files, in the stable order they are registered in. */
    fun files(context: Context): List<File> =
        dir(context)
            .listFiles { f -> f.isFile && f.extension.lowercase() in SUPPORTED }
            ?.sortedBy { it.name.lowercase() }
            .orEmpty()

    /** What the picker shows for a font file: its name without the extension. */
    fun displayName(file: File): String = file.nameWithoutExtension

    /**
     * Re-register every imported face with the core, replacing whatever was registered before.
     * Call once at startup and again after any import or removal — ids shift when the set changes,
     * so a partial update would leave the picker pointing at the wrong face.
     */
    fun register(context: Context) {
        try {
            NativeBridge.nativeClearReadingFonts()
        } catch (e: RuntimeException) {
            Log.e(TAG, "clear failed: ${e.message}")
            return
        }
        for (f in files(context)) {
            val id = try {
                NativeBridge.nativeRegisterReadingFont(displayName(f), f.readBytes())
            } catch (e: Exception) {
                // Exception, not RuntimeException: readBytes throws IOException if the file goes
                // away mid-read, and one bad font must never abort the rest of the list.
                Log.e(TAG, "register ${f.name} failed: ${e.message}")
                -1
            }
            if (id < 0) Log.w(TAG, "not a usable font, skipped: ${f.name}")
        }
    }

    /**
     * Copy a picked font into `fonts/` and re-register, so it appears in the picker. Returns the
     * stored file, or `null` if it could not be read, is not a font the core can parse, or is
     * absurdly large — in which case nothing is left behind.
     */
    fun import(context: Context, uri: Uri, suggestedName: String?): File? {
        val name = suggestedName ?: Books.queryName(context, uri)
        val stem = Books.sanitizeStem(name?.substringBeforeLast('.').orEmpty().ifBlank { "font" })
        val ext = name?.substringAfterLast('.', "")?.lowercase()
            ?.takeIf { it in SUPPORTED } ?: "ttf"
        val dest = uniqueFile(context, stem, ext)
        // Books' capped copy, not a plain copyTo: it aborts mid-write once the source runs past the
        // cap, rather than filling storage with a file we would only then delete.
        val copied = try {
            context.contentResolver.openInputStream(uri)?.use { input ->
                dest.outputStream().use { out -> Books.copyCapped(input, out, MAX_BYTES) }
            } ?: false
        } catch (e: Exception) {
            Log.e(TAG, "import failed: ${e.message}")
            false
        }
        if (!copied || dest.length() == 0L) {
            dest.delete()
            return null
        }
        // Validate by asking the core to parse it: a file that will not register would sit in the
        // directory forever, invisible in the picker and impossible to explain.
        val usable = try {
            NativeBridge.nativeRegisterReadingFont(displayName(dest), dest.readBytes()) >= 0
        } catch (e: Exception) {
            Log.e(TAG, "validate failed: ${e.message}")
            false
        }
        register(context) // rebuild the registry either way, so ids stay in directory order
        if (!usable) {
            dest.delete()
            register(context)
            return null
        }
        return dest
    }

    /** Delete an imported font and re-register the rest. */
    fun remove(context: Context, file: File): Boolean {
        val gone = file.delete()
        register(context)
        return gone
    }

    /** `stem-2.ttf`, `stem-3.ttf`… so importing the same name twice doesn't overwrite. */
    private fun uniqueFile(context: Context, stem: String, ext: String): File {
        val dir = dir(context)
        var candidate = File(dir, "$stem.$ext")
        var n = 2
        while (candidate.exists()) {
            candidate = File(dir, "$stem-$n.$ext")
            n++
        }
        return candidate
    }
}
