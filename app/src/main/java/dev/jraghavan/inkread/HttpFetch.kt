package dev.jraghavan.inkread

import android.util.Base64
import android.util.Log
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.URL

/**
 * The shell's one bounded HTTP GET, shared by the network features (the daily fetch #66, the
 * self-updater ADR-INKREAD-0014, and the OPDS library ADR-INKREAD-0016 — the only one that
 * authenticates). The Rust core stays IO-free (IR-7) — every HTTP byte enters here.
 * Both entry points cap the response so a runaway/hostile body can never exhaust memory or disk, and
 * both swallow failures into a `null`/`false` so callers degrade silently (offline ⇒ no-op).
 *
 * Being the single choke point is also where the **cleartext policy** lives — see
 * [isCleartextAllowed].
 */
object HttpFetch {

    /**
     * Build an HTTP **Basic** `Authorization` value, or `null` when there is no username to send.
     *
     * Basic only: Calibre-Web authenticates OPDS this way, and calibre's content server does when
     * started with `--auth-mode=basic` (its `auto` default resolves to Digest without SSL, which
     * Android's OkHttp-backed `HttpURLConnection` cannot answer) — see ADR-INKREAD-0016 Decision 4.
     */
    fun basicAuth(user: String, password: String): String? {
        if (user.isBlank()) return null
        val raw = "$user:$password".toByteArray(Charsets.UTF_8)
        return "Basic " + Base64.encodeToString(raw, Base64.NO_WRAP)
    }

    /**
     * Whether a plain-`http://` request to [url] is allowed: **only when the host is on the local
     * network.**
     *
     * The manifest carries `usesCleartextTraffic="true"` and has to. A calibre content server or a
     * Calibre-Web instance on the LAN is `http://192.168.1.20:8080`, self-signed TLS on a home
     * server is worse than none, and [OpdsController] defaults a bare host the reader typed to
     * `http://` for exactly that reason. But the flag is application-wide, and the platform's
     * network-security config cannot narrow it: `<domain>` matches a name or an exact literal, and
     * there is no way to write "the RFC 1918 ranges" in it. So the policy is expressed here, where
     * an address actually can be classified.
     *
     * That matters most for OPDS, the one caller that authenticates. It sends HTTP Basic
     * pre-emptively on every catalog page (ADR-INKREAD-0016 Decision 4), so a reader who typed a
     * public host — a mistyped domain, a catalog link that walked off the server — was putting a
     * base64'd password on the wire to whoever answered. Refusing that is the whole point.
     *
     * No DNS: the check runs on the caller's thread before the connection opens, and a lookup here
     * would both cost a round trip and be a different answer than the one the connection resolves.
     * Numeric literals are classified directly; names are classified by shape, which is sound
     * because none of the permitted shapes can be a public DNS name — `localhost`, a single-label
     * host (`nas:8080`, which public DNS cannot resolve), and the reserved local suffixes.
     *
     * `https://` is always allowed. A non-HTTP scheme is refused: the callers only ever fetch.
     */
    fun isCleartextAllowed(url: String): Boolean {
        val host = hostOf(url) ?: return false
        if (!url.trim().lowercase().startsWith("http://")) return true // https (or refused above)
        if (host.equals("localhost", ignoreCase = true)) return true
        val lower = host.lowercase().removeSuffix(".")
        // Reserved suffixes that cannot be delegated on the public internet.
        if (LOCAL_SUFFIXES.any { lower.endsWith(it) }) return true
        val literal = privateLiteral(lower)
        if (literal != null) return literal
        // A single-label name has no dot, so it is a LAN/mDNS short name, not a public host.
        return !lower.contains('.')
    }

    /** Reserved DNS suffixes for names that only ever resolve inside a local network. */
    private val LOCAL_SUFFIXES = listOf(".local", ".localhost", ".home.arpa", ".internal", ".lan")

    /**
     * `true`/`false` if [host] is a numeric IP literal (private / public), `null` if it is a name.
     *
     * IPv4 covers loopback, the three RFC 1918 ranges, and RFC 3927 link-local. IPv6 covers `::1`,
     * `fc00::/7` unique-local and `fe80::/10` link-local — including the `[...]` brackets a URL
     * puts around a v6 literal, which is why the caller must not have stripped them.
     */
    private fun privateLiteral(host: String): Boolean? {
        val h = host.removePrefix("[").removeSuffix("]")
        if (h.contains(':')) { // IPv6
            val v6 = h.substringBefore('%') // drop a zone id (fe80::1%wlan0)
            if (v6 == "::1") return true
            val head = v6.take(4).padEnd(4, '0')
            val first = head.take(2).toIntOrNull(16) ?: return false
            if (first and 0xFE == 0xFC) return true // fc00::/7 unique-local
            val ten = head.take(3).toIntOrNull(16) ?: return false
            return ten in 0xFE8..0xFEB // fe80::/10 link-local
        }
        val octets = h.split('.')
        if (octets.size != 4) return null // not a v4 literal → a name
        val v = octets.map { it.toIntOrNull() ?: return null }
        if (v.any { it !in 0..255 }) return null
        return when {
            v[0] == 127 -> true // loopback
            v[0] == 10 -> true // 10/8
            v[0] == 172 && v[1] in 16..31 -> true // 172.16/12
            v[0] == 192 && v[1] == 168 -> true // 192.168/16
            v[0] == 169 && v[1] == 254 -> true // 169.254/16 link-local
            else -> false
        }
    }

    /** The host component of [url], or `null` if it is not an http(s) URL we will fetch. */
    private fun hostOf(url: String): String? {
        val trimmed = url.trim()
        val lower = trimmed.lowercase()
        if (!lower.startsWith("http://") && !lower.startsWith("https://")) return null
        val afterScheme = trimmed.substringAfter("://")
        val authority = afterScheme.substringBefore('/').substringBefore('?').substringBefore('#')
        val hostPort = authority.substringAfterLast('@') // drop any userinfo
        val host = if (hostPort.startsWith("[")) {
            hostPort.substringBefore(']') + "]" // keep a bracketed v6 literal intact
        } else {
            hostPort.substringBefore(':')
        }
        return host.ifBlank { null }
    }

    /** GET [url] as UTF-8 text, capped at [capBytes]; `null` on blank URL / non-2xx / IO error. */
    fun getText(
        url: String,
        userAgent: String,
        accept: String?,
        timeoutMs: Int,
        capBytes: Int,
        authorization: String? = null,
    ): String? = getTextWithStatus(url, userAgent, accept, timeoutMs, capBytes, authorization).body

    /**
     * The same GET, reporting the HTTP [status] alongside the [body].
     *
     * Callers that only degrade to a no-op want [getText]; a caller that has to *explain* the
     * failure to the reader needs to tell "the server said no" from "there was no server". A 401 is
     * a wrong password, not an unreachable host, and telling someone to check their network when
     * their password is wrong sends them to fix the wrong thing.
     *
     * [status] is `0` when the request never got a response at all.
     */
    fun getTextWithStatus(
        url: String,
        userAgent: String,
        accept: String?,
        timeoutMs: Int,
        capBytes: Int,
        authorization: String? = null,
    ): Response {
        if (url.isBlank()) return Response(0, null)
        return try {
            val conn = open(url, userAgent, accept, timeoutMs, authorization)
            val code = conn.responseCode
            if (code !in 200..299) {
                Log.w(TAG, "GET $url -> HTTP $code")
                Response(code, null)
            } else {
                Response(code, conn.inputStream.use { String(it.readCapped(capBytes), Charsets.UTF_8) })
            }
        } catch (e: Exception) {
            Log.w(TAG, "GET $url failed: ${e.message}")
            Response(0, null)
        }
    }

    /** An HTTP response: the [status] (`0` = never reached the server) and the body when 2xx. */
    data class Response(val status: Int, val body: String?)

    /** Stream [url] to [dest], aborting past [capBytes]; `false` on non-2xx / IO error / oversize. */
    fun download(
        url: String,
        dest: File,
        userAgent: String,
        timeoutMs: Int,
        capBytes: Long,
        authorization: String? = null,
    ): Boolean {
        if (url.isBlank()) return false
        return try {
            val conn = open(url, userAgent, null, timeoutMs, authorization)
            if (conn.responseCode !in 200..299) {
                Log.w(TAG, "download $url -> HTTP ${conn.responseCode}")
                return false
            }
            conn.inputStream.use { input ->
                dest.outputStream().use { out ->
                    val buf = ByteArray(64 * 1024)
                    var total = 0L
                    while (true) {
                        val n = input.read(buf)
                        if (n < 0) break
                        total += n
                        if (total > capBytes) {
                            Log.w(TAG, "download $url exceeds ${capBytes}B cap")
                            return false
                        }
                        out.write(buf, 0, n)
                    }
                }
            }
            true
        } catch (e: Exception) {
            Log.w(TAG, "download $url failed: ${e.message}")
            false
        }
    }

    private fun open(
        url: String,
        userAgent: String,
        accept: String?,
        timeoutMs: Int,
        authorization: String?,
    ): HttpURLConnection {
        // Both entry points funnel through here, so this is the one place the cleartext policy has
        // to hold. Thrown rather than returned: every caller already treats an exception as "the
        // request did not happen", which is exactly the outcome, and adding a third failure shape
        // would touch all of them for no gain.
        if (!isCleartextAllowed(url)) {
            Log.w(TAG, "refusing cleartext to a non-local host: ${hostOf(url) ?: "?"}")
            throw java.io.IOException("cleartext HTTP is only allowed on the local network")
        }
        return (URL(url).openConnection() as HttpURLConnection).apply {
            connectTimeout = timeoutMs
            readTimeout = timeoutMs
            instanceFollowRedirects = true // GitHub asset URLs 302 to an HTTPS CDN (same-protocol → followed)
            setRequestProperty("User-Agent", userAgent)
            if (accept != null) setRequestProperty("Accept", accept)
            // Sent pre-emptively rather than waiting for a 401 challenge: it saves a round trip on
            // every catalog page, and Basic gains nothing from the challenge anyway. The underlying
            // client drops this header when a redirect changes host, so a redirecting catalog cannot
            // hand the credential to a third party.
            if (authorization != null) setRequestProperty("Authorization", authorization)
        }
    }

    /** Read up to [cap] bytes (the last chunk may cross it by &lt;64 KiB), then stop. */
    private fun InputStream.readCapped(cap: Int): ByteArray {
        val out = ByteArrayOutputStream()
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

    private const val TAG = "HttpFetch"
}
