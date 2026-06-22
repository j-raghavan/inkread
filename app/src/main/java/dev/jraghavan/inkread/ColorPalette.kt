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
 * A color-swatch **column** (ADR-INKREAD-0010 — NeoReader's brush "Colors" row): a vertical stack of
 * filled circle swatches, the selected one ringed, each captioned with its name. Colors are stored
 * true per stroke; on the MONOCHROME Supernote the swatches render as greys, so the name caption is
 * what disambiguates them.
 *
 * CRITICAL — this is an **in-window overlay view** added to the activity's root layout, NOT a
 * PopupWindow/Dialog, and once shown it **stays put**: picking a colour restyles the rings IN PLACE
 * rather than removing/re-adding the view. The reasons, both proven on-device:
 *   1. A separate window steals input focus, and the Supernote firmware drops its live-ink overlay
 *      (the only thing that displays committed strokes) the instant another window takes focus — so
 *      a popup palette erased the user's ink.
 *   2. Each time an overlay view is added to / removed from the host the firmware does a full
 *      auto-refresh of the window, which repaints the page from the app surface and wipes the
 *      firmware ink overlay — so toggling the column off and on again erased the notes. Keeping the
 *      view mounted and mutating it in place avoids that churn entirely.
 */
class ColorPalette(
    private val activity: Activity,
    private val host: FrameLayout,
) {
    private val density = activity.resources.displayMetrics.density
    private fun dp(v: Int) = (v * density).toInt()

    private var panel: LinearLayout? = null
    private var circles: MutableList<View> = mutableListOf()
    private var labels: MutableList<TextView> = mutableListOf()
    private var palette: IntArray = IntArray(0)

    /** Whether the column is currently mounted. */
    fun isShowing(): Boolean = panel != null

    /**
     * Show the column, or — if it's already up for the same palette — just move the selection ring.
     * [colors] are packed `r<<24|g<<16|b<<8|a`; [names] parallel; [selected] ringed; [onPick] gets the
     * chosen index. Never auto-dismisses: the caller hides it only on a tool switch.
     */
    fun show(title: String, colors: IntArray, names: Array<String>, selected: Int, onPick: (Int) -> Unit) {
        // Already mounted for this same palette → restyle in place, no remove/add (no EPD churn).
        if (panel != null && colors.contentEquals(palette)) {
            restyle(selected)
            return
        }
        dismiss()
        palette = colors.copyOf()
        circles = mutableListOf()
        labels = mutableListOf()
        val col = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            background = Ink.cardBg(22)
            setPadding(Ink.dp(10), Ink.dp(12), Ink.dp(10), Ink.dp(12))
        }
        col.addView(Ink.eyebrow(activity, title).apply { setPadding(0, 0, 0, Ink.dp(8)) })
        colors.forEachIndexed { i, c ->
            col.addView(swatchCell(c, names.getOrElse(i) { "" }, i == selected) {
                onPick(i)
                restyle(i) // update the ring in place — DON'T remove/re-add (that wipes the ink)
            })
        }
        val lp = FrameLayout.LayoutParams(
            ViewGroup.LayoutParams.WRAP_CONTENT,
            ViewGroup.LayoutParams.WRAP_CONTENT,
        ).apply { gravity = Gravity.END or Gravity.CENTER_VERTICAL; marginEnd = dp(88) }
        host.addView(col, lp)
        panel = col
    }

    /** Remove the column (only on a tool switch away from an inking tool). */
    fun dismiss() {
        panel?.let { host.removeView(it) }
        panel = null
        circles.clear()
        labels.clear()
        palette = IntArray(0)
    }

    /** Re-ring the [selected] swatch in place — no view add/remove, so the firmware ink is untouched. */
    private fun restyle(selected: Int) {
        circles.forEachIndexed { i, v ->
            v.background = circleBg(palette[i], i == selected)
        }
        labels.forEachIndexed { i, t ->
            t.setTextColor(if (i == selected) Ink.ink else Ink.muted)
        }
    }

    private fun circleBg(packed: Int, selected: Boolean): GradientDrawable {
        val r = (packed ushr 24) and 0xFF
        val g = (packed ushr 16) and 0xFF
        val b = (packed ushr 8) and 0xFF
        return GradientDrawable().apply {
            shape = GradientDrawable.OVAL
            setColor(Color.rgb(r, g, b)) // full opacity even for translucent inks
            // Selected = thick black ring; others = thin grey ring so light swatches still read.
            setStroke(if (selected) dp(4) else Ink.hair(), if (selected) Ink.ink else Ink.ringSoft)
        }
    }

    private fun swatchCell(packed: Int, name: String, selected: Boolean, onTap: () -> Unit): View {
        val cell = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setPadding(dp(8), dp(6), dp(8), dp(6))
            isClickable = true
            setOnClickListener { onTap() }
        }
        val side = dp(40)
        val circle = View(activity).apply {
            layoutParams = LinearLayout.LayoutParams(side, side)
            background = circleBg(packed, selected)
        }
        val label = TextView(activity).apply {
            text = name
            textSize = 11f
            typeface = Ink.mono
            letterSpacing = 0.04f
            gravity = Gravity.CENTER
            setTextColor(if (selected) Ink.ink else Ink.muted)
            setPadding(0, dp(4), 0, 0)
        }
        cell.addView(circle)
        cell.addView(label)
        circles.add(circle)
        labels.add(label)
        return cell
    }
}
