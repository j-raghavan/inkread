package dev.jraghavan.inkread

import android.content.Context
import android.util.Log
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.net.URL
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.concurrent.Callable
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread

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
    /**
     * A followed feed. [limit] is how many of its articles an issue takes (#193) — sources differ a
     * lot in volume, and one cap for all of them either starves a good feed or floods the issue with
     * a noisy one.
     */
    data class Source(
        val name: String,
        val url: String,
        val enabled: Boolean = true,
        val limit: Int = PER_SOURCE,
    )

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
                // Pre-existing stored sources have neither "enabled" nor "limit" → on, and the
                // cap every source used before this was configurable.
                Source(
                    o.optString("name"),
                    o.optString("url"),
                    o.optBoolean("enabled", true),
                    clampLimit(o.optInt("limit", PER_SOURCE)),
                )
            }
        }.getOrDefault(emptyList())

    /** Add a source from a pasted feed URL; derives a byline from the host. Returns the stored URL,
     *  or null on a blank one. */
    fun addSource(url: String): String? {
        val u = url.trim()
        if (u.isEmpty()) return null
        update { cur -> cur.filterNot { it.url == u } + Source(bylineFor(u), u) }
        return u
    }

    /**
     * Name a just-added source from its feed's own title (#268), off the UI thread, so the reader
     * does not see the host until the next compile. [onNamed] runs on a worker thread, and only if
     * the name changed.
     */
    fun nameFromFeed(url: String, onNamed: () -> Unit) {
        thread(name = "daily-name") {
            try {
                val title = fetchFeed(url, booleanArrayOf(false)).title
                if (title != null && adoptFeedTitles(mapOf(url to title))) onNamed()
            } catch (e: Exception) {
                Log.w(TAG, "naming $url from its feed failed", e)
            }
        }
    }

    /**
     * Persist the Sources editor's list wholesale (order, edits, mutes, removals). [opened] is the
     * list the editor started from: a compile may have named a source from its feed while the
     * editor was open, and writing back the editor's stale host name would undo that
     * ([keepAdoptedNames]).
     */
    fun setSources(list: List<Source>, opened: List<Source>) =
        update { stored -> keepAdoptedNames(list, opened, stored) }

    /** The sources a compile actually fetches (muted ones excluded). */
    fun enabledSources(): List<Source> = sources().filter { it.enabled }

    /** Bulk-add sources (the suggested-feeds picker), de-duped against what's already followed. */
    fun addSources(list: List<Source>) =
        update { cur -> cur + list.filterNot { s -> cur.any { it.url == s.url } } }

    /** A small curated catalog of well-known feeds, so a new user can start with one tap instead of
     *  hunting for feed URLs. Shown as a default-on checklist. */
    fun suggestedSources(): List<Source> = SUGGESTED

    /** Seed the curated feeds on first run so the Daily is ready to compile out of the box (no
     *  "set up" step). Runs once; afterwards the user's edits stand even if they remove them all. */
    fun ensureSeeded() {
        val p = prefs()
        if (p.getBoolean("seeded", false)) return
        update { cur -> cur.ifEmpty { SUGGESTED } }
        p.edit().putBoolean("seeded", true).apply()
    }

    /** Read-modify-write the stored source list under [SOURCES_LOCK], so a background compile
     *  naming sources and the reader editing them cannot drop each other's change. */
    private fun update(f: (List<Source>) -> List<Source>): Boolean = synchronized(SOURCES_LOCK) {
        val cur = sources()
        val next = f(cur)
        if (next != cur) save(next)
        next != cur
    }

    private fun save(list: List<Source>) {
        val arr = JSONArray()
        list.forEach {
            arr.put(
                JSONObject()
                    .put("name", it.name)
                    .put("url", it.url)
                    .put("enabled", it.enabled)
                    .put("limit", clampLimit(it.limit)),
            )
        }
        prefs().edit().putString("sources", arr.toString()).apply()
    }

    // ── Compile today's issue ─────────────────────────────────────────────────────────────────────

    /**
     * Fetch every source, build today's issue, and assemble it to an EPUB on disk. Runs entirely off
     * the UI thread; [onDone] is invoked (on a worker thread) with success + a short status message.
     */
    fun compile(onDone: (Boolean, String) -> Unit) {
        // A plain thread rather than an executor that was never shut down — one idle thread leaked
        // per compile.
        thread(name = "daily-compile") {
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
        // Collected per source first, then interleaved below (#107): appending source-by-source put
        // late-list sources' articles at the tail, and the front page stores only the first
        // HEADLINES_SHOWN headlines — so a newly added feed could compile into the issue yet never
        // appear on the front page. Round-robin gives every source front-of-issue presence.
        val perSource = mutableListOf<List<JSONObject>>()
        // The bylines in source order, for grouping the issue's contents (#269) — collected as each
        // source is read so they match the `source` its articles carry exactly.
        val sourceOrder = JSONArray()
        try {
            for (src in active) {
                val reached = booleanArrayOf(false)
                val feed = fetchFeed(src.url, reached)
                val items = feed.items
                // Name a source still called after its host from its feed (#268) before building its
                // articles, so the very compile that learns the name already prints it.
                feed.title?.let { adoptFeedTitles(mapOf(src.url to it)) }
                val name = sources().find { it.url == src.url }?.name ?: src.name
                sourceOrder.put(name)
                if (reached[0]) feedsReached++
                itemsFound += items.length()
                val take = minOf(items.length(), clampLimit(src.limit))
                Log.i(TAG, "feed ${src.url}: ${items.length()} items, fetching $take")
                // Fetch this source's article pages in parallel.
                val tasks = (0 until take).map { i ->
                    val item = items.getJSONObject(i)
                    Callable {
                        val html = fetch(item.optString("url")) ?: ""
                        if (html.isBlank()) null
                        else JSONObject()
                            .put("title", item.optString("title"))
                            .put("source", name)
                            .put("url", item.optString("url"))
                            .put("published", item.optString("published"))
                            // The feed's own description, for the contents page (#198). Absent on
                            // feeds that give none; the core falls back to a body excerpt.
                            .put("summary", item.optString("summary"))
                            .put("html", html)
                    }
                }
                perSource.add(pool.invokeAll(tasks).mapNotNull { f -> runCatching { f.get() }.getOrNull() })
            }
        } finally {
            pool.shutdown()
            pool.awaitTermination(2, TimeUnit.SECONDS)
        }
        // Interleave: every source's 1st article, then every source's 2nd, … Source order still
        // decides ties, so the issue keeps a stable, predictable reading order.
        interleaveByRank(perSource).forEach { articles.put(it) }
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
            .put("sources", sourceOrder)
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
    private fun fetchFeed(url: String, reached: BooleanArray): Feed {
        val body = fetch(url) ?: return noFeed()
        reached[0] = true
        parseFeed(body)?.let { if (it.items.length() > 0) return it }
        // Not a feed — discover one from the page, then fall back to common paths.
        val candidates = buildList {
            discoverFeedUrl(body, url)?.let { add(it) }
            addAll(commonFeedPaths(url))
        }.distinct()
        for (c in candidates) {
            if (c == url) continue
            val b = fetch(c) ?: continue
            parseFeed(b)?.let {
                if (it.items.length() > 0) {
                    Log.i(TAG, "discovered feed for $url -> $c (${it.items.length()} items)")
                    return it
                }
            }
        }
        return noFeed()
    }

    /** A parsed feed: its own title (null when it gives none) and its entries. */
    private class Feed(val title: String?, val items: JSONArray)

    /** A fresh empty [Feed] each time: `JSONArray` is mutable, so a shared instance is a hazard. */
    private fun noFeed() = Feed(null, JSONArray())

    private fun parseFeed(xml: String): Feed? =
        runCatching {
            val o = JSONObject(NativeBridge.nativeDailyParseFeed(xml))
            Feed(
                // optString would turn JSON null into the string "null" — a source named "null".
                if (o.isNull("title")) null else o.getString("title"),
                o.optJSONArray("items") ?: JSONArray(),
            )
        }.getOrNull()

    /**
     * Rename every source still named after its host to the title its feed gave this compile (#268),
     * so a feed added as `rss.elpais.com` reads "EL PAÍS" from then on — including feeds added before
     * feeds could name themselves. Names the reader typed and curated names are left alone ([named]).
     *
     * Applied to the stored list, not the one the compile started from. Returns whether a name
     * changed.
     */
    private fun adoptFeedTitles(titles: Map<String, String>): Boolean =
        titles.isNotEmpty() && update { cur -> adoptTitles(cur, titles) }

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

    private fun fetch(url: String): String? =
        HttpFetch.getText(url, FETCH_USER_AGENT, FETCH_ACCEPT, TIMEOUT_MS, MAX_BYTES)

    private fun todayKey(): String = SimpleDateFormat("yyyy-MM-dd", Locale.US).format(Date())
    private fun todayDisplay(): String = SimpleDateFormat("EEEE, MMMM d, yyyy", Locale.getDefault()).format(Date())
    private fun dateLabelFromName(name: String): String =
        name.removePrefix("inkread-daily-").removeSuffix(".epub")

    /** Internal rather than private so the pure limit/ordering logic is host-testable (#193). */
    internal companion object {
        const val TAG = "DailyController"

        /** Curated popular feeds for the suggested-sources picker (stable, well-known RSS/Atom). */
        /**
         * Point [source] at [newUrl], keeping everything else about it (#166).
         *
         * The byline follows the URL only when it was derived from one. A source added by pasting a URL
         * is bylined with its host, so moving it to another host should move the byline too; a curated
         * source is bylined "BBC News", and re-deriving would rename it to `feeds.bbci.co.uk` for the
         * sake of a URL correction. Comparing the stored name against what [bylineFor] would have
         * produced for the *old* URL is what tells the two apart.
         *
         * A blank [newUrl] leaves the source alone: an emptied field is a slip, not an instruction.
         */
        fun withUrl(source: Source, newUrl: String): Source {
            val u = newUrl.trim()
            if (u.isEmpty() || u == source.url) return source
            return source.copy(name = if (isHostNamed(source)) bylineFor(u) else source.name, url = u)
        }

        /**
         * The byline shown for a feed added by URL: its host, minus `www.`. Falls back to the whole
         * string when it does not parse as a URL, so a mistyped entry still shows the reader something
         * they recognise rather than a blank row.
         */
        fun bylineFor(url: String): String =
            runCatching { URL(url).host.removePrefix("www.") }.getOrDefault(url).ifBlank { url }

        /** Whether [source] still carries the default byline derived from its URL — the one name
         *  the app may replace on the reader's behalf ([withUrl], [named]). */
        fun isHostNamed(source: Source): Boolean = source.name == bylineFor(source.url)

        /**
         * [source] renamed to its feed's own [feedTitle] (#268) — but only while its name is still the
         * host-derived default. A curated byline ("BBC News") and a name the reader typed are theirs to
         * keep; a feed must not overwrite either on every compile. A blank or missing title changes
         * nothing.
         */
        fun named(source: Source, feedTitle: String?): Source {
            val t = feedTitle?.trim().orEmpty()
            if (t.isEmpty() || !isHostNamed(source)) return source
            return source.copy(name = t)
        }

        /**
         * [source] after the Sources editor's Edit (#166, #268): re-pointed at [newUrl] via [withUrl],
         * then named [newName] if the reader changed it. A name left as it was lets [withUrl] move a
         * host-derived byline with the URL; a blank name is a slip and keeps the current one.
         */
        fun edited(source: Source, newName: String, newUrl: String): Source {
            val moved = withUrl(source, newUrl)
            val n = newName.trim()
            return if (n.isEmpty() || n == source.name) moved else moved.copy(name = n)
        }

        /**
         * [sources] with each host-named source renamed to its feed's title in [titles] (by URL) via
         * [named] — unless another source already goes by that name. The front page groups headlines
         * by name, so two feeds sharing one would merge into a single section.
         */
        fun adoptTitles(sources: List<Source>, titles: Map<String, String>): List<Source> {
            val out = sources.toMutableList()
            out.indices.forEach { i ->
                val title = titles[out[i].url] ?: return@forEach
                val renamed = named(out[i], title)
                val taken = out.withIndex().any { (j, o) -> j != i && o.name.equals(renamed.name, ignoreCase = true) }
                if (!taken) out[i] = renamed
            }
            return out
        }

        /**
         * The Sources editor's [edited] list, keeping a name a compile adopted while the editor was
         * open (#268): a row host-named both when the editor opened and on Save, but named otherwise
         * in [stored], takes the stored name.
         */
        fun keepAdoptedNames(edited: List<Source>, opened: List<Source>, stored: List<Source>): List<Source> =
            edited.map { e ->
                val was = opened.find { it.url == e.url }
                val now = stored.find { it.url == e.url }
                if (isHostNamed(e) && was != null && isHostNamed(was) && now != null && !isHostNamed(now)) {
                    e.copy(name = now.name)
                } else {
                    e
                }
            }

        /** Serialises read-modify-write of the stored source list ([update]) across controller
         *  instances — the activity's and the background compile's. */
        private val SOURCES_LOCK = Any()

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
        const val PER_SOURCE = 5 // default articles taken per source; per-source override in #193
        const val MIN_PER_SOURCE = 1 // a source taking nothing should be muted, not set to zero
        const val MAX_PER_SOURCE = 20 // a ceiling, so one busy feed cannot swamp an issue

        /**
         * Hold a per-source article limit inside [MIN_PER_SOURCE]..[MAX_PER_SOURCE].
         *
         * Applied on read as well as write: a limit that arrives out of range — hand-edited prefs,
         * or a value stored by a build with a different ceiling — would otherwise decide how many
         * articles get fetched, and zero or negative would silently drop a source the reader still
         * sees listed as active.
         */
        fun clampLimit(n: Int): Int = n.coerceIn(MIN_PER_SOURCE, MAX_PER_SOURCE)

        /**
         * The front page's source sections ([sections], as first seen in the issue) put in the
         * reader's source order ([order], source names) (#267). First-seen order is not enough: the
         * issue round-robins sources and drops articles that failed to fetch, so a source whose first
         * article failed would sink below sources the reader put after it — and a reorder would not
         * show until the next compile. A section matching no current source (renamed since the issue
         * was compiled) keeps its first-seen place after the known ones.
         */
        fun inSourceOrder(sections: List<String>, order: List<String>): List<String> =
            sections.sortedBy { order.indexOf(it).let { i -> if (i < 0) Int.MAX_VALUE else i } }

        /**
         * How many of the front page's sections (by [weights], in order) go in the left column: the
         * split that brings the two columns closest to even, so the sections stay in order down them.
         * Always at least one section on the left when there is any.
         */
        fun columnSplit(weights: List<Int>): Int {
            if (weights.isEmpty()) return 0
            val total = weights.sum()
            var left = 0
            var best = 1
            var bestGap = Int.MAX_VALUE
            for (k in 1..weights.size) {
                left += weights[k - 1]
                val gap = kotlin.math.abs(total - 2 * left)
                if (gap <= bestGap) { best = k; bestGap = gap } // a tie goes to the longer left column
            }
            return best
        }

        /**
         * [list] with [item] moved [by] places (negative = towards the front), for reordering
         * sources (#267). Places are counted among the elements that are not [hidden] — a removed
         * row is still in the staged order but no longer on screen, and one tap must move a row past
         * the neighbour the reader can see. Hidden elements go to the end, where Save drops them.
         * Clamped at the ends, so ▲ on the first row or ▼ on the last changes nothing; an [item] not
         * in the visible list leaves it unchanged.
         */
        fun <T> moved(list: List<T>, item: T, by: Int, hidden: (T) -> Boolean = { false }): List<T> {
            val visible = list.filterNot(hidden).toMutableList()
            val from = visible.indexOf(item)
            if (from < 0) return list
            visible.add((from + by).coerceIn(0, visible.lastIndex), visible.removeAt(from))
            return visible + list.filter(hidden)
        }

        /**
         * Round-robin the sources: every source's 1st article, then every source's 2nd, and so on.
         * Source order decides ties, so the reading order stays stable and predictable.
         *
         * This runs to the longest list rather than to a fixed count (#193). With per-source limits
         * the lists are different lengths, and a fixed bound would silently drop everything a
         * source contributed past it — a feed set to 10 would deliver 5. Sources that run out drop
         * away and the rest keep interleaving, so a high-limit feed tails the issue rather than
         * being truncated.
         */
        fun <T> interleaveByRank(perSource: List<List<T>>): List<T> {
            val deepest = perSource.maxOfOrNull { it.size } ?: 0
            val out = ArrayList<T>(perSource.sumOf { it.size })
            for (rank in 0 until deepest) {
                for (list in perSource) {
                    list.getOrNull(rank)?.let { out.add(it) }
                }
            }
            return out
        }
        const val MAX_PARALLEL = 6 // concurrent article fetches
        const val HEADLINES_SHOWN = 60 // headlines stored for the front page (grouped by source)
        const val TIMEOUT_MS = 10_000
        const val MAX_BYTES = 2 * 1024 * 1024 // cap a fetched page at 2 MiB
        const val FETCH_USER_AGENT = "Mozilla/5.0 (inkread-daily/0.1)"
        const val FETCH_ACCEPT = "text/html,application/xhtml+xml,application/xml,application/rss+xml,*/*"
    }
}
