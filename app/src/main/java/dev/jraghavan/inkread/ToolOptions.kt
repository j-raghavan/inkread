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
 * The inking tool's options **column** (ADR-INKREAD-0010 — NeoReader's brush row): a vertical stack
 * of filled circle colour swatches, the selected one ringed and captioned with its name, optionally
 * followed by a line-thickness row for the pen (#199). Colors are stored true per stroke; on the
 * MONOCHROME Supernote the swatches render as greys, so the name caption is what disambiguates
 * them. The thickness swatches need no such help — each is drawn at the width it selects.
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
class ToolOptions(
    private val activity: Activity,
    private val host: FrameLayout,
    /**
     * Where the tool palette currently sits, or `null` before it has been laid out. The column
     * opens beside the pill rather than at a fixed edge — it used to be pinned to the right
     * whatever the reader had chosen, so docking the bar left or on top left the colours and
     * thicknesses stranded on the far side of the page (#200).
     *
     * Read at [show] time, not captured: the pill is draggable. The column does **not** follow a
     * drag that happens while it is already open — re-placing a mounted view would cost an EPD
     * repaint, and the pill moving out from under its own options is rare next to that.
     */
    private val toolbar: () -> ToolPalette.Placement? = { null },
) {
    private val density = activity.resources.displayMetrics.density
    private fun dp(v: Int) = (v * density).toInt()

    private var panel: LinearLayout? = null
    private var circles: MutableList<View> = mutableListOf()
    private var labels: MutableList<TextView> = mutableListOf()
    private var palette: IntArray = IntArray(0)
    private var bars: MutableList<View> = mutableListOf()
    private var barLabels: MutableList<TextView> = mutableListOf()
    private var widths: FloatArray = FloatArray(0)

    /** Whether the column is currently mounted. */
    fun isShowing(): Boolean = panel != null

    /** Show the colour column alone — for a tool whose line width is fixed (the highlighter). */
    fun show(title: String, colors: IntArray, names: Array<String>, selected: Int, onPick: (Int) -> Unit) {
        show(title, colors, names, selected, FloatArray(0), emptyArray(), -1, onPick) {}
    }

    /**
     * Show the column, or — if it's already up for the same options — just move the selection rings.
     * [colors] are packed `r<<24|g<<16|b<<8|a`; [names] parallel; [selected] ringed; [onPick] gets
     * the chosen index. [lineWidths] (view px, thinnest first) adds a thickness row below the
     * colours; pass an empty array to omit it. Never auto-dismisses: the caller hides it only on a
     * tool switch.
     */
    fun show(
        title: String,
        colors: IntArray,
        names: Array<String>,
        selected: Int,
        lineWidths: FloatArray,
        widthNames: Array<String>,
        widthSelected: Int,
        onPick: (Int) -> Unit,
        onPickWidth: (Int) -> Unit,
    ) {
        // Already mounted for these same options → restyle in place, no remove/add (no EPD churn).
        if (panel != null && colors.contentEquals(palette) && lineWidths.contentEquals(widths)) {
            restyle(selected)
            restyleWidth(widthSelected)
            return
        }
        dismiss()
        palette = colors.copyOf()
        widths = lineWidths.copyOf()
        circles = mutableListOf()
        labels = mutableListOf()
        bars = mutableListOf()
        barLabels = mutableListOf()
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
        if (lineWidths.isNotEmpty()) {
            col.addView(Ink.eyebrow(activity, "Thickness").apply {
                setPadding(0, Ink.dp(10), 0, Ink.dp(6))
            })
            lineWidths.forEachIndexed { i, w ->
                col.addView(widthCell(w, widthNames.getOrElse(i) { "" }, i == widthSelected) {
                    onPickWidth(i)
                    restyleWidth(i) // in place, for the same reason the colour ring is
                })
            }
        }
        host.addView(col, placement())
        panel = col
    }

    /**
     * Lay the column out beside the palette.
     *
     * The column is displaced from the pill along one axis — across from a vertical pill, above or
     * below a horizontal bar — and lines up with the pill's docked edge on the other. Both are on
     * screen by construction: the pill's drag is clamped to keep it fully within the host, so a
     * margin measured from one of its edges cannot be negative or push past the far side.
     *
     * With no palette laid out yet, this falls back to the right edge, which is where the palette
     * itself starts. Only reachable before first layout, when no tool button exists to tap.
     */
    private fun placement(): FrameLayout.LayoutParams {
        val lp = FrameLayout.LayoutParams(
            ViewGroup.LayoutParams.WRAP_CONTENT,
            ViewGroup.LayoutParams.WRAP_CONTENT,
        )
        // Absolute LEFT/RIGHT rather than START/END: every input here is an absolute pixel
        // coordinate, and the manifest declares supportsRtl, so a direction-resolved gravity would
        // mirror the margins out from under coordinates that had not mirrored with them.
        val pill = toolbar() ?: return lp.apply {
            gravity = Gravity.RIGHT or Gravity.CENTER_VERTICAL
            rightMargin = Ink.sdp(FALLBACK_INSET)
        }
        val gap = Ink.dp(GAP)
        when (sideFor(pill, host.width, host.height)) {
            Side.BELOW_PILL -> {
                lp.gravity = Gravity.TOP or dockedEdge(pill)
                lp.topMargin = pill.bottom + gap
                alignToDockedEdge(lp, pill)
            }
            Side.ABOVE_PILL -> {
                lp.gravity = Gravity.BOTTOM or dockedEdge(pill)
                lp.bottomMargin = (host.height - pill.top) + gap
                alignToDockedEdge(lp, pill)
            }
            Side.LEFT_OF_PILL -> {
                lp.gravity = Gravity.RIGHT or Gravity.CENTER_VERTICAL
                lp.rightMargin = (host.width - pill.left) + gap
            }
            Side.RIGHT_OF_PILL -> {
                lp.gravity = Gravity.LEFT or Gravity.CENTER_VERTICAL
                lp.leftMargin = pill.right + gap
            }
        }
        return lp
    }

    /** The horizontal edge a top/bottom-placed column lines up with — the one the bar docks to. */
    private fun dockedEdge(pill: ToolPalette.Placement) =
        if (pill.dockedStart) Gravity.LEFT else Gravity.RIGHT

    /**
     * Line the column up with the docked end of the bar rather than centring it on the page.
     *
     * Centring is safe from an overflow point of view but reproduces a milder form of the bug being
     * fixed: the bar docks into a *corner*, so a centred column lands a long way from the button
     * that summoned it. The bar's own docked edge is on screen by construction, so aligning to it
     * cannot overflow either.
     */
    private fun alignToDockedEdge(lp: FrameLayout.LayoutParams, pill: ToolPalette.Placement) {
        if (pill.dockedStart) lp.leftMargin = pill.left
        else lp.rightMargin = host.width - pill.right
    }

    /** Which side of the palette the options column opens on. */
    enum class Side { BELOW_PILL, ABOVE_PILL, LEFT_OF_PILL, RIGHT_OF_PILL }

    companion object {
        /** Spacing between the pill and the column, and the pre-layout fallback inset, in dp. */
        const val GAP = 16
        const val FALLBACK_INSET = 88

        /**
         * Pick the side the column opens on (#200).
         *
         * Pure, and tested on the JVM like the pill's own placement maths, because this is the
         * decision that was wrong: the column used to be pinned to the right edge unconditionally,
         * so docking the bar on the left or across the top left it on the far side of the page.
         *
         * Both axes get a midline test. Orientation says which axis the column is displaced along;
         * position says which way. A horizontal bar is not necessarily a *top* bar — it is dragged
         * over the whole page, and assuming it stays near the top pushed the column off the bottom.
         */
        fun sideFor(pill: ToolPalette.Placement, hostWidth: Int, hostHeight: Int): Side =
            if (pill.horizontal) {
                if (pill.centerY * 2 > hostHeight) Side.ABOVE_PILL else Side.BELOW_PILL
            } else {
                if (pill.centerX * 2 > hostWidth) Side.LEFT_OF_PILL else Side.RIGHT_OF_PILL
            }
    }

    /** Remove the column (only on a tool switch away from an inking tool). */
    fun dismiss() {
        panel?.let { host.removeView(it) }
        panel = null
        circles.clear()
        labels.clear()
        palette = IntArray(0)
        bars.clear()
        barLabels.clear()
        widths = FloatArray(0)
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

    /** Re-mark the [selected] thickness in place — same no-add/remove rule as the colour ring. */
    private fun restyleWidth(selected: Int) {
        bars.forEachIndexed { i, v ->
            v.background = barBg(i == selected)
        }
        barLabels.forEachIndexed { i, t ->
            t.setTextColor(if (i == selected) Ink.ink else Ink.muted)
        }
    }

    /** Selected = thick black ring, as the colour swatches use; others = a soft hairline. */
    private fun barBg(selected: Boolean): GradientDrawable = GradientDrawable().apply {
        shape = GradientDrawable.RECTANGLE
        cornerRadius = dp(8).toFloat()
        setColor(Color.TRANSPARENT)
        setStroke(if (selected) dp(3) else Ink.hair(), if (selected) Ink.ink else Ink.ringSoft)
    }

    /**
     * One thickness choice: a black bar drawn at the width it selects, so the control shows the
     * outcome rather than naming it — the panel is monochrome, and a line is the one swatch that
     * needs no caption to be understood.
     */
    private fun widthCell(widthPx: Float, name: String, selected: Boolean, onTap: () -> Unit): View {
        val cell = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setPadding(dp(8), dp(6), dp(8), dp(6))
            isClickable = true
            setOnClickListener { onTap() }
            background = barBg(selected)
        }
        val bar = View(activity).apply {
            background = GradientDrawable().apply {
                shape = GradientDrawable.RECTANGLE
                cornerRadius = widthPx / 2f
                setColor(Ink.ink)
            }
        }
        // The bar is drawn at the true selected width, floored so the thinnest option still reads
        // on a high-density panel.
        cell.addView(
            bar,
            LinearLayout.LayoutParams(dp(40), widthPx.toInt().coerceAtLeast(dp(1))).apply {
                topMargin = dp(6)
                bottomMargin = dp(4)
            },
        )
        // Same caption styling as the colour swatches, so the two rows read as one control.
        val label = TextView(activity).apply {
            text = name
            textSize = Ink.sp(11f)
            typeface = Ink.mono
            letterSpacing = 0.04f
            gravity = Gravity.CENTER
            setTextColor(if (selected) Ink.ink else Ink.muted)
            setPadding(0, dp(2), 0, 0)
        }
        cell.addView(label)
        bars.add(cell)
        barLabels.add(label)
        return cell
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
            textSize = Ink.sp(11f)
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
