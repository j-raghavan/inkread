package dev.jraghavan.inkread

import android.util.Log
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.URL

/**
 * The shell's one bounded HTTP GET, shared by the network features (the daily fetch #66 and the
 * self-updater, ADR-INKREAD-0014). The Rust core stays IO-free (IR-7) — every HTTP byte enters here.
 * Both entry points cap the response so a runaway/hostile body can never exhaust memory or disk, and
 * both swallow failures into a `null`/`false` so callers degrade silently (offline ⇒ no-op).
 */
object HttpFetch {

    /** GET [url] as UTF-8 text, capped at [capBytes]; `null` on blank URL / non-2xx / IO error. */
    fun getText(url: String, userAgent: String, accept: String?, timeoutMs: Int, capBytes: Int): String? {
        if (url.isBlank()) return null
        return try {
            val conn = open(url, userAgent, accept, timeoutMs)
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
    fun download(url: String, dest: File, userAgent: String, timeoutMs: Int, capBytes: Long): Boolean {
        if (url.isBlank()) return false
        return try {
            val conn = open(url, userAgent, null, timeoutMs)
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

    private fun open(url: String, userAgent: String, accept: String?, timeoutMs: Int): HttpURLConnection =
        (URL(url).openConnection() as HttpURLConnection).apply {
            connectTimeout = timeoutMs
            readTimeout = timeoutMs
            instanceFollowRedirects = true // GitHub asset URLs 302 to an HTTPS CDN (same-protocol → followed)
            setRequestProperty("User-Agent", userAgent)
            if (accept != null) setRequestProperty("Accept", accept)
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
