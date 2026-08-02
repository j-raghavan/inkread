package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.app.Dialog
import android.content.Intent
import android.graphics.drawable.GradientDrawable
import android.text.InputType
import android.util.Log
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.Window
import android.widget.EditText
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.SeekBar
import android.widget.TextView
import android.widget.Toast

/**
 * The reader's bottom control bar + the navigation panels it opens (RR16/RR25), extracted from
 * `ReaderActivity` (SRP): the KOReader-style bar (page slider, chapter jumps, control row), the
 * go-to-page entry, bookmarks (toggle + list), Contents (TOC), the handwritten-annotations list,
 * and go-home. Sub-surfaces owned by other controllers (Search / Export / Dicts / Adjust) open
 * through [Host] callbacks.
 *
 * Threading mirrors the original inline code: panels are built on the UI thread; document state
 * (TOC, bookmarks, ink pages) is fetched on the engine thread via [Host.engineExecute].
 */
class BottomBarController(private val host: Host) {

    /** What the bar + panels need from the reader shell. */
    interface Host {
        /** Context for dialogs/toasts/resources, `runOnUiThread`, `startActivity`, `finish`. */
        val activity: Activity

        /** The open document handle (`0` = none); read live per call. */
        val docHandle: Long

        /** Cached page state, so showing the bar needs no engine round-trip. */
        val pageCount: Int

        val currentPage: Int

        /** Chapters as (start page, title), sorted; empty = no TOC. */
        val chapters: List<Pair<Int, String>>

        /** Per-book bookmarks store (engine thread only); null until a book is open. */
        val bookmarks: Bookmarks?

        /** Filesystem path of the open document (Daily issues live under `/daily/`). */
        val requestedPath: String?

        /** Run [block] on the single engine thread (serializes native access). */
        fun engineExecute(block: () -> Unit)

        /** Queue a jump to a 0-based page (render + refresh + persist position). */
        fun postJump(page: Int)

        /** Re-render + blit the current page (any thread). */
        fun repaintPanel()

        fun openPicker()

        /** Wrap dialog content so a resting palm can't tap through it. */
        fun palmGuard(content: View): View

        /** Quick zoom from the bar (the shell owns the zoom model + step constant). */
        fun zoomIn()

        fun zoomOut()

        // Sub-surfaces owned by sibling controllers.
        fun openSearch()

        fun openExport()

        fun openDicts()

        fun openAdjust()

        /** Manual full (flashing) EPD refresh to clear ghosting now (#99); also resets the cadence. */
        fun refreshNow()
    }

    private val activity: Activity get() = host.activity
    private fun runOnUiThread(block: () -> Unit) = activity.runOnUiThread(block)

    /**
     * The reader's bottom control bar (RR16/RR25), KOReader-style: a thin panel **hugging the
     * bottom edge** — a page slider with a tappable page indicator, above a flat row of controls
     * (Home · Library · Bookmark · Marks · Contents · Open). Built programmatically, high-contrast
     * for e-ink; uses the cached page state so showing it needs no engine round-trip.
     */
    fun showBottomBar() {
        if (host.docHandle == 0L) {
            host.openPicker()
            return
        }
        val total = host.pageCount.coerceAtLeast(1)
        val cur = host.currentPage.coerceIn(0, total - 1)
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()

        val dialog = Dialog(activity).apply { requestWindowFeature(Window.FEATURE_NO_TITLE) }
        val container = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Ink.paper)
        }
        // A crisp black keyline up top so the bar reads as a docked surface, not a floating box.
        container.addView(
            View(activity).apply { setBackgroundColor(Ink.ink) },
            LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, Ink.hair()),
        )

        // Page-slider row:  [N / Total]  ────────●────────  (grayscale, tap the chip to type a page)
        val pageLabel = TextView(activity).apply {
            text = "${cur + 1} / $total"
            setTextColor(Ink.ink)
            textSize = Ink.sp(12f)
            typeface = Ink.mono
            letterSpacing = 0.04f
            gravity = Gravity.CENTER
            setPadding(dp(12), dp(6), dp(12), dp(6))
            background = GradientDrawable().apply {
                setColor(Ink.fill)
                cornerRadius = Ink.dpf(40)
            }
            setOnClickListener { dialog.dismiss(); showPageEntry(total) }
        }
        // A refined, thin grayscale track + small round thumb (the default SeekBar reads clunky).
        val trackH = dp(3).coerceAtLeast(2)
        fun bar(c: Int) = GradientDrawable().apply { setColor(c); cornerRadius = trackH.toFloat(); setSize(0, trackH) }
        val track = android.graphics.drawable.LayerDrawable(
            arrayOf(
                bar(Ink.hairline),
                android.graphics.drawable.ClipDrawable(bar(Ink.ink), Gravity.START, android.graphics.drawable.ClipDrawable.HORIZONTAL),
            ),
        ).apply { setId(0, android.R.id.background); setId(1, android.R.id.progress) }
        val knob = GradientDrawable().apply { shape = GradientDrawable.OVAL; setColor(Ink.ink); setSize(dp(16), dp(16)) }
        val seek = SeekBar(activity).apply {
            max = total - 1
            progress = cur
            progressDrawable = track
            thumb = knob
            splitTrack = false
            setPadding(dp(10), dp(4), dp(10), dp(4))
            setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
                override fun onProgressChanged(sb: SeekBar, p: Int, fromUser: Boolean) {
                    if (fromUser) pageLabel.text = "${p + 1} / $total"
                }
                override fun onStartTrackingTouch(sb: SeekBar) {}
                override fun onStopTrackingTouch(sb: SeekBar) { dialog.dismiss(); host.postJump(sb.progress) }
            })
        }
        // Double-chevron chapter jumps flank the scrubber (distinct from single-page edge taps),
        // shown only when the document has a table of contents (1.7).
        fun chapterBtn(glyph: String, dir: Int) = TextView(activity).apply {
            text = glyph; setTextColor(Ink.ink); textSize = Ink.sp(20f); typeface = Ink.serifBold
            gravity = Gravity.CENTER; setPadding(dp(8), dp(2), dp(8), dp(2)); isClickable = true
            setOnClickListener { dialog.dismiss(); chapterJump(dir) }
        }
        container.addView(
            LinearLayout(activity).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                setPadding(dp(16), dp(12), dp(16), dp(6))
                if (host.chapters.isNotEmpty()) addView(chapterBtn("‹‹", -1))
                addView(seek, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
                if (host.chapters.isNotEmpty()) addView(chapterBtn("››", +1))
                addView(pageLabel, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply { marginStart = dp(12) })
            },
        )
        // Current-chapter line under the scrubber: orients the reader and makes the ‹‹/›› obvious.
        // Left = "Ch i/n · Title" (ellipsized); right = in-chapter "p/q" (stays visible). Tap → the
        // full Contents sheet. Shown only when the document has chapters.
        currentChapterLabel()?.let { lbl ->
            container.addView(LinearLayout(activity).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                setPadding(dp(24), 0, dp(24), dp(8))
                isClickable = true
                setOnClickListener { dialog.dismiss(); showContentsLazy() }
                addView(TextView(activity).apply {
                    text = lbl; setTextColor(Ink.inkSoft); textSize = Ink.sp(12f); typeface = Ink.serif
                    maxLines = 1; ellipsize = android.text.TextUtils.TruncateAt.END
                }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
                inChapterPosition()?.let { pos ->
                    addView(TextView(activity).apply {
                        text = pos; setTextColor(Ink.muted); textSize = Ink.sp(12f); typeface = Ink.mono
                        setPadding(dp(10), 0, 0, 0)
                    })
                }
            })
        }

        // Control row: flat, evenly-weighted icon+label cells.
        val controls = LinearLayout(activity).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(dp(2), dp(6), dp(2), dp(12))
        }
        // One control = a line icon over a small label (Boox/NeoReader bottom-bar style, frame 069).
        fun control(iconRes: Int, label: String, onClick: () -> Unit) {
            val cell = LinearLayout(activity).apply {
                orientation = LinearLayout.VERTICAL
                gravity = Gravity.CENTER
                setPadding(dp(2), dp(8), dp(2), dp(8))
                isClickable = true
                setOnClickListener { dialog.dismiss(); onClick() }
            }
            cell.addView(
                ImageView(activity).apply {
                    setImageResource(iconRes); setColorFilter(Ink.ink)
                },
                // Scale the icon box with the label (#133) so a raised menu size grows the whole
                // control coherently, not just its text. The cell is WRAP_CONTENT, so this can't clip.
                LinearLayout.LayoutParams(Ink.sdp(39), Ink.sdp(39)),
            )
            cell.addView(TextView(activity).apply {
                text = label; setTextColor(Ink.inkSoft); textSize = Ink.sp(11f)
                typeface = Ink.mono; letterSpacing = 0.02f
                gravity = Gravity.CENTER; setPadding(0, Ink.sdp(5), 0, 0)
            })
            controls.addView(cell, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        }
        // Reading a compiled Daily issue → a back-to-the-front-page control (DailyActivity is the
        // parent in the back stack, so finishing returns to the article list).
        if (host.requestedPath?.contains("/daily/") == true) {
            control(R.drawable.ic_menu_contents, "Daily") { activity.finish() }
        }
        // "Home" already opens the library home, so a separate Library item here is redundant.
        control(R.drawable.ic_menu_home, "Home") { goHome() }
        // (Bookmark toggle moved to the top-right corner dog-ear; "Marks" lists them.)
        control(R.drawable.ic_menu_marks, "Marks") { showBookmarks() }
        control(R.drawable.ic_tool_pen, "Notes") { showAnnotations() }
        control(R.drawable.ic_menu_contents, "Contents") { showContentsLazy() }
        control(R.drawable.ic_menu_search, "Search") { host.openSearch() }
        // Quick zoom (circle −/+ icons — not magnifiers, which are reserved for Search). Also in Adjust → Zoom.
        control(R.drawable.ic_menu_zoom_out, "Zoom −") { host.zoomOut() }
        control(R.drawable.ic_menu_zoom_in, "Zoom +") { host.zoomIn() }
        control(R.drawable.ic_menu_export, "Export") { host.openExport() }
        control(R.drawable.ic_menu_dict, "Dicts") { host.openDicts() }
        // Document controls consolidated into one KOReader-style tabbed sheet (Rotate/Fit/Font/Display).
        control(R.drawable.ic_menu_adjust, "Adjust") { host.openAdjust() }
        control(R.drawable.ic_menu_refresh, "Refresh") { host.refreshNow() } // manual full flash (#99)
        control(R.drawable.ic_menu_open, "Open") { host.openPicker() }
        container.addView(controls)

        // Palm guard: a hand resting on the bottom-anchored bar must not press a control (esp. Home).
        dialog.setContentView(host.palmGuard(container))
        dialog.window?.apply {
            setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
            setGravity(Gravity.BOTTOM)
            setBackgroundDrawable(android.graphics.drawable.ColorDrawable(Ink.paper))
        }
        dialog.show()
    }

    /** Jump to the previous (`dir<0`) / next (`dir>0`) chapter start relative to the current page
     *  (1.7). No-op with a brief toast at the document ends or when the doc has no chapters. */
    private fun chapterJump(dir: Int) {
        val starts = host.chapters.map { it.first }
        if (starts.isEmpty()) {
            Toast.makeText(activity, "No chapters in this document", Toast.LENGTH_SHORT).show()
            return
        }
        val cur = host.currentPage
        val target = if (dir > 0) starts.firstOrNull { it > cur } else starts.lastOrNull { it < cur }
        if (target != null) {
            host.postJump(target)
        } else {
            Toast.makeText(activity, if (dir > 0) "Last chapter" else "First chapter", Toast.LENGTH_SHORT).show()
        }
    }

    /** "Ch 2/9 · The Crossing" for the chapter the current page sits in, or null if the doc has no
     *  chapters. The current chapter is the last one whose start page is ≤ the current page. */
    private fun currentChapterLabel(): String? {
        val ch = host.chapters
        if (ch.isEmpty()) return null
        val idx = ch.indexOfLast { it.first <= host.currentPage }.coerceAtLeast(0)
        return "Ch ${idx + 1}/${ch.size} · ${ch[idx].second}"
    }

    /** In-chapter position "3/24" — the current page within the current chapter (this chapter's start
     *  → the next chapter's start; the last chapter runs to the end of the document). Null with no
     *  chapters, or before the first chapter start (front matter). */
    private fun inChapterPosition(): String? {
        val ch = host.chapters
        if (ch.isEmpty()) return null
        val idx = ch.indexOfLast { it.first <= host.currentPage }
        if (idx < 0) return null
        val start = ch[idx].first
        val end = if (idx + 1 < ch.size) ch[idx + 1].first else host.pageCount
        val total = (end - start).coerceAtLeast(1)
        return "${host.currentPage - start + 1}/$total"
    }

    /** A "go to page" text-entry dialog (RR11-FR1): type a 1-based page number to jump. */
    private fun showPageEntry(total: Int) {
        val input = EditText(activity).apply {
            inputType = InputType.TYPE_CLASS_NUMBER
            hint = "1 – $total"
        }
        AlertDialog.Builder(activity, R.style.InkDialog)
            .setTitle("Go to page")
            .setView(input)
            .setPositiveButton("Go") { _, _ ->
                val n = input.text.toString().toIntOrNull()
                if (n != null && n in 1..total) {
                    host.postJump(n - 1)
                } else {
                    Toast.makeText(activity, "Enter a page from 1 to $total", Toast.LENGTH_SHORT).show()
                }
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    /** Toggle a bookmark on the current page (RR16); redraw so the dog-ear appears/disappears. */
    fun toggleBookmark() {
        host.engineExecute {
            val bm = host.bookmarks ?: return@engineExecute
            val page = host.currentPage
            val now = bm.toggle(page)
            runOnUiThread {
                val msg = if (now) "Bookmarked page ${page + 1}" else "Bookmark removed"
                Toast.makeText(activity, msg, Toast.LENGTH_SHORT).show()
            }
            host.repaintPanel()
        }
    }

    /** List the bookmarked pages (RR16); tap one to jump. */
    private fun showBookmarks() {
        host.engineExecute {
            val marks = host.bookmarks?.pages() ?: emptyList()
            runOnUiThread {
                if (marks.isEmpty()) {
                    Toast.makeText(activity, "No bookmarks yet — tap Bookmark to add one", Toast.LENGTH_SHORT).show()
                    return@runOnUiThread
                }
                val labels = marks.map { "Page ${it + 1}" }.toTypedArray()
                AlertDialog.Builder(activity, R.style.InkDialog)
                    .setTitle("Bookmarks")
                    .setItems(labels) { _, which -> host.postJump(marks[which]) }
                    .show()
            }
        }
    }

    /** Fetch the TOC on the engine thread, then show it (RR11-FR2). */
    private fun showContentsLazy() {
        host.engineExecute {
            if (host.docHandle == 0L) return@engineExecute
            val toc = try {
                WireCodec.decodeToc(NativeBridge.nativeToc(host.docHandle))
            } catch (e: RuntimeException) {
                Log.e(TAG, "toc failed: ${e.message}")
                emptyList()
            }
            runOnUiThread {
                if (toc.isEmpty()) {
                    Toast.makeText(activity, "No contents in this document", Toast.LENGTH_SHORT).show()
                } else {
                    showContents(toc)
                }
            }
        }
    }

    /** Handwritten-notes annotations list (1.5): fetch inked pages + their stroke counts off-thread,
     *  then show a tap-to-jump list. */
    private fun showAnnotations() {
        host.engineExecute {
            if (host.docHandle == 0L) return@engineExecute
            val pages = try {
                NativeBridge.nativeInkPages(host.docHandle)
            } catch (e: RuntimeException) {
                Log.e(TAG, "ink pages failed: ${e.message}"); IntArray(0)
            }
            val items = pages.map { p ->
                val count = try {
                    WireCodec.decodeStrokes(NativeBridge.nativeInkStrokesForDraw(host.docHandle, p)).size
                } catch (e: RuntimeException) { 0 }
                p to count
            }
            runOnUiThread {
                if (items.isEmpty()) {
                    Toast.makeText(activity, "No handwritten notes yet", Toast.LENGTH_SHORT).show()
                } else {
                    showAnnotationsList(items)
                }
            }
        }
    }

    /** The inked pages as a scrollable "Page N · M notes" list; tap a row to jump there. */
    private fun showAnnotationsList(items: List<Pair<Int, Int>>) {
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val dialog = Dialog(activity).apply { requestWindowFeature(Window.FEATURE_NO_TITLE) }
        val outer = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(24), dp(22), dp(24), dp(14))
        }
        outer.addView(Ink.eyebrow(activity, "Annotations"))
        outer.addView(Ink.gap(activity, 10))
        outer.addView(Ink.rule(activity))
        outer.addView(Ink.gap(activity, 4))
        val list = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
        items.forEachIndexed { i, (page, count) ->
            if (i > 0) list.addView(View(activity).apply { setBackgroundColor(Ink.hairline) },
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, Ink.hair()))
            list.addView(LinearLayout(activity).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                setPadding(dp(4), dp(14), dp(4), dp(14))
                isClickable = true
                setOnClickListener { dialog.dismiss(); host.postJump(page) }
                addView(TextView(activity).apply {
                    text = "Page ${page + 1}"
                    setTextColor(Ink.ink); textSize = Ink.sp(17f); typeface = Ink.serifBold
                }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
                addView(TextView(activity).apply {
                    text = if (count == 1) "1 note" else "$count notes"
                    setTextColor(Ink.muted); textSize = Ink.sp(12f); typeface = Ink.mono
                })
            })
        }
        outer.addView(ScrollView(activity).apply { addView(list) },
            LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, (activity.resources.displayMetrics.heightPixels * 0.7f).toInt()))
        dialog.setContentView(outer)
        dialog.window?.apply {
            setLayout((activity.resources.displayMetrics.widthPixels * 0.82f).toInt(), ViewGroup.LayoutParams.WRAP_CONTENT)
            setBackgroundDrawable(Ink.cardBg())
        }
        dialog.show()
    }

    /** The document's table of contents (RR11-FR2), shown as a scrollable list from the popup. */
    private fun showContents(toc: List<TocItem>) {
        if (toc.isEmpty()) return
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val dialog = Dialog(activity).apply { requestWindowFeature(Window.FEATURE_NO_TITLE) }

        val outer = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(24), dp(22), dp(24), dp(14))
        }
        outer.addView(Ink.eyebrow(activity, "Contents"))
        outer.addView(Ink.gap(activity, 10))
        outer.addView(Ink.rule(activity))
        outer.addView(Ink.gap(activity, 4))
        val list = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
        toc.forEachIndexed { i, item ->
            if (i > 0) list.addView(View(activity).apply { setBackgroundColor(Ink.hairline) },
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, Ink.hair()))
            list.addView(LinearLayout(activity).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                setPadding(dp(4) + item.depth * dp(18), dp(14), dp(4), dp(14))
                isClickable = true
                setOnClickListener { dialog.dismiss(); item.targetPage?.let { host.postJump(it) } }
                addView(TextView(activity).apply {
                    text = item.title
                    setTextColor(if (item.targetPage != null) Ink.ink else Ink.muted)
                    textSize = Ink.sp(if (item.depth == 0) 17f else 15f)
                    typeface = if (item.depth == 0) Ink.serifBold else Ink.serif
                    maxLines = 2
                }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
                item.targetPage?.let { p ->
                    addView(TextView(activity).apply {
                        text = "${p + 1}"; setTextColor(Ink.muted); textSize = Ink.sp(12f); typeface = Ink.mono
                        setPadding(dp(12), 0, 0, 0)
                    })
                }
            })
        }
        outer.addView(ScrollView(activity).apply { addView(list) },
            LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, (activity.resources.displayMetrics.heightPixels * 0.7f).toInt()))

        dialog.setContentView(outer)
        dialog.window?.apply {
            setLayout((activity.resources.displayMetrics.widthPixels * 0.82f).toInt(), ViewGroup.LayoutParams.WRAP_CONTENT)
            setBackgroundDrawable(Ink.cardBg())
        }
        dialog.show()
    }

    /** Return to the home screen (RR16), leaving the reader. */
    private fun goHome() {
        activity.startActivity(
            Intent(activity, HomeActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP),
        )
        activity.finish()
    }

    companion object {
        private const val TAG = "BottomBar"
    }
}
