package dev.jraghavan.inkread

import java.security.MessageDigest

/**
 * Pure integrity checks for a downloaded update APK (ADR-INKREAD-0014 UPD-FR3/FR4). Kept free of
 * Android types so it is host-unit-tested under `:app:testDebugUnitTest` (the [PalmFilter]
 * precedent) — the [UpdateController] does the Android I/O (download, PackageManager cert
 * extraction) and delegates the actual comparisons here.
 *
 * Two independent gates must both pass before the system installer is invoked:
 *  - **checksum** ([matchesPublishedSha]) — the bytes are exactly what the release published;
 *  - **signer pin** ([certsMatch]) — the APK is signed by *this* installed app's key, the defense
 *    that makes the debug-key interim (Decision 0) fail closed.
 */
object UpdateVerify {

    /** Length of a SHA-256 digest as lowercase hex. */
    private const val SHA256_HEX_LEN = 64

    /**
     * Extract the expected hex digest from a `sha256sum`-style checksum file, whose first line is
     * `"<64-hex>  <filename>"`. Returns the lowercased digest, or `null` if the text carries no
     * well-formed 64-char hex token — the caller treats `null` as "no checksum published" (fall
     * back to signer-pin only) and a *present but mismatched* digest as a hard abort, so the two
     * cases must stay distinguishable.
     */
    fun expectedShaFrom(checksumFileText: String?): String? {
        if (checksumFileText.isNullOrBlank()) return null
        val token = checksumFileText.trimStart().substringBefore(' ').substringBefore('\n').trim()
        val hex = token.lowercase()
        return if (isSha256Hex(hex)) hex else null
    }

    /** Lowercase hex SHA-256 of [bytes]. */
    fun sha256Hex(bytes: ByteArray): String {
        val digest = MessageDigest.getInstance("SHA-256").digest(bytes)
        val sb = StringBuilder(digest.size * 2)
        for (b in digest) {
            val v = b.toInt() and 0xFF
            sb.append(HEX[v ushr 4]).append(HEX[v and 0x0F])
        }
        return sb.toString()
    }

    /**
     * Whether [apkBytes] hashes to the digest published in [checksumFileText]. Returns `false` when
     * the checksum text is missing/malformed — so the caller MUST first decide via [expectedShaFrom]
     * whether a checksum was published at all (absent ⇒ skip this gate; present ⇒ require a match).
     */
    fun matchesPublishedSha(apkBytes: ByteArray, checksumFileText: String?): Boolean {
        val expected = expectedShaFrom(checksumFileText) ?: return false
        return constantTimeEquals(expected, sha256Hex(apkBytes))
    }

    /**
     * Whether the candidate APK's signer set equals the installed app's signer set. Both lists hold
     * the raw signing-certificate bytes (`Signature.toByteArray()`); a match requires the same
     * multiset of certificates and at least one signer on each side (an empty set never matches —
     * an unsigned or signer-stripped APK is rejected).
     */
    fun certsMatch(installed: List<ByteArray>, candidate: List<ByteArray>): Boolean {
        if (installed.isEmpty() || candidate.isEmpty()) return false
        if (installed.size != candidate.size) return false
        val remaining = candidate.toMutableList()
        for (cert in installed) {
            val idx = remaining.indexOfFirst { it.contentEquals(cert) }
            if (idx < 0) return false
            remaining.removeAt(idx)
        }
        return remaining.isEmpty()
    }

    private fun isSha256Hex(s: String): Boolean =
        s.length == SHA256_HEX_LEN && s.all { it in '0'..'9' || it in 'a'..'f' }

    /** Length-aware constant-time hex compare (inputs are already lowercased). */
    private fun constantTimeEquals(a: String, b: String): Boolean {
        if (a.length != b.length) return false
        var diff = 0
        for (i in a.indices) diff = diff or (a[i].code xor b[i].code)
        return diff == 0
    }

    private val HEX = "0123456789abcdef".toCharArray()
}
