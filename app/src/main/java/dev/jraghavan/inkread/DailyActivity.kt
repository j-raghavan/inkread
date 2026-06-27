package dev.jraghavan.inkread

import android.app.Activity
import android.content.Intent
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
import android.widget.Toast
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * **The InkRead Daily** — the inkread-daily front page (#66). A 1-bit *newspaper* home for the daily
 * reading companion: a masthead, a dated folio, the day's headlines (which double as the issue's
 * table of contents), a **Today's Desk** control panel (Read Today's Issue · Regenerate · Sources ·
 * Archive), and a reverse-chronological **Back Issues** strip. The newspaper metaphor is native to
 * e-ink — rules, columns, halftone, no glow, no colour — and matches the home screen's Inkwell voice
 * ([Ink], [HomeActivity]).
 *
 * Honest by default (like [HomeActivity]): with no compiled issue yet this shows the first-run empty
 * state and real chrome rather than faked headlines/counts. The fetch + extract + assemble backend
 * (the inkread-daily crate's EPUB assembly, plus a later fetch slice) wires the populated front page
 * and the control actions; today they are stubs, so nothing decorative is invented.
 */
class DailyActivity : Activity() {

    private val density get() = resources.displayMetrics.density
    private fun dp(v: Int) = (v * density).toInt()

    private val script by lazy { Typeface.createFromAsset(assets, "fonts/pinyon_script.ttf") }
    private val serif = Ink.serif
    private val serifBold = Ink.serifBold
    private val serifItalic = Ink.serifItalic
    private val mono = Ink.mono
    private val ink = Ink.ink
    private val paper = Ink.paper

    /** Width-driven scale over the Daily artboard's ~748-unit content column (mirrors HomeActivity). */
    private var scale = 1f
    private fun dim(u: Number) = dp((u.toFloat() * scale).toInt()).coerceAtLeast(1)
    private fun fs(u: Number) = u.toFloat() * scale

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(buildView())
    }

    private fun buildView(): View {
        val w = resources.displayMetrics.widthPixels
        val side = (w * 0.06f).toInt().coerceIn(dp(18), dp(48))
        val contentW = (w - 2 * side).coerceAtLeast(dp(280))
        scale = (contentW.toFloat() / dp(748)).coerceIn(0.6f, 1.15f)

        // Today's issue is unavailable until the fetch/assemble backend lands — first-run state.
        val hasIssue = false
        val sourceCount = 0

        val page = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(side, dim(26), side, dim(24))
            layoutParams = ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
        }

        page.addView(utilityRow())
        page.addView(masthead())
        page.addView(folio(hasIssue))
        page.addView(gap(dim(20)))
        page.addView(emptyState())
        page.addView(gap(dim(22)))
        page.addView(todaysDesk(hasIssue, sourceCount))
        page.addView(gap(dim(20)))
        page.addView(backIssues())
        page.addView(gap(dim(22)))
        page.addView(folioFooter())

        return ScrollView(this).apply {
            setBackgroundColor(paper)
            isFillViewport = true
            isVerticalScrollBarEnabled = false
            addView(page)
        }
    }

    // ── Utility row: ‹ Library · Daily · Settings ───────────────────────────────────────────────

    private fun utilityRow(): View = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        addView(label("‹ Library", 11f, 0.12f).apply {
            isClickable = true; setOnClickListener { finish() }
        })
        addView(weighted())
        addView(label("Daily", 11f, 0.12f))
        addView(weighted())
        addView(label("Settings", 11f, 0.12f).apply {
            isClickable = true
            setOnClickListener { startActivity(Intent(this@DailyActivity, SettingsActivity::class.java)) }
        })
    }

    // ── Masthead: THE / [icon] InkRead / —— DAILY —— ────────────────────────────────────────────

    private fun masthead(): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        gravity = Gravity.CENTER_HORIZONTAL
        setPadding(0, dim(14), 0, dim(4))
        // A heavy top rule opens the masthead (newspaper convention).
        addView(blackRule(dim(3)).apply { (layoutParams as LinearLayout.LayoutParams).bottomMargin = dim(12) })
        addView(label("The", 11f, 0.34f).apply { gravity = Gravity.CENTER })
        addView(LinearLayout(this@DailyActivity).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER
            setPadding(0, dim(2), 0, 0)
            addView(android.widget.ImageView(this@DailyActivity).apply { setImageResource(R.mipmap.ic_launcher) },
                LinearLayout.LayoutParams(dim(38), dim(38)).apply { marginEnd = dim(14); gravity = Gravity.CENTER_VERTICAL })
            addView(TextView(this@DailyActivity).apply {
                text = "InkRead"; setTextColor(ink); textSize = fs(64f); typeface = script
                includeFontPadding = false; paint.isFakeBoldText = true
            })
        })
        addView(LinearLayout(this@DailyActivity).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(0, dim(6), 0, 0)
            addView(blackRule(dim(2)), LinearLayout.LayoutParams(dim(110), dim(2)))
            addView(TextView(this@DailyActivity).apply {
                text = "DAILY"; setTextColor(ink); textSize = fs(22f); typeface = serifBold
                letterSpacing = 0.45f; setPadding(dim(14), 0, dim(8), 0); includeFontPadding = false
            })
            addView(blackRule(dim(2)), LinearLayout.LayoutParams(dim(110), dim(2)))
        })
    }

    // ── Folio bar: status · date · issue summary (between heavy rules) ───────────────────────────

    private fun folio(hasIssue: Boolean): View = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        setPadding(dim(2), dim(7), dim(2), dim(7))
        val top = blackRule(dim(3))
        val bottom = blackRule(dim(3))
        // wrap content between a heavy top + bottom rule
        addView(label("Offline", 10f, 0.10f))
        addView(weighted())
        addView(label(todayLong(), 10f, 0.10f).apply { gravity = Gravity.CENTER })
        addView(weighted())
        addView(label(if (hasIssue) "Today's issue" else "No issue yet", 10f, 0.10f))
        // compose the bordered band
    }.let { band ->
        LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(0, dim(12), 0, 0)
            addView(blackRule(dim(3)))
            addView(band)
            addView(blackRule(dim(3)))
        }
    }

    // ── First-run empty state: no issue compiled yet ────────────────────────────────────────────

    private fun emptyState(): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        gravity = Gravity.CENTER_HORIZONTAL
        setPadding(dim(8), dim(28), dim(8), dim(28))
        // a ringed plus mark
        addView(TextView(this@DailyActivity).apply {
            text = "＋"; setTextColor(ink); textSize = fs(30f); gravity = Gravity.CENTER
            val d = dim(74)
            layoutParams = LinearLayout.LayoutParams(d, d)
            background = GradientDrawable().apply {
                setColor(paper); shape = GradientDrawable.OVAL; setStroke(Ink.keyline(), ink)
            }
            includeFontPadding = false
        })
        addView(TextView(this@DailyActivity).apply {
            text = "No issue compiled yet"; setTextColor(ink); textSize = fs(22f); typeface = serif
            gravity = Gravity.CENTER; setPadding(0, dim(18), 0, 0)
        })
        addView(TextView(this@DailyActivity).apply {
            text = "Add a few feeds and InkRead will assemble today's reading into a single calm issue."
            setTextColor(Ink.inkSoft); textSize = fs(16f); typeface = serifItalic
            gravity = Gravity.CENTER; setLineSpacing(0f, 1.4f); setPadding(dim(10), dim(8), dim(10), 0)
        })
        addView(TextView(this@DailyActivity).apply {
            text = "＋  Add your first source"; setTextColor(paper); textSize = fs(17f); typeface = serifBold
            gravity = Gravity.CENTER; setPadding(dim(24), dim(14), dim(24), dim(14))
            background = GradientDrawable().apply { setColor(ink) }
            isClickable = true; setOnClickListener { stub("Sources") }
            layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
                .apply { topMargin = dim(22) }
        })
        addView(label("RSS · Atom · Hacker News · paste a URL", 11f, 0.10f).apply {
            gravity = Gravity.CENTER; setPadding(0, dim(12), 0, 0)
        })
    }

    // ── Today's Desk: the control panel ─────────────────────────────────────────────────────────

    private fun todaysDesk(hasIssue: Boolean, sources: Int): View {
        val header = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setBackgroundColor(ink)
            setPadding(dim(16), dim(8), dim(16), dim(8))
            addView(label("Today's Desk", 11f, 0.2f).apply { setTextColor(paper) })
            addView(weighted())
            addView(label(if (hasIssue) "Compiled" else "Awaiting sources", 11f, 0.1f).apply { setTextColor(paper) })
        }
        val controls = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(dim(14), dim(14), dim(14), dim(14))
            // Primary read (wide). Disabled-looking until an issue exists.
            addView(deskPrimary(hasIssue), LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, 1.6f))
            addView(deskButton("Regenerate") { stub("Regenerate") }, deskLp(dim(12)))
            addView(deskButton("Sources · $sources") { stub("Sources") }, deskLp(dim(12)))
            addView(deskButton("Archive") { stub("Archive") }, deskLp(dim(12)))
        }
        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            background = GradientDrawable().apply { setColor(paper); setStroke(Ink.keyline(), ink) }
            addView(header)
            addView(controls)
        }
    }

    private fun deskPrimary(hasIssue: Boolean): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        gravity = Gravity.CENTER_VERTICAL
        setBackgroundColor(if (hasIssue) ink else Color.parseColor("#9E9E9E"))
        setPadding(dim(18), dim(16), dim(18), dim(16))
        isClickable = true
        setOnClickListener { if (hasIssue) stub("Read Today's Issue") else stub("Add sources to compile today's issue") }
        addView(TextView(this@DailyActivity).apply {
            text = "Read Today's Issue"; setTextColor(paper); textSize = fs(20f); typeface = serifBold
            includeFontPadding = false
        })
        addView(label(if (hasIssue) "Opens the reflowable EPUB" else "No issue yet", 10f, 0.08f).apply {
            setTextColor(Color.parseColor("#E8E8E8")); setPadding(0, dim(5), 0, 0)
        })
    }

    private fun deskButton(text: String, onTap: () -> Unit): View = TextView(this).apply {
        this.text = text; setTextColor(ink); textSize = fs(13f); typeface = serif
        gravity = Gravity.CENTER; setPadding(dim(10), dim(16), dim(10), dim(16))
        background = GradientDrawable().apply { setColor(paper); setStroke(Ink.hair(), ink) }
        isClickable = true; setOnClickListener { onTap() }
    }

    private fun deskLp(start: Int) = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, 1f)
        .apply { marginStart = start }

    // ── Back Issues strip ───────────────────────────────────────────────────────────────────────

    private fun backIssues(): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        addView(LinearLayout(this@DailyActivity).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            addView(label("Back Issues", 11f, 0.18f))
            addView(blackRule(Ink.hair()), LinearLayout.LayoutParams(0, Ink.hair(), 1f).apply {
                marginStart = dim(14); marginEnd = dim(14)
            })
            addView(label("View all ›", 11f, 0.1f).apply { isClickable = true; setOnClickListener { stub("Archive") } })
        })
        // No archive yet — honest placeholder rather than faked dated cards.
        addView(TextView(this@DailyActivity).apply {
            text = "Past issues appear here once you've compiled a few."
            setTextColor(Ink.muted); textSize = fs(14f); typeface = serifItalic
            gravity = Gravity.CENTER; setPadding(0, dim(18), 0, dim(6))
        })
    }

    // ── Folio footer ────────────────────────────────────────────────────────────────────────────

    private fun folioFooter(): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        addView(blackRule(Ink.hair()).apply { (layoutParams as LinearLayout.LayoutParams).bottomMargin = dim(12) })
        addView(LinearLayout(this@DailyActivity).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            addView(label("Compiled on device · Offline", 10f, 0.12f))
            addView(weighted())
            addView(label("Export PDF ›", 10f, 0.12f).apply {
                isClickable = true; setOnClickListener { stub("Export PDF") }
            })
        })
    }

    // ── Shared pieces ───────────────────────────────────────────────────────────────────────────

    /** A mono, uppercase, letter-spaced label — the newspaper's Space-Mono kicker. */
    private fun label(text: String, size: Float, spacing: Float): TextView = TextView(this).apply {
        this.text = text.uppercase(); setTextColor(ink); textSize = fs(size)
        typeface = mono; letterSpacing = spacing
    }

    /** A solid black rule (newspapers use black keylines, not the hairline grey). */
    private fun blackRule(thickness: Int): View = View(this).apply {
        setBackgroundColor(ink)
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, thickness)
    }

    private fun gap(h: Int): View = View(this).apply {
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, h)
    }

    private fun weighted(): View = View(this).apply {
        layoutParams = LinearLayout.LayoutParams(0, 1, 1f)
    }

    private fun todayLong(): String =
        SimpleDateFormat("EEEE, MMMM d, yyyy", Locale.getDefault()).format(Date())

    /** Placeholder for an action whose backend slice hasn't landed yet (#66). */
    private fun stub(what: String) =
        Toast.makeText(this, "$what — coming soon", Toast.LENGTH_SHORT).show()
}
