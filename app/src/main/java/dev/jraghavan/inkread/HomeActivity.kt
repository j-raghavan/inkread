package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.graphics.Color
import android.graphics.Typeface
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast

/**
 * The InkRead home screen and launcher (RR16/RR17/RR26): brand (icon · name · version · tagline),
 * a **Recently opened** shelf with first-page thumbnails, and entry points (Open a PDF, Library).
 * A plain framework Activity to match the shell's no-AppCompat, programmatic-UI style.
 */
class HomeActivity : Activity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(buildView())
    }

    override fun onResume() {
        super.onResume()
        // Rebuild so the recents shelf + thumbnails refresh after returning from the reader.
        setContentView(buildView())
    }

    private fun buildView(): View {
        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()

        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setBackgroundColor(Color.WHITE)
            setPadding(dp(32), dp(40), dp(32), dp(40))
        }

        // Brand.
        root.addView(
            ImageView(this).apply { setImageResource(R.mipmap.ic_launcher) },
            LinearLayout.LayoutParams(dp(96), dp(96)),
        )
        root.addView(
            TextView(this).apply {
                text = "InkRead"
                setTextColor(Color.BLACK)
                textSize = 36f
                typeface = Typeface.DEFAULT_BOLD
                gravity = Gravity.CENTER
                setPadding(0, dp(12), 0, 0)
            },
        )
        root.addView(
            TextView(this).apply {
                text = "v${versionName()}"
                setTextColor(Color.DKGRAY)
                textSize = 13f
                gravity = Gravity.CENTER
            },
        )
        root.addView(
            TextView(this).apply {
                text = "A better reader."
                setTextColor(Color.DKGRAY)
                textSize = 17f
                setTypeface(typeface, Typeface.ITALIC)
                gravity = Gravity.CENTER
                setPadding(0, dp(6), 0, dp(28))
            },
        )

        // Recently opened shelf.
        val recents = Books.recents(this)
        if (recents.isNotEmpty()) {
            root.addView(
                TextView(this).apply {
                    text = "Recently opened"
                    setTextColor(Color.DKGRAY)
                    textSize = 13f
                    typeface = Typeface.DEFAULT_BOLD
                    setPadding(0, 0, 0, dp(6))
                },
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT),
            )
            for (r in recents.take(5)) root.addView(recentCard(r))
            root.addView(View(this), LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(20)))
        }

        fun button(label: String, onClick: () -> Unit) {
            root.addView(
                Button(this).apply {
                    text = label
                    isAllCaps = false
                    textSize = 18f
                    setOnClickListener { onClick() }
                },
                LinearLayout.LayoutParams(dp(280), dp(60)).apply { topMargin = dp(10) },
            )
        }
        button("📂   Open a PDF") { openReader(Intent().putExtra(ReaderActivity.EXTRA_PICK, true)) }
        button("📚   Library") { showLibrary() }

        return ScrollView(this).apply {
            setBackgroundColor(Color.WHITE)
            addView(root)
        }
    }

    /** A recent-book row: first-page thumbnail + title, tappable to open. */
    private fun recentCard(r: Books.Recent): View {
        val d = resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(6), dp(8), dp(6), dp(8))
            isClickable = true
            setOnClickListener {
                openReader(
                    Intent()
                        .putExtra(ReaderActivity.EXTRA_BOOK_PATH, r.path)
                        .putExtra(ReaderActivity.EXTRA_BOOK_ID, r.id),
                )
            }
        }
        row.addView(
            ImageView(this).apply {
                scaleType = ImageView.ScaleType.CENTER_CROP
                val bmp = Books.loadThumbnail(this@HomeActivity, r.id)
                if (bmp != null) setImageBitmap(bmp) else setBackgroundColor(Color.parseColor("#EEEEEE"))
            },
            LinearLayout.LayoutParams(dp(84), dp(112)).apply { marginEnd = dp(14) },
        )
        row.addView(
            TextView(this).apply {
                text = r.title
                setTextColor(Color.BLACK)
                textSize = 17f
                maxLines = 2
            },
        )
        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(row, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
        }
    }

    private fun openReader(extras: Intent) {
        startActivity(Intent(this, ReaderActivity::class.java).putExtras(extras))
    }

    private fun showLibrary() {
        val books = Books.list(this)
        if (books.isEmpty()) {
            Toast.makeText(this, "No books yet — open a PDF first", Toast.LENGTH_SHORT).show()
            return
        }
        val labels = books.map { Books.title(it) }.toTypedArray()
        AlertDialog.Builder(this)
            .setTitle("Library")
            .setItems(labels) { _, which ->
                val f = books[which]
                openReader(
                    Intent()
                        .putExtra(ReaderActivity.EXTRA_BOOK_PATH, f.absolutePath)
                        .putExtra(ReaderActivity.EXTRA_BOOK_ID, f.name),
                )
            }
            .show()
    }

    private fun versionName(): String =
        try {
            packageManager.getPackageInfo(packageName, 0).versionName ?: ""
        } catch (e: Exception) {
            ""
        }
}
