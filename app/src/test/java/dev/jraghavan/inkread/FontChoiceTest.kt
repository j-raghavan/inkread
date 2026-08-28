package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test

/**
 * Host JVM tests for resolving the saved reading face (#169).
 *
 * The core numbers faces positionally — bundled families first, then the reader's imported fonts in
 * sorted order — so any change to the imported set renumbers the faces after it. The reader
 * therefore stores the *name*; these tests are about what that buys over storing the index.
 */
class FontChoiceTest {

    /** The bundled families, in the order the core registers them. */
    private val bundled = listOf("Spectral", "Noto Serif", "Noto Sans", "Free Serif", "Free Sans", "Droid Mono")

    private fun facesWith(vararg imported: String) = bundled + imported.toList()

    @Test
    fun aRegisteredNameResolvesToItsId() {
        val faces = facesWith("Alegreya", "Proza Libre")
        assertEquals(0, DisplayPrefs.fontIdFor("Spectral", faces))
        assertEquals(6, DisplayPrefs.fontIdFor("Alegreya", faces))
        assertEquals(7, DisplayPrefs.fontIdFor("Proza Libre", faces))
    }

    /** Nothing saved is the default face, not a failure. */
    @Test
    fun noSavedNameIsTheDefaultFace() {
        assertEquals(0, DisplayPrefs.fontIdFor("", facesWith("Alegreya")))
    }

    /**
     * The defect this replaced. Reading in Proza Libre (id 7) and removing Alegreya renumbers Proza
     * to 6 — so the *stored index* 7 now runs off the end, and clamping it lands on Proza's
     * neighbour rather than on Proza. Resolving by name follows the face instead.
     */
    @Test
    fun removingAnEarlierFontKeepsTheChosenFace() {
        val before = facesWith("Alegreya", "Proza Libre")
        val savedId = DisplayPrefs.fontIdFor("Proza Libre", before)
        assertEquals(7, savedId)

        val after = facesWith("Proza Libre")
        assertEquals(6, DisplayPrefs.fontIdFor("Proza Libre", after))
        assertNotEquals("the old index no longer names the chosen face", savedId, DisplayPrefs.fontIdFor("Proza Libre", after))
    }

    /** An import sorts into the list and renumbers everything after it; the name still holds. */
    @Test
    fun importingAnEarlierFontKeepsTheChosenFace() {
        val before = facesWith("Proza Libre")
        assertEquals(6, DisplayPrefs.fontIdFor("Proza Libre", before))
        // "Alegreya" sorts first, so it takes id 6 and pushes Proza to 7.
        val after = facesWith("Alegreya", "Proza Libre")
        assertEquals(7, DisplayPrefs.fontIdFor("Proza Libre", after))
        assertEquals(6, DisplayPrefs.fontIdFor("Alegreya", after))
    }

    /**
     * Removing the face in use returns the reader to the default rather than to whichever font has
     * inherited its id — the silent typeface change the index-based store allowed.
     */
    @Test
    fun removingTheChosenFontFallsBackToTheDefault() {
        val after = facesWith("Proza Libre")
        assertEquals(0, DisplayPrefs.fontIdFor("Alegreya", after))
    }

    /** A registry that could not be read resolves to the default and never throws. */
    @Test
    fun anEmptyRegistryResolvesToTheDefault() {
        assertEquals(0, DisplayPrefs.fontIdFor("Alegreya", emptyList()))
        assertEquals(0, DisplayPrefs.fontIdFor("", emptyList()))
    }
}
