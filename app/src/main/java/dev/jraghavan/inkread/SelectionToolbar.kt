package dev.jraghavan.inkread

import android.app.Activity
import android.graphics.Color
import android.graphics.RectF
import android.graphics.drawable.GradientDrawable
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.PopupWindow

/** A lasso-selection operation (ADR-INKREAD-0010), mirroring NeoReader's selection menu. */
enum class SelAction(val label: String, val iconRes: Int) {
    DELETE("Delete", R.drawable.ic_sel_delete),
    CUT("Cut", R.drawable.ic_sel_cut),
    COPY("Copy", R.drawable.ic_sel_copy),
    PASTE("Paste", R.drawable.ic_sel_paste),
    SELECT_ALL("Select all", R.drawable.ic_sel_all),
    DIGEST("Add to Digest", R.drawable.ic_sel_digest),
    DONE("Done", R.drawable.ic_sel_done),
}

/**
 * The floating **selection toolbar** over an active lasso selection (ADR-INKREAD-0010 — NeoReader's
 * selection menu, video frame 157): a single row of **square icon cells** anchored just above the
 * selection. Icons (not text/emoji) — crisp on e-ink. Move is done by dragging the selection itself.
 * Pure presentation + an action callback.
 */
class SelectionToolbar(
    private val activity: Activity,
    private val host: FrameLayout,
    private val onAction: (SelAction) -> Unit,
) {
    private val density = activity.resources.displayMetrics.density
    private fun dp(v: Int) = (v * density).toInt()

    private var popup: PopupWindow? = null

    val isShowing: Boolean get() = popup?.isShowing == true

    /** Show the toolbar anchored above [boundsPx]; [canPaste] gates the Paste cell. */
    fun show(boundsPx: RectF, canPaste: Boolean) {
        dismiss()
        val row = LinearLayout(activity).apply {
            orientation = LinearLayout.HORIZONTAL
            background = GradientDrawable().apply {
                setColor(Color.WHITE)
                setStroke(maxOf(2, dp(1)), Color.BLACK)
                cornerRadius = dp(10).toFloat()
            }
            setPadding(dp(4), dp(4), dp(4), dp(4))
        }
        val win = PopupWindow(row, ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
            isOutsideTouchable = false // keep it up while the user works the selection
            isFocusable = false
        }
        for (action in SelAction.values()) {
            val enabled = action != SelAction.PASTE || canPaste
            row.addView(cell(action, win, enabled))
        }

        // Anchor: centered horizontally on the selection, just above it; clamped on-screen.
        val hostLoc = IntArray(2)
        host.getLocationOnScreen(hostLoc)
        row.measure(View.MeasureSpec.UNSPECIFIED, View.MeasureSpec.UNSPECIFIED)
        val pw = row.measuredWidth
        val ph = row.measuredHeight
        val margin = dp(6)
        val cx = hostLoc[0] + ((boundsPx.left + boundsPx.right) / 2f).toInt()
        var x = cx - pw / 2
        x = x.coerceIn(margin, (hostLoc[0] + host.width - pw - margin).coerceAtLeast(margin))
        var y = hostLoc[1] + boundsPx.top.toInt() - ph - dp(8)
        if (y < hostLoc[1] + margin) y = hostLoc[1] + boundsPx.bottom.toInt() + dp(8) // below if no room above
        popup = win
        win.showAtLocation(host, Gravity.NO_GRAVITY, x, y)
    }

    fun dismiss() {
        popup?.dismiss()
        popup = null
    }

    /** One square icon cell. */
    private fun cell(action: SelAction, win: PopupWindow, enabled: Boolean): ImageView =
        ImageView(activity).apply {
            setImageResource(action.iconRes)
            setColorFilter(if (enabled) Color.BLACK else Color.parseColor("#BDBDBD"))
            val pad = dp(10)
            setPadding(pad, pad, pad, pad)
            val side = dp(48)
            layoutParams = LinearLayout.LayoutParams(side, side)
            contentDescription = action.label
            isClickable = enabled
            if (enabled) {
                setOnClickListener {
                    if (action == SelAction.DONE) win.dismiss() // DONE closes; others keep it up
                    onAction(action)
                }
            }
        }
}
