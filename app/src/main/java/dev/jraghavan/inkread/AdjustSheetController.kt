package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.app.Dialog
import android.content.pm.ActivityInfo
import android.graphics.Color
import android.graphics.drawable.GradientDrawable
import android.util.Log
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.Window
import android.widget.HorizontalScrollView
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast

/**
 * The KOReader-style tabbed document-settings sheet (RR4), extracted from `ReaderActivity` (SRP):
 * one bottom-bar entry consolidates the document controls (Style / Rotate / Crop / Zoom / Page /
 * Font / Display). Owns the tab panels, the segmented / cell-bar / settings-row widgets, the style
 * presets, and the reflow font-scale application. Page geometry stays in the shell — the reflow
 * toggle and zoom mutations go through [Host].
 *
 * Threading mirrors the original inline code: panels are built on the UI thread; native calls run
 * on the engine thread via [Host.engineExecute].
 */
class AdjustSheetController(private val host: Host) {

    /** What the settings sheet needs from the reader shell. */
    interface Host {
        /** Context for dialogs/toasts/resources, `runOnUiThread`, `requestedOrientation`. */
        val activity: Activity

        /** The open document handle (`0` = none); read live per call. */
        val docHandle: Long

        /** The persisted display/typography settings. */
        val prefs: DisplayPrefs

        /** Whether PDF reflow mode is currently on (ADR-INKREAD-0011). */
        val reflowOn: Boolean

        /** Current zoom as a percentage, for the Zoom tab's label. */
        val zoomPercent: Int

        /** Run [block] on the single engine thread (serializes native access). */
        fun engineExecute(block: () -> Unit)

        /** Re-render + blit the current page (any thread). */
        fun repaintPanel()

        /** Engine thread only: re-read the page count after a repagination. */
        fun refreshPageCount()

        /** Apply a changed periodic full-refresh interval (#99) to the live page-turn cadence. */
        fun applyFullRefreshInterval(n: Int)

        /** Toggle PDF reflow; owns the zoom/pan/progress-dialog state the toggle disturbs. */
        fun setReflowMode(on: Boolean)

        /** Zoom one step in/out (the shell owns the zoom model + step constant). */
        fun zoomIn()

        fun zoomOut()

        /** No document open: fall back to the system file picker. */
        fun openPicker()

        /** Pick a font file to add to the reader's own faces (RR28-FR3). */
        fun openFontPicker()

        /** Wrap dialog content so a resting palm can't tap through it (mirrors the bottom bar). */
        fun palmGuard(content: View): View

        /** Verbose diagnostic log, gated by the shell's `DIAG` flag. */
        fun diag(msg: () -> String)
    }

    private val activity: Activity get() = host.activity
    private val prefs: DisplayPrefs get() = host.prefs

    /**
     * A KOReader-style tabbed settings sheet that consolidates the document controls
     * (Rotate / Fit / Font / Display) behind one bottom-bar entry — matching KOReader's bottom
     * sheet structure. Each tab swaps an inline control panel; changes apply live + persist.
     */
    fun show() {
        if (host.docHandle == 0L) { host.openPicker(); return }
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val dialog = Dialog(activity).apply { requestWindowFeature(Window.FEATURE_NO_TITLE) }
        val content = android.widget.FrameLayout(activity)

        val panels: List<Triple<String, Int, () -> View>> = listOf(
            Triple("Style", R.drawable.ic_menu_adjust) { stylePanel() },
            Triple("Rotate", R.drawable.ic_menu_rotate) { rotationPanel() },
            Triple("Crop", R.drawable.ic_menu_crop) { cropPanel() },
            Triple("Zoom", R.drawable.ic_menu_fit) { zoomPanel() },
            Triple("Page", R.drawable.ic_menu_page) { pagePanel() },
            Triple("Font", R.drawable.ic_menu_font) { fontPanel() },
            Triple("Display", R.drawable.ic_menu_display) { displayPanel() },
        )
        val cells = ArrayList<LinearLayout>()
        fun select(i: Int) {
            content.removeAllViews()
            content.addView(panels[i].third())
            cells.forEachIndexed { j, c ->
                // Active tab: white, boxed (connected to the panel) + bold label. Inactive: flat gray.
                if (j == i) {
                    c.background = GradientDrawable().apply {
                        setColor(Ink.paper); setStroke(Ink.keyline(), Ink.ink)
                    }
                } else {
                    c.background = null
                    c.setBackgroundColor(Ink.fill)
                }
                val tab = c.getChildAt(1) as? TextView
                tab?.setTypeface(Ink.mono, if (j == i) android.graphics.Typeface.BOLD else android.graphics.Typeface.NORMAL)
                tab?.setTextColor(if (j == i) Ink.ink else Ink.inkSoft)
            }
        }
        val tabRow = LinearLayout(activity).apply {
            orientation = LinearLayout.HORIZONTAL
            setBackgroundColor(Ink.fill)
        }
        panels.forEachIndexed { i, (label, icon, _) ->
            val cell = LinearLayout(activity).apply {
                orientation = LinearLayout.VERTICAL; gravity = Gravity.CENTER
                setPadding(dp(4), dp(10), dp(4), dp(10)); isClickable = true
                setOnClickListener { select(i) }
                addView(
                    ImageView(activity).apply { setImageResource(icon); setColorFilter(Ink.ink) },
                    LinearLayout.LayoutParams(dp(24), dp(24)),
                )
                addView(TextView(activity).apply {
                    text = label; textSize = Ink.sp(10f); setTextColor(Ink.inkSoft); typeface = Ink.mono
                    gravity = Gravity.CENTER; setPadding(0, dp(3), 0, 0)
                })
            }
            cells.add(cell)
            tabRow.addView(cell, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        }
        val container = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Ink.paper)
            // Black keyline up top so the sheet reads as a docked surface (bottom-bar template).
            addView(
                View(activity).apply { setBackgroundColor(Ink.ink) },
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, Ink.hair()),
            )
            // Active tab's panel — WRAP_CONTENT so the sheet GROWS UP per tab (KOReader-style),
            // bottom-anchored, instead of a fixed box with dead space.
            addView(
                content,
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT),
            )
            addView(
                View(activity).apply { setBackgroundColor(Ink.hairline) },
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, Ink.hair()),
            )
            addView(tabRow)
        }
        select(0)
        dialog.setContentView(host.palmGuard(container)) // same palm guard as the bottom bar
        dialog.window?.apply {
            setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
            setGravity(Gravity.BOTTOM)
            setBackgroundDrawable(android.graphics.drawable.ColorDrawable(Ink.paper))
        }
        dialog.show()
    }

    /** A KOReader-style **segmented control**: a rounded pill of [options] with the [selected]
     *  segment filled dark. Updates its own highlight on tap, then calls [onSelect]. */
    private fun segmented(options: List<String>, selected: Int, onSelect: (Int) -> Unit): View {
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val radius = dp(20).toFloat()
        var sel = selected
        val segs = ArrayList<TextView>()
        fun style(tv: TextView, on: Boolean) {
            if (on) {
                tv.setTextColor(Ink.paper)
                tv.setTypeface(null, android.graphics.Typeface.BOLD)
                tv.background = GradientDrawable().apply { setColor(Ink.ink); cornerRadius = radius }
            } else {
                tv.setTextColor(Ink.ink)
                tv.setTypeface(null, android.graphics.Typeface.NORMAL)
                tv.background = null
            }
        }
        return LinearLayout(activity).apply {
            orientation = LinearLayout.HORIZONTAL
            background = GradientDrawable().apply {
                setColor(Ink.paper); cornerRadius = radius
                setStroke(Ink.hair(), Ink.ringSoft)
            }
            val p = dp(3); setPadding(p, p, p, p)
            options.forEachIndexed { i, opt ->
                val tv = TextView(activity).apply {
                    text = opt; textSize = Ink.sp(15f); gravity = Gravity.CENTER
                    setPadding(dp(6), dp(10), dp(6), dp(10)); isClickable = true
                    setOnClickListener { sel = i; segs.forEachIndexed { j, t -> style(t, j == sel) }; onSelect(i) }
                }
                style(tv, i == sel)
                segs.add(tv)
                addView(tv, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            }
        }
    }

    /** A KOReader-style **cell bar**: [count] boxes filled up to the current level; tapping box i
     *  sets level i+1 (tapping the current top cell turns it off → 0). Repaints on tap. */
    private fun cellBar(count: Int, initial: Int, onSet: (Int) -> Unit): View {
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        var filled = initial
        val draws = ArrayList<GradientDrawable>()
        fun repaint() = draws.forEachIndexed { i, g ->
            g.setColor(if (i < filled) Ink.ink else Ink.paper)
        }
        return LinearLayout(activity).apply {
            orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
            for (i in 0 until count) {
                val g = GradientDrawable().apply { setStroke(Ink.hair(), Ink.ringSoft) }
                draws.add(g)
                addView(View(activity).apply {
                    background = g; isClickable = true
                    setOnClickListener {
                        filled = if (i + 1 == filled) 0 else i + 1
                        repaint(); invalidate()
                        onSet(filled)
                    }
                }, LinearLayout.LayoutParams(0, dp(30), 1f).apply { val m = dp(2); setMargins(m, 0, m, 0) })
            }
            repaint()
        }
    }

    /** A KOReader-style settings row: a right-aligned [label] on the left, the [control] on the right. */
    /** Let a control that can outgrow the sheet scroll sideways instead of running off the edge.
     *  The typeface picker is the one that can: six bundled families already fill the row, and every
     *  imported font (RR28-FR3) adds a segment, which would otherwise put the last faces out of
     *  reach. Scrollbar suppressed — it would draw over the pill's rounded edge. */
    private fun scrollable(content: View): View =
        HorizontalScrollView(activity).apply {
            isHorizontalScrollBarEnabled = false
            addView(content)
        }

    /** "Add…" / "Remove…" for the reader's imported faces (RR28-FR3). */
    private fun fontLibraryControls(): View {
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        return LinearLayout(activity).apply {
            orientation = LinearLayout.HORIZONTAL
            addView(pill("Add…") { host.openFontPicker() })
            if (UserFonts.files(activity).isNotEmpty()) {
                addView(
                    pill("Remove…") { showRemoveFontDialog() },
                    LinearLayout.LayoutParams(
                        ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT,
                    ).apply { leftMargin = dp(8) },
                )
            }
        }
    }

    /** Pick one imported font to delete. Removing shifts the remaining ids, so the picker is
     *  rebuilt from the core's list afterwards rather than patched. */
    private fun showRemoveFontDialog() {
        val fonts = UserFonts.files(activity)
        if (fonts.isEmpty()) return
        val names = fonts.map(UserFonts::displayName).toTypedArray()
        AlertDialog.Builder(activity)
            .setTitle("Remove a font")
            .setItems(names) { _, which ->
                val font = fonts[which]
                UserFonts.remove(activity, font)
                // The removed face may have been the one in use; re-apply the (clamped) choice so
                // the open book is never left rendering with an id that now means something else.
                val faces = try {
                    NativeBridge.nativeFontNames().split("\n").filter { it.isNotBlank() }
                } catch (e: RuntimeException) { emptyList() }
                val id = prefs.font.coerceIn(0, (faces.size - 1).coerceAtLeast(0))
                prefs.font = id
                host.engineExecute {
                    try { NativeBridge.nativeSetFont(host.docHandle, id) } catch (e: RuntimeException) { -1 }
                    host.refreshPageCount()
                    host.repaintPanel()
                }
                Toast.makeText(activity, "Removed ${UserFonts.displayName(font)}", Toast.LENGTH_SHORT).show()
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun settingRow(label: String, control: View): View {
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        return LinearLayout(activity).apply {
            orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(16), dp(14), dp(16), dp(14))
            addView(TextView(activity).apply {
                text = label; textSize = Ink.sp(16f); setTextColor(Color.BLACK); gravity = Gravity.END
            }, LinearLayout.LayoutParams(dp(96), ViewGroup.LayoutParams.WRAP_CONTENT))
            addView(control, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
                marginStart = dp(14)
            })
        }
    }

    private fun rotationPanel(): View {
        val orients = intArrayOf(
            ActivityInfo.SCREEN_ORIENTATION_PORTRAIT,
            ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE,
            ActivityInfo.SCREEN_ORIENTATION_REVERSE_PORTRAIT,
            ActivityInfo.SCREEN_ORIENTATION_REVERSE_LANDSCAPE,
        )
        val sel = orients.indexOf(prefs.orientation).coerceAtLeast(0)
        return settingRow("Rotation", segmented(listOf("0°", "90°", "180°", "270°"), sel) { which ->
            host.diag { "DIAG rotation -> $which" }
            applyOrientation(orients[which])
        })
    }

    /** Set + persist the screen orientation; the resize re-renders the page (engine via surfaceChanged). */
    private fun applyOrientation(orientation: Int) {
        prefs.orientation = orientation
        activity.requestedOrientation = orientation
    }

    private fun fitPanel(): View {
        val sel = prefs.fit.coerceIn(0, 2) // index = core FitMode code
        return settingRow("Fit", segmented(listOf("Full", "Width", "Height"), sel) { which ->
            prefs.fit = which
            host.diag { "DIAG fit -> mode=$which" }
            host.engineExecute {
                try { NativeBridge.nativeSetFit(host.docHandle, which) } catch (e: RuntimeException) {}
                host.repaintPanel()
            }
        })
    }

    /** The "Crop" tab: Page Crop (None/Auto) + a Margin cell bar (margin kept around the content). */
    private fun cropPanel(): View {
        val container = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
        container.addView(settingRow("Page Crop", segmented(listOf("None", "Auto"), if (prefs.cropAuto) 1 else 0) { which ->
            prefs.cropAuto = which == 1
            host.diag { "DIAG crop auto=${which == 1}" }
            host.engineExecute {
                try { NativeBridge.nativeSetCrop(host.docHandle, which, prefs.cropMargin) } catch (e: RuntimeException) {}
                host.repaintPanel()
            }
        }))
        container.addView(settingRow("Margin", cellBar(8, prefs.cropMargin) { level ->
            prefs.cropMargin = level
            host.diag { "DIAG crop margin=$level" }
            host.engineExecute {
                try { NativeBridge.nativeSetCrop(host.docHandle, if (prefs.cropAuto) 1 else 0, level) } catch (e: RuntimeException) {}
                host.repaintPanel()
            }
        }))
        return container
    }

    /** The "Page" tab: reflow Line Spacing + Alignment (EPUB; a toast on fixed-layout PDF). */
    private fun pagePanel(): View {
        fun applyReflow(call: () -> Int) =
            runReflow(call, whenFixedLayout = "Page layout adjusts reflowable books (EPUB)")
        val container = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
        // Reflow toggle — only for a text-layer PDF (EPUB is always reflowable; a scanned PDF can't).
        // It gates whether the Line Spacing / Alignment / Font Size controls take effect on a PDF.
        val supportsReflow = try { NativeBridge.nativeSupportsReflow(host.docHandle) } catch (e: RuntimeException) { false }
        if (supportsReflow) {
            container.addView(settingRow("Reflow", segmented(listOf("Off", "On"), if (host.reflowOn) 1 else 0) { which ->
                host.diag { "DIAG reflow=${which == 1}" }
                host.setReflowMode(which == 1)
            }))
        }
        container.addView(settingRow("Line Spacing", segmented(DisplayPrefs.LINE_SPACING_LABELS, prefs.lineSpacingIndex()) { which ->
            prefs.lineSpacingMult = DisplayPrefs.LINE_SPACINGS[which]
            host.diag { "DIAG line spacing=${DisplayPrefs.LINE_SPACINGS[which]}" }
            applyReflow { NativeBridge.nativeSetLineSpacing(host.docHandle, DisplayPrefs.LINE_SPACINGS[which]) }
        }))
        container.addView(settingRow("Alignment", segmented(listOf("Left", "Justify", "Center", "Right"), prefs.alignment) { which ->
            prefs.alignment = which
            host.diag { "DIAG alignment=$which" }
            applyReflow { NativeBridge.nativeSetAlignment(host.docHandle, which) }
        }))
        // #194. The core declines two columns on a page too narrow for a readable measure. Say so:
        // silently doing nothing is indistinguishable from a broken control, and the setting is
        // still stored — it takes effect once the text is smaller or the page wider.
        container.addView(settingRow("Columns", segmented(listOf("Single", "Two"), prefs.columns - 1) { which ->
            prefs.columns = which + 1
            applyReflow {
                val page = NativeBridge.nativeSetColumns(host.docHandle, prefs.columns)
                val effective =
                    runCatching { NativeBridge.nativeEffectiveColumns(host.docHandle) }.getOrDefault(1)
                android.util.Log.i(
                    "AdjustSheet",
                    "columns: asked ${prefs.columns}, layout using $effective, page=$page",
                )
                if (prefs.columns > effective) activity.runOnUiThread {
                    Toast.makeText(
                        activity,
                        "Two columns need a wider page or smaller text",
                        Toast.LENGTH_SHORT,
                    ).show()
                }
                page
            }
        }))
        return container
    }

    /** A white rounded stepper pill (shared by the Zoom and Font tabs). */
    private fun pill(t: String, on: () -> Unit): TextView {
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        return TextView(activity).apply {
            text = t; textSize = Ink.sp(16f); gravity = Gravity.CENTER; setTextColor(Color.BLACK)
            setPadding(dp(18), dp(10), dp(18), dp(10)); isClickable = true
            background = GradientDrawable().apply {
                setColor(Color.WHITE); cornerRadius = dp(20).toFloat(); setStroke(maxOf(1, dp(1)), Color.parseColor("#9E9E9E"))
            }
            setOnClickListener { on() }
        }
    }

    /** The "Zoom" tab: the Fit segmented row + a live zoom −/+ stepper (zoom moved off the bar). */
    private fun zoomPanel(): View {
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val zlabel = TextView(activity).apply {
            textSize = Ink.sp(16f); setTextColor(Color.BLACK); gravity = Gravity.CENTER; minWidth = dp(64)
        }
        fun refresh() { zlabel.text = "${host.zoomPercent}%" }
        refresh()
        val zoomControl = LinearLayout(activity).apply {
            orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
            addView(pill("−") { host.zoomOut(); refresh() })
            addView(zlabel, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
                val m = dp(10); setMargins(m, 0, m, 0)
            })
            addView(pill("+") { host.zoomIn(); refresh() })
        }
        return LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            addView(fitPanel())
            addView(settingRow("Zoom", zoomControl))
        }
    }

    private fun displayPanel(): View {
        val container = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
        container.addView(settingRow("Contrast", cellBar(DisplayPrefs.CONTRAST_MAX, prefs.contrast) { level ->
            prefs.contrast = level
            host.diag { "DIAG contrast step=$level" }
            host.engineExecute {
                try { NativeBridge.nativeSetContrast(host.docHandle, level) } catch (e: RuntimeException) {}
                host.repaintPanel()
            }
        }))
        container.addView(settingRow("Quality", segmented(listOf("Low", "Default", "High"), prefs.renderQuality) { which ->
            prefs.renderQuality = which
            host.diag { "DIAG render quality=$which" }
            host.engineExecute {
                try { NativeBridge.nativeSetRenderQuality(host.docHandle, which) } catch (e: RuntimeException) {}
                host.repaintPanel()
            }
        }))
        container.addView(settingRow("Menu Size", segmented(DisplayPrefs.UI_SCALE_LABELS, DisplayPrefs.nearestUiScaleIndex(prefs.uiScale)) { which ->
            applyUiScale(DisplayPrefs.UI_SCALES[which])
        }))
        container.addView(settingRow("Full Refresh", segmented(DisplayPrefs.REFRESH_INTERVAL_LABELS, DisplayPrefs.REFRESH_INTERVALS.indexOf(prefs.fullRefreshEvery).coerceAtLeast(0)) { which ->
            val n = DisplayPrefs.REFRESH_INTERVALS[which]
            prefs.fullRefreshEvery = n
            host.applyFullRefreshInterval(n)
            host.diag { "DIAG full-refresh every=$n pages" }
        }))
        return container
    }

    /** Persist + apply the menu/chrome scale (#133) for large panels (e.g. the Manta). The reader's
     *  menus read [Ink.uiScale] as they build, so the new size lands as each menu/sheet is next
     *  opened; this open sheet keeps its current size, so a toast confirms the change. */
    private fun applyUiScale(scale: Float) {
        prefs.uiScale = scale
        Ink.uiScale = scale
        val label = DisplayPrefs.UI_SCALE_LABELS[DisplayPrefs.nearestUiScaleIndex(scale)]
        Toast.makeText(activity, "Menu size $label — reopen a menu to see it", Toast.LENGTH_SHORT).show()
    }

    /** A reading style preset (1.10): a bundle of (text scale, line-spacing index, contrast step,
     *  night). Font/spacing are no-ops on fixed-layout PDFs; contrast/night apply to both. */
    private data class StyleSpec(val scale: Float, val spacing: Float, val contrast: Int, val night: Boolean)

    private fun styleSpec(name: String): StyleSpec = when (name) {
        "Bold" -> StyleSpec(1.0f, DisplayPrefs.DEFAULT_LINE_SPACING, 4, false) // darker text (heavier ink)
        "Night" -> StyleSpec(1.0f, DisplayPrefs.DEFAULT_LINE_SPACING, 0, true) // inverted (light on dark)
        "Outdoor" -> StyleSpec(1.0f, DisplayPrefs.DEFAULT_LINE_SPACING, DisplayPrefs.CONTRAST_MAX, false) // maximum contrast
        "Relaxed" -> StyleSpec(1.15f, 1.7f, 0, false) // larger + airier
        else -> StyleSpec(1.0f, DisplayPrefs.DEFAULT_LINE_SPACING, 0, false) // Original — defaults
    }

    /** The "Style" tab: one-tap presets that bundle font size, spacing, contrast, and night. */
    private fun stylePanel(): View {
        val container = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
        container.addView(
            settingRow(
                "Preset",
                segmented(DisplayPrefs.STYLE_PRESETS, DisplayPrefs.STYLE_PRESETS.indexOf(prefs.stylePreset).coerceAtLeast(0)) { which ->
                    applyStylePreset(DisplayPrefs.STYLE_PRESETS[which])
                },
            ),
        )
        return container
    }

    /** Apply + persist a style preset: set every knob, then repaginate/re-render. */
    private fun applyStylePreset(name: String) {
        val s = styleSpec(name)
        prefs.stylePreset = name
        prefs.textScale = s.scale
        prefs.lineSpacingMult = s.spacing
        prefs.contrast = s.contrast
        prefs.night = s.night
        host.engineExecute {
            try {
                NativeBridge.nativeSetTextScale(host.docHandle, s.scale)
                NativeBridge.nativeSetLineSpacing(host.docHandle, s.spacing)
                NativeBridge.nativeSetContrast(host.docHandle, s.contrast)
                NativeBridge.nativeSetNight(host.docHandle, s.night)
                host.refreshPageCount()
            } catch (e: RuntimeException) {
                Log.e(TAG, "style preset apply failed: ${e.message}")
            }
            host.repaintPanel()
        }
    }

    private fun fontPanel(): View {
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        var idx = DisplayPrefs.nearestScaleIndex(prefs.textScale)
        val value = TextView(activity).apply {
            textSize = Ink.sp(16f); setTextColor(Color.BLACK); gravity = Gravity.CENTER; minWidth = dp(64)
        }
        fun refresh() { value.text = "${(DisplayPrefs.TEXT_SCALES[idx] * 100).toInt()}%" }
        refresh()
        fun apply() { refresh(); applyReflowScale(idx, warnIfFixed = true) }
        val control = LinearLayout(activity).apply {
            orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
            addView(pill("A−") { if (idx > 0) { idx--; apply() } })
            addView(value, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
                val m = dp(10); setMargins(m, 0, m, 0)
            })
            addView(pill("A+") { if (idx < DisplayPrefs.TEXT_SCALES.size - 1) { idx++; apply() } })
            // Quick presets next to the steppers: jump straight to default / largest (#55).
            fun preset(p: View) = addView(p, LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT,
            ).apply { leftMargin = dp(8) })
            preset(pill("100%") { idx = DisplayPrefs.nearestScaleIndex(1.0f); apply() })
            preset(pill("XL") { idx = DisplayPrefs.TEXT_SCALES.size - 1; apply() })
        }
        val container = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL }
        // Typeface picker — the bundled reading faces (EPUB/reflow; a toast on fixed-layout PDF).
        val faces = try {
            NativeBridge.nativeFontNames().split("\n").filter { it.isNotBlank() }
        } catch (e: RuntimeException) { emptyList() }
        if (faces.isNotEmpty()) {
            container.addView(settingRow("Typeface", scrollable(segmented(faces, prefs.font.coerceIn(0, faces.size - 1)) { which ->
                prefs.font = which
                host.engineExecute {
                    val np = try { NativeBridge.nativeSetFont(host.docHandle, which) } catch (e: RuntimeException) { -1 }
                    if (np >= 0) { host.refreshPageCount(); host.repaintPanel() }
                    else activity.runOnUiThread {
                        Toast.makeText(activity, "Typeface adjusts reflowable books (EPUB)", Toast.LENGTH_SHORT).show()
                    }
                }
            })))
            container.addView(settingRow("Your fonts", fontLibraryControls()))
        }
        container.addView(settingRow("Font Size", control))
        return container
    }

    /** Apply reflow font-size preset [rawIdx] (clamped): persist, repaginate off-thread, repaint.
     *  Pinch-zoom on a reflowable view and the Font panel's A-/A+ both route here (DRY).
     *  [announce] toasts the new size (pinch feedback); [warnIfFixed] toasts when the doc is
     *  fixed-layout so the size can't change (Font panel feedback). */
    fun applyReflowScale(rawIdx: Int, announce: Boolean = false, warnIfFixed: Boolean = false) {
        val idx = rawIdx.coerceIn(0, DisplayPrefs.TEXT_SCALES.size - 1)
        prefs.textScale = DisplayPrefs.TEXT_SCALES[idx]
        if (announce) activity.runOnUiThread {
            Toast.makeText(activity, "Font ${(DisplayPrefs.TEXT_SCALES[idx] * 100).toInt()}%", Toast.LENGTH_SHORT).show()
        }
        runReflow(
            { NativeBridge.nativeSetTextScale(host.docHandle, DisplayPrefs.TEXT_SCALES[idx]) },
            whenFixedLayout = "Font size adjusts reflowable books (EPUB)".takeIf { warnIfFixed },
        )
    }

    /**
     * Run a repagination on the engine thread with progress + cancel in front of it (#161), then
     * repaint. [call] returns the new page index, or `-1` for a fixed-layout document — in which
     * case [whenFixedLayout] is toasted, if given.
     *
     * Every reflow entry point routes through here, so the reader sees the same thing whether the
     * relayout came from the Font panel, the Page panel or a pinch.
     */
    private fun runReflow(call: () -> Int, whenFixedLayout: String?) {
        val progress = ReflowProgress(activity)
        progress.begin()
        host.engineExecute {
            val np = try { call() } catch (e: RuntimeException) { -1 }
            activity.runOnUiThread { progress.end() }
            if (np >= 0) {
                host.refreshPageCount()
                host.repaintPanel()
            } else if (whenFixedLayout != null) {
                activity.runOnUiThread {
                    Toast.makeText(activity, whenFixedLayout, Toast.LENGTH_SHORT).show()
                }
            }
        }
    }

    companion object {
        private const val TAG = "AdjustSheet"
    }
}
