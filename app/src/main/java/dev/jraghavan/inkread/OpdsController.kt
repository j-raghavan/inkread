package dev.jraghavan.inkread

import android.content.Context
import android.util.Log
import org.json.JSONObject
import java.io.File
import java.net.URL
import java.net.URLEncoder

/**
 * The OPDS library client's shell half (ADR-INKREAD-0016, #175): fetch a catalog document, hand it
 * to the core to classify, resolve the relative hrefs it returns, and download a chosen book into
 * the shelf.
 *
 * The split follows the daily companion (#66) and the self-updater (ADR-INKREAD-0014): the core is
 * pure and knows nothing about the server, while everything here is I/O and URLs. Reaching a
 * catalog server means [fetchCatalog] and [download] block — call them off the UI thread.
 *
 * The URL work lives in [Companion] as pure functions because that is where this class can actually
 * go wrong: a mis-resolved relative href walks off the server, and a mis-encoded search term
 * silently returns nothing. Those are host-tested; the catalog semantics are tested in Rust.
 */
class OpdsController(private val context: Context) {

    /** A book format offered by the catalog. [ext] is empty when inkread cannot open it. */
    data class Format(val mime: String, val href: String, val bytes: Long, val ext: String) {
        val openable: Boolean get() = ext.isNotEmpty()
    }

    /** One catalog row: either a link to another feed, or a book to download. */
    data class Entry(
        val navigation: Boolean,
        val title: String,
        val author: String,
        val summary: String,
        val published: String,
        /** Absolute URL — of the next feed for a navigation entry, empty for a book. */
        val href: String,
        val cover: String,
        val formats: List<Format>,
    ) {
        /** The format to download by default: the first the reader can open, or null if none can. */
        val best: Format? get() = formats.firstOrNull { it.openable }
    }

    /** One fetched feed, with its hrefs already resolved to absolute URLs. */
    data class Catalog(
        /** The document really was a feed — see [Fetch.NotACatalog]. */
        val isCatalog: Boolean,
        val title: String,
        val entries: List<Entry>,
        val next: String,
        val prev: String,
        val start: String,
        val searchTemplate: String,
    )

    /**
     * What came back from asking the server for a feed.
     *
     * Failure is modelled rather than collapsed to `null` because the reader has to be told which
     * thing to go and fix. "Could not reach the library" is the wrong sentence for a wrong password,
     * and "your library is empty" is the wrong sentence for an address that is not a catalog — each
     * sends someone to correct something that was never broken.
     */
    sealed interface Fetch {
        data class Ok(val catalog: Catalog) : Fetch

        /** The server answered 401/403: credentials are missing or wrong. */
        data object Unauthorized : Fetch

        /** Nothing answered, or it answered with an error status. */
        data class Unreachable(val status: Int) : Fetch

        /** Something answered, but it was not an OPDS feed — usually a web UI, not a catalog. */
        data object NotACatalog : Fetch
    }

    /** Fetch [url] and classify it. Never throws; every failure is one of the [Fetch] cases. */
    fun fetchCatalog(url: String): Fetch {
        if (!isHttpUrl(url)) return Fetch.Unreachable(0)
        val response =
            HttpFetch.getTextWithStatus(url, USER_AGENT, ACCEPT, TIMEOUT_MS, MAX_CATALOG_BYTES, auth())
        val xml = response.body ?: return failureFor(response.status)
        val json = runCatching { NativeBridge.nativeOpdsParseCatalog(xml) }.getOrElse {
            Log.e(TAG, "catalog parse failed: ${it.message}")
            return Fetch.NotACatalog
        }
        val catalog = runCatching { parseCatalog(json, url) }.getOrElse {
            Log.e(TAG, "catalog decode failed: ${it.message}")
            return Fetch.NotACatalog
        }
        return if (catalog.isCatalog) Fetch.Ok(catalog) else Fetch.NotACatalog
    }

    /**
     * Download [format] of [entry] into the shelf, returning the stored file (or null on failure).
     * The catalog's own title names the file, because an acquisition URL generally carries no
     * usable name of its own; the format's extension is what the core dispatches a backend on.
     *
     * Downloads land in a temp file and are renamed into place only once complete. Writing straight
     * to the destination would truncate it on open, so a re-download that then failed — a dropped
     * Wi-Fi mid-transfer is the ordinary case here — would destroy the perfectly good copy already
     * on the shelf, with annotations attached to it. A failed download must cost nothing.
     */
    fun download(entry: Entry, format: Format): File? {
        if (!format.openable || !isHttpUrl(format.href)) return null
        val dest = Books.destinationFor(context, entry.title.ifBlank { "document" }, format.ext)
        val partial = File(dest.parentFile, "${dest.name}.part")
        val ok = HttpFetch.download(format.href, partial, USER_AGENT, TIMEOUT_MS, MAX_BOOK_BYTES, auth())
        if (!ok || !partial.renameTo(dest)) {
            partial.delete() // never leave a partial book on the shelf pretending to be readable
            return null
        }
        return dest
    }

    /** The configured server's Basic credential, or null when the server needs no login. */
    private fun auth(): String? =
        HttpFetch.basicAuth(AppSettings.opdsUser(context), AppSettings.opdsPassword(context))

    /** Turn the core's catalog JSON into the model, resolving every href against [base]. */
    private fun parseCatalog(json: String, base: String): Catalog {
        val root = JSONObject(json)
        val entriesJson = root.optJSONArray("entries")
        val entries = buildList {
            for (i in 0 until (entriesJson?.length() ?: 0)) {
                val e = entriesJson!!.getJSONObject(i)
                val formatsJson = e.optJSONArray("formats")
                val formats = buildList {
                    for (j in 0 until (formatsJson?.length() ?: 0)) {
                        val f = formatsJson!!.getJSONObject(j)
                        val href = resolve(base, f.optString("href"))
                        if (href.isNotEmpty()) {
                            add(
                                Format(
                                    mime = f.optString("mime"),
                                    href = href,
                                    bytes = f.optLong("bytes", 0L),
                                    ext = f.optString("ext"),
                                ),
                            )
                        }
                    }
                }
                add(
                    Entry(
                        navigation = e.optString("kind") == "navigation",
                        title = e.optString("title"),
                        author = e.optString("author"),
                        summary = e.optString("summary"),
                        published = e.optString("published"),
                        href = resolve(base, e.optString("href")),
                        cover = resolve(base, e.optString("cover")),
                        formats = formats,
                    ),
                )
            }
        }
        return Catalog(
            isCatalog = root.optBoolean("isCatalog", false),
            title = root.optString("title"),
            entries = entries,
            next = resolve(base, root.optString("next")),
            prev = resolve(base, root.optString("prev")),
            start = resolve(base, root.optString("start")),
            // Left as a template (its {searchTerms} placeholder intact) — resolved, not substituted.
            searchTemplate = resolve(base, root.optString("searchTemplate")),
        )
    }

    companion object {
        private const val TAG = "Opds"

        /** Named so a server's logs show who called; catalogs sometimes vary by client. */
        const val USER_AGENT = "inkread"

        /** OPDS catalogs are Atom; ask for it explicitly so a server does not hand us its HTML UI. */
        private const val ACCEPT = "application/atom+xml, application/xml;q=0.9, */*;q=0.1"

        private const val TIMEOUT_MS = 15_000

        /** A catalog page is text; a server sending far more than this is not sending a page. */
        private const val MAX_CATALOG_BYTES = 4 * 1024 * 1024

        /** Matches the shelf's own import cap (the core refuses larger documents at open anyway). */
        private const val MAX_BOOK_BYTES = 2L shl 30

        /**
         * The failure a non-success HTTP [status] represents.
         *
         * 401 and 403 are the reader's credentials, everything else is the address or the network —
         * a distinction worth getting right, because each sends someone to fix a different thing.
         * Pure so the rule is tested rather than inferred from a `when` buried in an IO path.
         */
        fun failureFor(status: Int): Fetch = when (status) {
            401, 403 -> Fetch.Unauthorized
            else -> Fetch.Unreachable(status)
        }

        /**
         * Resolve [href] against [base] into an absolute URL, or "" when it is absent or does not
         * survive the check.
         *
         * The scheme test is the point: a catalog is a remote party naming the next thing we fetch,
         * and `file:`, `jar:` or `content:` in that position would turn a browse into a local read.
         * Resolution itself is delegated to [URL], which already implements RFC 3986 — hand-rolling
         * that would be new, subtle, security-relevant code for nothing.
         */
        fun resolve(base: String, href: String): String {
            if (href.isBlank()) return ""
            return runCatching {
                val resolved = URL(URL(base), href).toString()
                if (isHttpUrl(resolved)) resolved else ""
            }.getOrDefault("")
        }

        /** Only `http`/`https` are ever fetched — see [resolve]. */
        fun isHttpUrl(url: String): Boolean {
            val lower = url.trim().lowercase()
            return lower.startsWith("http://") || lower.startsWith("https://")
        }

        /**
         * Substitute [terms] into an OpenSearch [template]'s `{searchTerms}` placeholder.
         *
         * `+` is *not* a space outside a query string, and calibre's template puts the terms in a
         * path segment (`/opds/search/{searchTerms}`), so the `+` that [URLEncoder] emits would be
         * searched for literally. Percent-encoding instead is correct in both positions.
         */
        fun searchUrl(template: String, terms: String): String {
            if (template.isBlank() || terms.isBlank()) return ""
            val encoded = URLEncoder.encode(terms, "UTF-8").replace("+", "%20")
            return template.replace("{searchTerms}", encoded)
        }

        /** The user's server address, normalized to the catalog root the browse screen starts at. */
        fun catalogRoot(serverUrl: String): String {
            val trimmed = serverUrl.trim().trimEnd('/')
            if (trimmed.isEmpty()) return ""
            val withScheme = if (isHttpUrl(trimmed)) trimmed else "http://$trimmed"
            // A bare host is the common way people write it down; both calibre and Calibre-Web
            // serve the catalog at /opds, so complete it rather than making the user know that.
            return if (URL(withScheme).path.trimEnd('/').isEmpty()) "$withScheme/opds" else withScheme
        }
    }
}
