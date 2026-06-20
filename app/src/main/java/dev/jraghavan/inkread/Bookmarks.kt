package dev.jraghavan.inkread

import android.util.Log
import org.json.JSONArray
import java.io.File

/**
 * Per-book bookmarks (RR16/RR12): a sorted set of 0-based page indices persisted to a small JSON
 * sidecar so they survive page turns and restarts. A small Kotlin-side sidecar; the canonical
 * model is destined for `reader-core` (ink already lives there). Engine-thread only (RR21).
 */
class Bookmarks(private val file: File) {

    private val pages = sortedSetOf<Int>()

    /** Load the sidecar (if present); a corrupt/missing file yields an empty set, never throws. */
    fun load() {
        pages.clear()
        if (!file.exists()) return
        try {
            val arr = JSONArray(file.readText())
            for (i in 0 until arr.length()) pages.add(arr.getInt(i))
        } catch (e: Exception) {
            Log.w(TAG, "bookmarks load failed (${file.name}): ${e.message}")
            pages.clear()
        }
    }

    /** Bookmarked pages, ascending. */
    fun pages(): List<Int> = pages.toList()

    fun has(page: Int): Boolean = pages.contains(page)

    /** Toggle `page`; returns true if it is now bookmarked. Persists immediately. */
    fun toggle(page: Int): Boolean {
        val added = if (pages.contains(page)) {
            pages.remove(page)
            false
        } else {
            pages.add(page)
            true
        }
        save()
        return added
    }

    private fun save() {
        try {
            val arr = JSONArray()
            for (p in pages) arr.put(p)
            file.parentFile?.mkdirs()
            file.writeText(arr.toString())
        } catch (e: Exception) {
            Log.w(TAG, "bookmarks save failed (${file.name}): ${e.message}")
        }
    }

    private companion object {
        const val TAG = "Bookmarks"
    }
}
