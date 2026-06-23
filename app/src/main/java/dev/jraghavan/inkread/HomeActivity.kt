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
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView

/**
 * The InkRead home / launcher — the "Inkwell" home screen (RR16/RR17/RR26). A calm, 1-bit
 * stationery layout: the brand (inkwell mark · script name · version) like a book title page, a
 * **Continue where you left off** hero for the most-recent book, a **Recently on your shelf** row of
 * staggered covers with read-percentages, an *Open a Document* action, and a closing flourish.
 *
 * Plain framework Activity with programmatic Views, to match the shell. The design (claude.ai/design
 * "Inkwell") leans on three faces: Pinyon Script (bundled) for the brand, a serif for body, and a
 * mono for the small uppercase eyebrows. Only the script is bundled (offline e-ink), so body/eyebrow
 * fall back to the platform serif/monospace. Every figure shown is real device data — the design's
 * decorative streak/stats/author/page-count chrome is omitted rather than faked.
 */
class HomeActivity : Activity() {

    private val density get() = resources.displayMetrics.density
    private fun dp(v: Int) = (v * density).toInt()
    private val phi = 1.618f

    /** Spencerian-inspired script (Pinyon Script, OFL); bold = synthetic (single-weight font). */
    private val script by lazy { Typeface.createFromAsset(assets, "fonts/pinyon_script.ttf") }
    private val scriptBold by lazy { Typeface.create(script, Typeface.BOLD) }
    /** Body serif (Crimson Pro in the design → platform serif offline). */
    private val serif = Typeface.SERIF
    /** Eyebrow / label face (Space Mono in the design → platform monospace offline). */
    private val mono = Typeface.MONOSPACE

    private val ink = Color.BLACK
    private val inkSoft = Color.parseColor("#3A3A3A")
    private val textSecondary = Color.parseColor("#333333")
    private val textMuted = Color.parseColor("#757575")

    /**
     * Width-driven scale so the one screen reads well on both the 10" tablet (primary) and the 7"
     * reader (the design's two frames are the same layout at two sizes). Set per build; design values
     * below are expressed in the artboard's ~716-unit content column.
     */
    private var scale = 1f
    private fun dim(designUnits: Number) = dp((designUnits.toFloat() * scale).toInt()).coerceAtLeast(1)
    private fun fs(designUnits: Number) = designUnits.toFloat() * scale

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

    /** Signature of the shelf state the current view was built from — lets onResume skip a full
     *  rebuild (and the e-ink full refresh it triggers) when nothing the home screen shows changed,
     *  e.g. returning from Settings rather than the reader. */
    private var shelfSig: String? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(buildView())
        shelfSig = shelfSignature()
    }

    override fun onResume() {
        super.onResume()
        val sig = shelfSignature()
        if (sig != shelfSig) {
            setContentView(buildView()) // refresh covers + progress after returning from the reader
            shelfSig = sig
        }
    }

    /** A stable digest of what the shelf renders — recents order + per-book read progress. */
    private fun shelfSignature(): String =
        Books.recents(this).joinToString("|") { "${it.id}@${Books.progress(this, it.id)}" }

    private fun buildView(): View {
        val w = resources.displayMetrics.widthPixels
        // Slim golden margin (≈ W/φ⁵) so content fills the width rather than ~half of it.
        val side = (w / (phi * phi * phi * phi * phi)).toInt().coerceIn(dp(20), dp(52))
        val contentW = (w - 2 * side).coerceAtLeast(dp(280))
        scale = (contentW.toFloat() / dp(716)).coerceIn(0.62f, 1.15f)

        val column = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setPadding(side, dim(40), side, dim(40))
            // fillViewport stretches us to the screen; weighted spacers then centre short content
            // and the closing flourish settles toward the bottom.
            layoutParams = ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
        }

        column.addView(brandBlock())

        val recents = Books.recents(this)
        column.addView(spacerWeighted(1f))
        if (recents.isEmpty()) {
            column.addView(TextView(this).apply {
                text = "Open a document to start your library."
                setTextColor(textMuted); textSize = fs(16f); gravity = Gravity.CENTER
                typeface = serif; setPadding(0, 0, 0, dim(20))
            })
            column.addView(openButton())
        } else {
            column.addView(heroCard(recents.first(), contentW))
            if (recents.size >= 2) {
                column.addView(spacer(dim(28)))
                column.addView(eyebrowItalic("Recently on your shelf"))
                column.addView(spacer(dim(16)))
                column.addView(shelf(recents.take(3), contentW))
            }
            column.addView(spacer(dim(28)))
            column.addView(openButton())
        }
        column.addView(spacerWeighted(phi))
        column.addView(closingMark())

        val scroll = ScrollView(this).apply {
            isFillViewport = true
            isVerticalScrollBarEnabled = false
            addView(column)
        }
        // A subtle top-corner gear → app Settings (export behaviour, help, about).
        return FrameLayout(this).apply {
            setBackgroundColor(Color.WHITE)
            addView(scroll, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
            addView(ImageView(this@HomeActivity).apply {
                setImageResource(R.drawable.ic_settings)
                val p = dp(8); setPadding(p, p, p, p)
                isClickable = true
                setOnClickListener { startActivity(Intent(this@HomeActivity, SettingsActivity::class.java)) }
            }, FrameLayout.LayoutParams(dp(40), dp(40), Gravity.TOP or Gravity.END).apply {
                topMargin = dp(10); marginEnd = dp(10)
            })
        }
    }

    // ── Brand: inkwell mark · script name · version, like a title page ────────────────────────────

    private fun brandBlock(): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        gravity = Gravity.CENTER_HORIZONTAL
        val logo = dim(56)
        addView(ImageView(this@HomeActivity).apply { setImageResource(R.mipmap.ic_launcher) }, LinearLayout.LayoutParams(logo, logo).apply { bottomMargin = dim(8) })
        addView(TextView(this@HomeActivity).apply {
            text = swashCaps("InkRead"); setTextColor(ink); textSize = fs(58f)
            typeface = scriptBold; paint.isFakeBoldText = true; gravity = Gravity.CENTER
            includeFontPadding = false
        })
        addView(eyebrow("·  v${versionName()}  ·").apply { gravity = Gravity.CENTER; setPadding(0, dim(10), 0, 0) })
    }

    // ── Continue where you left off (the most-recent book) ───────────────────────────────────────

    private fun heroCard(r: Books.Recent, contentW: Int): View {
        val coverW = dim(114)
        val coverH = dim(162)
        val pad = dim(20)
        val gap = dim(20)
        return LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(pad, pad, pad, pad)
            background = outlined(dim(12))
            isClickable = true
            setOnClickListener { open(r) }
            layoutParams = LinearLayout.LayoutParams(contentW, ViewGroup.LayoutParams.WRAP_CONTENT)

            addView(cover(r, coverW, coverH, spine = true))

            addView(LinearLayout(this@HomeActivity).apply {
                orientation = LinearLayout.VERTICAL
                addView(TextView(this@HomeActivity).apply {
                    text = "Continue where you left off"; setTextColor(inkSoft); textSize = fs(15f)
                    typeface = Typeface.create(serif, Typeface.ITALIC)
                })
                addView(TextView(this@HomeActivity).apply {
                    text = r.title; setTextColor(ink); textSize = fs(26f); typeface = serif
                    maxLines = 3; setLineSpacing(0f, 1.1f); setPadding(0, dim(7), 0, 0)
                })
                addView(spacerWeighted(1f))
                val percent = Books.progress(this@HomeActivity, r.id)
                addView(LinearLayout(this@HomeActivity).apply {
                    orientation = LinearLayout.HORIZONTAL
                    gravity = Gravity.CENTER_VERTICAL
                    setPadding(0, dim(14), 0, 0)
                    addView(eyebrow("$percent% complete"))
                    addView(spacerWeighted(1f))
                    addView(TextView(this@HomeActivity).apply {
                        text = "Resume →"; setTextColor(ink); textSize = fs(15f); typeface = serif
                        paintFlags = paintFlags or android.graphics.Paint.UNDERLINE_TEXT_FLAG
                    })
                })
                addView(progressBar(percent).apply {
                    (layoutParams as LinearLayout.LayoutParams).topMargin = dim(8)
                })
            }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, 1f).apply { marginStart = gap })
        }
    }

    // ── Recently on your shelf: staggered covers on a shelf, read-% beneath ──────────────────────

    /** Per-cover height multipliers (of cell width) — the design's 226/242/220-on-148 stagger. */
    private val shelfRatios = floatArrayOf(1.50f, 1.62f, 1.46f)

    private fun shelf(recents: List<Books.Recent>, contentW: Int): View {
        val gap = dim(22)
        val cells = recents.size.coerceAtMost(3)
        val cellW = ((contentW - (cells - 1) * gap) / cells).coerceAtLeast(dp(72))

        val covers = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.BOTTOM or Gravity.CENTER_HORIZONTAL
            recents.take(3).forEachIndexed { i, r ->
                val h = (cellW * shelfRatios[i % shelfRatios.size]).toInt()
                addView(cover(r, cellW, h, spine = true).apply {
                    isClickable = true; setOnClickListener { open(r) }
                }, LinearLayout.LayoutParams(cellW, h).apply { marginStart = if (i == 0) 0 else gap })
            }
        }
        val shelfBar = View(this).apply {
            background = GradientDrawable().apply { setColor(ink); cornerRadius = dim(2).toFloat() }
            layoutParams = LinearLayout.LayoutParams(contentW, dim(5)).apply { topMargin = dim(1) }
        }
        val labels = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_HORIZONTAL
            setPadding(0, dim(10), 0, 0)
            recents.take(3).forEachIndexed { i, r ->
                addView(eyebrow("${Books.progress(this@HomeActivity, r.id)}% read").apply {
                    gravity = Gravity.CENTER
                }, LinearLayout.LayoutParams(cellW, ViewGroup.LayoutParams.WRAP_CONTENT).apply { marginStart = if (i == 0) 0 else gap })
            }
        }
        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            addView(covers)
            addView(shelfBar)
            addView(labels)
        }
    }

    // ── Closing flourish: script line · inkwell mark between rules ────────────────────────────────

    private fun closingMark(): View = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        gravity = Gravity.CENTER_HORIZONTAL
        addView(TextView(this@HomeActivity).apply {
            text = swashCaps("Super Reader for a Super You")
            // Subtle footer: small (matches the version eyebrow's 12sp) and light (muted grey, not
            // the heavy dark script it was) so it reads as a quiet tagline, not a headline.
            setTextColor(textMuted); textSize = fs(12f); typeface = script
            gravity = Gravity.CENTER; includeFontPadding = false
        })
        addView(LinearLayout(this@HomeActivity).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER
            setPadding(0, dim(6), 0, 0)
            addView(rule())
            addView(ImageView(this@HomeActivity).apply { setImageResource(R.mipmap.ic_launcher) },
                LinearLayout.LayoutParams(dim(20), dim(20)).apply { marginStart = dim(12); marginEnd = dim(12) })
            addView(rule())
        })
    }

    private fun rule(): View = View(this).apply {
        setBackgroundColor(ink)
        layoutParams = LinearLayout.LayoutParams(dim(40), maxOf(1, dp(1)))
    }

    // ── Shared pieces ────────────────────────────────────────────────────────────────────────────

    /** A book cover: cached first-page thumbnail, else a black "spine" (or bordered) placeholder. */
    private fun cover(r: Books.Recent, w: Int, h: Int, spine: Boolean): View {
        val bmp = Books.loadThumbnail(this, r.id)
        if (bmp != null) {
            return ImageView(this).apply {
                scaleType = ImageView.ScaleType.CENTER_CROP
                setImageBitmap(bmp)
                background = GradientDrawable().apply { setStroke(maxOf(1, dp(1)), ink) }
                layoutParams = LinearLayout.LayoutParams(w, h)
            }
        }
        if (!spine) {
            return TextView(this).apply {
                text = r.title; setTextColor(textSecondary); textSize = fs(12f); typeface = serif
                gravity = Gravity.CENTER; setPadding(dim(8), dim(8), dim(8), dim(8))
                background = GradientDrawable().apply {
                    setColor(Color.parseColor("#EFEFEF")); setStroke(maxOf(1, dp(1)), ink)
                }
                layoutParams = LinearLayout.LayoutParams(w, h)
            }
        }
        // Black book spine with a white page-edge and the title set in white, bottom-aligned.
        return LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            background = GradientDrawable().apply { setColor(ink); cornerRadius = dim(5).toFloat() }
            layoutParams = LinearLayout.LayoutParams(w, h)
            addView(View(this@HomeActivity).apply { setBackgroundColor(Color.WHITE) },
                LinearLayout.LayoutParams(dim(4), ViewGroup.LayoutParams.MATCH_PARENT))
            addView(TextView(this@HomeActivity).apply {
                text = r.title; setTextColor(Color.WHITE); textSize = fs(15f); typeface = serif
                gravity = Gravity.BOTTOM; setLineSpacing(0f, 1.12f)
                setPadding(dim(12), dim(12), dim(12), dim(12))
                layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, 1f)
            })
        }
    }

    /** A thin read-progress bar inside a black-outlined capsule (filled portion solid black). */
    private fun progressBar(percent: Int): View {
        val p = percent.coerceIn(0, 100)
        return LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            background = GradientDrawable().apply {
                setColor(Color.WHITE); setStroke(maxOf(1, dp(1)), ink); cornerRadius = dim(6).toFloat()
            }
            val padInner = maxOf(1, dp(1))
            setPadding(padInner, padInner, padInner, padInner)
            layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dim(11))
            if (p > 0) addView(View(this@HomeActivity).apply {
                background = GradientDrawable().apply { setColor(ink); cornerRadius = dim(4).toFloat() }
            }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, p.toFloat()))
            if (p < 100) addView(View(this@HomeActivity), LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, (100 - p).toFloat()))
        }
    }

    private fun openButton(): View = TextView(this).apply {
        text = "＋  Open a Document"; setTextColor(ink); textSize = fs(16f); typeface = serif
        gravity = Gravity.CENTER
        setPadding(dim(30), dim(13), dim(30), dim(13))
        background = GradientDrawable().apply {
            setColor(Color.WHITE); setStroke(maxOf(1, dp(1)), ink); cornerRadius = dim(40).toFloat()
        }
        isClickable = true; setOnClickListener { openPicker() }
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT)
    }

    /** A small uppercase, letter-spaced mono eyebrow (the design's Space Mono labels). */
    private fun eyebrow(text: String): TextView = TextView(this).apply {
        this.text = text.uppercase(); setTextColor(ink); textSize = fs(12f)
        typeface = mono; letterSpacing = 0.12f
    }

    private fun eyebrowItalic(text: String): TextView = TextView(this).apply {
        this.text = text; setTextColor(inkSoft); textSize = fs(16f); gravity = Gravity.CENTER
        typeface = Typeface.create(serif, Typeface.ITALIC)
    }

    private fun outlined(radius: Int): GradientDrawable = GradientDrawable().apply {
        setColor(Color.WHITE); setStroke(maxOf(1, (1.5f * density).toInt()), ink); cornerRadius = radius.toFloat()
    }

    private fun spacer(h: Int): View = View(this).apply {
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, h)
    }

    private fun spacerWeighted(weight: Float): View = View(this).apply {
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, weight)
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
