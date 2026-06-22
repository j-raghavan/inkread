package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.app.Dialog
import android.content.Intent
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.ColorDrawable
import android.graphics.drawable.GradientDrawable
import android.net.Uri
import android.os.Environment
import android.provider.Settings
import android.util.Log
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.Window
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import org.json.JSONObject

/**
 * Dictionary lookup + management (RR12 / ADR-INKREAD-0009 D2/D3), extracted from `ReaderActivity`
 * (SRP). Owns the on-device corpus handle (open/close), the on-device→online (Wiktionary) lookup
 * chain, the definition card, and the user-dictionary install/remove surface.
 *
 * Threading mirrors the original: [defineWord]/[defineRect] run synchronous native lookups and so
 * must be called on the engine thread (the reader shell wraps them, as it did inline); the UI
 * entries ([defineSelectionText], [showDictionariesDialog]) and [close] manage their own threads.
 */
class DictController(private val host: Host) {

    /** What the dictionary needs from the reader shell. */
    interface Host {
        /** Context for dialogs/toasts, assets/files, `runOnUiThread`. */
        val activity: Activity

        /** The open document handle (`0` = none); read live for the word/rect resolution. */
        val docHandle: Long

        /** Run [block] on the single engine thread (serializes native access). */
        fun engineExecute(block: () -> Unit)
    }

    /** The on-device dictionary corpus handle (`0` = closed); opened lazily by [ensureDictOpen]. */
    private var dictHandle = 0L

    private val activity: Activity get() = host.activity
    private fun runOnUiThread(block: () -> Unit) = activity.runOnUiThread(block)

    /** Resolve the word under a normalized point and look it up (engine thread). */
    fun defineWord(page: Int, nx: Float, ny: Float) {
        if (host.docHandle == 0L) return
        val sel = try {
            WireCodec.decodeSelection(NativeBridge.nativeWordAt(host.docHandle, page, nx, ny))
        } catch (e: RuntimeException) {
            Log.e(TAG, "wordAt failed: ${e.message}"); return
        }
        if (sel.isEmpty) {
            runOnUiThread { Toast.makeText(activity, "No word there", Toast.LENGTH_SHORT).show() }
            return
        }
        lookupAndShow(sel.text)
    }

    /** Resolve the text within a highlighted rect and look up its first word (engine thread). */
    fun defineRect(page: Int, r: FloatArray) {
        if (host.docHandle == 0L) return
        val sel = try {
            WireCodec.decodeSelection(NativeBridge.nativeTextInRect(host.docHandle, page, r[0], r[1], r[2], r[3]))
        } catch (e: RuntimeException) {
            Log.e(TAG, "textInRect failed: ${e.message}"); return
        }
        val word = sel.text.split(Regex("\\s+")).firstOrNull().orEmpty()
        if (word.isBlank()) {
            runOnUiThread { Toast.makeText(activity, "No text selected", Toast.LENGTH_SHORT).show() }
            return
        }
        lookupAndShow(word)
    }

    /** Define the first word-like token of a printed-text selection (lookup is per-word; UI entry). */
    fun defineSelectionText(text: String) {
        val word = text.split(Regex("\\s+")).firstOrNull { it.any(Char::isLetter) } ?: return
        host.engineExecute { lookupAndShow(word) }
    }

    /** On-device lookup → online fallback (cached) → show the popup (engine thread). */
    private fun lookupAndShow(rawWord: String) {
        val word = rawWord.trim().trim { !it.isLetter() && it != '\'' && it != '-' }
        if (word.isEmpty()) return
        if (!ensureDictOpen()) {
            runOnUiThread { Toast.makeText(activity, "Dictionary not available", Toast.LENGTH_SHORT).show() }
            return
        }
        var def = try {
            WireCodec.decodeDefinition(NativeBridge.nativeDefine(dictHandle, word, "en"))
        } catch (e: RuntimeException) {
            WordDefinition(false, "", "", emptyList(), emptyList())
        }
        if (!def.found && AppSettings.onlineLookup(activity)) {
            onlineLookup(word)?.let { online ->
                try {
                    NativeBridge.nativeDictPut(dictHandle, online.lang, online.headword, online.senses.joinToString("\n"))
                } catch (e: RuntimeException) {
                    Log.w(TAG, "dict cache failed: ${e.message}")
                }
                def = online
            }
        }
        val result = def
        runOnUiThread {
            if (result.found) showDictPopup(word, result)
            else Toast.makeText(activity, "\"$word\" not found", Toast.LENGTH_SHORT).show()
        }
    }

    /** Best-effort online lookup via Wiktionary's REST API (RR12; opt-in network). Engine thread. */
    private fun onlineLookup(word: String): WordDefinition? {
        return try {
            val url = URL("https://en.wiktionary.org/api/rest_v1/page/definition/${Uri.encode(word)}")
            val conn = (url.openConnection() as HttpURLConnection).apply {
                connectTimeout = 4000
                readTimeout = 6000
                setRequestProperty("User-Agent", "InkRead/0.1 (offline e-ink reader)")
            }
            if (conn.responseCode != 200) return null
            // Cap the body read: a definition response is a few KB, but the network is untrusted —
            // a runaway/slow body shouldn't be slurped whole (DoS + radio cost). A truncated body
            // simply fails JSON parsing below and falls through to the no-result path.
            val body = conn.inputStream.use { input ->
                val out = java.io.ByteArrayOutputStream()
                val chunk = ByteArray(8192)
                var total = 0
                while (true) {
                    val n = input.read(chunk)
                    if (n < 0) break
                    total += n
                    if (total > MAX_DEFINITION_BYTES) break
                    out.write(chunk, 0, n)
                }
                out.toString("UTF-8")
            }
            val root = JSONObject(body)
            val lang = if (root.has("en")) "en" else root.keys().asSequence().firstOrNull() ?: return null
            val arr = root.getJSONArray(lang)
            val senses = ArrayList<String>()
            outer@ for (i in 0 until arr.length()) {
                val group = arr.getJSONObject(i)
                val pos = group.optString("partOfSpeech", "")
                val defs = group.optJSONArray("definitions") ?: continue
                for (j in 0 until defs.length()) {
                    val dd = stripHtmlTags(defs.getJSONObject(j).optString("definition", ""))
                    if (dd.isNotBlank()) senses.add(if (pos.isNotEmpty()) "($pos) $dd" else dd)
                    if (senses.size >= 6) break@outer
                }
            }
            if (senses.isEmpty()) null else WordDefinition(true, word, lang, senses, emptyList())
        } catch (e: Exception) {
            Log.w(TAG, "online lookup failed: ${e.message}")
            null
        }
    }

    private fun stripHtmlTags(s: String): String =
        s.replace(Regex("<[^>]*>"), "").replace("&amp;", "&").replace("&#39;", "'").trim()

    /** Copy the bundled corpus out of assets (once) and open it; returns true if usable. */
    private fun ensureDictOpen(): Boolean {
        if (dictHandle != 0L) return true
        val dest = File(activity.filesDir, "dict.db")
        if (!dest.exists() || dest.length() == 0L) {
            runOnUiThread { Toast.makeText(activity, "Preparing dictionary…", Toast.LENGTH_SHORT).show() }
            try {
                activity.assets.open("dict.db").use { input -> dest.outputStream().use { input.copyTo(it) } }
            } catch (e: Exception) {
                Log.e(TAG, "dict copy failed: ${e.message}")
                return false
            }
        }
        dictHandle = try {
            NativeBridge.nativeDictOpen(dest.absolutePath)
        } catch (e: RuntimeException) {
            Log.e(TAG, "dict open failed: ${e.message}"); 0L
        }
        return dictHandle != 0L
    }

    /** Free the corpus handle (idempotent; call on teardown). */
    fun close() {
        val h = dictHandle
        dictHandle = 0L
        if (h != 0L) {
            try { NativeBridge.nativeDictClose(h) } catch (e: RuntimeException) { /* ignore */ }
        }
    }

    /**
     * The definition card (RR12 / ADR-INKREAD-0009 D3) — a bottom sheet styled after the Supernote
     * dictionary plugin: the headword (with the *looked-up* word bracketed when it differs, e.g.
     * `run ⟨running⟩`), a **WordNet** source label, a **Definition / Thesaurus** toggle, and senses
     * grouped by part of speech with numbered glosses, examples in curly quotes, and per-sense
     * synonyms. WordNet ships no phonetics/audio, so none are shown (no faux IPA). On-device
     * results parse via [WordNet]; non-WordNet online hits fall back to plain numbered glosses.
     */
    private fun showDictPopup(word: String, def: WordDefinition) {
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val grey = Color.parseColor("#6B6B6B")
        val faint = Color.parseColor("#9E9E9E")
        val serif = Typeface.create("serif", Typeface.NORMAL)
        val serifBold = Typeface.create("serif", Typeface.BOLD)

        val parsed = WordNet.parse(def.senses)
        val headword = def.headword.ifEmpty { word }
        // Thesaurus = the synonyms table plus every per-sense [syn:] set, deduped, headword removed.
        val thesaurus = (def.synonyms + parsed.senses.flatMap { it.synonyms })
            .map { it.trim() }.filter { it.isNotEmpty() && !it.equals(headword, ignoreCase = true) }
            .distinct()

        val dialog = Dialog(activity).apply { requestWindowFeature(Window.FEATURE_NO_TITLE) }
        val root = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            background = GradientDrawable().apply {
                setColor(Color.WHITE)
                cornerRadii = floatArrayOf(dp(18).toFloat(), dp(18).toFloat(), dp(18).toFloat(), dp(18).toFloat(), 0f, 0f, 0f, 0f)
            }
            setPadding(dp(24), dp(12), dp(24), dp(20))
        }

        // ── grab handle (calm sheet affordance) ──────────────────────────────────
        root.addView(View(activity).apply {
            background = GradientDrawable().apply { setColor(Color.parseColor("#D8D8D8")); cornerRadius = dp(2).toFloat() }
            layoutParams = LinearLayout.LayoutParams(dp(36), dp(4)).apply {
                gravity = Gravity.CENTER_HORIZONTAL; bottomMargin = dp(12)
            }
        })

        // ── header: headword + looked-up chip · WordNet source ───────────────────
        val header = LinearLayout(activity).apply { orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL }
        header.addView(TextView(activity).apply {
            text = headword; setTextColor(Color.BLACK); textSize = 27f; typeface = serifBold
        })
        if (!word.equals(headword, ignoreCase = true) && word.isNotEmpty()) {
            header.addView(TextView(activity).apply {
                text = "⟨ $word ⟩"; setTextColor(grey); textSize = 14f; typeface = serif
                setPadding(dp(10), dp(8), 0, 0)
            })
        }
        header.addView(View(activity), LinearLayout.LayoutParams(0, 0, 1f)) // spacer
        header.addView(TextView(activity).apply {
            text = if (def.lang.isNotEmpty() && def.lang != "en") "WordNet · ${def.lang}" else "WordNet"
            setTextColor(faint); textSize = 11f; letterSpacing = 0.06f
        })
        root.addView(header)

        // ── Definition / Thesaurus toggle ────────────────────────────────────────
        val body = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL; setPadding(0, dp(14), 0, 0) }
        lateinit var tabDef: TextView
        lateinit var tabThe: TextView
        fun styleTab(tab: TextView, active: Boolean) {
            tab.setTextColor(if (active) Color.BLACK else faint)
            tab.typeface = if (active) Typeface.DEFAULT_BOLD else Typeface.DEFAULT
            tab.paintFlags = if (active) tab.paintFlags or android.graphics.Paint.UNDERLINE_TEXT_FLAG
            else tab.paintFlags and android.graphics.Paint.UNDERLINE_TEXT_FLAG.inv()
        }
        fun renderDefinition() {
            body.removeAllViews()
            if (parsed.parseFailed) {
                for ((i, s) in def.senses.filter { it.isNotBlank() }.take(8).withIndex()) {
                    body.addView(senseRow(i + 1, s, dp(0)))
                }
                return
            }
            var lastPos: String? = "?" // sentinel so the first group always prints its badge
            for (sense in parsed.senses) {
                if (sense.pos != lastPos) {
                    lastPos = sense.pos
                    body.addView(posBadge(WordNet.labelForPos(sense.pos)))
                }
                body.addView(senseRow(sense.index, sense.definition, dp(2)))
                for (ex in sense.examples) {
                    body.addView(TextView(activity).apply {
                        text = "“$ex”"; setTextColor(grey); textSize = 14f
                        typeface = Typeface.create(Typeface.DEFAULT, Typeface.ITALIC)
                        setPadding(dp(22), dp(3), 0, 0)
                    })
                }
                if (sense.synonyms.isNotEmpty()) {
                    body.addView(TextView(activity).apply {
                        text = "≈ ${sense.synonyms.joinToString(", ")}"
                        setTextColor(faint); textSize = 13f; setPadding(dp(22), dp(3), 0, 0)
                    })
                }
            }
        }
        fun renderThesaurus() {
            body.removeAllViews()
            if (thesaurus.isEmpty()) {
                body.addView(TextView(activity).apply {
                    text = "No thesaurus entries for this word."
                    setTextColor(grey); textSize = 15f; setPadding(0, dp(4), 0, 0)
                })
                return
            }
            body.addView(posBadge("synonyms"))
            body.addView(TextView(activity).apply {
                text = thesaurus.joinToString(" · ")
                setTextColor(Color.BLACK); textSize = 16f; setLineSpacing(dp(4).toFloat(), 1f)
                setPadding(0, dp(4), 0, 0)
            })
        }
        val tabs = LinearLayout(activity).apply { orientation = LinearLayout.HORIZONTAL; setPadding(0, dp(10), 0, 0) }
        tabDef = TextView(activity).apply {
            text = "Definition"; textSize = 14f; setPadding(0, dp(4), dp(22), dp(4)); isClickable = true
            setOnClickListener { styleTab(tabDef, true); styleTab(tabThe, false); renderDefinition() }
        }
        tabThe = TextView(activity).apply {
            text = "Thesaurus"; textSize = 14f; setPadding(0, dp(4), 0, dp(4)); isClickable = true
            setOnClickListener { styleTab(tabThe, true); styleTab(tabDef, false); renderThesaurus() }
        }
        tabs.addView(tabDef); tabs.addView(tabThe)
        root.addView(tabs)
        root.addView(View(activity).apply {
            setBackgroundColor(Color.parseColor("#ECECEC"))
            layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, maxOf(1, dp(1))).apply {
                topMargin = dp(8)
            }
        })
        root.addView(ScrollView(activity).apply {
            isVerticalScrollBarEnabled = false
            addView(body)
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                (activity.resources.displayMetrics.heightPixels * 0.5f).toInt(),
            )
        })

        styleTab(tabDef, true); styleTab(tabThe, false); renderDefinition()
        dialog.setContentView(root)
        dialog.window?.apply {
            setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
            setGravity(Gravity.BOTTOM)
            setBackgroundDrawable(ColorDrawable(Color.TRANSPARENT))
        }
        dialog.show()
    }

    /** A part-of-speech badge (a small dark-outlined pill) heading a group of senses. */
    private fun posBadge(label: String): TextView {
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        return TextView(activity).apply {
            text = label; setTextColor(Color.BLACK); textSize = 12f; typeface = Typeface.DEFAULT_BOLD
            letterSpacing = 0.04f
            setPadding(dp(10), dp(3), dp(10), dp(4))
            background = GradientDrawable().apply {
                setColor(Color.parseColor("#F0F0F0"))
                setStroke(maxOf(1, dp(1)), Color.parseColor("#C9C9C9"))
                cornerRadius = dp(10).toFloat()
            }
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT,
            ).apply { topMargin = dp(14); bottomMargin = dp(2) }
        }
    }

    /** A numbered sense line: a fixed-width index gutter and the gloss filling the rest. */
    private fun senseRow(index: Int, text: String, topPad: Int): View {
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        return LinearLayout(activity).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(0, maxOf(topPad, dp(7)), 0, 0)
            addView(TextView(activity).apply {
                this.text = "$index."; setTextColor(Color.parseColor("#6B6B6B")); textSize = 15f
                typeface = Typeface.DEFAULT_BOLD
                layoutParams = LinearLayout.LayoutParams(dp(22), ViewGroup.LayoutParams.WRAP_CONTENT)
            })
            addView(TextView(activity).apply {
                this.text = text; setTextColor(Color.BLACK); textSize = 15f
                setLineSpacing(dp(3).toFloat(), 1f)
                layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
            })
        }
    }

    /**
     * Manage user-installed dictionaries (RR12 / ADR-INKREAD-0009 D2) — the KOReader-style "install
     * your own dictionary" surface. Lists StarDict folders found under [Dictionaries.roots] with an
     * Install / Remove action each; install compiles the bundle into the writable corpus via
     * [NativeBridge.nativeDictImport] on the engine thread. Reading the public folders needs
     * all-files access (same gate as export).
     */
    fun showDictionariesDialog() {
        if (!Environment.isExternalStorageManager()) {
            AlertDialog.Builder(activity, R.style.InkDialog)
                .setTitle("Allow file access for dictionaries")
                .setMessage("To find dictionaries you've copied to the device, inkread needs \"All files access\". Grant it on the next screen, then open Dicts again.")
                .setPositiveButton("Open settings") { _, _ ->
                    val uri = Uri.parse("package:${activity.packageName}")
                    runCatching {
                        activity.startActivity(Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, uri))
                    }.onFailure { activity.startActivity(Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION)) }
                }
                .setNegativeButton("Cancel", null)
                .show()
            return
        }
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()
        val home = Dictionaries.homeRoot()
        val list = LinearLayout(activity).apply { orientation = LinearLayout.VERTICAL; setPadding(dp(20), dp(8), dp(20), dp(8)) }

        val dialog = AlertDialog.Builder(activity, R.style.InkDialog)
            .setTitle("Dictionaries")
            .setView(ScrollView(activity).apply { addView(list) })
            .setPositiveButton("Done", null)
            .create()

        fun refresh() {
            list.removeAllViews()
            val bundles = Dictionaries.discover(activity)
            if (bundles.isEmpty()) {
                list.addView(TextView(activity).apply {
                    text = "No dictionaries found.\n\nCopy a StarDict folder (its .ifo, .idx and .dict/.dict.dz files) into:\n${home.absolutePath}\n\nthen reopen this screen."
                    setTextColor(Color.parseColor("#555555")); textSize = 14f; setLineSpacing(dp(3).toFloat(), 1f)
                })
                return
            }
            for (b in bundles) {
                val row = LinearLayout(activity).apply {
                    orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
                    setPadding(0, dp(10), 0, dp(10))
                }
                row.addView(LinearLayout(activity).apply {
                    orientation = LinearLayout.VERTICAL
                    layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
                    addView(TextView(activity).apply {
                        text = b.name; setTextColor(Color.BLACK); textSize = 16f
                    })
                    addView(TextView(activity).apply {
                        text = if (b.installed) "Installed" else "Not installed"
                        setTextColor(Color.parseColor("#9E9E9E")); textSize = 12f
                    })
                })
                row.addView(TextView(activity).apply {
                    text = if (b.installed) "Remove" else "Install"
                    setTextColor(Color.BLACK); textSize = 14f; typeface = Typeface.DEFAULT_BOLD
                    setPadding(dp(14), dp(6), dp(14), dp(6))
                    background = GradientDrawable().apply {
                        setColor(Color.WHITE); setStroke(maxOf(1, dp(1)), Color.BLACK); cornerRadius = dp(16).toFloat()
                    }
                    isClickable = true
                    setOnClickListener {
                        if (b.installed) removeDictionary(b) { refresh() }
                        else installDictionary(b) { refresh() }
                    }
                })
                list.addView(row)
                list.addView(View(activity).apply {
                    setBackgroundColor(Color.parseColor("#EEEEEE"))
                    layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, maxOf(1, dp(1)))
                })
            }
        }
        refresh()
        dialog.show()
    }

    /** Compile a StarDict bundle into the corpus on the engine thread, with a blocking progress note. */
    private fun installDictionary(b: Dictionaries.Bundle, onDone: () -> Unit) {
        val progress = AlertDialog.Builder(activity, R.style.InkDialog)
            .setTitle("Installing ${b.name}")
            .setMessage("Large dictionaries can take a while. Please keep inkread open.")
            .setCancelable(false)
            .create()
        progress.show()
        host.engineExecute {
            val ok = ensureDictOpen()
            val result = if (ok) {
                try {
                    val n = NativeBridge.nativeDictImport(dictHandle, b.dir.absolutePath, b.sourceTag, false)
                    Dictionaries.markInstalled(activity, b.sourceTag)
                    "Installed ${b.name} ($n entries)"
                } catch (e: RuntimeException) {
                    Log.e(TAG, "dict import failed: ${e.message}")
                    "Couldn't install ${b.name}"
                }
            } else {
                "Dictionary store unavailable"
            }
            runOnUiThread {
                progress.dismiss()
                Toast.makeText(activity, result, Toast.LENGTH_SHORT).show()
                onDone()
            }
        }
    }

    /** Drop every entry for a user dictionary's source tag (the inverse of install). */
    private fun removeDictionary(b: Dictionaries.Bundle, onDone: () -> Unit) {
        host.engineExecute {
            if (ensureDictOpen()) {
                try {
                    NativeBridge.nativeDictForget(dictHandle, b.sourceTag)
                } catch (e: RuntimeException) {
                    Log.e(TAG, "dict forget failed: ${e.message}")
                }
            }
            Dictionaries.markRemoved(activity, b.sourceTag)
            runOnUiThread {
                Toast.makeText(activity, "Removed ${b.name}", Toast.LENGTH_SHORT).show()
                onDone()
            }
        }
    }

    private companion object {
        const val TAG = "DictController"

        /** Cap on the online-definition response body (RR12). A real Wiktionary definition is a few
         *  KB; this bounds an untrusted/runaway body without affecting valid lookups. */
        const val MAX_DEFINITION_BYTES = 512 * 1024
    }
}
