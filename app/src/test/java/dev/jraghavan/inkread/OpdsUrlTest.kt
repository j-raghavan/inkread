package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for [OpdsController]'s URL handling (ADR-INKREAD-0016, #175).
 *
 * This is the half of the client that can go wrong quietly: a catalog hands us relative hrefs and we
 * turn them into the next thing the app fetches, so a bad resolution walks off the server and a bad
 * encoding searches for the wrong string. The catalog *semantics* are tested in Rust
 * (`inkread-opds`); what is left here is URLs, which is plain `java.net` and needs no device.
 */
class OpdsUrlTest {

    private val base = "http://192.168.1.20:8080/opds"

    // ---- resolve ----

    @Test
    fun absolutePathsResolveAgainstTheServerRoot() {
        assertEquals(
            "http://192.168.1.20:8080/get/EPUB/42/lib",
            OpdsController.resolve(base, "/get/EPUB/42/lib"),
        )
    }

    @Test
    fun relativePathsResolveAgainstTheFeed() {
        assertEquals(
            "http://192.168.1.20:8080/navcatalog/616263",
            OpdsController.resolve(base, "navcatalog/616263"),
        )
    }

    @Test
    fun anAbsoluteUrlIsKeptAsIs() {
        val other = "https://books.example.org/opds/page/2"
        assertEquals(other, OpdsController.resolve(base, other))
    }

    @Test
    fun queryStringsAndPortsSurvive() {
        assertEquals(
            "http://192.168.1.20:8080/opds/navcatalog/4e?offset=25",
            OpdsController.resolve(base, "/opds/navcatalog/4e?offset=25"),
        )
    }

    @Test
    fun anAbsentHrefResolvesToEmpty() {
        assertEquals("", OpdsController.resolve(base, ""))
        assertEquals("", OpdsController.resolve(base, "   "))
    }

    @Test
    fun nonHttpSchemesAreRefused() {
        // The catalog is a remote party naming what we fetch next. These would turn a browse into a
        // local read, so resolution must drop them rather than hand them to the fetcher.
        for (hostile in listOf(
            "file:///data/data/dev.jraghavan.inkread/shared_prefs/settings.xml",
            "jar:file:///tmp/x.jar!/y",
            "content://com.android.providers/downloads",
            "javascript:alert(1)",
        )) {
            assertEquals("refused: $hostile", "", OpdsController.resolve(base, hostile))
        }
    }

    @Test
    fun aGarbageBaseDoesNotThrow() {
        assertEquals("", OpdsController.resolve("not a url", "/opds"))
    }

    // ---- isHttpUrl ----

    @Test
    fun onlyHttpAndHttpsAreFetchable() {
        assertTrue(OpdsController.isHttpUrl("http://a/b"))
        assertTrue(OpdsController.isHttpUrl("HTTPS://A/B"))
        assertFalse(OpdsController.isHttpUrl("ftp://a/b"))
        assertFalse(OpdsController.isHttpUrl(""))
        assertFalse(OpdsController.isHttpUrl("//a/b"))
    }

    // ---- searchUrl ----

    @Test
    fun searchTermsArePercentEncodedNotPlusEncoded() {
        // calibre's template puts the terms in a PATH segment, where "+" is a literal plus. Getting
        // this wrong returns no results rather than failing loudly, so it is worth pinning.
        assertEquals(
            "/opds/search/le%20guin",
            OpdsController.searchUrl("/opds/search/{searchTerms}", "le guin"),
        )
    }

    @Test
    fun searchTermsWithReservedCharactersAreEscaped() {
        val url = OpdsController.searchUrl("/opds/search/{searchTerms}", "sci-fi & fantasy/short")
        assertFalse("no bare ampersand", url.contains("&"))
        assertFalse("no bare slash in the term", url.removePrefix("/opds/search/").contains("/"))
    }

    @Test
    fun searchWithoutATemplateOrTermsYieldsNothing() {
        assertEquals("", OpdsController.searchUrl("", "x"))
        assertEquals("", OpdsController.searchUrl("/opds/search/{searchTerms}", ""))
    }

    // ---- catalogRoot ----

    @Test
    fun aBareHostGetsTheSchemeAndTheCatalogPath() {
        // What people actually write down is "192.168.1.20:8080"; both servers publish the catalog
        // at /opds, so completing it is knowledge the app should hold, not the reader.
        assertEquals("http://192.168.1.20:8080/opds", OpdsController.catalogRoot("192.168.1.20:8080"))
        assertEquals("http://calibre.local/opds", OpdsController.catalogRoot("http://calibre.local"))
        assertEquals("https://books.example.org/opds", OpdsController.catalogRoot("https://books.example.org/"))
    }

    @Test
    fun anExplicitPathIsRespected() {
        // Calibre-Web behind a reverse proxy is commonly mounted on a sub-path; never clobber it.
        assertEquals(
            "https://example.org/calibre/opds",
            OpdsController.catalogRoot("https://example.org/calibre/opds"),
        )
        assertEquals("http://host/custom", OpdsController.catalogRoot("http://host/custom/"))
    }

    @Test
    fun anEmptyAddressYieldsNothing() {
        assertEquals("", OpdsController.catalogRoot(""))
        assertEquals("", OpdsController.catalogRoot("   "))
    }
}
