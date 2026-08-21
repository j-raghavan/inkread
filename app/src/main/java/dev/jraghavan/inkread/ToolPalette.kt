package dev.jraghavan.inkread

import android.app.Activity
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
    ;

    companion object {
        /**
         * Which tool a stylus event actually drives (#158). The palette decides — except for an
         * **inverted pen**, which Android reports as [MotionEvent.TOOL_TYPE_ERASER] and which must
         * erase whatever the palette says. The hardware end of the pen is a deliberate, unambiguous
         * statement of intent; no user flips the pen over meaning to write.
         *
         * Getting this wrong corrupted the page rather than merely doing the wrong thing: an
         * inverted pen used to fall through to the ink path, so the firmware's own eraser wiped the
         * live ink off the panel (the stroke *looked* erased) while the app committed the eraser
         * sweep to the core as a **Pen** stroke. Nothing was deleted from the model, so the next
         * full render brought the original stroke back with the sweep inked on top of it.
         */
        fun forStylus(palette: Tool, motionToolType: Int): Tool =
            if (motionToolType == MotionEvent.TOOL_TYPE_ERASER) ERASER else palette
    }
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

    // Opens collapsed (a small circular inkwell puck) so it never covers the text on document open;
    // tap to expand into the tool strip.
    private var expanded = false
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
        container.alpha = IDLE_ALPHA // see-through while reading so it doesn't cover the text
        render()
    }

    /** Rounded white pill with a black keyline — high contrast on e-ink (Inkwell card language). */
    private fun pill() = Ink.cardBg(22)

    /** A circular white puck with a black keyline — the collapsed form (the inkwell mark). */
    private fun circle() = GradientDrawable().apply {
        shape = GradientDrawable.OVAL
        setColor(Ink.paper)
        setStroke(Ink.keyline(), Ink.ink)
    }

    private fun render() {
        container.removeAllViews()
        if (expanded) {
            container.background = pill()
            container.setPadding(dp(5), dp(7), dp(5), dp(7))
            container.addView(handle()) // grip: collapse / move
            container.addView(divider())
            for (tool in Tool.values()) container.addView(iconButton(tool))
            container.addView(divider())
            container.addView(actionButton(R.drawable.ic_sel_undo, "Undo", onUndo))
            container.addView(actionButton(R.drawable.ic_sel_redo, "Redo", onRedo))
        } else {
            // Collapsed: a circular inkwell puck — tap to expand, drag to move.
            container.background = circle()
            container.setPadding(0, 0, 0, 0)
            container.addView(collapsedPuck())
        }
    }

    /** The collapsed puck: the inkwell brand mark in the circle (tap = expand, drag = move). */
    private fun collapsedPuck(): ImageView = ImageView(activity).apply {
        setImageResource(R.drawable.ic_inkwell)
        setColorFilter(Ink.ink)
        val pad = dp(15)
        setPadding(pad, pad, pad, pad)
        val side = dp(60)
        layoutParams = LinearLayout.LayoutParams(side, side)
        contentDescription = "Tools — tap to open, drag to move"
        applyDragToggle(this)
    }

    /** A hairline separator between the handle, the tools, and the undo/redo actions. */
    private fun divider(): View = View(activity).apply {
        setBackgroundColor(Ink.hairline)
        layoutParams = LinearLayout.LayoutParams(dp(42), Ink.hair()).apply {
            gravity = Gravity.CENTER_HORIZONTAL
            val v = dp(4); setMargins(0, v, 0, v)
        }
    }

    /** An action icon (undo/redo) — runs [onTap], never becomes the active tool. */
    private fun actionButton(iconRes: Int, desc: String, onTap: () -> Unit): ImageView =
        ImageView(activity).apply {
            setImageResource(iconRes)
            setColorFilter(Ink.ink)
            val pad = dp(14); setPadding(pad, pad, pad, pad)
            val side = dp(60)
            layoutParams = LinearLayout.LayoutParams(side, side).apply { val m = dp(2); setMargins(m, m, m, m) }
            isClickable = true
            contentDescription = desc
            setOnClickListener { onTap() }
        }

    /** First icon: a grip that drags the pill (move) and, on a tap, collapses/expands it. */
    private fun handle(): ImageView = ImageView(activity).apply {
        setImageResource(R.drawable.ic_tool_handle)
        setColorFilter(Ink.ink)
        val pad = dp(14)
        setPadding(pad, pad, pad, pad)
        val side = dp(60)
        layoutParams = LinearLayout.LayoutParams(side, side).apply {
            val m = dp(2); setMargins(m, m, m, m)
        }
        contentDescription = "Collapse / move tools"
        applyDragToggle(this)
    }

    /** Shared touch behaviour for the grip + the collapsed puck: drag moves the pill (clamped to the
     *  host), a tap toggles collapsed/expanded. Opaque while touched, faded back on release. */
    @android.annotation.SuppressLint("ClickableViewAccessibility")
    private fun applyDragToggle(v: View) {
        var downX = 0f; var downY = 0f; var startTx = 0f; var startTy = 0f; var moved = false
        v.setOnTouchListener { _, e ->
            when (e.actionMasked) {
                MotionEvent.ACTION_DOWN -> {
                    downX = e.rawX; downY = e.rawY
                    startTx = container.translationX; startTy = container.translationY
                    moved = false
                    container.alpha = 1f // opaque while in use, so it's crisp to grab/drag
                    true
                }
                MotionEvent.ACTION_MOVE -> {
                    val dx = e.rawX - downX; val dy = e.rawY - downY
                    if (!moved && kotlin.math.hypot(dx, dy) > touchSlop) moved = true
                    if (moved) {
                        container.translationX =
                            clampX(startTx + dx, host.width, container.width)
                        container.translationY =
                            clampY(startTy + dy, host.height, container.height)
                    }
                    true
                }
                MotionEvent.ACTION_UP -> {
                    container.alpha = IDLE_ALPHA // fade back so it doesn't sit over the text
                    if (moved) { reattach(); onChrome() } // re-add forces an EPD refresh at the new spot
                    else { toggleExpanded() }
                    true
                }
                else -> false
            }
        }
    }

    /**
     * Collapse or expand, keeping the grip under the finger and the pill on screen (#200).
     *
     * The pill is anchored `CENTER_VERTICAL`, so changing its height moves it *both* ways from the
     * centre. Two things went wrong because of that. The grip jumped away from the finger that had
     * just tapped it; and — the reported bug — a puck dragged to the top or bottom edge expanded
     * past the edge, taking the grip with it. With the grip off screen there was no way to collapse
     * the pill again, so the only escape was to reopen the document.
     *
     * The height is not known until the new children are laid out, so the correction runs on the
     * next layout pass rather than here.
     */
    private fun toggleExpanded() {
        val heightBefore = container.height
        expanded = !expanded
        render()
        // A one-shot layout listener, not `post`: `render` only requests a layout, and a posted
        // runnable can run before the traversal that measures the new children — which would read
        // the OLD height and correct by the wrong amount.
        container.addOnLayoutChangeListener(
            object : View.OnLayoutChangeListener {
                override fun onLayoutChange(
                    v: View,
                    left: Int,
                    top: Int,
                    right: Int,
                    bottom: Int,
                    oldLeft: Int,
                    oldTop: Int,
                    oldRight: Int,
                    oldBottom: Int,
                ) {
                    v.removeOnLayoutChangeListener(this)
                    val height = bottom - top
                    container.translationY =
                        clampY(
                            anchorY(container.translationY, heightBefore, height),
                            host.height,
                            height,
                        )
                    container.translationX =
                        clampX(container.translationX, host.width, right - left)
                    reattach()
                    onChrome()
                }
            },
        )
    }

    private fun iconButton(tool: Tool): ImageView = ImageView(activity).apply {
        setImageResource(tool.iconRes)
        val active = tool == current
        setColorFilter(if (active) Ink.paper else Ink.ink)
        alpha = if (tool.phase2) 0.35f else 1f
        val pad = dp(14)
        setPadding(pad, pad, pad, pad)
        val side = dp(60)
        layoutParams = LinearLayout.LayoutParams(side, side).apply {
            val m = dp(2); setMargins(m, m, m, m)
        }
        if (active) {
            background = GradientDrawable().apply {
                setColor(Ink.ink); cornerRadius = Ink.dpf(Ink.RADIUS)
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

    internal companion object {
        /** Resting opacity of the docked puck — translucent so the text behind it stays readable. */
        const val IDLE_ALPHA = 0.55f

        /**
         * Clamp the horizontal offset so the pill stays inside the host. The base anchor is
         * `END`, so the pill only ever moves left: the offset is negative, bounded by how much
         * wider the host is than the pill.
         */
        fun clampX(tx: Float, hostWidth: Int, viewWidth: Int): Float {
            val min = -(hostWidth - viewWidth).toFloat().coerceAtLeast(0f)
            return tx.coerceIn(min, 0f)
        }

        /**
         * Clamp the vertical offset so the pill stays inside the host. The base anchor is
         * `CENTER_VERTICAL`, so the offset runs symmetrically about the centre and is bounded by
         * half the slack. A pill taller than the host has no slack at all and stays centred, which
         * is the least-bad answer: some of it is off screen whatever we do.
         */
        fun clampY(ty: Float, hostHeight: Int, viewHeight: Int): Float {
            val half = ((hostHeight - viewHeight) / 2f).coerceAtLeast(0f)
            return ty.coerceIn(-half, half)
        }

        /**
         * The vertical offset that keeps the pill's **top** where it was when its height changes
         * from `oldHeight` to `newHeight`.
         *
         * Under `CENTER_VERTICAL` the top sits at `(host - height) / 2 + ty`, so growing by `d`
         * moves the top up by `d / 2` unless the offset compensates. The top is where the grip is,
         * which is what the finger just tapped and what has to stay reachable.
         */
        fun anchorY(ty: Float, oldHeight: Int, newHeight: Int): Float =
            ty + (newHeight - oldHeight) / 2f
    }
}
