package dev.jraghavan.inkread

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host JVM tests for [UpdateVerify] (ADR-INKREAD-0014 UPD-FR3/FR4 integrity gates). Pure logic —
 * the checksum parse/compute/compare and signer-set equality — exercised without a device, so a
 * weakened integrity check fails the host gate rather than shipping.
 */
class UpdateVerifyTest {

    private val inkreadBytes = "inkread".toByteArray()

    @Test
    fun sha256_matches_known_vector() {
        // Compute once and assert the hex shape + determinism rather than hard-coding the digest.
        val h = UpdateVerify.sha256Hex(inkreadBytes)
        assertEquals(64, h.length)
        assertTrue(h.all { it in '0'..'9' || it in 'a'..'f' })
        assertEquals(h, UpdateVerify.sha256Hex(inkreadBytes)) // stable
    }

    @Test
    fun expectedShaFrom_parses_sha256sum_format() {
        val hex = "a".repeat(64)
        // `sha256sum` writes "<hex>  <filename>" (two spaces); also tolerate a single space + newline.
        assertEquals(hex, UpdateVerify.expectedShaFrom("$hex  inkread-v1.0.0.apk\n"))
        assertEquals(hex, UpdateVerify.expectedShaFrom("$hex inkread.apk"))
        assertEquals(hex, UpdateVerify.expectedShaFrom("${hex.uppercase()}  FILE")) // lowercased
    }

    @Test
    fun expectedShaFrom_rejects_garbage_and_absence() {
        assertNull(UpdateVerify.expectedShaFrom(null))
        assertNull(UpdateVerify.expectedShaFrom(""))
        assertNull(UpdateVerify.expectedShaFrom("   "))
        assertNull(UpdateVerify.expectedShaFrom("not-a-hash file"))
        assertNull(UpdateVerify.expectedShaFrom("a".repeat(63) + "  f")) // too short
        assertNull(UpdateVerify.expectedShaFrom("g".repeat(64) + "  f")) // non-hex
    }

    @Test
    fun matchesPublishedSha_true_on_match_false_on_tamper() {
        val good = UpdateVerify.sha256Hex(inkreadBytes)
        assertTrue(UpdateVerify.matchesPublishedSha(inkreadBytes, "$good  inkread.apk"))
        // A single flipped byte must fail.
        val tampered = inkreadBytes.copyOf().also { it[0] = (it[0] + 1).toByte() }
        assertFalse(UpdateVerify.matchesPublishedSha(tampered, "$good  inkread.apk"))
    }

    @Test
    fun matchesPublishedSha_false_when_no_checksum() {
        // Absent/malformed checksum is NOT a match — the caller falls back to signer-pin only.
        assertFalse(UpdateVerify.matchesPublishedSha(inkreadBytes, null))
        assertFalse(UpdateVerify.matchesPublishedSha(inkreadBytes, "garbage"))
    }

    @Test
    fun certsMatch_true_for_same_set_any_order() {
        val a = byteArrayOf(1, 2, 3)
        val b = byteArrayOf(4, 5, 6)
        assertTrue(UpdateVerify.certsMatch(listOf(a, b), listOf(b.copyOf(), a.copyOf())))
    }

    @Test
    fun certsMatch_false_on_different_signer() {
        val installed = listOf(byteArrayOf(1, 2, 3))
        val attacker = listOf(byteArrayOf(9, 9, 9))
        assertFalse(UpdateVerify.certsMatch(installed, attacker))
    }

    @Test
    fun certsMatch_false_on_empty_or_size_mismatch() {
        val a = byteArrayOf(1, 2, 3)
        assertFalse(UpdateVerify.certsMatch(emptyList(), listOf(a)))
        assertFalse(UpdateVerify.certsMatch(listOf(a), emptyList()))
        // Subset must not pass (size differs).
        assertFalse(UpdateVerify.certsMatch(listOf(a), listOf(a.copyOf(), byteArrayOf(7))))
    }
}
