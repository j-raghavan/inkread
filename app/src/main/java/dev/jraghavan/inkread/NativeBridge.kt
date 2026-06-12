package dev.jraghavan.inkread

import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * The JNI seam to the Rust core (`libreader.so`) — RR1-FR3.
 *
 * Function names mirror `reader-core/src/jni.rs` exactly
 * (`Java_dev_jraghavan_inkread_NativeBridge_<name>`). The core never names a vendor (IR-7):
 * it speaks the versioned wire formats below, and the Kotlin adapters do the device work.
 *
 * Handle model (Amendment 2): [nativeOpenDocument] returns an opaque `long`; the caller
 * holds it and passes it back. Only [nativeCloseDocument] frees it; callers MUST zero their
 * stored handle on close so a double-close is a no-op.
 */
object NativeBridge {
    init {
        System.loadLibrary("reader")
    }

    /** Proves the JNI boundary end to end (RR1-AC2): returns the core version string. */
    external fun nativeHello(): String

    /** Hand the selected adapter's capabilities to the core (Fork 3 caps bytes). */
    external fun nativeInit(capsBytes: ByteArray): Boolean

    /** Open a PDF at [path]; returns the opaque handle (0 on failure → exception thrown). */
    external fun nativeOpenDocument(
        path: String,
        capsBytes: ByteArray,
        width: Int,
        height: Int,
        dpi: Int,
    ): Long

    /**
     * Open a PDF AND attach a SQLite store at [dbPath], resuming the saved reading position
     * and persisted e-ink settings for [bookId] (RR12 / RR27). [dbPath] lives under app
     * storage; [bookId] is the stable per-book identity (≤512 chars). Returns the handle.
     */
    external fun nativeOpenDocumentWithStore(
        path: String,
        capsBytes: ByteArray,
        width: Int,
        height: Int,
        dpi: Int,
        dbPath: String,
        bookId: String,
    ): Long

    /** Persist the current reading position (RR12-FR3); store-less session = no-op. */
    external fun nativeSavePosition(handle: Long)

    /** The current 0-based page index (RR11) — page indicator + resume verification. */
    external fun nativeCurrentPage(handle: Long): Int

    /** Free the session. Null-safe + double-close tolerant (Amendment 2). */
    external fun nativeCloseDocument(handle: Long)

    /** Page count of the open document. */
    external fun nativePageCount(handle: Long): Int

    /**
     * Render the current page into [directBuffer] — a DIRECT [ByteBuffer] of exactly
     * `width*height*4` bytes (RGBA). The core borrows it for the call only (Amendment 5).
     */
    external fun nativeRenderPage(handle: Long, directBuffer: ByteBuffer)

    /**
     * Apply a navigation gesture (code per [Gesture]); returns the encoded RefreshCommand
     * stream (Fork 2). Decode with [WireCodec.decodeCommands].
     */
    external fun nativeOnGesture(handle: Long, code: Int): ByteArray
}

/** Navigation gestures — the int code mapping mirrors `Gesture::from_code` in the core. */
enum class Gesture(val code: Int) {
    NEXT_PAGE(0),
    PREV_PAGE(1),
}

/** A device-agnostic refresh intent (mirrors `RefreshIntent`, RR2-FR1). */
enum class RefreshIntent {
    FULL, PARTIAL, UI, FAST, FLASH_UI, FLASH_PARTIAL;

    companion object {
        fun fromCode(code: Int): RefreshIntent = when (code) {
            0 -> FULL; 1 -> PARTIAL; 2 -> UI; 3 -> FAST; 4 -> FLASH_UI; 5 -> FLASH_PARTIAL
            else -> throw IllegalArgumentException("unknown intent code $code")
        }
    }
}

/** A vendor-neutral refresh instruction decoded from the core's command stream (Fork 2). */
sealed interface RefreshCommand {
    data class Update(
        val x: Int, val y: Int, val w: Int, val h: Int,
        val intent: RefreshIntent, val dither: Boolean,
    ) : RefreshCommand
    data object WaitForLast : RefreshCommand
    data object EnterFastMode : RefreshCommand
    data object LeaveFastMode : RefreshCommand
}

/**
 * The Kotlin half of the JNI wire codecs (Forks 2 & 3). Little-endian to match the Rust
 * side (which pins LE explicitly); see `device-eink/src/wire.rs` for the byte layout.
 */
object WireCodec {
    private const val WIRE_VERSION: Byte = 0x01
    private const val COMMAND_HEADER_LEN = 4
    private const val COMMAND_RECORD_LEN = 20

    /**
     * Encode [DeviceCapabilities] to the Fork-3 caps bytes:
     * `[version][nflags][flags... in declaration order]`.
     */
    fun encodeCapabilities(caps: DeviceCapabilities): ByteArray {
        val flags = caps.flags()
        val out = ByteArray(2 + flags.size)
        out[0] = WIRE_VERSION
        out[1] = flags.size.toByte()
        for (i in flags.indices) out[2 + i] = if (flags[i]) 1 else 0
        return out
    }

    /**
     * Decode the Fork-2 command stream the core returns from [NativeBridge.nativeOnGesture].
     */
    fun decodeCommands(bytes: ByteArray): List<RefreshCommand> {
        require(bytes.size >= COMMAND_HEADER_LEN) { "command stream truncated" }
        require(bytes[0] == WIRE_VERSION) { "bad wire version ${bytes[0]}" }
        val buf = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
        val count = bytes[1].toInt() and 0xFF
        val need = COMMAND_HEADER_LEN + count * COMMAND_RECORD_LEN
        require(bytes.size >= need) { "command stream too short: have ${bytes.size}, need $need" }

        val out = ArrayList<RefreshCommand>(count)
        for (i in 0 until count) {
            val off = COMMAND_HEADER_LEN + i * COMMAND_RECORD_LEN
            when (val tag = bytes[off].toInt() and 0xFF) {
                0 -> {
                    val intent = RefreshIntent.fromCode(bytes[off + 1].toInt() and 0xFF)
                    val dither = (bytes[off + 2].toInt() and 0xFF) != 0
                    val x = buf.getInt(off + 4)
                    val y = buf.getInt(off + 8)
                    val w = buf.getInt(off + 12)
                    val h = buf.getInt(off + 16)
                    out.add(RefreshCommand.Update(x, y, w, h, intent, dither))
                }
                1 -> out.add(RefreshCommand.WaitForLast)
                2 -> out.add(RefreshCommand.EnterFastMode)
                3 -> out.add(RefreshCommand.LeaveFastMode)
                else -> throw IllegalArgumentException("unknown command tag $tag")
            }
        }
        return out
    }
}
