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

    /** GET [url] as UTF-8 text, capped at [capBytes]; `null` on blank URL / non-2xx / IO error. */
    fun getText(
        url: String,
        userAgent: String,
        accept: String?,
        timeoutMs: Int,
        capBytes: Int,
        authorization: String? = null,
    ): String? {
        if (url.isBlank()) return null
        return try {
            val conn = open(url, userAgent, accept, timeoutMs, authorization)
            if (conn.responseCode !in 200..299) {
                Log.w(TAG, "GET $url -> HTTP ${conn.responseCode}")
                null
            } else {
                conn.inputStream.use { String(it.readCapped(capBytes), Charsets.UTF_8) }
            }
        } catch (e: Exception) {
            Log.w(TAG, "GET $url failed: ${e.message}")
            null
        }
    }

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
    ): HttpURLConnection =
        (URL(url).openConnection() as HttpURLConnection).apply {
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
