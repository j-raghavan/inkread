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
    /** Where the reader last parked the pill (#200); null opens it at the default dock. */
    private val savedPosition: Position? = null,
    /** The pill came to rest somewhere new — persist it so the next document opens there (#200). */
    private val onMoved: (Position) -> Unit = {},
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
        savedPosition?.let(::restore)
    }

    /**
     * Put the pill back where the reader parked it, once the first layout pass has given the host
     * and the pill real sizes (#200).
     *
     * Restoring is deliberately clamped rather than trusted: the fraction may have been saved on
     * the other rotation or on another panel, and a corner that was on screen then can be off it
     * now. [reattach] afterwards because a bare translate does not refresh this EPD — without it
     * the pill would sit at the default dock on the panel while being somewhere else to the touch.
     */
    private fun restore(position: Position) = onNextLayout { width, height ->
        container.translationX = translationXFor(position.x, host.width, width)
        container.translationY = translationYFor(position.y, host.height, height)
        reattach()
        onChrome()
    }

    /**
     * Run [action] with the container's size after the next layout pass.
     *
     * A one-shot layout listener, not `post`: [render] only *requests* a layout, and a posted
     * runnable can run before the traversal that measures the new children — which would read the
     * stale size and correct by the wrong amount.
     */
    private fun onNextLayout(action: (width: Int, height: Int) -> Unit) {
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
                    action(right - left, bottom - top)
                }
            },
        )
    }

    /** Hand the pill's resting place to the host to remember (#200). */
    private fun persist() {
        onMoved(
            Position(
                fractionX(container.translationX, host.width, container.width),
                fractionY(container.translationY, host.height, container.height),
            ),
        )
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
                    if (moved) { reattach(); persist(); onChrome() } // re-add forces an EPD refresh at the new spot
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
        reanchor(heightBefore) {
            reattach()
            persist() // the clamp may have nudged the pill; remember where it actually landed
            onChrome()
        }
    }

    /**
     * After the pill changes size, put its **top-left corner** back where it was and pull it inside
     * the host, then run [onSettled].
     *
     * Every size change needs this, not just the deliberate ones: the pill is anchored
     * `CENTER_VERTICAL`, so growing or shrinking it moves the corner by half the difference unless
     * the offset compensates. The corner is where the grip is — the thing the finger just tapped,
     * and the only way to collapse the pill again.
     */
    private fun reanchor(heightBefore: Int, onSettled: () -> Unit) = onNextLayout { width, height ->
        container.translationY =
            clampY(anchorY(container.translationY, heightBefore, height), host.height, height)
        container.translationX = clampX(container.translationX, host.width, width)
        onSettled()
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

    /**
     * Collapse the pill (call from the host's onPause) — it stays docked, never removed.
     *
     * Re-anchored like any other collapse: this used to shrink the pill without compensating, so
     * backgrounding the app and returning to it left the puck half the pill's height further down
     * the screen than the reader had put it. Nothing is persisted or repainted here — the panel is
     * going away, and the corner the reader chose was already recorded when they chose it.
     */
    fun dismiss() {
        if (!expanded) return
        val heightBefore = container.height
        expanded = false
        render()
        reanchor(heightBefore) {}
    }

    /**
     * A parked position for the pill, held as host-relative fractions (#200). Persisted by the
     * host so the toolbar reopens where the reader left it instead of back at the default dock.
     */
    data class Position(val x: Float, val y: Float) {
        companion object {
            /**
             * Decode a stored pair, or null when there isn't one to decode.
             *
             * "Never parked" and "parked before this feature existed" both read back as the NaN
             * default, and a NaN that reached a layout pass would place the pill nowhere at all —
             * so anything non-finite means the default dock, not a position.
             */
            fun of(x: Float, y: Float): Position? =
                if (x.isFinite() && y.isFinite()) Position(x, y) else null
        }
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

        /**
         * Where the pill's **top-left corner** sits, as a fraction of the host's width and height.
         *
         * Fractions rather than raw pixels because a remembered position has to survive things the
         * pixels do not: a rotation, a different panel (a Nomad is not a Manta), and the pill's own
         * size changing between its collapsed and expanded forms. The corner is where the grip is,
         * which is the part the reader actually aims at.
         */
        fun fractionX(tx: Float, hostWidth: Int, viewWidth: Int): Float =
            if (hostWidth <= 0) 0f else ((hostWidth - viewWidth) + tx) / hostWidth

        fun fractionY(ty: Float, hostHeight: Int, viewHeight: Int): Float =
            if (hostHeight <= 0) 0f else ((hostHeight - viewHeight) / 2f + ty) / hostHeight

        /**
         * Turn a remembered [fractionX] back into a horizontal offset for the *current* geometry,
         * clamped so it lands on screen.
         *
         * The clamp is the point, not a formality: the fraction may have been saved on the other
         * rotation, on another device, or against a pill that a later build sized differently, and
         * a position that was legal then can be off screen now. Restoring the pill out of reach
         * would recreate the very trap #200 was opened about.
         */
        fun translationXFor(fraction: Float, hostWidth: Int, viewWidth: Int): Float =
            if (hostWidth <= 0 || !fraction.isFinite()) {
                0f
            } else {
                clampX(fraction * hostWidth - (hostWidth - viewWidth), hostWidth, viewWidth)
            }

        fun translationYFor(fraction: Float, hostHeight: Int, viewHeight: Int): Float =
            if (hostHeight <= 0 || !fraction.isFinite()) {
                0f
            } else {
                clampY(fraction * hostHeight - (hostHeight - viewHeight) / 2f, hostHeight, viewHeight)
            }
    }
}
