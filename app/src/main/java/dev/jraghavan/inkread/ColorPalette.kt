package dev.jraghavan.inkread

import android.app.Activity
import android.graphics.Color
import android.graphics.drawable.GradientDrawable
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.TextView

/**
 * A color-swatch popup (ADR-INKREAD-0010 — NeoReader's brush "Colors" row, video frame 129): a
 * titled row of filled circle swatches, the selected one ringed, each captioned with its name.
 * Colors are stored true per stroke; on the MONOCHROME Supernote the swatches render as greys, so
 * the name caption is what disambiguates them. Tapping a swatch picks it and dismisses.
 */
class ColorPalette(
    private val activity: Activity,
    private val host: FrameLayout,
) {
    private val density = activity.resources.displayMetrics.density
    private fun dp(v: Int) = (v * density).toInt()
    private var popup: android.widget.PopupWindow? = null

    /**
     * Show the palette. [colors] are packed `r<<24|g<<16|b<<8|a`; [names] parallel to them;
     * [selected] is ringed; [onPick] receives the chosen index.
     */
    fun show(title: String, colors: IntArray, names: Array<String>, selected: Int, onPick: (Int) -> Unit) {
        dismiss()
        val col = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            background = Ink.cardBg()
            setPadding(Ink.dp(20), Ink.dp(16), Ink.dp(20), Ink.dp(16))
        }
        col.addView(Ink.eyebrow(activity, title).apply { setPadding(0, 0, 0, Ink.dp(12)) })
        val row = LinearLayout(activity).apply { orientation = LinearLayout.HORIZONTAL }
        val win = android.widget.PopupWindow(col, ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
            isOutsideTouchable = true; isFocusable = true
        }
        colors.forEachIndexed { i, c ->
            row.addView(swatchCell(c, names.getOrElse(i) { "" }, i == selected) {
                onPick(i); win.dismiss()
            })
        }
        col.addView(row)
        popup = win
        win.showAtLocation(host, Gravity.CENTER, 0, 0)
    }

    fun dismiss() { popup?.dismiss(); popup = null }

    private fun swatchCell(packed: Int, name: String, selected: Boolean, onTap: () -> Unit): View {
        val r = (packed ushr 24) and 0xFF
        val g = (packed ushr 16) and 0xFF
        val b = (packed ushr 8) and 0xFF
        val opaque = Color.rgb(r, g, b) // show the swatch at full opacity even for translucent inks
        val cell = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setPadding(dp(10), dp(6), dp(10), dp(6))
            isClickable = true
            setOnClickListener { onTap() }
        }
        val side = dp(40)
        cell.addView(View(activity).apply {
            layoutParams = LinearLayout.LayoutParams(side, side)
            background = GradientDrawable().apply {
                shape = GradientDrawable.OVAL
                setColor(opaque)
                // Selected = thick black ring; others = thin grey ring so light swatches still read.
                setStroke(if (selected) dp(4) else Ink.hair(), if (selected) Ink.ink else Ink.ringSoft)
            }
        })
        cell.addView(TextView(activity).apply {
            text = name
            textSize = 11f
            typeface = Ink.mono
            letterSpacing = 0.04f
            gravity = Gravity.CENTER
            setTextColor(if (selected) Ink.ink else Ink.muted)
            setPadding(0, dp(6), 0, 0)
        })
        return cell
    }
}
