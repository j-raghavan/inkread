package dev.jraghavan.inkread

import android.app.Activity
import android.app.Dialog
import android.graphics.Bitmap
import android.graphics.Color
import android.graphics.drawable.ColorDrawable
import android.graphics.drawable.GradientDrawable
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.Window
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast

/**
 * Nine-page visual navigation for the reader. Native rendering stays on the serialized engine
 * thread; the UI receives a complete batch and replaces the grid once, avoiding an e-ink flicker
 * loop while thumbnails arrive.
 */
class ThumbnailGridController(private val host: Host) {

    interface Host {
        val activity: Activity
        val docHandle: Long
        val pageCount: Int
        val currentPage: Int

        fun engineExecute(block: () -> Unit)
        fun renderPageThumbnails(
            pages: List<Int>,
            maxWidth: Int,
            maxHeight: Int,
            isCancelled: () -> Boolean,
        ): List<PageThumbnail>
        fun postJump(page: Int)
        fun refreshPanel()
        fun palmGuardFullScreen(content: View): View
    }

    data class PageThumbnail(val page: Int, val bitmap: Bitmap)

    private val activity get() = host.activity
    private var dialog: Dialog? = null
    private var gridHost: LinearLayout? = null
    private var rangeLabel: TextView? = null
    private var prevButton: TextView? = null
    private var nextButton: TextView? = null
    private var windowStart = 0
    @Volatile private var requestGeneration = 0
    private var bitmaps: List<Bitmap> = emptyList()

    fun show() {
        if (host.docHandle == 0L || host.pageCount <= 0) return
        windowStart = ThumbnailGridModel.windowFor(host.currentPage, host.pageCount).start
        load(windowStart, opening = true)
    }

    fun dismiss() {
        requestGeneration++
        dialog?.dismiss()
        dialog = null
        gridHost = null
        rangeLabel = null
        prevButton = null
        nextButton = null
        recycleBitmaps()
    }

    private fun load(start: Int, opening: Boolean) {
        val total = host.pageCount
        val window = ThumbnailGridModel.windowAt(start, total)
        val metrics = activity.resources.displayMetrics
        val maxWidth = (metrics.widthPixels - dp(40)) / ThumbnailGridModel.COLUMNS
        val maxHeight = (metrics.heightPixels - dp(160)) / ThumbnailGridModel.ROWS
        val generation = ++requestGeneration

        setNavigationEnabled(false)
        host.engineExecute {
            val rendered =
                host.renderPageThumbnails(window.pages, maxWidth, maxHeight) {
                    generation != requestGeneration
                }
            activity.runOnUiThread {
                if (generation != requestGeneration || activity.isFinishing) {
                    rendered.forEach { it.bitmap.recycle() }
                    return@runOnUiThread
                }
                if (rendered.isEmpty()) {
                    setNavigationEnabled(true)
                    Toast.makeText(activity, "Couldn't render page previews", Toast.LENGTH_SHORT).show()
                    return@runOnUiThread
                }
                windowStart = window.start
                if (opening || dialog == null) showDialog(window, rendered) else installBatch(window, rendered)
                setNavigationEnabled(true)
                host.refreshPanel()
            }
        }
    }

    private fun showDialog(window: ThumbnailGridModel.Window, rendered: List<PageThumbnail>) {
        val sheet = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Ink.paper)
            setPadding(dp(14), dp(12), dp(14), dp(12))
        }

        val title = Ink.title(activity, "Pages", 20f)
        val close = iconButton("×", "Close page thumbnails") { dialog?.dismiss() }
        sheet.addView(
            LinearLayout(activity).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                addView(title, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
                addView(close, LinearLayout.LayoutParams(dp(52), dp(52)))
            },
        )
        sheet.addView(Ink.rule(activity))

        gridHost = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
        sheet.addView(gridHost, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))

        prevButton = iconButton("‹", "Previous nine pages") {
            val previous = ThumbnailGridModel.shift(windowStart, -1, host.pageCount)
            if (previous != windowStart) load(previous, opening = false)
        }
        rangeLabel = TextView(activity).apply {
            setTextColor(Ink.inkSoft)
            textSize = 12f
            typeface = Ink.mono
            gravity = Gravity.CENTER
        }
        nextButton = iconButton("›", "Next nine pages") {
            val next = ThumbnailGridModel.shift(windowStart, 1, host.pageCount)
            if (next != windowStart) load(next, opening = false)
        }
        sheet.addView(Ink.rule(activity))
        sheet.addView(
            LinearLayout(activity).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                setPadding(0, dp(8), 0, 0)
                addView(prevButton, LinearLayout.LayoutParams(dp(64), dp(52)))
                addView(rangeLabel, LinearLayout.LayoutParams(0, dp(52), 1f))
                addView(nextButton, LinearLayout.LayoutParams(dp(64), dp(52)))
            },
        )

        val created = Dialog(activity).apply {
            requestWindowFeature(Window.FEATURE_NO_TITLE)
            setContentView(host.palmGuardFullScreen(sheet))
            setOnDismissListener {
                if (dialog === this) {
                    requestGeneration++
                    dialog = null
                    gridHost = null
                    rangeLabel = null
                    prevButton = null
                    nextButton = null
                    recycleBitmaps()
                }
            }
            show()
            this.window?.apply {
                setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)
                setGravity(Gravity.CENTER)
                setBackgroundDrawable(ColorDrawable(Ink.paper))
            }
        }
        dialog = created
        installBatch(window, rendered)
    }

    private fun installBatch(window: ThumbnailGridModel.Window, rendered: List<PageThumbnail>) {
        val byPage = rendered.associateBy { it.page }
        val current = host.currentPage
        val oldBitmaps = bitmaps
        bitmaps = rendered.map { it.bitmap }

        gridHost?.apply {
            removeAllViews()
            repeat(ThumbnailGridModel.ROWS) { rowIndex ->
                val row = LinearLayout(activity).apply { orientation = LinearLayout.HORIZONTAL }
                addView(
                    row,
                    LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f),
                )
                repeat(ThumbnailGridModel.COLUMNS) { columnIndex ->
                    val index = rowIndex * ThumbnailGridModel.COLUMNS + columnIndex
                    val page = window.pages.getOrNull(index)
                    val cell = if (page == null) {
                        View(activity)
                    } else {
                        thumbnailCell(page, byPage[page]?.bitmap, page == current)
                    }
                    row.addView(
                        cell,
                        LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, 1f).apply {
                            val gap = dp(4)
                            setMargins(gap, gap, gap, gap)
                        },
                    )
                }
            }
        }
        rangeLabel?.text = "${window.start + 1}–${window.endExclusive} / ${window.total}"
        prevButton?.isEnabled = window.start > 0
        nextButton?.isEnabled = window.endExclusive < window.total
        oldBitmaps.forEach { if (!it.isRecycled) it.recycle() }
    }

    private fun thumbnailCell(page: Int, bitmap: Bitmap?, selected: Boolean): View =
        LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER
            setPadding(dp(4), dp(4), dp(4), dp(4))
            background = GradientDrawable().apply {
                setColor(Ink.paper)
                cornerRadius = Ink.dpf(4)
                setStroke(if (selected) dp(3) else Ink.hair(), if (selected) Ink.ink else Ink.hairline)
            }
            isClickable = true
            isFocusable = true
            contentDescription = if (selected) "Page ${page + 1}, current page" else "Page ${page + 1}"
            setOnClickListener {
                dialog?.dismiss()
                host.postJump(page)
            }
            addView(
                ImageView(activity).apply {
                    setImageBitmap(bitmap)
                    scaleType = ImageView.ScaleType.FIT_CENTER
                    setBackgroundColor(Color.WHITE)
                    importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
                },
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f),
            )
            addView(
                TextView(activity).apply {
                    text = "Page ${page + 1}"
                    setTextColor(if (selected) Ink.ink else Ink.inkSoft)
                    textSize = 11f
                    typeface = Ink.mono
                    gravity = Gravity.CENTER
                    setPadding(0, dp(3), 0, 0)
                    importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
                },
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(24)),
            )
        }

    private fun iconButton(glyph: String, description: String, onClick: () -> Unit): TextView =
        TextView(activity).apply {
            text = glyph
            contentDescription = description
            setTextColor(Ink.ink)
            textSize = 30f
            typeface = Ink.serif
            gravity = Gravity.CENTER
            isClickable = true
            isFocusable = true
            setOnClickListener { onClick() }
        }

    private fun setNavigationEnabled(enabled: Boolean) {
        prevButton?.isEnabled = enabled && windowStart > 0
        nextButton?.isEnabled =
            enabled && ThumbnailGridModel.windowAt(windowStart, host.pageCount).endExclusive < host.pageCount
    }

    private fun recycleBitmaps() {
        bitmaps.forEach { if (!it.isRecycled) it.recycle() }
        bitmaps = emptyList()
    }

    private fun dp(value: Int) = Ink.dp(value)
}

/** Pure grid/window and thumbnail-sizing rules, host-tested without Android rendering. */
internal object ThumbnailGridModel {
    const val COLUMNS = 3
    const val ROWS = 3
    const val PAGE_SIZE = COLUMNS * ROWS

    data class Window(
        val start: Int,
        val endExclusive: Int,
        val total: Int,
        val pages: List<Int>,
    )

    data class Size(val width: Int, val height: Int)

    fun windowFor(currentPage: Int, total: Int): Window {
        if (total <= 0) return Window(0, 0, 0, emptyList())
        val current = currentPage.coerceIn(0, total - 1)
        return windowAt((current / PAGE_SIZE) * PAGE_SIZE, total)
    }

    fun windowAt(start: Int, total: Int): Window {
        if (total <= 0) return Window(0, 0, 0, emptyList())
        val lastStart = ((total - 1) / PAGE_SIZE) * PAGE_SIZE
        val clampedStart = start.coerceIn(0, lastStart)
        val end = (clampedStart + PAGE_SIZE).coerceAtMost(total)
        return Window(clampedStart, end, total, (clampedStart until end).toList())
    }

    fun shift(start: Int, direction: Int, total: Int): Int {
        if (total <= 0 || direction == 0) return 0
        val delta = if (direction < 0) -PAGE_SIZE else PAGE_SIZE
        return windowAt(start + delta, total).start
    }

    fun fitSize(sourceWidth: Int, sourceHeight: Int, maxWidth: Int, maxHeight: Int): Size {
        if (sourceWidth <= 0 || sourceHeight <= 0 || maxWidth <= 0 || maxHeight <= 0) return Size(0, 0)
        val scale = minOf(maxWidth.toDouble() / sourceWidth, maxHeight.toDouble() / sourceHeight)
        return Size(
            width = (sourceWidth * scale).toInt().coerceAtLeast(1),
            height = (sourceHeight * scale).toInt().coerceAtLeast(1),
        )
    }
}
