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
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView

/**
 * The InkRead home / launcher (RR16/RR17/RR26). A calm "title-page" layout: the brand (logo · name ·
 * version) up top like a book title, the **top 3 recent books** as covers with a read-percentage in
 * the middle, and an inspirational tagline pinned at the bottom. Plain framework Activity,
 * programmatic UI to match the shell.
 */
class HomeActivity : Activity() {

    private val density get() = resources.displayMetrics.density
    private fun dp(v: Int) = (v * density).toInt()
    private val phi = 1.618f
    /** Spencerian-inspired script (Pinyon Script, OFL); bold = synthetic (single-weight font). */
    private val script by lazy { Typeface.createFromAsset(assets, "fonts/pinyon_script.ttf") }
    private val scriptBold by lazy { Typeface.create(script, Typeface.BOLD) }

    /** Enlarge the capital letters (swash initials) for a Spencerian flourish. */
    private fun swashCaps(text: String, factor: Float = 1.45f): CharSequence {
        val s = android.text.SpannableString(text)
        text.forEachIndexed { i, c ->
            if (c.isUpperCase()) {
                s.setSpan(android.text.style.RelativeSizeSpan(factor), i, i + 1, android.text.Spannable.SPAN_EXCLUSIVE_EXCLUSIVE)
            }
        }
        return s
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(buildView())
    }

    override fun onResume() {
        super.onResume()
        setContentView(buildView()) // refresh covers + progress after returning from the reader
    }

    private fun buildView(): View {
        // Golden-ratio composition: side margins = W/φ⁴ band; the page splits brand · library ·
        // tagline with weighted spacers in φ proportion (1 : φ), so the library lands on the
        // upper golden line and the whole height is used (not ~50%).
        val w = resources.displayMetrics.widthPixels
        // Slim golden margin (W/φ⁵ ≈ 0.09·W) so the covers fill ~82% of the width, not ~50%.
        val side = (w / (phi * phi * phi * phi * phi)).toInt().coerceIn(dp(20), dp(48))
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setBackgroundColor(Color.WHITE)
            setPadding(side, (side / phi).toInt(), side, (side / phi).toInt())
            layoutParams = ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)
        }

        // ── Brand, presented like a book title page ──────────────────────────────
        val logo = (w / (phi * phi * phi)).toInt().coerceIn(dp(72), dp(140)) // ≈ golden of the margin
        root.addView(ImageView(this).apply { setImageResource(R.mipmap.ic_launcher) }, LinearLayout.LayoutParams(logo, logo))
        root.addView(TextView(this).apply {
            text = swashCaps("InkRead"); setTextColor(Color.BLACK); textSize = 56f
            typeface = scriptBold; paint.isFakeBoldText = true; gravity = Gravity.CENTER
            setPadding(0, dp(16), 0, 0)
        })
        root.addView(TextView(this).apply {
            text = "v${versionName()}"; setTextColor(Color.parseColor("#9E9E9E"))
            textSize = 13f; gravity = Gravity.CENTER; letterSpacing = 0.12f
            setPadding(0, dp(6), 0, 0)
        })

        // ── Upper golden spacer (weight 1) ───────────────────────────────────────
        root.addView(View(this), LinearLayout.LayoutParams(0, 0, 1f))

        // ── Library: the top 3 books with covers + read % ────────────────────────
        val recents = Books.recents(this).take(3)
        if (recents.isEmpty()) {
            root.addView(TextView(this).apply {
                text = "Open a document to start your library."
                setTextColor(Color.parseColor("#757575")); textSize = 15f; gravity = Gravity.CENTER
                setPadding(0, 0, 0, dp(18))
            })
            root.addView(pillButton("＋ Open Document") { openPicker() })
        } else {
            root.addView(TextView(this).apply {
                text = swashCaps("Library"); setTextColor(Color.BLACK); textSize = 48f
                typeface = scriptBold; gravity = Gravity.CENTER // Spencerian-inspired script heading
                paint.isFakeBoldText = true
                setPadding(0, 0, 0, dp(16))
            })
            root.addView(libraryRow(recents, w, side))
            root.addView(spacer(dp(22)))
            root.addView(pillButton("＋ Open Document") { openPicker() })
        }

        // ── Lower golden spacer (weight φ → library sits on the upper golden line) ─
        root.addView(View(this), LinearLayout.LayoutParams(0, 0, phi))
        root.addView(TextView(this).apply {
            text = swashCaps("Super Reader for a Super You")
            setTextColor(Color.parseColor("#3A3A3A")); textSize = 32f
            typeface = scriptBold; paint.isFakeBoldText = true; gravity = Gravity.CENTER
        })
        return root
    }

    /** A centered row of up to three book covers (golden-ratio aspect, filling the content width),
     *  each with a read-percentage bar beneath it. */
    private fun libraryRow(recents: List<Books.Recent>, w: Int, side: Int): View {
        val gap = (side / phi).toInt().coerceAtLeast(dp(10))
        val cellW = ((w - 2 * side - 2 * gap) / 3).coerceAtLeast(dp(80))
        val coverH = (cellW * phi).toInt() // golden cover: height = width · φ
        return LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER
            recents.forEachIndexed { i, r ->
                addView(bookTile(r, cellW, coverH), LinearLayout.LayoutParams(cellW, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
                    marginStart = if (i == 0) 0 else gap
                })
            }
        }
    }

    private fun bookTile(r: Books.Recent, cellW: Int, coverH: Int): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        gravity = Gravity.CENTER_HORIZONTAL
        isClickable = true
        setOnClickListener { open(r) }
        addView(cover(r, cellW, coverH))
        addView(TextView(this@HomeActivity).apply {
            text = r.title; setTextColor(Color.parseColor("#333333")); textSize = 12f
            gravity = Gravity.CENTER; maxLines = 2; setPadding(0, dp(8), 0, dp(6))
            layoutParams = LinearLayout.LayoutParams(cellW, ViewGroup.LayoutParams.WRAP_CONTENT)
        })
        addView(progressBar(Books.progress(this@HomeActivity, r.id), cellW))
        addView(TextView(this@HomeActivity).apply {
            text = "${Books.progress(this@HomeActivity, r.id)}% read"
            setTextColor(Color.parseColor("#757575")); textSize = 11f
            gravity = Gravity.CENTER; setPadding(0, dp(4), 0, 0)
        })
    }

    /** A thin read-progress bar (filled portion black, remainder light grey). */
    private fun progressBar(percent: Int, width: Int): View {
        val p = percent.coerceIn(0, 100)
        return LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            background = GradientDrawable().apply { setColor(Color.parseColor("#E0E0E0")); cornerRadius = dp(2).toFloat() }
            layoutParams = LinearLayout.LayoutParams(width, dp(4))
            if (p > 0) addView(View(this@HomeActivity).apply {
                background = GradientDrawable().apply { setColor(Color.BLACK); cornerRadius = dp(2).toFloat() }
            }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, p.toFloat()))
            if (p < 100) addView(View(this@HomeActivity), LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, (100 - p).toFloat()))
        }
    }

    /** A cover image: the cached first-page thumbnail, or a bordered placeholder. */
    private fun cover(r: Books.Recent, w: Int, h: Int): View {
        val bmp = Books.loadThumbnail(this, r.id)
        return if (bmp != null) {
            ImageView(this).apply {
                scaleType = ImageView.ScaleType.CENTER_CROP
                setImageBitmap(bmp)
                background = GradientDrawable().apply { setStroke(maxOf(1, dp(1)), Color.parseColor("#D0D0D0")) }
                layoutParams = LinearLayout.LayoutParams(w, h)
            }
        } else {
            TextView(this).apply {
                text = r.title; setTextColor(Color.parseColor("#555555")); textSize = 12f
                gravity = Gravity.CENTER; setPadding(dp(8), dp(8), dp(8), dp(8))
                background = GradientDrawable().apply {
                    setColor(Color.parseColor("#EFEFEF")); setStroke(maxOf(1, dp(1)), Color.parseColor("#D0D0D0"))
                }
                layoutParams = LinearLayout.LayoutParams(w, h)
            }
        }
    }

    private fun pillButton(label: String, onClick: () -> Unit): View = TextView(this).apply {
        text = label; setTextColor(Color.BLACK); textSize = 15f; gravity = Gravity.CENTER
        setPadding(dp(24), dp(10), dp(24), dp(10))
        background = GradientDrawable().apply {
            setColor(Color.WHITE); setStroke(maxOf(1, dp(1)), Color.BLACK); cornerRadius = dp(22).toFloat()
        }
        isClickable = true; setOnClickListener { onClick() }
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT)
    }

    private fun spacer(h: Int): View = View(this).apply {
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, h)
    }

    private fun openPicker() = openReader(Intent().putExtra(ReaderActivity.EXTRA_PICK, true))

    private fun open(r: Books.Recent) = openReader(
        Intent().putExtra(ReaderActivity.EXTRA_BOOK_PATH, r.path).putExtra(ReaderActivity.EXTRA_BOOK_ID, r.id),
    )

    private fun openReader(extras: Intent) =
        startActivity(Intent(this, ReaderActivity::class.java).putExtras(extras))

    private fun versionName(): String =
        try { packageManager.getPackageInfo(packageName, 0).versionName ?: "" } catch (e: Exception) { "" }
}
