package dev.jraghavan.inkread

import android.app.Dialog
import android.content.Context
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.Window
import android.widget.LinearLayout
import android.widget.TextView

/**
 * The shared **Inkwell editorial** visual language for inkread's popups, dialogs, and overlays
 * (ADR-INKREAD-0008) — the home screen's voice, centralized so every surface reads as one product
 * instead of drifting per file (each used to hardcode its own greys, radii, and strokes).
 *
 * Monochrome by necessity: nothing here leans on colour, only on **contrast, weight, and
 * whitespace** — the levers that actually read on a flashing e-ink panel. Pure, stateless tokens
 * plus tiny view builders. Density comes from the system metrics, so [dp] needs no `Context`; only
 * the builders that create views do.
 */
object Ink {
    // ── Palette — the single source of truth for greys (was scattered hex across files) ──
    val ink = Color.BLACK // primary text / keyline
    val inkSoft = Color.parseColor("#3A3A3A") // secondary text
    val muted = Color.parseColor("#757575") // captions / eyebrow labels
    val hairline = Color.parseColor("#E0E0E0") // rules & dividers
    val ringSoft = Color.parseColor("#9E9E9E") // soft ring around light swatches
    val disabled = Color.parseColor("#BDBDBD") // disabled icon tint
    val fill = Color.parseColor("#EFEFEF") // subtle filled-chip background
    val paper = Color.WHITE

    // ── Type — the Inkwell voice (serif display, mono labels) ──
    val serif: Typeface = Typeface.SERIF
    val serifItalic: Typeface = Typeface.create(Typeface.SERIF, Typeface.ITALIC)
    val serifBold: Typeface = Typeface.create(Typeface.SERIF, Typeface.BOLD)
    val mono: Typeface = Typeface.MONOSPACE

    // ── Metrics (system density → no Context needed) ──
    private val density = android.content.res.Resources.getSystem().displayMetrics.density

    /**
     * UI-chrome scale (#133) — the user's menu-size preference ([DisplayPrefs.uiScale]), set once at
     * Activity start. Chrome text sizes are declared at their design value and passed through [sp],
     * so a larger panel (e.g. the Manta) gets proportionally bigger menus. 1.0 = the design sizing.
     */
    @JvmField
    var uiScale: Float = 1f

    /** A chrome text size, in sp, scaled by the user's [uiScale]. */
    fun sp(designSp: Float): Float = designSp * uiScale

    /** A chrome dimension, in px, scaled by the user's [uiScale] — for the fixed-size boxes (icon
     *  cells, etc.) around [sp] text, so a raised menu size grows the whole control, not just the
     *  glyph. `uiScale == 1f` returns exactly [dp] (no change at the default). */
    fun sdp(v: Int): Int = (dp(v) * uiScale).toInt()

    fun dp(v: Int): Int = (v * density).toInt()

    fun dpf(v: Int): Float = v * density

    /**
     * Minimum extent for a **compact** tappable control (#245) — a button or an option cell, where
     * the target is roughly as wide as it is tall.
     *
     * Deliberately **not** scaled by [uiScale]. Menu Size changes how big the chrome looks; it does
     * not change how big a fingertip is, and the smaller steps were putting clickable cells at
     * 20-32dp. Measured on a Manta at XS: the page label 21.9dp, the chapter buttons 24.0dp, the
     * Adjust sheet's segmented options 31.5dp — including the Menu Size selector itself, so the
     * control that gets you back out of XS was among the hardest to hit.
     *
     * 44 rather than the 48 the platform recommends. 48 was tried first and made the reading bar
     * 26% taller at the default, which reads as a bar that has taken over the page. 44 keeps every
     * control comfortably hittable while giving that back. Applied as a minimum, so a control
     * already larger is untouched.
     */
    fun tap(): Int = dp(44)

    /**
     * Minimum height for a **full-width row** (#245) — a list row or the chapter line, spanning the
     * whole panel.
     *
     * Lower than [tap] on purpose. A row 1920px wide is a far more forgiving target than a 90px
     * button of the same height: the hard part of hitting a control is its smaller dimension, and
     * these have none. Trading 4dp of height per row back to the page is worth more here than the
     * margin it buys.
     */
    fun tapRow(): Int = dp(40)

    /** A hairline that never collapses to 0 px. */
    fun hair(): Int = maxOf(1, dp(1))

    /** A card keyline — a touch heavier so the edge survives a GC16 full-flash. */
    fun keyline(): Int = maxOf(2, dp(1))

    // ── Shape tokens (one rhythm instead of 10/12/16/22 ad hoc) ──
    const val RADIUS = 16 // card corner
    const val RADIUS_CHIP = 10 // pill / chip corner
    const val PAD = 22 // card inner padding

    // ── Builders ──

    /** A white rounded card with a single black keyline — the base of every editorial surface. */
    fun cardBg(radiusDp: Int = RADIUS): GradientDrawable =
        GradientDrawable().apply {
            setColor(paper)
            cornerRadius = dpf(radiusDp)
            setStroke(keyline(), ink)
        }

    /** A mono, uppercase, letter-spaced **eyebrow** — the small-caps kicker above a title. */
    fun eyebrow(ctx: Context, text: String): TextView =
        TextView(ctx).apply {
            this.text = text.uppercase()
            setTextColor(muted)
            textSize = sp(11f)
            typeface = mono
            letterSpacing = 0.14f
        }

    /** A serif display title. */
    fun title(ctx: Context, text: String, size: Float = 22f): TextView =
        TextView(ctx).apply {
            this.text = text
            setTextColor(ink)
            textSize = sp(size)
            typeface = serif
            includeFontPadding = false
        }

    /** A full-width hairline rule. */
    fun rule(ctx: Context): View =
        View(ctx).apply {
            setBackgroundColor(hairline)
            layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, hair())
        }

    /** A fixed vertical spacer of [h] dp. */
    fun gap(ctx: Context, h: Int): View =
        View(ctx).apply {
            layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(h))
        }

    /** A non-cancelable busy notice ("Reflowing…", "Compiling…") on the white card, shown
     *  immediately; the caller keeps the returned dialog and dismisses it when the work completes.
     *  The explicit light window background matters: without it the dialog renders as a black box
     *  on this e-ink theme (#55). */
    fun progressDialog(ctx: Context, message: String): Dialog =
        Dialog(ctx).apply {
            requestWindowFeature(Window.FEATURE_NO_TITLE)
            setCancelable(false)
            setContentView(
                TextView(ctx).apply {
                    text = message
                    textSize = sp(16f)
                    setTextColor(ink)
                    setPadding(dp(28), dp(22), dp(28), dp(22))
                },
            )
            window?.setBackgroundDrawable(cardBg())
            runCatching { show() }
        }

    /** A pill button — filled (black) for the primary action, outlined for secondary. */
    fun pillButton(ctx: Context, label: String, primary: Boolean, onTap: () -> Unit): TextView =
        TextView(ctx).apply {
            text = label
            textSize = sp(13f)
            typeface = mono
            letterSpacing = 0.08f
            gravity = Gravity.CENTER
            setPadding(dp(20), dp(9), dp(20), dp(9))
            setTextColor(if (primary) paper else ink)
            background =
                GradientDrawable().apply {
                    setColor(if (primary) ink else paper)
                    setStroke(keyline(), ink)
                    cornerRadius = dpf(40)
                }
            isClickable = true
            setOnClickListener { onTap() }
        }
}
