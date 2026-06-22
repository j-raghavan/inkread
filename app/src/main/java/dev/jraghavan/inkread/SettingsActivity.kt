package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView

/**
 * App-level settings, reached from the home gear. A calm, monochrome, scrollable page matching the
 * "Inkwell" home: an **Export** preference (safe `-annotated` copy vs. overwrite the original), a
 * **How it works** cheatsheet of the non-obvious features, and an **About** block. Programmatic
 * Views, like the rest of the shell; backed by [AppSettings].
 */
class SettingsActivity : Activity() {

    private val density get() = resources.displayMetrics.density
    private fun dp(v: Int) = (v * density).toInt()

    private val serif = Typeface.SERIF
    private val serifItalic = Typeface.create(Typeface.SERIF, Typeface.ITALIC)
    private val mono = Typeface.MONOSPACE
    private val script by lazy { Typeface.createFromAsset(assets, "fonts/pinyon_script.ttf") }

    private val ink = Color.BLACK
    private val inkSoft = Color.parseColor("#3A3A3A")
    private val textMuted = Color.parseColor("#757575")

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(buildView())
    }

    private fun refresh() = setContentView(buildView())

    private fun buildView(): View {
        val side = dp(28)
        val column = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(side, dp(24), side, dp(40))
            layoutParams = ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
        }

        // ── Title bar: back · "Settings" ─────────────────────────────────────────
        column.addView(LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            addView(TextView(this@SettingsActivity).apply {
                text = "←"; setTextColor(ink); textSize = 26f; typeface = serif
                isClickable = true; setOnClickListener { finish() }
                setPadding(0, 0, dp(16), 0)
            })
            addView(TextView(this@SettingsActivity).apply {
                text = "Settings"; setTextColor(ink); textSize = 28f; typeface = serif
            })
        })
        column.addView(spacer(dp(28)))

        // ── Export ───────────────────────────────────────────────────────────────
        column.addView(eyebrow("Export"))
        column.addView(spacer(dp(12)))
        column.addView(
            toggleRow(
                title = "Overwrite original on export",
                desc = "When on, exporting replaces your original PDF with the annotated version " +
                    "instead of saving a separate “-annotated” copy beside it. This can’t be undone.",
                on = AppSettings.overwriteOnExport(this),
            ) { onExportOverwriteTapped() },
        )

        // ── How it works ───────────────────────────────────────────────────────────
        column.addView(spacer(dp(34)))
        column.addView(eyebrow("How it works"))
        column.addView(spacer(dp(8)))
        for ((title, desc) in HELP) column.addView(helpRow(title, desc))

        // ── About ──────────────────────────────────────────────────────────────────
        column.addView(spacer(dp(34)))
        column.addView(eyebrow("About"))
        column.addView(spacer(dp(10)))
        column.addView(TextView(this).apply {
            text = "InkRead"; setTextColor(ink); textSize = 40f; typeface = Typeface.create(script, Typeface.BOLD)
            paint.isFakeBoldText = true; includeFontPadding = false
        })
        column.addView(TextView(this).apply {
            text = "A handwriting-first reader for e-ink."
            setTextColor(inkSoft); textSize = 15f; typeface = serifItalic; setPadding(0, dp(4), 0, 0)
        })
        column.addView(TextView(this).apply {
            text = "Version ${versionName()}   ·   AGPL-3.0"
            setTextColor(textMuted); textSize = 13f; typeface = mono; letterSpacing = 0.04f
            setPadding(0, dp(8), 0, 0)
        })

        return ScrollView(this).apply {
            setBackgroundColor(Color.WHITE)
            isVerticalScrollBarEnabled = false
            addView(column)
        }
    }

    /** Turning overwrite ON is destructive, so confirm; turning it OFF is immediate. */
    private fun onExportOverwriteTapped() {
        if (AppSettings.overwriteOnExport(this)) {
            AppSettings.setOverwriteOnExport(this, false); refresh(); return
        }
        AlertDialog.Builder(this, R.style.InkDialog)
            .setTitle("Overwrite originals on export?")
            .setMessage("Exporting will replace your original PDF with the annotated version instead of saving a separate “-annotated” copy. This can’t be undone.")
            .setPositiveButton("Overwrite") { _, _ -> AppSettings.setOverwriteOnExport(this, true); refresh() }
            .setNegativeButton("Keep safe copy", null)
            .show()
    }

    // ── Pieces ──────────────────────────────────────────────────────────────────────

    /** A setting with a title + description on the left and an On/Off pill on the right. */
    private fun toggleRow(title: String, desc: String, on: Boolean, onTap: () -> Unit): View =
        LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            isClickable = true; setOnClickListener { onTap() }
            addView(LinearLayout(this@SettingsActivity).apply {
                orientation = LinearLayout.VERTICAL
                addView(TextView(this@SettingsActivity).apply {
                    text = title; setTextColor(ink); textSize = 17f; typeface = serif
                })
                addView(TextView(this@SettingsActivity).apply {
                    text = desc; setTextColor(textMuted); textSize = 13f; typeface = serif
                    setLineSpacing(0f, 1.15f); setPadding(0, dp(3), 0, 0)
                })
            }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply { marginEnd = dp(16) })
            addView(pill(if (on) "On" else "Off", on))
        }

    /** A bordered status pill — filled (black) when on, outlined when off. */
    private fun pill(label: String, filled: Boolean): View = TextView(this).apply {
        text = label.uppercase(); textSize = 13f; typeface = mono; letterSpacing = 0.1f
        gravity = Gravity.CENTER; setPadding(dp(18), dp(7), dp(18), dp(7))
        setTextColor(if (filled) Color.WHITE else ink)
        background = GradientDrawable().apply {
            setColor(if (filled) ink else Color.WHITE)
            setStroke(maxOf(1, dp(1)), ink); cornerRadius = dp(40).toFloat()
        }
        minWidth = dp(64)
    }

    private fun helpRow(title: String, desc: String): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(0, dp(12), 0, 0)
        addView(TextView(this@SettingsActivity).apply {
            text = title; setTextColor(ink); textSize = 16f; typeface = Typeface.create(serif, Typeface.BOLD)
        })
        addView(TextView(this@SettingsActivity).apply {
            text = desc; setTextColor(inkSoft); textSize = 14f; typeface = serif
            setLineSpacing(0f, 1.18f); setPadding(0, dp(2), 0, 0)
        })
    }

    private fun eyebrow(text: String): TextView = TextView(this).apply {
        this.text = text.uppercase(); setTextColor(ink); textSize = 12f
        typeface = mono; letterSpacing = 0.14f
    }

    private fun spacer(h: Int): View = View(this).apply {
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, h)
    }

    private fun versionName(): String =
        try { packageManager.getPackageInfo(packageName, 0).versionName ?: "" } catch (e: Exception) { "" }

    private companion object {
        /** Concise, accurate cheatsheet of the non-obvious features (title → one line). */
        val HELP = listOf(
            "Pen & Highlighter" to "Write and highlight on the page with the stylus; the floating tool palette switches between them.",
            "Eraser" to "Switch to the eraser in the tool palette to wipe strokes.",
            "Lasso select" to "Circle ink or text to move, copy, delete, or look it up.",
            "Define" to "Select a word and tap Define to look it up in the on-device dictionaries.",
            "Search" to "Find text anywhere in the document and jump between matches.",
            "Bookmarks" to "Mark pages and return to them from the marks list.",
            "Export" to "Save your annotations as a PDF into a synced folder — or overwrite the original (see above).",
            "Display & Font" to "Tune contrast, margins, page crop, and reflow font from the reader’s Adjust sheet.",
        )
    }
}
