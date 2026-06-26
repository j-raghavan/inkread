package dev.jraghavan.inkread

import android.content.Context
import android.util.Log
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.concurrent.Callable
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/**
 * inkread-daily orchestration (#66): the Android shell's half of the daily pipeline. Stores the
 * user's feed sources, **fetches** them over HTTPS (the network lives here, by IR-7 / the project
 * decision — the Rust core stays IO-free), then hands the fetched bytes to the core to parse, extract
 * readable text, and assemble a single **issue EPUB** the reader opens. Fetches run in parallel on a
 * small pool. All blocking work is off the UI thread; callers pass a completion callback.
 */
class DailyController(private val context: Context) {

    /** A followed source: a display name (byline) + its feed URL. */
    data class Source(val name: String, val url: String)

    /** A compiled issue's headline (front-page line / TOC entry). */
    data class Headline(val source: String, val title: String)

    /** A past issue on disk. */
    data class BackIssue(val dateLabel: String, val count: Int, val file: File)

    private fun prefs() = context.getSharedPreferences("daily", Context.MODE_PRIVATE)

    private fun dailyDir(): File = File(context.filesDir, "daily").apply { mkdirs() }

    // ── Sources ─────────────────────────────────────────────────────────────────────────────────

    fun sources(): List<Source> =
        runCatching {
            val arr = JSONArray(prefs().getString("sources", "[]"))
            (0 until arr.length()).map {
                val o = arr.getJSONObject(it)
                Source(o.optString("name"), o.optString("url"))
            }
        }.getOrDefault(emptyList())

    /** Add a source from a pasted feed URL; derives a byline from the host. No-op on a blank URL. */
    fun addSource(url: String) {
        val u = url.trim()
        if (u.isEmpty()) return
        val name = runCatching { URL(u).host.removePrefix("www.") }.getOrDefault(u).ifBlank { u }
        val updated = sources().filterNot { it.url == u } + Source(name, u)
        save(updated)
    }

    fun removeSource(url: String) = save(sources().filterNot { it.url == url })

    private fun save(list: List<Source>) {
        val arr = JSONArray()
        list.forEach { arr.put(JSONObject().put("name", it.name).put("url", it.url)) }
        prefs().edit().putString("sources", arr.toString()).apply()
    }

    // ── Compile today's issue ─────────────────────────────────────────────────────────────────────

    /**
     * Fetch every source, build today's issue, and assemble it to an EPUB on disk. Runs entirely off
     * the UI thread; [onDone] is invoked (on a worker thread) with success + a short status message.
     */
    fun compile(onDone: (Boolean, String) -> Unit) {
        Executors.newSingleThreadExecutor().execute {
            try {
                onDone(compileBlocking(), lastStatus)
            } catch (e: Exception) {
                Log.e(TAG, "compile failed", e)
                onDone(false, "Couldn't compile today's issue")
            }
        }
    }

    private var lastStatus = ""

    private fun compileBlocking(): Boolean {
        val sources = sources()
        if (sources.isEmpty()) {
            lastStatus = "Add a source first"
            return false
        }
        val pool = Executors.newFixedThreadPool(MAX_PARALLEL)
        val articles = JSONArray()
        try {
            for (src in sources) {
                val feed = fetch(src.url) ?: continue
                val items = runCatching { JSONArray(NativeBridge.nativeDailyParseFeed(feed)) }
                    .getOrDefault(JSONArray())
                val take = minOf(items.length(), PER_SOURCE)
                // Fetch this source's article pages in parallel.
                val tasks = (0 until take).map { i ->
                    val item = items.getJSONObject(i)
                    Callable {
                        val html = fetch(item.optString("url")) ?: ""
                        if (html.isBlank()) null
                        else JSONObject()
                            .put("title", item.optString("title"))
                            .put("source", src.name)
                            .put("url", item.optString("url"))
                            .put("published", item.optString("published"))
                            .put("html", html)
                    }
                }
                pool.invokeAll(tasks).forEach { f ->
                    runCatching { f.get() }.getOrNull()?.let { articles.put(it) }
                }
            }
        } finally {
            pool.shutdown()
            pool.awaitTermination(2, TimeUnit.SECONDS)
        }
        if (articles.length() == 0) {
            lastStatus = "No articles could be fetched"
            return false
        }
        val issueJson = JSONObject()
            .put("title", "inkread daily")
            .put("date", todayDisplay())
            .put("articles", articles)
            .toString()
        val bytes = NativeBridge.nativeDailyAssemble(issueJson)
        val file = File(dailyDir(), "inkread-daily-${todayKey()}.epub")
        file.writeBytes(bytes)
        storeIssueMeta(articles, file)
        lastStatus = "Compiled ${articles.length()} articles"
        return true
    }

    // ── Today's issue + archive ───────────────────────────────────────────────────────────────────

    /** Today's compiled issue EPUB, or null if none was compiled today. */
    fun todayIssue(): File? {
        val f = File(dailyDir(), "inkread-daily-${todayKey()}.epub")
        return if (f.exists()) f else null
    }

    /** Today's headlines (for the front page / TOC), from the stored issue meta. */
    fun todayHeadlines(): List<Headline> =
        runCatching {
            val o = JSONObject(prefs().getString("today", "{}"))
            if (o.optString("key") != todayKey()) return emptyList()
            val arr = o.optJSONArray("headlines") ?: JSONArray()
            (0 until arr.length()).map {
                val h = arr.getJSONObject(it)
                Headline(h.optString("source"), h.optString("title"))
            }
        }.getOrDefault(emptyList())

    /** Past issues (excluding today), most-recent first. */
    fun backIssues(): List<BackIssue> {
        val today = todayKey()
        return dailyDir().listFiles { f -> f.isFile && f.name.endsWith(".epub") }
            ?.filterNot { it.name.contains(today) }
            ?.sortedByDescending { it.name }
            ?.map { BackIssue(dateLabelFromName(it.name), 0, it) }
            ?: emptyList()
    }

    private fun storeIssueMeta(articles: JSONArray, file: File) {
        val headlines = JSONArray()
        for (i in 0 until minOf(articles.length(), HEADLINES_SHOWN)) {
            val a = articles.getJSONObject(i)
            headlines.put(JSONObject().put("source", a.optString("source")).put("title", a.optString("title")))
        }
        prefs().edit().putString(
            "today",
            JSONObject().put("key", todayKey()).put("count", articles.length())
                .put("path", file.absolutePath).put("headlines", headlines).toString(),
        ).apply()
    }

    // ── Fetch (HTTPS/HTTP, off the UI thread) ─────────────────────────────────────────────────────

    private fun fetch(url: String): String? {
        if (url.isBlank()) return null
        return try {
            val conn = (URL(url).openConnection() as HttpURLConnection).apply {
                connectTimeout = TIMEOUT_MS
                readTimeout = TIMEOUT_MS
                instanceFollowRedirects = true
                setRequestProperty("User-Agent", "inkread-daily/0.1")
                setRequestProperty("Accept", "text/html,application/xhtml+xml,application/xml,application/rss+xml")
            }
            conn.inputStream.use { input ->
                val bytes = input.readBytes(MAX_BYTES)
                String(bytes, Charsets.UTF_8)
            }
        } catch (e: Exception) {
            Log.e(TAG, "fetch failed: $url — ${e.message}")
            null
        }
    }

    /** Read up to [cap] bytes, then stop (a runaway page never exhausts memory). */
    private fun java.io.InputStream.readBytes(cap: Int): ByteArray {
        val out = java.io.ByteArrayOutputStream()
        val buf = ByteArray(16 * 1024)
        var total = 0
        while (total < cap) {
            val n = read(buf)
            if (n < 0) break
            out.write(buf, 0, n)
            total += n
        }
        return out.toByteArray()
    }

    private fun todayKey(): String = SimpleDateFormat("yyyy-MM-dd", Locale.US).format(Date())
    private fun todayDisplay(): String = SimpleDateFormat("EEEE, MMMM d, yyyy", Locale.getDefault()).format(Date())
    private fun dateLabelFromName(name: String): String =
        name.removePrefix("inkread-daily-").removeSuffix(".epub")

    private companion object {
        const val TAG = "DailyController"
        const val PER_SOURCE = 6 // articles taken per source
        const val MAX_PARALLEL = 4 // concurrent article fetches
        const val HEADLINES_SHOWN = 8 // headlines stored for the front page
        const val TIMEOUT_MS = 10_000
        const val MAX_BYTES = 2 * 1024 * 1024 // cap a fetched page at 2 MiB
    }
}
