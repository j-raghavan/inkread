package dev.jraghavan.inkread

import android.util.Log
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/**
 * Per-book handwriting store (RR19). Holds the user's pen strokes per page and persists them to a
 * JSON sidecar so they survive page turns and app restarts. Each stroke is a [FloatArray] of
 * interleaved **normalized** coordinates `[x0,y0,x1,y1,…]` in `[0,1]` page space, so they re-align
 * regardless of the on-device render size.
 *
 * Interim home (RR19): the canonical ink model + persistence is destined for `reader-core` (Rust,
 * host-testable) per the handwriting design; this Kotlin sidecar is the first cut that gets ink
 * saving on-device. Migrate the data+persistence behind the same capture/draw path later.
 *
 * Engine-thread only (mirrors the document handle's threading, RR21).
 */
class InkStore(private val file: File) {

    private val pages = HashMap<Int, MutableList<FloatArray>>()

    /** Load the sidecar (if present). A corrupt/missing file yields an empty store, never throws. */
    fun load() {
        pages.clear()
        if (!file.exists()) return
        try {
            val root = JSONObject(file.readText())
            for (key in root.keys()) {
                val page = key.toIntOrNull() ?: continue
                val arr = root.getJSONArray(key)
                val list = ArrayList<FloatArray>(arr.length())
                for (i in 0 until arr.length()) {
                    val s = arr.getJSONArray(i)
                    list.add(FloatArray(s.length()) { s.getDouble(it).toFloat() })
                }
                pages[page] = list
            }
            Log.i(TAG, "loaded ink for ${pages.size} page(s) from ${file.name}")
        } catch (e: Exception) {
            Log.w(TAG, "ink load failed (${file.name}): ${e.message}")
            pages.clear()
        }
    }

    fun strokesFor(page: Int): List<FloatArray> = pages[page] ?: emptyList()

    /** Append a normalized stroke to [page] and persist. */
    fun addStroke(page: Int, norm: FloatArray) {
        if (norm.size < 2) return
        pages.getOrPut(page) { ArrayList() }.add(norm)
        save()
    }

    private fun save() {
        try {
            val root = JSONObject()
            for ((page, list) in pages) {
                val arr = JSONArray()
                for (s in list) {
                    val js = JSONArray()
                    for (v in s) js.put(v.toDouble())
                    arr.put(js)
                }
                root.put(page.toString(), arr)
            }
            file.parentFile?.mkdirs()
            file.writeText(root.toString())
        } catch (e: Exception) {
            Log.w(TAG, "ink save failed (${file.name}): ${e.message}")
        }
    }

    private companion object {
        const val TAG = "InkStore"
    }
}
