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

    /** A followed source: a display name (byline) + its feed URL. [enabled] sources are the ones a
     *  compile fetches; muting one (unchecking it) keeps it in the list without pulling it. */
    data class Source(val name: String, val url: String, val enabled: Boolean = true)

    /** A compiled issue's headline. [index] is the article's position in the issue (0-based), so a
     *  tap can open the issue at that article. */
    data class Headline(val source: String, val title: String, val index: Int)

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
                // Pre-existing stored sources have no "enabled" key → default to on.
                Source(o.optString("name"), o.optString("url"), o.optBoolean("enabled", true))
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

    /** Mute/unmute a source without removing it (the Sources checklist). */
    fun setSourceEnabled(url: String, enabled: Boolean) =
        save(sources().map { if (it.url == url) it.copy(enabled = enabled) else it })

    /** Persist an edited source list wholesale (enable/disable + removals from the Sources editor). */
    fun setSources(list: List<Source>) = save(list)

    /** The sources a compile actually fetches (muted ones excluded). */
    fun enabledSources(): List<Source> = sources().filter { it.enabled }

    /** Bulk-add sources (the suggested-feeds picker), de-duped against what's already followed. */
    fun addSources(list: List<Source>) {
        val cur = sources()
        save(cur + list.filterNot { s -> cur.any { it.url == s.url } })
    }

    /** A small curated catalog of well-known feeds, so a new user can start with one tap instead of
     *  hunting for feed URLs. Shown as a default-on checklist. */
    fun suggestedSources(): List<Source> = SUGGESTED

    /** Seed the curated feeds on first run so the Daily is ready to compile out of the box (no
     *  "set up" step). Runs once; afterwards the user's edits stand even if they remove them all. */
    fun ensureSeeded() {
        val p = prefs()
        if (p.getBoolean("seeded", false)) return
        if (sources().isEmpty()) save(SUGGESTED)
        p.edit().putBoolean("seeded", true).apply()
    }

    private fun save(list: List<Source>) {
        val arr = JSONArray()
        list.forEach {
            arr.put(JSONObject().put("name", it.name).put("url", it.url).put("enabled", it.enabled))
        }
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

    /** Blocking compile for the background scheduler ([DailyAutoCompileWorker]); call off the UI
     *  thread. Returns whether an issue was produced. */
    fun compileSync(): Boolean = compileBlocking()

    /** Epoch millis of the last successful compile (0 if never). Drives the "Compiled HH:MM" stamp. */
    fun lastCompiledAt(): Long = prefs().getLong("compiledAtMillis", 0L)

    private var lastStatus = ""

    private fun compileBlocking(): Boolean {
        val active = enabledSources()
        if (active.isEmpty()) {
            lastStatus = if (sources().isEmpty()) "Add a source first" else "All sources are muted"
            return false
        }
        val pool = Executors.newFixedThreadPool(MAX_PARALLEL)
        val articles = JSONArray()
        var feedsReached = 0
        var itemsFound = 0
        try {
            for (src in active) {
                val reached = booleanArrayOf(false)
                val items = fetchFeedItems(src.url, reached)
                if (reached[0]) feedsReached++
                itemsFound += items.length()
                val take = minOf(items.length(), PER_SOURCE)
                Log.i(TAG, "feed ${src.url}: ${items.length()} items, fetching $take")
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
        Log.i(TAG, "compile: feedsReached=$feedsReached itemsFound=$itemsFound articles=${articles.length()}")
        // Specific failure messages so the cause is obvious without a logcat.
        if (feedsReached == 0) {
            lastStatus = "Couldn't reach any source (check Wi-Fi)"
            return false
        }
        if (itemsFound == 0) {
            lastStatus = "No feed found at that URL — paste a site's RSS/Atom link"
            return false
        }
        if (articles.length() == 0) {
            lastStatus = "Reached the feed but couldn't fetch any articles"
            return false
        }
        val issueJson = JSONObject()
            .put("title", "inkread daily")
            .put("date", todayDisplay())
            .put("articles", articles)
            .toString()
        val bytes = try {
            NativeBridge.nativeDailyAssemble(issueJson)
        } catch (e: RuntimeException) {
            Log.e(TAG, "assemble failed: ${e.message}")
            lastStatus = "Couldn't assemble the issue"
            return false
        }
        val file = File(dailyDir(), "inkread-daily-${todayKey()}.epub")
        file.writeBytes(bytes)
        storeIssueMeta(articles, file)
        prefs().edit().putLong("compiledAtMillis", System.currentTimeMillis()).apply()
        Log.i(TAG, "compile OK: ${articles.length()} articles → ${file.name} (${bytes.size} bytes)")
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
                Headline(h.optString("source"), h.optString("title"), h.optInt("index", it))
            }
        }.getOrDefault(emptyList())

    /** Whether article [index] of today's issue has been opened. Keyed by date so marks reset daily. */
    fun isRead(index: Int): Boolean =
        prefs().getStringSet("readArticles", emptySet())!!.contains("${todayKey()}#$index")

    /** Mark article [index] of today's issue as read; prunes other days' marks so the set stays small. */
    fun markRead(index: Int) {
        val today = todayKey()
        val next = prefs().getStringSet("readArticles", emptySet())!!
            .filter { it.startsWith("$today#") } // drop stale days
            .toMutableSet()
            .apply { add("$today#$index") }
        prefs().edit().putStringSet("readArticles", next).apply()
    }

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
            headlines.put(
                JSONObject().put("source", a.optString("source"))
                    .put("title", a.optString("title"))
                    .put("index", i), // the article's position in the issue = its chapter order
            )
        }
        prefs().edit().putString(
            "today",
            JSONObject().put("key", todayKey()).put("count", articles.length())
                .put("path", file.absolutePath).put("headlines", headlines).toString(),
        ).apply()
    }

    // ── Feed resolution: a feed URL, or a site URL we auto-discover the feed from ─────────────────

    /**
     * Fetch a source's feed items. Accepts either a real RSS/Atom URL or a **site** URL: if the URL
     * doesn't parse as a feed, look for a `<link rel="alternate" type="…rss/atom…">` in the page, then
     * try common feed paths (`/feed`, `/rss`, …) — so a user can paste a site, not just a feed.
     * `reached[0]` is set true if any URL responded (to distinguish "no network" from "not a feed").
     */
    private fun fetchFeedItems(url: String, reached: BooleanArray): JSONArray {
        val body = fetch(url) ?: return JSONArray()
        reached[0] = true
        parseFeed(body)?.let { if (it.length() > 0) return it }
        // Not a feed — discover one from the page, then fall back to common paths.
        val candidates = buildList {
            discoverFeedUrl(body, url)?.let { add(it) }
            addAll(commonFeedPaths(url))
        }.distinct()
        for (c in candidates) {
            if (c == url) continue
            val b = fetch(c) ?: continue
            parseFeed(b)?.let {
                if (it.length() > 0) {
                    Log.i(TAG, "discovered feed for $url -> $c (${it.length()} items)")
                    return it
                }
            }
        }
        return JSONArray()
    }

    private fun parseFeed(xml: String): JSONArray? =
        runCatching { JSONArray(NativeBridge.nativeDailyParseFeed(xml)) }.getOrNull()

    /** Find a feed URL advertised in a page's `<link rel="alternate" type="…rss/atom+xml" href="…">`. */
    private fun discoverFeedUrl(html: String, base: String): String? {
        val link = Regex("<link\\b[^>]*>", RegexOption.IGNORE_CASE)
        val typeRss = Regex("type=[\"'](application/(rss|atom)\\+xml)[\"']", RegexOption.IGNORE_CASE)
        val href = Regex("href=[\"']([^\"']+)[\"']", RegexOption.IGNORE_CASE)
        for (m in link.findAll(html)) {
            val tag = m.value
            if (typeRss.containsMatchIn(tag)) {
                href.find(tag)?.groupValues?.get(1)?.let { return resolve(base, it) }
            }
        }
        return null
    }

    /** Common feed paths to probe on a site's origin when no `<link>` is advertised. */
    private fun commonFeedPaths(url: String): List<String> =
        runCatching {
            val u = URL(url)
            val origin = "${u.protocol}://${u.host}"
            listOf("/feed", "/rss", "/feed.xml", "/rss.xml", "/index.xml", "/atom.xml", "/feed/")
                .map { origin + it }
        }.getOrDefault(emptyList())

    /** Resolve a possibly-relative href against a base URL. */
    private fun resolve(base: String, href: String): String =
        runCatching { URL(URL(base), href).toString() }.getOrDefault(href)

    // ── Fetch (HTTPS/HTTP, off the UI thread) ─────────────────────────────────────────────────────

    private fun fetch(url: String): String? {
        if (url.isBlank()) return null
        return try {
            val conn = (URL(url).openConnection() as HttpURLConnection).apply {
                connectTimeout = TIMEOUT_MS
                readTimeout = TIMEOUT_MS
                instanceFollowRedirects = true
                setRequestProperty("User-Agent", "Mozilla/5.0 (inkread-daily/0.1)")
                setRequestProperty("Accept", "text/html,application/xhtml+xml,application/xml,application/rss+xml,*/*")
            }
            val code = conn.responseCode
            if (code !in 200..299) {
                Log.e(TAG, "fetch $url -> HTTP $code")
                return null
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

        /** Curated popular feeds for the suggested-sources picker (stable, well-known RSS/Atom). */
        val SUGGESTED = listOf(
            Source("Hacker News", "https://hnrss.org/frontpage"),
            Source("Lobsters", "https://lobste.rs/rss"),
            Source("Ars Technica", "https://feeds.arstechnica.com/arstechnica/index"),
            Source("The Verge", "https://www.theverge.com/rss/index.xml"),
            Source("TechCrunch", "https://techcrunch.com/feed/"),
            Source("BBC News", "https://feeds.bbci.co.uk/news/rss.xml"),
            Source("NPR News", "https://feeds.npr.org/1001/rss.xml"),
            Source("Quanta Magazine", "https://api.quantamagazine.org/feed/"),
            Source("Daring Fireball", "https://daringfireball.net/feeds/main"),
            Source("Smashing Magazine", "https://www.smashingmagazine.com/feed/"),
        )
        const val PER_SOURCE = 5 // articles taken per source
        const val MAX_PARALLEL = 6 // concurrent article fetches
        const val HEADLINES_SHOWN = 60 // headlines stored for the front page (grouped by source)
        const val TIMEOUT_MS = 10_000
        const val MAX_BYTES = 2 * 1024 * 1024 // cap a fetched page at 2 MiB
    }
}
