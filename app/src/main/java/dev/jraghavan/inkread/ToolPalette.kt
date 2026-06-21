package dev.jraghavan.inkread

import android.app.Activity
import android.graphics.Color
import android.graphics.drawable.GradientDrawable
import android.view.Gravity
import android.view.MotionEvent
import android.view.ViewConfiguration
import android.view.View
import android.view.ViewGroup
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout

/**
 * The annotation **tool model** (ADR-INKREAD-0010). On the Supernote the stylus inks via the
 * firmware and the finger navigates; finger `ACTION_UP` is unreliable, so the field's *modeless*
 * gesture tricks don't transfer. We disambiguate "ink" / "erase" / "lasso" / "define" by an
 * **explicit selected tool**, never by guessing a gesture — the reMarkable/Boox/Scribe modal family.
 */
enum class Tool(val label: String, val iconRes: Int, val phase2: Boolean) {
    PEN("Pen", R.drawable.ic_tool_pen, false),
    HIGHLIGHTER("Highlight", R.drawable.ic_tool_highlighter, false),
    ERASER("Eraser", R.drawable.ic_tool_eraser, false),
    LASSO("Lasso", R.drawable.ic_tool_lasso, false),
    DEFINE("Define", R.drawable.ic_menu_dict, false),
}

/**
 * A **collapsible, movable vertical icon pill** — NeoReader's Floating Toolbar (video frames 146/147):
 * a rounded white pill of monochrome line icons whose **first icon is a grip handle**. Tapping the
 * handle collapses the pill down to just the handle (and expands it again); dragging the handle moves
 * the whole pill. Every state change runs [onChrome] so the e-ink panel actually refreshes — the
 * earlier draggable puck "vanished" precisely because a view move triggered no EPD refresh.
 *
 * Icon-only (crisp on e-ink); the active tool is a filled dark chip. Pure presentation + callbacks.
 */
class ToolPalette(
    private val activity: Activity,
    private val host: FrameLayout,
    /** Asked to switch to [tool]; return true to commit (false vetoes, e.g. a not-yet-wired tool). */
    private val onToolSelected: (Tool) -> Boolean,
    /** Repaint the panel after a move/collapse so the EPD reflects the pill's new state. */
    private val onChrome: () -> Unit = {},
    /** Global ink undo / redo (these are actions, not tools — they don't change the active tool). */
    private val onUndo: () -> Unit = {},
    private val onRedo: () -> Unit = {},
) {
    var current: Tool = Tool.PEN
        private set

    private val density = activity.resources.displayMetrics.density
    private fun dp(v: Int) = (v * density).toInt()
    private val touchSlop = ViewConfiguration.get(activity).scaledTouchSlop

    private var expanded = true
    private val container = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }

    init {
        host.addView(
            container,
            FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            ).apply {
                gravity = Gravity.END or Gravity.CENTER_VERTICAL
                marginEnd = dp(6)
            },
        )
        render()
    }

    /** Rounded white pill with a black ring — high contrast on e-ink. */
    private fun pill() = GradientDrawable().apply {
        setColor(Color.WHITE)
        setStroke(maxOf(2, dp(1)), Color.BLACK)
        cornerRadius = dp(22).toFloat()
    }

    private fun render() {
        container.background = pill()
        container.setPadding(dp(5), dp(7), dp(5), dp(7))
        container.removeAllViews()
        container.addView(handle())
        if (expanded) {
            container.addView(divider())
            for (tool in Tool.values()) container.addView(iconButton(tool))
            container.addView(divider())
            container.addView(actionButton(R.drawable.ic_sel_undo, "Undo", onUndo))
            container.addView(actionButton(R.drawable.ic_sel_redo, "Redo", onRedo))
        }
    }

    /** A hairline separator between the handle, the tools, and the undo/redo actions. */
    private fun divider(): View = View(activity).apply {
        setBackgroundColor(Color.parseColor("#E0E0E0"))
        layoutParams = LinearLayout.LayoutParams(dp(28), maxOf(1, dp(1))).apply {
            gravity = Gravity.CENTER_HORIZONTAL
            val v = dp(4); setMargins(0, v, 0, v)
        }
    }

    /** An action icon (undo/redo) — runs [onTap], never becomes the active tool. */
    private fun actionButton(iconRes: Int, desc: String, onTap: () -> Unit): ImageView =
        ImageView(activity).apply {
            setImageResource(iconRes)
            setColorFilter(Color.BLACK)
            val pad = dp(9); setPadding(pad, pad, pad, pad)
            val side = dp(40)
            layoutParams = LinearLayout.LayoutParams(side, side).apply { val m = dp(2); setMargins(m, m, m, m) }
            isClickable = true
            contentDescription = desc
            setOnClickListener { onTap() }
        }

    /** First icon: a grip that drags the pill (move) and, on a tap, collapses/expands it. */
    private fun handle(): ImageView = ImageView(activity).apply {
        setImageResource(R.drawable.ic_tool_handle)
        setColorFilter(Color.BLACK)
        val pad = dp(9)
        setPadding(pad, pad, pad, pad)
        val side = dp(40)
        layoutParams = LinearLayout.LayoutParams(side, side).apply {
            val m = dp(2); setMargins(m, m, m, m)
        }
        contentDescription = if (expanded) "Collapse / move tools" else "Expand / move tools"
        var downX = 0f; var downY = 0f; var startTx = 0f; var startTy = 0f; var moved = false
        setOnTouchListener { _, e ->
            when (e.actionMasked) {
                MotionEvent.ACTION_DOWN -> {
                    downX = e.rawX; downY = e.rawY
                    startTx = container.translationX; startTy = container.translationY
                    moved = false; true
                }
                MotionEvent.ACTION_MOVE -> {
                    val dx = e.rawX - downX; val dy = e.rawY - downY
                    if (!moved && kotlin.math.hypot(dx, dy) > touchSlop) moved = true
                    if (moved) {
                        // Base anchor = END | CENTER_VERTICAL: X only moves left (≤0), Y from centre.
                        val xMin = -(host.width - container.width).toFloat().coerceAtLeast(0f)
                        val yHalf = ((host.height - container.height) / 2f).coerceAtLeast(0f)
                        container.translationX = (startTx + dx).coerceIn(xMin, 0f)
                        container.translationY = (startTy + dy).coerceIn(-yHalf, yHalf)
                    }
                    true
                }
                MotionEvent.ACTION_UP -> {
                    if (moved) { reattach(); onChrome() } // re-add forces an EPD refresh at the new spot
                    else { expanded = !expanded; render(); reattach(); onChrome() }
                    true
                }
                else -> false
            }
        }
    }

    private fun iconButton(tool: Tool): ImageView = ImageView(activity).apply {
        setImageResource(tool.iconRes)
        val active = tool == current
        setColorFilter(if (active) Color.WHITE else Color.BLACK)
        alpha = if (tool.phase2) 0.35f else 1f
        val pad = dp(9)
        setPadding(pad, pad, pad, pad)
        val side = dp(40)
        layoutParams = LinearLayout.LayoutParams(side, side).apply {
            val m = dp(2); setMargins(m, m, m, m)
        }
        if (active) {
            background = GradientDrawable().apply {
                setColor(Color.BLACK); cornerRadius = dp(16).toFloat()
            }
        }
        isClickable = true
        contentDescription = tool.label
        setOnClickListener {
            if (onToolSelected(tool)) current = tool
            render()
        }
    }

    /**
     * Re-add the container to the host (keeping its translation) — on this e-ink panel a view
     * *add* triggers an EPD refresh, whereas an in-place move/translate does not (overlay views
     * only refresh on add; the SurfaceView layer refreshes on blit). So after a move/collapse we
     * detach + re-attach to force the pill to repaint at its new position instead of vanishing.
     */
    private fun reattach() {
        val lp = container.layoutParams
        val tx = container.translationX
        val ty = container.translationY
        host.removeView(container)
        host.addView(container, lp)
        container.translationX = tx
        container.translationY = ty
        android.util.Log.i("ToolPalette", "reattach: expanded=$expanded tx=$tx ty=$ty")
    }

    /** Collapse the pill (call from the host's onPause) — it stays docked, never removed. */
    fun dismiss() {
        if (expanded) { expanded = false; render() }
    }
}
