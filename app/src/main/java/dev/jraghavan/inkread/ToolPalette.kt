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
    /** Which way the strip runs (#200). */
    private val orientation: Orientation = Orientation.VERTICAL,
    /** Which side it docks to: [Anchor.END] is the right edge, [Anchor.START] the left. */
    private val side: Anchor = Anchor.END,
    /**
     * Extra clearance along the docked side, in pixels, for chrome the strip must not cover (#200).
     *
     * The host owns this because the host owns the chrome. A strip docked into a corner that already
     * holds something does not merely look wrong — it eats the touches meant for it, since the strip
     * consumes every event inside its bounds.
     */
    private val dockClearance: Int = 0,
    /** Where the reader last parked the pill (#200); null opens it at the default dock. */
    private val savedPosition: Position? = null,
    /** The pill came to rest somewhere new — persist it so the next document opens there (#200). */
    private val onMoved: (Position) -> Unit = {},
    /** Whether to open already expanded — the state the reader last left it in (#200). */
    private val startExpanded: Boolean = false,
    /** The reader opened or closed the strip; remember it for the next book (#200). */
    private val onExpandedChanged: (Boolean) -> Unit = {},
) {
    var current: Tool = Tool.PEN
        private set

    /**
     * The pill's sizing (#133 / #200). Menu Size scales the **glyph**; the spacing around it does
     * not move.
     *
     * The reader asked for "smaller icons ... but with some margin to keep some distance between
     * button for ease of activation". Scaling everything by one factor cannot deliver that — every
     * ratio survives, so the buttons end up proportionally as crowded as they started. Scaling only
     * the glyph and holding the inset and the gap at their design size means a smaller icon leaves
     * *more* clearance around it, not the same.
     *
     * The box is derived rather than set, so the glyph is always exactly [TOOL_GLYPH] scaled and
     * always centred, including when [TOOL_BOX_MIN] catches it.
     */
    private fun glyphPx() = Ink.dp(glyphDp(Ink.uiScale))

    private fun boxPx() = Ink.dp(boxDp(Ink.uiScale))

    private fun insetPx() = (boxPx() - glyphPx()) / 2

    /** The separator runs most of the way across the strip, so it tracks the button box. */
    private fun dividerRunPx() = boxPx() * DIVIDER_RUN_PERCENT / 100
    private val touchSlop = ViewConfiguration.get(activity).scaledTouchSlop

    /**
     * Open or shut, carried over from the last book (#200).
     *
     * Not reset on open. Whether the tools are out is a working posture, not a per-document
     * property: a reader who is annotating opens the next book to annotate it too, and having to
     * reach for the strip again each time is friction in exactly the place they were trying to
     * avoid it.
     */
    private var expanded = startExpanded
    private val horizontal get() = orientation == Orientation.HORIZONTAL

    /**
     * Which host edge each axis is measured from, matching the container's layout gravity.
     *
     * Both forms dock to a side; only the cross axis differs. A horizontal bar sits against the top,
     * so it lands in a corner — which is the point. Collapsed in the middle of the top edge it would
     * float in the centre of the page with nothing to relate to, and expanding from there pushed it
     * hard against one side with 920px of dead space behind it.
     */
    private val axisX get() = side
    private val axisY get() = if (horizontal) Anchor.START else Anchor.CENTER

    /** The axis the strip grows along when it expands — the one it is laid out on. */
    private val growthAxis get() = if (horizontal) axisX else axisY

    private val container = LinearLayout(activity).apply {
        this.orientation = if (horizontal) LinearLayout.HORIZONTAL else LinearLayout.VERTICAL
    }

    init {
        host.addView(
            container,
            FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            ).apply {
                // The docked edge is the one the strip runs along: a vertical pill hugs the
                // right, a horizontal bar sits across the top, which is where it was asked for.
                val sideGravity = if (side == Anchor.START) Gravity.START else Gravity.END
                gravity = sideGravity or if (horizontal) Gravity.TOP else Gravity.CENTER_VERTICAL
                val inset = Ink.dp(6)
                // Clearance applies along the docked side, and only to the horizontal form: the
                // vertical pill rides the middle of its edge and never reaches the corner.
                val docked = inset + if (horizontal) dockClearance else 0
                if (horizontal) topMargin = inset
                if (side == Anchor.START) marginStart = docked else marginEnd = docked
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
        container.translationX = offsetForFraction(axisX, position.x, host.width, width)
        container.translationY = offsetForFraction(axisY, position.y, host.height, height)
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

    /**
     * Hand the pill's resting place to the host to remember (#200).
     *
     * Never from a degenerate layout. A zero-sized host reads as the default dock, and writing that
     * would quietly discard the corner the reader had chosen — losing the parked position is the
     * one failure this whole change exists to prevent, so an unmeasured pill records nothing.
     */
    private fun persist() {
        if (host.width <= 0 || host.height <= 0 || container.width <= 0 || container.height <= 0) return
        onMoved(
            Position(
                fraction(axisX, container.translationX, host.width, container.width),
                fraction(axisY, container.translationY, host.height, container.height),
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
            if (horizontal) {
                container.setPadding(Ink.dp(7), Ink.dp(5), Ink.dp(7), Ink.dp(5))
            } else {
                container.setPadding(Ink.dp(5), Ink.dp(7), Ink.dp(5), Ink.dp(7))
            }
            // The grip belongs on the docked edge, so the strip grows inward from it and the grip
            // itself never moves. Docked right, that means building the row back to front.
            val parts = buildList {
                add(handle()) // grip: collapse / move
                add(divider())
                for (tool in Tool.values()) add(iconButton(tool))
                add(divider())
                add(actionButton(R.drawable.ic_sel_undo, "Undo", onUndo))
                add(actionButton(R.drawable.ic_sel_redo, "Redo", onRedo))
            }
            val ordered = if (horizontal && side == Anchor.END) parts.reversed() else parts
            for (view in ordered) container.addView(view)
        } else {
            // Collapsed: a circular inkwell puck — tap to expand, drag to move.
            container.background = circle()
            container.setPadding(0, 0, 0, 0)
            container.addView(collapsedPuck())
        }
    }

    /**
     * Where the pill actually is, and how it is oriented — `null` until the first layout has given
     * it a size.
     *
     * Satellite chrome (the tool-options column) has to open beside the pill wherever the reader
     * parked it, and only the pill knows where that is. Reported as geometry rather than as a
     * placement so the caller keeps its own layout decisions (#200). Bounds and orientation travel
     * together because they are one fact: there is no such thing as an orientation without a pill.
     */
    fun placement(): Placement? {
        if (container.width == 0 || container.height == 0) return null
        val left = (container.left + container.translationX).toInt()
        val top = (container.top + container.translationY).toInt()
        return Placement(
            left = left,
            top = top,
            right = left + container.width,
            bottom = top + container.height,
            horizontal = horizontal,
            dockedStart = side == Anchor.START,
        )
    }

    /** The collapsed puck: the inkwell brand mark in the circle (tap = expand, drag = move). */
    private fun collapsedPuck(): ImageView = ImageView(activity).apply {
        setImageResource(R.drawable.ic_inkwell)
        setColorFilter(Ink.ink)
        val pad = insetPx()
        setPadding(pad, pad, pad, pad)
        val side = boxPx()
        layoutParams = LinearLayout.LayoutParams(side, side)
        contentDescription = "Tools — tap to open, drag to move"
        applyDragToggle(this)
    }

    /** A hairline separator between the handle, the tools, and the undo/redo actions. */
    private fun divider(): View = View(activity).apply {
        setBackgroundColor(Ink.hairline)
        // The separator runs across the strip, so it swaps axis with it.
        layoutParams = if (horizontal) {
            LinearLayout.LayoutParams(Ink.hair(), dividerRunPx()).apply {
                gravity = Gravity.CENTER_VERTICAL
                val h = Ink.dp(DIVIDER_GAP); setMargins(h, 0, h, 0)
            }
        } else {
            LinearLayout.LayoutParams(dividerRunPx(), Ink.hair()).apply {
                gravity = Gravity.CENTER_HORIZONTAL
                val v = Ink.dp(DIVIDER_GAP); setMargins(0, v, 0, v)
            }
        }
    }

    /** An action icon (undo/redo) — runs [onTap], never becomes the active tool. */
    private fun actionButton(iconRes: Int, desc: String, onTap: () -> Unit): ImageView =
        ImageView(activity).apply {
            setImageResource(iconRes)
            setColorFilter(Ink.ink)
            val pad = insetPx(); setPadding(pad, pad, pad, pad)
            val side = boxPx()
            layoutParams = LinearLayout.LayoutParams(side, side).apply { val m = Ink.dp(TOOL_GAP); setMargins(m, m, m, m) }
            isClickable = true
            contentDescription = desc
            setOnClickListener { onTap() }
        }

    /** First icon: a grip that drags the pill (move) and, on a tap, collapses/expands it. */
    private fun handle(): ImageView = ImageView(activity).apply {
        setImageResource(R.drawable.ic_tool_handle)
        setColorFilter(Ink.ink)
        val pad = insetPx()
        setPadding(pad, pad, pad, pad)
        val side = boxPx()
        layoutParams = LinearLayout.LayoutParams(side, side).apply {
            val m = Ink.dp(TOOL_GAP); setMargins(m, m, m, m)
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
                            clamp(axisX, startTx + dx, host.width, container.width)
                        container.translationY =
                            clamp(axisY, startTy + dy, host.height, container.height)
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
     * Rebuild the strip at the current [Ink.uiScale] (#133 / #200).
     *
     * The reader changes Menu Size in a sheet that sits over the pill; without this the pill keeps
     * whatever size it was built at until something else happens to re-render it, so the setting
     * looks as though it does not reach the toolbar at all. Re-anchored for the same reason a
     * collapse is: the pill is centred on its axis, so changing its size moves the grip unless the
     * offset compensates.
     */
    fun refreshChrome() {
        val sizeBefore = if (horizontal) container.width else container.height
        render()
        reanchor(sizeBefore) {
            reattach()
            persist()
            onChrome()
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
        val sizeBefore = if (horizontal) container.width else container.height
        expanded = !expanded
        onExpandedChanged(expanded)
        render()
        reanchor(sizeBefore) {
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
    private fun reanchor(sizeBefore: Int, onSettled: () -> Unit) = onNextLayout { width, height ->
        // Only the axis the strip grows along needs its docked edge held; the other just re-clamps.
        if (horizontal) {
            container.translationX = clamp(
                axisX,
                keepEdge(growthAxis, container.translationX, sizeBefore, width),
                host.width,
                width,
            )
            container.translationY = clamp(axisY, container.translationY, host.height, height)
        } else {
            container.translationY = clamp(
                axisY,
                keepEdge(growthAxis, container.translationY, sizeBefore, height),
                host.height,
                height,
            )
            container.translationX = clamp(axisX, container.translationX, host.width, width)
        }
        onSettled()
    }

    private fun iconButton(tool: Tool): ImageView = ImageView(activity).apply {
        setImageResource(tool.iconRes)
        val active = tool == current
        setColorFilter(if (active) Ink.paper else Ink.ink)
        alpha = if (tool.phase2) 0.35f else 1f
        val pad = insetPx()
        setPadding(pad, pad, pad, pad)
        val side = boxPx()
        layoutParams = LinearLayout.LayoutParams(side, side).apply {
            val m = Ink.dp(TOOL_GAP); setMargins(m, m, m, m)
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
     * Which edge of the host an axis is measured from — the layout gravity, as arithmetic.
     *
     * A vertical pill hangs off the right edge and is centred down the page; a horizontal bar
     * sits against the top and is centred across it. The same offset therefore means different
     * things on different axes, and every clamp, fraction and re-anchor below has to know which.
     */
    enum class Anchor { START, CENTER, END }

    /**
     * Which way the strip runs (#200).
     *
     * Both forms are the same strip; only the axis it grows along and the edge it docks to differ.
     * [HORIZONTAL] answers the request for a toolbar across the top: on a narrow panel a vertical
     * pill is wider than the page margin, so it clips the end of every line it spans -- twenty of
     * them -- while a horizontal one at the top clips part of a single line.
     */
    enum class Orientation { VERTICAL, HORIZONTAL }

    /**
     * Everything satellite chrome needs to sit beside the pill: where it is, which way it runs, and
     * which edge it docks to (#200). One object rather than several accessors, so a caller cannot
     * hold an orientation for a pill that is not laid out.
     */
    data class Placement(
        val left: Int,
        val top: Int,
        val right: Int,
        val bottom: Int,
        /** The strip runs across the screen rather than down an edge. */
        val horizontal: Boolean,
        /** It docks to the left/top edge rather than the right. */
        val dockedStart: Boolean,
    ) {
        // Plain Ints rather than a `Rect`: this is the input to placement decisions that are tested
        // on the JVM, and an android.graphics.Rect is a stub there.
        val centerX get() = (left + right) / 2
        val centerY get() = (top + bottom) / 2
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
         * Design sizes for one tool button, in dp (#200).
         *
         * Only [TOOL_GLYPH] answers to Menu Size. [TOOL_INSET] and [TOOL_GAP] are held at their
         * design size, so shrinking the icon widens the clearance around it rather than closing it
         * up — see the sizing helpers above for why one uniform factor cannot do that.
         *
         * At Menu Size M this reproduces the original 32dp glyph in a 60dp box exactly, so a reader
         * who never touches the setting sees the toolbar they already had.
         */
        const val TOOL_GLYPH = 32
        const val TOOL_INSET = 14
        const val TOOL_GAP = 2

        /**
         * The smallest touch target the box may be derived down to, whatever the glyph does.
         *
         * 48dp is the platform's recommended minimum. It is a floor rather than the target itself
         * because this is a stylus-first panel where the pen tip is under a millimetre — the floor
         * is there for the finger, not the pen.
         */
        const val TOOL_BOX_MIN = 48

        /** How far the hairline separator runs across the strip, as a percentage of the box. */
        const val DIVIDER_RUN_PERCENT = 70

        /** Clearance either side of a separator. Spacing, so it does not scale. */
        const val DIVIDER_GAP = 4

        /**
         * The design sizes at a given Menu Size, in dp before density (#200).
         *
         * Resolved in dp and multiplied by density once, rather than scaling an already-converted
         * pixel size: two truncations would leave the box a fraction under the [TOOL_BOX_MIN] floor
         * the setting is chosen against.
         */
        fun glyphDp(uiScale: Float) = (TOOL_GLYPH * uiScale).toInt()

        fun boxDp(uiScale: Float) = maxOf(TOOL_BOX_MIN, glyphDp(uiScale) + 2 * TOOL_INSET)

        /** Where the view's leading edge sits, given its anchor and offset. */
        fun edge(anchor: Anchor, offset: Float, host: Int, view: Int): Float {
            val slack = (host - view).toFloat()
            return when (anchor) {
                Anchor.START -> offset
                Anchor.CENTER -> slack / 2f + offset
                Anchor.END -> slack + offset
            }
        }

        /** The offset that puts the view's leading edge at [edge] — the inverse of [edge]. */
        fun offsetFor(anchor: Anchor, edge: Float, host: Int, view: Int): Float {
            val slack = (host - view).toFloat()
            return when (anchor) {
                Anchor.START -> edge
                Anchor.CENTER -> edge - slack / 2f
                Anchor.END -> edge - slack
            }
        }

        /**
         * Clamp an offset so the view stays inside the host, whichever edge it is anchored to.
         *
         * A view with no slack — larger than its host — stays where its gravity puts it rather than
         * being pushed to an edge: some of it is off screen whatever we do, and moving it would only
         * change which part.
         */
        fun clamp(anchor: Anchor, offset: Float, host: Int, view: Int): Float {
            val slack = (host - view).toFloat()
            if (slack <= 0f) return 0f
            return offsetFor(anchor, edge(anchor, offset, host, view).coerceIn(0f, slack), host, view)
        }

        /**
         * Clamp the horizontal offset of a *vertical* pill, which hangs off the `END` edge.
         * Kept as its own name because that is what the vertical form's call sites mean.
         */
        fun clampX(tx: Float, hostWidth: Int, viewWidth: Int): Float =
            clamp(Anchor.END, tx, hostWidth, viewWidth)

        /**
         * Clamp the vertical offset so the pill stays inside the host. The base anchor is
         * `CENTER_VERTICAL`, so the offset runs symmetrically about the centre and is bounded by
         * half the slack. A pill taller than the host has no slack at all and stays centred, which
         * is the least-bad answer: some of it is off screen whatever we do.
         */
        fun clampY(ty: Float, hostHeight: Int, viewHeight: Int): Float =
            clamp(Anchor.CENTER, ty, hostHeight, viewHeight)

        /**
         * The vertical offset that keeps the pill's **top** where it was when its height changes
         * from `oldHeight` to `newHeight`.
         *
         * Under `CENTER_VERTICAL` the top sits at `(host - height) / 2 + ty`, so growing by `d`
         * moves the top up by `d / 2` unless the offset compensates. The top is where the grip is,
         * which is what the finger just tapped and what has to stay reachable.
         */
        fun anchorY(ty: Float, oldHeight: Int, newHeight: Int): Float =
            keepEdge(Anchor.CENTER, ty, oldHeight, newHeight)

        /**
         * The offset that keeps the strip's **docked edge** still when it grows or shrinks.
         *
         * The docked edge is the one the grip sits on — the thing the finger just tapped, and the
         * only way to collapse the strip again. A strip anchored to an edge already holds that edge
         * for free: under `START` the near edge is the offset itself, and under `END` the far edge
         * is `host + offset`, neither of which involves the strip's size. Only `CENTER` has to
         * compensate, by half the growth, because a centred strip grows both ways.
         */
        fun keepEdge(anchor: Anchor, offset: Float, oldSize: Int, newSize: Int): Float =
            when (anchor) {
                Anchor.START, Anchor.END -> offset
                Anchor.CENTER -> offset + (newSize - oldSize) / 2f
            }

        /**
         * Where the pill's **top-left corner** sits, as a fraction of the host's width and height.
         *
         * Fractions rather than raw pixels because a remembered position has to survive things the
         * pixels do not: a rotation, a different panel (a Nomad is not a Manta), and the pill's own
         * size changing between its collapsed and expanded forms. The corner is where the grip is,
         * which is the part the reader actually aims at.
         */
        fun fraction(anchor: Anchor, offset: Float, host: Int, view: Int): Float =
            if (host <= 0) 0f else edge(anchor, offset, host, view) / host

        fun fractionX(tx: Float, hostWidth: Int, viewWidth: Int): Float =
            fraction(Anchor.END, tx, hostWidth, viewWidth)

        fun fractionY(ty: Float, hostHeight: Int, viewHeight: Int): Float =
            fraction(Anchor.CENTER, ty, hostHeight, viewHeight)

        /**
         * Turn a remembered [fractionX] back into a horizontal offset for the *current* geometry,
         * clamped so it lands on screen.
         *
         * The clamp is the point, not a formality: the fraction may have been saved on the other
         * rotation, on another device, or against a pill that a later build sized differently, and
         * a position that was legal then can be off screen now. Restoring the pill out of reach
         * would recreate the very trap #200 was opened about.
         */
        fun offsetForFraction(anchor: Anchor, fraction: Float, host: Int, view: Int): Float =
            if (host <= 0 || !fraction.isFinite()) {
                0f
            } else {
                clamp(anchor, offsetFor(anchor, fraction * host, host, view), host, view)
            }

        fun translationXFor(fraction: Float, hostWidth: Int, viewWidth: Int): Float =
            offsetForFraction(Anchor.END, fraction, hostWidth, viewWidth)

        fun translationYFor(fraction: Float, hostHeight: Int, viewHeight: Int): Float =
            offsetForFraction(Anchor.CENTER, fraction, hostHeight, viewHeight)
    }
}
