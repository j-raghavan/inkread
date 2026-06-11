package dev.jraghavan.inkread.penspike

/**
 * JNI bridge to the native /dev/ebc helper (Route 3, RR19-FR4b / RR15-FR2).
 *
 * Kotlin cannot issue ioctl(); the native helper ([ebc_jni.c]) opens `/dev/ebc` and runs the
 * clean-room Rockchip ebc-dev ioctls (see the C file banner for the GPL UAPI sources). Every
 * method reports rc/errno faithfully — an EACCES under the untrusted_app SELinux domain is a
 * RESULT (Route 3 = red), not something to work around with root.
 */
object EbcNative {
    @Volatile var available: Boolean = false
        private set

    init {
        available = try {
            System.loadLibrary("penspike_ebc")
            true
        } catch (t: Throwable) {
            false
        }
    }

    /**
     * One-shot full diagnostic: open → GET_BUFFER_INFO → mmap → GET_BUFFER → paint bbox →
     * SEND_BUFFER(A2). Returns a human-readable multi-line report (each step's rc/errno).
     * This is the Route-3 reachability proof.
     */
    external fun probeA2(x1: Int, y1: Int, x2: Int, y2: Int): String

    /** Cheap reachability check: open()+close(). 0 = OK, negative = -errno. */
    external fun canOpen(): Int

    /**
     * Empirical ioctl-ABI discovery (RR19-FR4b round 2). With /dev/ebc open, runs a curated
     * clean-room candidate matrix (raw 0x7000..0x700d, GET_BUFFER_INFO across struct sizes,
     * _IO* macro encodings) and returns a human-readable table classifying each result as
     * ENOTTY (unrecognized) / EINVAL (recognized, bad arg) / EFAULT (recognized, bad ptr) /
     * OK. The non-ENOTTY rows reveal the real ABI. On a GET_BUFFER_INFO success it dumps the
     * returned struct ints + tries GET_BUFFER/mmap. Captured headlessly via the self-test.
     */
    external fun discoverAbi(): String

    /** Open a persistent /dev/ebc session (fd + mmap) for the per-stroke latency loop. */
    external fun openEbc(): Int

    /** Issue one A2 partial update for the rect on the open session. 0 = OK, negative = -errno. */
    external fun sendA2(x1: Int, y1: Int, x2: Int, y2: Int): Int

    /** Close the persistent session. */
    external fun closeEbc()
}
