package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * **The InkRead Daily** — the inkread-daily front page (#66). A 1-bit *newspaper* home: masthead +
 * dated folio, the day's headlines (which double as the issue's table of contents), a **Today's Desk**
 * control panel (Read Today's Issue · Regenerate · Sources · Archive), and a reverse-chronological
 * **Back Issues** strip. The newspaper metaphor is native to e-ink and matches the home's Inkwell
 * voice ([Ink], [HomeActivity]).
 *
 * Real data, end to end: [DailyController] stores feed sources, fetches them, and the Rust core
 * (inkread-daily) parses + extracts + assembles a reflowable issue EPUB the reader opens. With no
 * sources yet it shows the first-run empty state; with sources but no issue, a compile prompt; with a
 * compiled issue, the populated front page.
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

    private val daily by lazy { DailyController(this) }

    private var scale = 1f
    private fun dim(u: Number) = dp((u.toFloat() * scale).toInt()).coerceAtLeast(1)
    private fun fs(u: Number) = u.toFloat() * scale

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(buildView())
    }

    override fun onResume() {
        super.onResume()
        setContentView(buildView()) // refresh after returning (a freshly compiled issue, etc.)
    }

    private fun buildView(): View {
        val w = resources.displayMetrics.widthPixels
        val side = (w * 0.06f).toInt().coerceIn(dp(18), dp(48))
        val contentW = (w - 2 * side).coerceAtLeast(dp(280))
        scale = (contentW.toFloat() / dp(748)).coerceIn(0.6f, 1.15f)

        val issue = daily.todayIssue()
        val hasIssue = issue != null
        val headlines = if (hasIssue) daily.todayHeadlines() else emptyList()
        val sources = daily.sources()
        val backIssues = daily.backIssues()

        val page = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(side, dim(26), side, dim(24))
            layoutParams = ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
        }

        page.addView(utilityRow())
        page.addView(masthead())
        page.addView(folio(hasIssue, headlines.size, sources.size))
        page.addView(gap(dim(20)))
        when {
            hasIssue && headlines.isNotEmpty() -> page.addView(headlinesBlock(headlines, issue!!))
            sources.isNotEmpty() -> page.addView(compilePrompt(sources.size))
            else -> page.addView(emptyState())
        }
        page.addView(gap(dim(22)))
        page.addView(todaysDesk(hasIssue, sources.size, issue))
        page.addView(gap(dim(20)))
        page.addView(backIssuesStrip(backIssues))
        page.addView(gap(dim(22)))
        page.addView(folioFooter())

        return ScrollView(this).apply {
            setBackgroundColor(paper)
            isFillViewport = true
            isVerticalScrollBarEnabled = false
            addView(page)
        }
    }

    // ── Utility row ─────────────────────────────────────────────────────────────────────────────

    private fun utilityRow(): View = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        addView(label("‹ Library", 11f, 0.12f).apply { isClickable = true; setOnClickListener { finish() } })
        addView(weighted())
        addView(label("Daily", 11f, 0.12f))
        addView(weighted())
        addView(label("Settings", 11f, 0.12f).apply {
            isClickable = true
            setOnClickListener { startActivity(Intent(this@DailyActivity, SettingsActivity::class.java)) }
        })
    }

    // ── Masthead ────────────────────────────────────────────────────────────────────────────────

    private fun masthead(): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        gravity = Gravity.CENTER_HORIZONTAL
        setPadding(0, dim(14), 0, dim(4))
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

    // ── Folio ───────────────────────────────────────────────────────────────────────────────────

    private fun folio(hasIssue: Boolean, articleCount: Int, sourceCount: Int): View {
        val band = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dim(2), dim(7), dim(2), dim(7))
            addView(label("Offline", 10f, 0.10f))
            addView(weighted())
            addView(label(todayLong(), 10f, 0.10f).apply { gravity = Gravity.CENTER })
            addView(weighted())
            addView(label(
                if (hasIssue) "$articleCount articles · $sourceCount sources" else "No issue yet",
                10f, 0.10f,
            ))
        }
        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(0, dim(12), 0, 0)
            addView(blackRule(dim(3)))
            addView(band)
            addView(blackRule(dim(3)))
        }
    }

    // ── Populated front page: lead story + headline columns (headlines = the TOC) ────────────────

    private fun headlinesBlock(headlines: List<DailyController.Headline>, issue: File): View =
        LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            // Lead story — the first headline, large.
            val lead = headlines.first()
            addView(label(lead.source, 10f, 0.18f).apply { setPadding(0, dim(6), 0, dim(6)) })
            addView(TextView(this@DailyActivity).apply {
                text = lead.title; setTextColor(ink); textSize = fs(30f); typeface = serifBold
                setLineSpacing(0f, 1.05f); isClickable = true; setOnClickListener { openIssue(issue) }
            })
            addView(blackRule(Ink.hair()).apply { (layoutParams as LinearLayout.LayoutParams).topMargin = dim(16) })
            // The rest, as a single column of source-tagged headlines (each opens the issue).
            headlines.drop(1).forEach { h ->
                addView(LinearLayout(this@DailyActivity).apply {
                    orientation = LinearLayout.VERTICAL
                    setPadding(0, dim(12), 0, dim(12))
                    isClickable = true; setOnClickListener { openIssue(issue) }
                    addView(label(h.source, 10f, 0.16f))
                    addView(TextView(this@DailyActivity).apply {
                        text = h.title; setTextColor(ink); textSize = fs(20f); typeface = serif
                        setLineSpacing(0f, 1.1f); setPadding(0, dim(4), 0, 0)
                    })
                })
                addView(blackRule(Ink.hair()))
            }
        }

    // ── Sources-but-no-issue: compile prompt ─────────────────────────────────────────────────────

    private fun compilePrompt(sourceCount: Int): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        gravity = Gravity.CENTER_HORIZONTAL
        setPadding(dim(8), dim(26), dim(8), dim(26))
        addView(TextView(this@DailyActivity).apply {
            text = "Ready to compile"; setTextColor(ink); textSize = fs(22f); typeface = serif
            gravity = Gravity.CENTER
        })
        addView(TextView(this@DailyActivity).apply {
            text = "$sourceCount source${if (sourceCount == 1) "" else "s"} added. Compile today's issue to start reading."
            setTextColor(Ink.inkSoft); textSize = fs(16f); typeface = serifItalic
            gravity = Gravity.CENTER; setLineSpacing(0f, 1.4f); setPadding(dim(10), dim(8), dim(10), 0)
        })
        addView(primaryButton("Compile today's issue") { compileFlow() }.apply {
            (layoutParams as LinearLayout.LayoutParams).topMargin = dim(22)
        })
    }

    // ── First-run empty state ─────────────────────────────────────────────────────────────────────

    private fun emptyState(): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        gravity = Gravity.CENTER_HORIZONTAL
        setPadding(dim(8), dim(28), dim(8), dim(28))
        addView(TextView(this@DailyActivity).apply {
            text = "＋"; setTextColor(ink); textSize = fs(30f); gravity = Gravity.CENTER
            val d = dim(74)
            layoutParams = LinearLayout.LayoutParams(d, d)
            background = GradientDrawable().apply { setColor(paper); shape = GradientDrawable.OVAL; setStroke(Ink.keyline(), ink) }
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
        addView(primaryButton("＋  Add your first source") { addSourceDialog() }.apply {
            (layoutParams as LinearLayout.LayoutParams).topMargin = dim(22)
        })
        addView(label("RSS · Atom · paste a feed URL", 11f, 0.10f).apply {
            gravity = Gravity.CENTER; setPadding(0, dim(12), 0, 0)
        })
    }

    // ── Today's Desk ──────────────────────────────────────────────────────────────────────────────

    private fun todaysDesk(hasIssue: Boolean, sources: Int, issue: File?): View {
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
            addView(deskPrimary(hasIssue, issue), LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, 1.6f))
            addView(deskButton(if (hasIssue) "Regenerate" else "Compile") { compileFlow() }, deskLp(dim(12)))
            addView(deskButton("Sources · $sources") { sourcesDialog() }, deskLp(dim(12)))
            addView(deskButton("Archive") { archiveDialog() }, deskLp(dim(12)))
        }
        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            background = GradientDrawable().apply { setColor(paper); setStroke(Ink.keyline(), ink) }
            addView(header)
            addView(controls)
        }
    }

    private fun deskPrimary(hasIssue: Boolean, issue: File?): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        gravity = Gravity.CENTER_VERTICAL
        setBackgroundColor(if (hasIssue) ink else Color.parseColor("#9E9E9E"))
        setPadding(dim(18), dim(16), dim(18), dim(16))
        isClickable = true
        setOnClickListener {
            if (hasIssue && issue != null) openIssue(issue) else compileFlow()
        }
        addView(TextView(this@DailyActivity).apply {
            text = "Read Today's Issue"; setTextColor(paper); textSize = fs(20f); typeface = serifBold
            includeFontPadding = false
        })
        addView(label(if (hasIssue) "Opens the reflowable EPUB" else "Compile to read", 10f, 0.08f).apply {
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

    // ── Back Issues ───────────────────────────────────────────────────────────────────────────────

    private fun backIssuesStrip(issues: List<DailyController.BackIssue>): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        addView(LinearLayout(this@DailyActivity).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            addView(label("Back Issues", 11f, 0.18f))
            addView(blackRule(Ink.hair()), LinearLayout.LayoutParams(0, Ink.hair(), 1f).apply {
                marginStart = dim(14); marginEnd = dim(14)
            })
        })
        if (issues.isEmpty()) {
            addView(TextView(this@DailyActivity).apply {
                text = "Past issues appear here once you've compiled a few."
                setTextColor(Ink.muted); textSize = fs(14f); typeface = serifItalic
                gravity = Gravity.CENTER; setPadding(0, dim(18), 0, dim(6))
            })
        } else {
            addView(LinearLayout(this@DailyActivity).apply {
                orientation = LinearLayout.HORIZONTAL
                setPadding(0, dim(14), 0, 0)
                issues.take(4).forEachIndexed { i, bi ->
                    addView(TextView(this@DailyActivity).apply {
                        text = bi.dateLabel; setTextColor(ink); textSize = fs(15f); typeface = serif
                        gravity = Gravity.CENTER; setPadding(dim(8), dim(14), dim(8), dim(14))
                        background = GradientDrawable().apply { setColor(paper); setStroke(Ink.hair(), ink) }
                        isClickable = true; setOnClickListener { openIssue(bi.file) }
                    }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
                        marginStart = if (i == 0) 0 else dim(12)
                    })
                }
            })
        }
    }

    // ── Folio footer ──────────────────────────────────────────────────────────────────────────────

    private fun folioFooter(): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        addView(blackRule(Ink.hair()).apply { (layoutParams as LinearLayout.LayoutParams).bottomMargin = dim(12) })
        addView(label("Compiled on device · Offline", 10f, 0.12f).apply { gravity = Gravity.CENTER })
    }

    // ── Actions ───────────────────────────────────────────────────────────────────────────────────

    private fun openIssue(file: File) = startActivity(
        Intent(this, ReaderActivity::class.java)
            .putExtra(ReaderActivity.EXTRA_BOOK_PATH, file.absolutePath)
            .putExtra(ReaderActivity.EXTRA_BOOK_ID, file.name),
    )

    private fun compileFlow() {
        if (daily.sources().isEmpty()) {
            addSourceDialog()
            return
        }
        Toast.makeText(this, "Compiling today's issue…", Toast.LENGTH_SHORT).show()
        daily.compile { ok, msg ->
            runOnUiThread {
                Toast.makeText(this, msg, Toast.LENGTH_SHORT).show()
                if (ok) setContentView(buildView())
            }
        }
    }

    private fun addSourceDialog() {
        val input = EditText(this).apply {
            hint = "https://example.com/feed.xml"
            setSingleLine()
        }
        AlertDialog.Builder(this, R.style.InkDialog)
            .setTitle("Add a source")
            .setMessage("Paste an RSS or Atom feed URL.")
            .setView(input)
            .setPositiveButton("Add") { _, _ ->
                daily.addSource(input.text.toString())
                setContentView(buildView())
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun sourcesDialog() {
        val sources = daily.sources()
        if (sources.isEmpty()) {
            addSourceDialog()
            return
        }
        val labels = sources.map { "${it.name}\n${it.url}" }.toTypedArray()
        AlertDialog.Builder(this, R.style.InkDialog)
            .setTitle("Sources")
            .setItems(labels) { _, which ->
                val s = sources[which]
                AlertDialog.Builder(this, R.style.InkDialog)
                    .setTitle(s.name)
                    .setMessage(s.url)
                    .setPositiveButton("Remove") { _, _ -> daily.removeSource(s.url); setContentView(buildView()) }
                    .setNegativeButton("Keep", null)
                    .show()
            }
            .setPositiveButton("Add source") { _, _ -> addSourceDialog() }
            .setNegativeButton("Close", null)
            .show()
    }

    private fun archiveDialog() {
        val issues = daily.backIssues()
        if (issues.isEmpty()) {
            Toast.makeText(this, "No back issues yet", Toast.LENGTH_SHORT).show()
            return
        }
        val labels = issues.map { it.dateLabel }.toTypedArray()
        AlertDialog.Builder(this, R.style.InkDialog)
            .setTitle("Back Issues")
            .setItems(labels) { _, which -> openIssue(issues[which].file) }
            .setNegativeButton("Close", null)
            .show()
    }

    // ── Shared pieces ─────────────────────────────────────────────────────────────────────────────

    private fun primaryButton(text: String, onTap: () -> Unit): View = TextView(this).apply {
        this.text = text; setTextColor(paper); textSize = fs(17f); typeface = serifBold
        gravity = Gravity.CENTER; setPadding(dim(24), dim(14), dim(24), dim(14))
        background = GradientDrawable().apply { setColor(ink) }
        isClickable = true; setOnClickListener { onTap() }
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
    }

    private fun label(text: String, size: Float, spacing: Float): TextView = TextView(this).apply {
        this.text = text.uppercase(); setTextColor(ink); textSize = fs(size)
        typeface = mono; letterSpacing = spacing
    }

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
}
