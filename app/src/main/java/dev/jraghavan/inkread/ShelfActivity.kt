package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import java.io.File

/**
 * **Your shelf** — every book on the device, and the only place to take one off again.
 *
 * The home screen shows the most recent handful, which is the right thing for *reading*; it is the
 * wrong thing for *managing*, because a book that falls past the recents cut becomes invisible while
 * still occupying storage. This screen is the complete list, so the device can be curated rather
 * than only added to.
 *
 * Removing keeps handwritten annotations by default (see [Books.remove]) — the book is one tap from
 * a catalog or an import, the ink on it is not. Typographic and flat, like [HomeActivity]; no covers
 * are decoded, since a list on a panel that repaints in whole frames wants text.
 */
class ShelfActivity : Activity() {

    private val density get() = resources.displayMetrics.density
    private fun dp(v: Int) = (v * density).toInt()

    private val serif = Ink.serif
    private val serifItalic = Ink.serifItalic
    private val mono = Ink.mono
    private val ink = Ink.ink

    private lateinit var column: LinearLayout

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Ink.uiScale = DisplayPrefs(this).uiScale
        column = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(22), dp(20), dp(22), dp(28))
        }
        setContentView(ScrollView(this).apply { isFillViewport = true; addView(column) })
        render()
    }

    private fun render() {
        Books.sweepPartialDownloads(this)
        val books = Books.list(this)
        column.removeAllViews()
        column.addView(header(books))

        if (books.isEmpty()) {
            column.addView(TextView(this).apply {
                text = "No books on the device yet."
                setTextColor(Ink.inkSoft); textSize = Ink.sp(15f); typeface = serif
                setPadding(0, dp(18), 0, 0)
            })
            return
        }
        for (book in books) column.addView(row(book))
    }

    private fun header(books: List<File>): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        addView(Ink.eyebrow(this@ShelfActivity, "Your shelf"))
        addView(TextView(this@ShelfActivity).apply {
            text = if (books.size == 1) "1 book" else "${books.size} books"
            setTextColor(ink); textSize = Ink.sp(28f); typeface = serif
            setPadding(0, dp(4), 0, dp(2))
        })
        if (books.isNotEmpty()) {
            val total = books.sumOf { Books.sizeOnDisk(it) }
            addView(TextView(this@ShelfActivity).apply {
                text = "${Books.humanSize(total)} on this device"
                setTextColor(Ink.muted); textSize = Ink.sp(11f); typeface = mono; letterSpacing = 0.08f
                setPadding(0, 0, 0, dp(4))
            })
        }
        addView(Ink.rule(this@ShelfActivity))
        addView(spacer(dp(6)))
    }

    /** One book: tap the title to read it, tap Remove to take it off the device. */
    private fun row(book: File): View = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        setPadding(0, dp(12), 0, dp(12))

        addView(LinearLayout(this@ShelfActivity).apply {
            orientation = LinearLayout.VERTICAL
            isClickable = true
            setOnClickListener { open(book) }
            addView(TextView(this@ShelfActivity).apply {
                text = Books.metaTitle(this@ShelfActivity, book.name) ?: Books.title(book)
                setTextColor(ink); textSize = Ink.sp(18f); typeface = serif
                maxLines = 2; setLineSpacing(0f, 1.1f)
            })
            Books.metaAuthor(this@ShelfActivity, book.name)?.let { author ->
                addView(TextView(this@ShelfActivity).apply {
                    text = author
                    setTextColor(Ink.inkSoft); textSize = Ink.sp(14f); typeface = serifItalic
                    setPadding(0, dp(3), 0, 0); maxLines = 1
                })
            }
            addView(TextView(this@ShelfActivity).apply {
                text = subtitleFor(book)
                setTextColor(Ink.muted); textSize = Ink.sp(11f); typeface = mono; letterSpacing = 0.08f
                setPadding(0, dp(5), 0, 0)
            })
        }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))

        addView(Ink.pillButton(this@ShelfActivity, "Remove", primary = false) { confirmRemove(book) })
    }

    /** Format · size · how far in you are — enough to decide whether it can go. */
    private fun subtitleFor(book: File): String {
        val bits = mutableListOf(book.extension.uppercase(), Books.humanSize(Books.sizeOnDisk(book)))
        val percent = Books.progress(this, book.name)
        if (percent > 0) bits += "$percent% read"
        if (Books.hasNotes(book)) bits += "HAS NOTES"
        return bits.joinToString(" · ")
    }

    /**
     * Confirm before deleting, and make the annotations an explicit, separate decision — a reader who
     * is clearing space must not lose handwriting they did not think they were discarding.
     */
    private fun confirmRemove(book: File) {
        val title = Books.metaTitle(this, book.name) ?: Books.title(book)
        val hasNotes = Books.hasNotes(book)
        val message = if (hasNotes) {
            "Remove “$title” from this device? Your handwritten notes stay, and come back if you " +
                "add the book again."
        } else {
            "Remove “$title” from this device? It frees ${Books.humanSize(Books.sizeOnDisk(book))}."
        }
        val dialog = AlertDialog.Builder(this, R.style.InkDialog)
            .setTitle("Remove from device")
            .setMessage(message)
            .setPositiveButton("Remove") { _, _ -> doRemove(book, alsoNotes = false) }
            .setNegativeButton("Cancel", null)
        if (hasNotes) dialog.setNeutralButton("Remove with notes") { _, _ -> confirmNotesToo(book, title) }
        dialog.show()
    }

    /** Discarding handwriting is irreversible, so it gets its own confirmation. */
    private fun confirmNotesToo(book: File, title: String) {
        AlertDialog.Builder(this, R.style.InkDialog)
            .setTitle("Delete the notes too?")
            .setMessage("Your handwriting on “$title” will be deleted. This can't be undone.")
            .setPositiveButton("Delete notes") { _, _ -> doRemove(book, alsoNotes = true) }
            .setNegativeButton("Keep notes") { _, _ -> doRemove(book, alsoNotes = false) }
            .show()
    }

    private fun doRemove(book: File, alsoNotes: Boolean) {
        val freed = Books.sizeOnDisk(book)
        if (Books.remove(this, book, alsoNotes)) {
            Toast.makeText(this, "Removed · ${Books.humanSize(freed)} free", Toast.LENGTH_SHORT).show()
        } else {
            Toast.makeText(this, "Could not remove that book", Toast.LENGTH_SHORT).show()
        }
        render()
    }

    private fun open(book: File) {
        startActivity(
            Intent(this, ReaderActivity::class.java)
                .putExtra(ReaderActivity.EXTRA_BOOK_PATH, book.absolutePath)
                .putExtra(ReaderActivity.EXTRA_BOOK_ID, book.name),
        )
    }

    private fun spacer(h: Int): View = View(this).apply {
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, h)
    }

}
