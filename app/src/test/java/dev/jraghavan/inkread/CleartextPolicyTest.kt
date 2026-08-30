package dev.jraghavan.inkread

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for [HttpFetch.isCleartextAllowed].
 *
 * `usesCleartextTraffic="true"` is application-wide and the platform's network-security config
 * cannot narrow it to the RFC 1918 ranges, so the narrowing lives in Kotlin — which means it needs
 * the tests the manifest attribute would not have had. The case that matters is OPDS: it sends HTTP
 * Basic pre-emptively (ADR-INKREAD-0016 Decision 4), so "is this host on my network?" is the
 * question standing between a password and the open internet.
 *
 * Pure string and integer work, no DNS and no device.
 */
class CleartextPolicyTest {

    private fun allowed(url: String) = HttpFetch.isCleartextAllowed(url)

    // ---- https is never the question ----

    @Test
    fun httpsIsAlwaysAllowed() {
        assertTrue(allowed("https://api.github.com/repos/j-raghavan/inkread/releases/latest"))
        assertTrue(allowed("https://en.wiktionary.org/api/rest_v1/page/definition/ink"))
        assertTrue(allowed("https://192.168.1.20:8443/opds"))
    }

    // ---- the LAN calibre case, which is why cleartext is enabled at all ----

    @Test
    fun privateIpv4LiteralsAreAllowed() {
        assertTrue(allowed("http://192.168.1.20:8080/opds"))
        assertTrue(allowed("http://10.0.0.5/opds"))
        assertTrue(allowed("http://172.16.0.1:8080/opds"))
        assertTrue(allowed("http://172.31.255.254/opds"))
        assertTrue(allowed("http://127.0.0.1:8080/opds"))
        assertTrue(allowed("http://169.254.10.1/opds"))
    }

    /** 172.15 and 172.32 bracket the RFC 1918 block; both are public. */
    @Test
    fun theEdgesOfThe172BlockAreNotPrivate() {
        assertFalse(allowed("http://172.15.0.1/opds"))
        assertFalse(allowed("http://172.32.0.1/opds"))
    }

    @Test
    fun privateIpv6LiteralsAreAllowed() {
        assertTrue(allowed("http://[::1]:8080/opds"))
        assertTrue(allowed("http://[fd00::1]:8080/opds"))
        assertTrue(allowed("http://[fc00::1]/opds"))
        assertTrue(allowed("http://[fe80::1]/opds"))
        assertTrue(allowed("http://[fe80::1%wlan0]/opds"))
    }

    @Test
    fun publicIpv6LiteralsAreRefused() {
        assertFalse(allowed("http://[2001:4860:4860::8888]/opds"))
    }

    @Test
    fun localNamesAreAllowed() {
        assertTrue(allowed("http://localhost:8080/opds"))
        assertTrue(allowed("http://calibre.local:8080/opds"))
        assertTrue(allowed("http://nas.home.arpa/opds"))
        assertTrue(allowed("http://server.lan:8080/opds"))
    }

    /** A single-label host has no dot, so public DNS cannot resolve it — it is a LAN short name. */
    @Test
    fun aSingleLabelHostIsAllowed() {
        assertTrue(allowed("http://nas:8080/opds"))
        assertTrue(allowed("http://calibre/opds"))
    }

    // ---- the finding this closes ----

    @Test
    fun cleartextToAPublicHostIsRefused() {
        assertFalse(allowed("http://example.com/opds"))
        assertFalse(allowed("http://8.8.8.8/opds"))
        assertFalse(allowed("http://feeds.bbci.co.uk/news/rss.xml"))
    }

    /**
     * A URL can carry userinfo before the host. Parsing must take the host from *after* the `@`, or
     * `http://192.168.1.1@evil.example.com/` reads as private and sends the password to
     * `evil.example.com` — the exact shape of a phishing link.
     */
    @Test
    fun userinfoDoesNotDisguiseAPublicHost() {
        assertFalse(allowed("http://192.168.1.1@evil.example.com/opds"))
        assertFalse(allowed("http://user:pass@evil.example.com/opds"))
        assertTrue(allowed("http://user:pass@192.168.1.20:8080/opds"))
    }

    /** The host ends at the first `/`, `?` or `#` — a private-looking path proves nothing. */
    @Test
    fun aPrivateLookingPathOrQueryDoesNotAllowAPublicHost() {
        assertFalse(allowed("http://evil.example.com/192.168.1.1/opds"))
        assertFalse(allowed("http://evil.example.com?host=127.0.0.1"))
        assertFalse(allowed("http://evil.example.com#192.168.1.1"))
    }

    /** A trailing dot is a fully-qualified name and must not defeat the suffix match either way. */
    @Test
    fun aTrailingRootDotIsHandled() {
        assertTrue(allowed("http://calibre.local./opds"))
        assertFalse(allowed("http://example.com./opds"))
    }

    @Test
    fun caseDoesNotMatter() {
        assertTrue(allowed("HTTP://LOCALHOST:8080/opds"))
        assertTrue(allowed("http://Calibre.LOCAL/opds"))
        assertFalse(allowed("HTTP://EXAMPLE.COM/opds"))
    }

    // ---- anything that is not a fetchable http(s) URL ----

    @Test
    fun nonHttpSchemesAndJunkAreRefused() {
        assertFalse(allowed("file:///etc/passwd"))
        assertFalse(allowed("ftp://example.com/x"))
        assertFalse(allowed(""))
        assertFalse(allowed("   "))
        assertFalse(allowed("http://"))
        assertFalse(allowed("not a url"))
    }
}
