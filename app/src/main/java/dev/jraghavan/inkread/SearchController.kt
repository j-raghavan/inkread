package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.app.Dialog
import android.graphics.Color
import android.text.InputType
import android.util.Log
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.Window
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast

/**
 * In-document search (RR2), extracted from `ReaderActivity` (SRP). Owns the query/hit state, the
 * whole-document scan on the engine thread, the search + results dialogs, and hit-to-hit
 * navigation. The reader shell keeps only the view geometry: it asks [highlightForPage] for the
 * active hit's boxes and draws them itself.
 *
 * The state is read on both the UI and engine threads (the scan runs on the engine; the render
 * path reads the active boxes), so the cross-thread fields stay `@Volatile`, as in the original.
 */
class SearchController(private val host: Host) {

    /** What search needs from the reader shell — a narrow seam, no back-reference to the Activity's
     *  internals beyond these. */
    interface Host {
        /** Context for dialogs/toasts and `runOnUiThread`. */
        val activity: Activity

        /** The open document handle (`0` = none); read live — it changes on open/close. */
        val docHandle: Long

        /** The open document's page count; read live. */
        val pageCount: Int

        /** Run [block] on the single engine thread (serializes native access). */
        fun engineExecute(block: () -> Unit)

        /** Jump the reader to [page] (the policy-driven jump + repaint). */
        fun jumpToPage(page: Int)

        /** Re-render the current page + refresh the panel (engine thread). */
        fun repaintPanel()

        /** Prompt to open a document (search is a no-op with none open). */
        fun openPicker()
    }

    @Volatile private var hits: List<SearchHit> = emptyList()
    @Volatile private var index = -1
    @Volatile private var query = ""
    @Volatile private var boxes: List<SelBox> = emptyList()
    @Volatile private var boxesPage = -1

    /** The active hit's highlight boxes if it lives on [page], else empty. Read on the engine thread
     *  by the render path; the shell draws these over the page. */
    fun highlightForPage(page: Int): List<SelBox> = if (page == boxesPage) boxes else emptyList()

    /**
     * Prompt for a query, then scan the whole document on the engine thread (case-insensitive,
     * PDF + EPUB). On hits, jump to the first and offer the results list; on none, a toast. The
     * "Results" button reopens the last query's results without rescanning.
     */
    fun showSearchDialog() {
        val activity = host.activity
        if (host.docHandle == 0L) { host.openPicker(); return }
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val input = EditText(activity).apply {
            inputType = InputType.TYPE_CLASS_TEXT
            imeOptions = android.view.inputmethod.EditorInfo.IME_ACTION_SEARCH
            setSingleLine(true)
            hint = "Find in document"
            setText(query)
            setSelection(text?.length ?: 0)
        }
        val pad = dp(20)
        val wrap = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, dp(8), pad, 0)
            addView(input, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
        }
        val builder = AlertDialog.Builder(activity)
            .setTitle("Search")
            .setView(wrap)
            .setPositiveButton("Search") { _, _ -> runSearch(input.text.toString()) }
            .setNegativeButton("Cancel", null)
        if (hits.isNotEmpty()) builder.setNeutralButton("Results") { _, _ -> showResults() }
        val dialog = builder.create()
        input.setOnEditorActionListener { _, actionId, _ ->
            if (actionId == android.view.inputmethod.EditorInfo.IME_ACTION_SEARCH) {
                dialog.dismiss(); runSearch(input.text.toString()); true
            } else false
        }
        dialog.show()
    }

    /** Scan every page for [raw] on the engine thread, collecting hits (capped). Bails if the open
     *  document changes mid-scan. Jumps to the first hit and reports the count on the UI thread. */
    private fun runSearch(raw: String) {
        val q = raw.trim()
        if (q.isEmpty()) { clear(); return }
        val handle = host.docHandle
        if (handle == 0L) return
        Toast.makeText(host.activity, "Searching…", Toast.LENGTH_SHORT).show()
        host.engineExecute {
            if (host.docHandle != handle) return@engineExecute
            val total = host.pageCount.coerceAtLeast(1)
            val found = ArrayList<SearchHit>()
            var page = 0
            while (page < total && found.size < MAX_SEARCH_HITS) {
                if (host.docHandle != handle) return@engineExecute
                val matches = try {
                    WireCodec.decodeSearch(NativeBridge.nativeSearchPage(handle, page, q))
                } catch (e: RuntimeException) {
                    Log.e(TAG, "search p$page failed: ${e.message}"); emptyList()
                }
                for (m in matches) {
                    found.add(SearchHit(page, m))
                    if (found.size >= MAX_SEARCH_HITS) break
                }
                page++
            }
            query = q
            hits = found
            index = -1
            host.activity.runOnUiThread {
                if (found.isEmpty()) {
                    Toast.makeText(host.activity, "No matches for \"$q\"", Toast.LENGTH_SHORT).show()
                } else {
                    val capped = if (found.size >= MAX_SEARCH_HITS) " (first $MAX_SEARCH_HITS)" else ""
                    val plural = if (found.size == 1) "" else "es"
                    Toast.makeText(host.activity, "${found.size} match$plural$capped", Toast.LENGTH_SHORT).show()
                    gotoHit(0)
                }
            }
        }
    }

    /** Park on hit [i]: set its highlight boxes and jump to its page (the highlight draws once the
     *  reader is on that page). */
    private fun gotoHit(i: Int) {
        val h = hits
        if (i < 0 || i >= h.size) return
        val hit = h[i]
        index = i
        boxes = hit.match.boxes
        boxesPage = hit.page
        host.jumpToPage(hit.page)
    }

    /** Step to the next/previous hit (wrapping). With no active search, reopens the search dialog. */
    fun step(delta: Int) {
        val h = hits
        if (h.isEmpty()) { showSearchDialog(); return }
        val n = h.size
        val next = (((index + delta) % n) + n) % n
        gotoHit(next)
        Toast.makeText(host.activity, "${next + 1} / $n", Toast.LENGTH_SHORT).show()
    }

    /** A bottom sheet listing the current query's hits (page + snippet); tap a row to jump. */
    private fun showResults() {
        val activity = host.activity
        val h = hits
        if (h.isEmpty()) { showSearchDialog(); return }
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val dialog = Dialog(activity).apply { requestWindowFeature(Window.FEATURE_NO_TITLE) }
        val list = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
        h.forEachIndexed { i, hit ->
            list.addView(LinearLayout(activity).apply {
                orientation = LinearLayout.VERTICAL
                setPadding(dp(18), dp(12), dp(18), dp(12))
                isClickable = true
                setOnClickListener { dialog.dismiss(); gotoHit(i) }
                addView(TextView(activity).apply {
                    text = "p. ${hit.page + 1}"; setTextColor(Color.parseColor("#666666")); textSize = 11f
                })
                addView(TextView(activity).apply {
                    text = hit.match.snippet; setTextColor(Color.BLACK); textSize = 14f; setPadding(0, dp(2), 0, 0)
                })
            })
            list.addView(View(activity).apply { setBackgroundColor(Color.parseColor("#22000000")) },
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 1))
        }
        // Header: a count label flanked by Prev/Next steppers (step renders behind the sheet).
        fun stepper(label: String, delta: Int) = TextView(activity).apply {
            text = label; setTextColor(Color.BLACK); textSize = 18f; gravity = Gravity.CENTER
            setPadding(dp(16), dp(8), dp(16), dp(8)); isClickable = true
            setOnClickListener { step(delta) }
        }
        val header = LinearLayout(activity).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            addView(stepper("◀", -1))
            addView(TextView(activity).apply {
                text = "${h.size} results for \"$query\""
                setTextColor(Color.BLACK); textSize = 13f; gravity = Gravity.CENTER
                setPadding(dp(8), dp(12), dp(8), dp(8))
            }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            addView(stepper("▶", +1))
        }
        val container = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Color.WHITE)
            addView(header)
            addView(ScrollView(activity).apply { addView(list) },
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
        }
        dialog.setContentView(container)
        dialog.window?.apply {
            setLayout(ViewGroup.LayoutParams.MATCH_PARENT, (activity.resources.displayMetrics.heightPixels * 0.7f).toInt())
            setGravity(Gravity.BOTTOM)
            setBackgroundDrawable(android.graphics.drawable.ColorDrawable(Color.WHITE))
        }
        dialog.show()
    }

    /** Clear the active search and wipe its on-page highlight. */
    fun clear() {
        hits = emptyList(); index = -1; query = ""; boxes = emptyList()
        val hadBoxes = boxesPage >= 0
        boxesPage = -1
        if (hadBoxes) host.engineExecute { host.repaintPanel() }
    }

    private companion object {
        const val TAG = "SearchController"

        /** Cap a query's collected hits to bound memory + scan time (RR2/RR19). */
        const val MAX_SEARCH_HITS = 1000
    }
}
