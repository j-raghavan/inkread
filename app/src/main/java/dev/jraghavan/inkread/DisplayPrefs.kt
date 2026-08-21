package dev.jraghavan.inkread

import android.content.Context
import android.content.SharedPreferences
import android.content.pm.ActivityInfo

/**
 * Persisted display + typography settings (RR4), extracted from `ReaderActivity` (SRP). Two
 * stores mirror the original prefs files — `"display"` (contrast/night/fit/crop/quality/
 * orientation/style preset) and `"typography"` (text scale/typeface/line spacing/alignment) —
 * with unchanged keys, so existing installs keep their settings.
 *
 * `SharedPreferences` is thread-safe; these are read on the engine thread when a document
 * (re)opens and written from the settings-sheet UI.
 */
class DisplayPrefs(private val context: Context) {

    private val display: SharedPreferences
        get() = context.getSharedPreferences("display", Context.MODE_PRIVATE)
    private val typography: SharedPreferences
        get() = context.getSharedPreferences("typography", Context.MODE_PRIVATE)

    // ---- "display" store ----

    var contrast: Int
        get() = display.getInt("contrast", 0).coerceIn(0, CONTRAST_MAX)
        set(step) = display.edit().putInt("contrast", step).apply()

    var night: Boolean
        get() = display.getBoolean("night", false)
        set(on) = display.edit().putBoolean("night", on).apply()

    var cropAuto: Boolean
        get() = display.getBoolean("crop_auto", false)
        set(v) = display.edit().putBoolean("crop_auto", v).apply()

    var cropMargin: Int
        get() = display.getInt("crop_margin", 1).coerceIn(0, 8)
        set(v) = display.edit().putInt("crop_margin", v).apply()

    /** Page fit mode; the index mirrors the core `FitMode` code (0 = Page/contain). */
    var fit: Int
        get() = display.getInt("fit", 0)
        set(mode) = display.edit().putInt("fit", mode).apply()

    var orientation: Int
        get() = display.getInt("orientation", ActivityInfo.SCREEN_ORIENTATION_PORTRAIT)
        set(o) = display.edit().putInt("orientation", o).apply()

    var stylePreset: String
        get() = display.getString("style_preset", "Original") ?: "Original"
        set(name) = display.edit().putString("style_preset", name).apply()

    var renderQuality: Int
        get() = display.getInt("render_quality", 1).coerceIn(0, 2)
        set(q) = display.edit().putInt("render_quality", q).apply()

    /** UI-chrome scale for menus/sheets (#133). Larger panels (e.g. the Manta) want bigger menus;
     *  this scales the reader's chrome text/controls independently of [textScale] (the reading body
     *  text). 1.0 = the current sizing. Read clamped to the [UI_SCALES] range. */
    var uiScale: Float
        get() = display.getFloat("ui_scale", 1.0f).coerceIn(UI_SCALES.first(), UI_SCALES.last())
        set(s) = display.edit().putFloat("ui_scale", s).apply()

    /** Periodic full (flashing) refresh cadence (#99): force a FULL EPD refresh every Nth page-turn
     *  to clear accumulated e-ink ghosting. 0 = Off (partial refreshes only; the manual "Refresh
     *  now" still works). Read coerced to a valid [REFRESH_INTERVALS] step. */
    var fullRefreshEvery: Int
        get() = display.getInt("full_refresh_every", 0).let { if (it in REFRESH_INTERVALS) it else 0 }
        set(n) = display.edit().putInt("full_refresh_every", n).apply()

    // ---- "typography" store ----

    var textScale: Float
        get() = typography.getFloat("scale", 1.0f)
        set(scale) = typography.edit().putFloat("scale", scale).apply()

    var font: Int
        get() = typography.getInt("font_id", 0)
        set(id) = typography.edit().putInt("font_id", id).apply()

    /** The saved line-spacing multiplier (value-based; new key, so a changed option set never
     *  mis-maps an old index). Defaults to the core default 1.4. */
    var lineSpacingMult: Float
        get() = typography.getInt("line_spacing_x100", (DEFAULT_LINE_SPACING * 100).toInt()) / 100f
        set(m) = typography.edit().putInt("line_spacing_x100", Math.round(m * 100)).apply()

    var alignment: Int
        get() = typography.getInt("alignment", 0).coerceIn(0, 3)
        set(i) = typography.edit().putInt("alignment", i).apply()

    /** Text columns per page (#194): 1 or 2. Stored per reader, applied where the page can take it. */
    var columns: Int
        get() = typography.getInt("columns", 1).coerceIn(1, 2)
        set(i) = typography.edit().putInt("columns", i.coerceIn(1, 2)).apply()

    /** The segmented index for the saved multiplier (nearest [LINE_SPACINGS] entry; default Medium). */
    fun lineSpacingIndex(): Int {
        val m = lineSpacingMult
        val i = LINE_SPACINGS.indexOfFirst { kotlin.math.abs(it - m) < 0.001f }
        return if (i >= 0) i else {
            LINE_SPACINGS.indexOfFirst { kotlin.math.abs(it - DEFAULT_LINE_SPACING) < 0.001f }.coerceAtLeast(0)
        }
    }

    companion object {
        const val CONTRAST_MAX = 8 // mirrors reader-core render::contrast::MAX_CONTRAST_STEP (RR4).

        // Line-spacing multipliers (RR4), tight → loose. Stored as the value (not the index) so this
        // set can grow without corrupting saved prefs. 1.4 = the core default.
        val LINE_SPACINGS = floatArrayOf(1.0f, 1.1f, 1.2f, 1.4f, 1.7f)
        val LINE_SPACING_LABELS = listOf("Tightest", "Tighter", "Small", "Medium", "Large")
        const val DEFAULT_LINE_SPACING = 1.4f

        val STYLE_PRESETS = listOf("Original", "Bold", "Night", "Outdoor", "Relaxed") // 1.10

        /** Reflow font-size steps (multiples of the core's base body size); 1.0 = default. */
        val TEXT_SCALES = floatArrayOf(0.6f, 0.7f, 0.8f, 0.9f, 1.0f, 1.15f, 1.3f, 1.5f, 1.75f, 2.0f, 2.5f, 3.0f)

        /** Menu/chrome scale steps (#133); 1.0 = current sizing, the default (index 1). Kept small
         *  and coarse so the segmented control stays e-ink-tappable. */
        val UI_SCALES = floatArrayOf(0.9f, 1.0f, 1.15f, 1.3f, 1.5f)
        val UI_SCALE_LABELS = listOf("S", "M", "L", "XL", "2XL")

        /** Page-turn intervals for the periodic full refresh (#99); index 0 = Off, the default. */
        val REFRESH_INTERVALS = listOf(0, 1, 3, 5, 10)
        val REFRESH_INTERVAL_LABELS = listOf("Off", "1", "3", "5", "10")

        /** Index of the entry in [values] nearest to [target]. */
        private fun nearestIndex(values: FloatArray, target: Float): Int {
            var best = 0
            var bestDist = Float.MAX_VALUE
            values.forEachIndexed { i, v ->
                val dist = kotlin.math.abs(v - target)
                if (dist < bestDist) { bestDist = dist; best = i }
            }
            return best
        }

        /** Index of the [TEXT_SCALES] entry nearest to [scale]. */
        fun nearestScaleIndex(scale: Float): Int = nearestIndex(TEXT_SCALES, scale)

        /** Index of the [UI_SCALES] entry nearest to [scale]. */
        fun nearestUiScaleIndex(scale: Float): Int = nearestIndex(UI_SCALES, scale)
    }
}
