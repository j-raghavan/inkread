package dev.jraghavan.inkread

import dev.jraghavan.inkread.eink.LcdAdapter
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for what each adapter advertises to the core (#220).
 *
 * The capabilities are the whole point of picking an adapter: they decide whether the core applies
 * an e-ink refresh policy — ghosting cadence, full-screen flashes, refresh-after-resume — to the
 * display in front of the reader. Getting them wrong is not a crash, which is exactly why it needs
 * asserting rather than eyeballing.
 */
class DisplayCapabilitiesTest {

    /** The shipping device's profile must not shift while making room for another one. */
    @Test
    fun theSupernoteBaselineIsUnchanged() {
        val caps = DeviceCapabilities.supernoteBaseline()
        assertTrue("Supernote is e-ink", caps.eink)
        assertTrue("and needs a refresh after resume", caps.needsRefreshAfterResume)
        assertFalse("M0 has no full-refresh control", caps.einkFull)
        assertFalse("monochrome panel", caps.colorScreen)
    }

    /**
     * The bug this fixes: a non-Supernote was told `eink = true` and got a refresh policy written
     * for a panel that ghosts.
     */
    @Test
    fun anOrdinaryDisplayIsNotAdvertisedAsEink() {
        val caps = DeviceCapabilities.genericDisplay()
        assertFalse("an LCD is not e-ink", caps.eink)
        assertFalse("and needs no refresh after resume", caps.needsRefreshAfterResume)
        assertTrue("it is a colour screen", caps.colorScreen)
    }

    /** Every e-ink hardware feature is meaningless without an e-ink panel to apply it to. */
    @Test
    fun anOrdinaryDisplayClaimsNoEinkHardwareFeatures() {
        val caps = DeviceCapabilities.genericDisplay()
        for ((name, claimed) in listOf(
            "einkFull" to caps.einkFull,
            "regal" to caps.regal,
            "fastMode" to caps.fastMode,
            "regionalUpdate" to caps.regionalUpdate,
            "hwInvert" to caps.hwInvert,
            "hwDither" to caps.hwDither,
            "kaleidoWfm" to caps.kaleidoWfm,
        )) {
            assertFalse("an LCD must not claim $name", claimed)
        }
    }

    @Test
    fun theLcdAdapterAdvertisesTheOrdinaryDisplayProfile() {
        assertEquals(DeviceCapabilities.genericDisplay(), LcdAdapter().capabilities())
    }

    /** The two profiles must actually differ, or selecting between them changes nothing. */
    @Test
    fun theTwoProfilesDiffer() {
        assertTrue(DeviceCapabilities.supernoteBaseline() != DeviceCapabilities.genericDisplay())
    }
}
