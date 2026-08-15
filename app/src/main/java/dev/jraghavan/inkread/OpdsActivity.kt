package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.app.Dialog
import android.content.Intent
import android.graphics.Typeface
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import java.util.concurrent.Executors

/**
 * **Your library** — browsing a calibre content server or Calibre-Web over OPDS
 * (ADR-INKREAD-0016, #175). One screen, one job: walk the catalog, then bring a book onto the shelf.
 *
 * The shape follows the catalog rather than inventing one. A feed is a list of rows that are either
 * *navigation* (another feed: by author, by tag, recent) or *acquisition* (a book), so the screen is
 * a list of rows, a back stack of the feeds walked into, and a Next control when the server pages.
 * Deliberately typographic and flat like [DailyActivity] and [HomeActivity] — no covers are fetched,
 * because a grid of images on a panel that repaints in whole frames costs a great deal of flashing
 * for very little; the title and author are what you choose a book by anyway.
 *
 * Network work runs on a single background thread and posts results back; the screen shows what it
 * is doing, and a failure says so in place rather than leaving an empty list looking like an empty
 * library.
 */
class OpdsActivity : Activity() {

    private val density get() = resources.displayMetrics.density
    private fun dp(v: Int) = (v * density).toInt()

    private val serif = Ink.serif
    private val serifItalic = Ink.serifItalic
    private val mono = Ink.mono
    private val ink = Ink.ink

    private val opds by lazy { OpdsController(this) }
    private val io = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())

    /** The feeds walked into, so Back steps out one level instead of leaving the library. */
    private val trail = ArrayDeque<String>()

    private var current: OpdsController.Catalog? = null
    private var busy: Dialog? = null

    private lateinit var column: LinearLayout

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Ink.uiScale = DisplayPrefs(this).uiScale
        column = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(22), dp(20), dp(22), dp(28))
        }
        setContentView(ScrollView(this).apply { isFillViewport = true; addView(column) })

        val root = OpdsController.catalogRoot(AppSettings.opdsUrl(this))
        if (root.isEmpty()) {
            render(message = "No library is set up yet.\n\nAdd your calibre or Calibre-Web address in Settings → Library.")
            return
        }
        load(root, pushTrail = false)
    }

    override fun onDestroy() {
        super.onDestroy()
        io.shutdownNow()
    }

    /**
     * Back steps out of the feed we walked into; at the top it leaves the library as usual.
     *
     * Deprecated upstream in favour of `OnBackInvokedCallback`, which is API 33 — the Supernote runs
     * Android 11 (API 30), and the app does not depend on androidx.activity for its dispatcher
     * back-port. This override is the mechanism that actually exists on the target device.
     */
    @Suppress("DEPRECATION", "OVERRIDE_DEPRECATION")
    override fun onBackPressed() {
        val previous = trail.removeLastOrNull()
        if (previous == null) super.onBackPressed() else load(previous, pushTrail = false)
    }

    // ── loading ──────────────────────────────────────────────────────────────────────────────────

    private fun load(url: String, pushTrail: Boolean) {
        val from = currentUrl
        showBusy("Opening the library…")
        io.execute {
            val catalog = opds.fetchCatalog(url)
            main.post {
                dismissBusy()
                if (catalog == null) {
                    render(
                        message = "Could not reach the library.\n\nCheck that the server is running " +
                            "and reachable from this device. If it asks for a login, calibre must be " +
                            "started with --auth-mode=basic.",
                    )
                    return@post
                }
                if (pushTrail && from != null) trail.addLast(from)
                currentUrl = url
                current = catalog
                render()
            }
        }
    }

    private var currentUrl: String? = null

    // ── rendering ────────────────────────────────────────────────────────────────────────────────

    private fun render(message: String? = null) {
        column.removeAllViews()
        column.addView(header(current?.title?.ifBlank { null } ?: "Your library"))

        if (message != null) {
            column.addView(note(message))
            return
        }
        val catalog = current ?: return

        if (catalog.searchTemplate.isNotEmpty()) column.addView(searchRow(catalog.searchTemplate))

        if (catalog.entries.isEmpty()) {
            column.addView(note("Nothing here."))
        } else {
            for (entry in catalog.entries) column.addView(row(entry))
        }
        if (catalog.next.isNotEmpty()) {
            column.addView(spacer(dp(18)))
            column.addView(Ink.pillButton(this, "More →", primary = false) { load(catalog.next, pushTrail = true) })
        }
    }

    private fun header(title: String): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        addView(Ink.eyebrow(this@OpdsActivity, "Library"))
        addView(TextView(this@OpdsActivity).apply {
            text = title
            setTextColor(ink); textSize = Ink.sp(28f); typeface = serif
            setPadding(0, dp(4), 0, dp(2))
        })
        addView(Ink.rule(this@OpdsActivity))
        addView(spacer(dp(14)))
    }

    /** A catalog row. Navigation walks in; a book offers its best openable format. */
    private fun row(entry: OpdsController.Entry): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(0, dp(12), 0, dp(12))
        isClickable = true
        setOnClickListener { onRowTapped(entry) }

        addView(TextView(this@OpdsActivity).apply {
            text = entry.title.ifBlank { "Untitled" }
            setTextColor(ink); textSize = Ink.sp(18f); typeface = serif
            maxLines = 3; setLineSpacing(0f, 1.1f)
        })
        if (entry.author.isNotEmpty()) {
            addView(TextView(this@OpdsActivity).apply {
                text = entry.author
                setTextColor(Ink.inkSoft); textSize = Ink.sp(14f); typeface = serifItalic
                setPadding(0, dp(3), 0, 0); maxLines = 2
            })
        }
        addView(TextView(this@OpdsActivity).apply {
            text = subtitleFor(entry)
            setTextColor(Ink.muted); textSize = Ink.sp(11f); typeface = mono; letterSpacing = 0.08f
            setPadding(0, dp(5), 0, 0)
        })
        addView(Ink.rule(this@OpdsActivity))
    }

    /**
     * The one-line status under a row: where it leads, or what would be downloaded. A book whose
     * formats inkread cannot open says so here rather than failing on the tap.
     */
    private fun subtitleFor(entry: OpdsController.Entry): String {
        if (entry.navigation) return "BROWSE →"
        val best = entry.best
            ?: return "UNSUPPORTED FORMAT" + entry.formats.firstOrNull()?.let { " · ${it.mime}" }.orEmpty()
        val size = if (best.bytes > 0) " · ${best.bytes / 1024 / 1024} MB" else ""
        return "DOWNLOAD ${best.ext.uppercase()}$size"
    }

    private fun onRowTapped(entry: OpdsController.Entry) {
        if (entry.navigation) {
            if (entry.href.isEmpty()) return
            load(entry.href, pushTrail = true)
            return
        }
        val format = entry.best ?: run {
            Toast.makeText(this, "inkread can't open any format of this book", Toast.LENGTH_SHORT).show()
            return
        }
        showBusy("Downloading ${entry.title}…")
        io.execute {
            val file = opds.download(entry, format)
            main.post {
                dismissBusy()
                if (file == null) {
                    Toast.makeText(this, "Download failed", Toast.LENGTH_SHORT).show()
                } else {
                    offerToRead(entry.title, file.absolutePath, file.name)
                }
            }
        }
    }

    /** A downloaded book is on the shelf either way; this just saves a trip back to Home to open it. */
    private fun offerToRead(title: String, path: String, id: String) {
        AlertDialog.Builder(this)
            .setTitle("Added to your shelf")
            .setMessage("$title is now on your shelf.")
            .setPositiveButton("Read now") { _, _ ->
                startActivity(
                    Intent(this, ReaderActivity::class.java)
                        .putExtra(ReaderActivity.EXTRA_BOOK_PATH, path)
                        .putExtra(ReaderActivity.EXTRA_BOOK_ID, id),
                )
            }
            .setNegativeButton("Keep browsing", null)
            .show()
    }

    // ── search ───────────────────────────────────────────────────────────────────────────────────

    private fun searchRow(template: String): View = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        setPadding(0, 0, 0, dp(10))
        val field = EditText(this@OpdsActivity).apply {
            hint = "Search the library"
            setTextColor(ink); textSize = Ink.sp(15f); typeface = serif
            setSingleLine()
        }
        addView(field, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        addView(Ink.pillButton(this@OpdsActivity, "Find", primary = true) {
            val url = OpdsController.searchUrl(template, field.text.toString())
            if (url.isNotEmpty()) load(url, pushTrail = true)
        })
    }

    // ── chrome ───────────────────────────────────────────────────────────────────────────────────

    private fun note(text: String): View = TextView(this).apply {
        this.text = text
        setTextColor(Ink.inkSoft); textSize = Ink.sp(15f)
        typeface = Typeface.create(serif, Typeface.NORMAL)
        setLineSpacing(0f, 1.25f)
        setPadding(0, dp(18), 0, 0)
    }

    private fun spacer(h: Int): View = View(this).apply {
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, h)
    }

    private fun showBusy(message: String) {
        dismissBusy()
        busy = Ink.progressDialog(this, message)
    }

    private fun dismissBusy() {
        runCatching { busy?.dismiss() }
        busy = null
    }
}
