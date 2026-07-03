package dev.jraghavan.inkread

import android.util.Log
import java.io.File

/**
 * Registers the device's system fonts as reflow **fallback faces** (any-script text).
 *
 * The Rust core bundles Latin reading faces only, so scripts outside them (Chinese, Japanese,
 * Korean) rendered as `.notdef` □ boxes. Android ships pan-CJK Noto faces under `/system/fonts`;
 * the shell — the vendor-aware side of the IR-7 seam — reads them and hands **raw bytes** to the
 * core's process-wide fallback chain ([NativeBridge.nativeRegisterFallbackFont]). Registered once
 * per process, before a document opens; ~a one-off 20 MB read for the CJK collection.
 *
 * Scope (Tier 1): scripts that render correctly one glyph per character. Arabic/Hebrew/Indic need
 * shaping + BiDi (a Tier 2 effort) and are deliberately **not** registered — an unshaped joining
 * script renders misleadingly wrong letterforms, which is worse than an honest box.
 */
object FallbackFonts {
    private const val TAG = "FallbackFonts"

    @Volatile private var registered = false

    /** Marks candidates that carry the pan-CJK repertoire — one success covers all of CJK. */
    private data class Candidate(val path: String, val ttcIndex: Int, val cjk: Boolean)

    /** Candidate system faces, tried in order. */
    private val candidates = listOf(
        // Pan-CJK collection: every face in it carries the full Han repertoire plus kana + hangul;
        // the index only selects the regional glyph *style*. AOSP fonts.xml maps zh-Hans to face 2
        // — prefer it, and retry face 0 on builds whose collection is ordered differently.
        Candidate("/system/fonts/NotoSansCJK-Regular.ttc", 2, cjk = true),
        Candidate("/system/fonts/NotoSansCJK-Regular.ttc", 0, cjk = true),
        // Split-per-language builds ship a standalone Simplified Chinese face:
        Candidate("/system/fonts/NotoSansSC-Regular.otf", 0, cjk = true),
        // Legacy pan-CJK fallback on older/lean builds:
        Candidate("/system/fonts/DroidSansFallback.ttf", 0, cjk = true),
        // Symbol coverage beyond the bundled Noto Music (arrows, geometric shapes, dingbats):
        Candidate("/system/fonts/NotoSansSymbols-Regular-Subsetted.ttf", 0, cjk = false),
        Candidate("/system/fonts/NotoSansSymbols-Regular-Subsetted2.ttf", 0, cjk = false),
    )

    /** Register the chain once; safe to call from any thread before opening a document. */
    fun ensureRegistered() {
        if (registered) return
        synchronized(this) {
            if (registered) return
            var cjkDone = false
            for (c in candidates) {
                if (c.cjk && cjkDone) continue // one pan-CJK face is a full repertoire
                val file = File(c.path)
                if (!file.exists() || !file.canRead()) continue
                val ok = try {
                    NativeBridge.nativeRegisterFallbackFont(file.readBytes(), c.ttcIndex)
                } catch (e: Exception) {
                    // Exception, not RuntimeException: readBytes throws IOException on a mid-read
                    // failure (TOCTOU after the canRead check), and one bad candidate must never
                    // abort the loop — or propagate out and kill the book open.
                    Log.e(TAG, "register ${c.path}#${c.ttcIndex} failed: ${e.message}")
                    false
                }
                if (ok && c.cjk) cjkDone = true
                Log.i(TAG, "fallback ${c.path}#${c.ttcIndex} → $ok")
            }
            registered = true
        }
    }
}
