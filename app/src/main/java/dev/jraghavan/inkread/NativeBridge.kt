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

    /**
     * Shed bounded caches under platform memory pressure (RR24-FR3 / `onTrimMemory`). [level] is the
     * core severity code: 0 = moderate (drop the least-critical caches), 1 = critical (drop all).
     * Null/closed-handle safe — never throws.
     */
    external fun nativeOnTrimMemory(handle: Long, level: Int)

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

    /** The document outline as the flattened pre-order wire (RR11-FR2); decode with [WireCodec.decodeToc]. */
    external fun nativeToc(handle: Long): ByteArray

    /**
     * Jump to an absolute page index (clamped to range in the core); returns the encoded
     * RefreshCommand stream (RR11-FR1). Decode with [WireCodec.decodeCommands].
     */
    external fun nativeJumpToPage(handle: Long, page: Int): ByteArray

    /**
     * The clickable links on `page`, normalized to the rendered page (RR11-FR3); decode with
     * [WireCodec.decodeLinks]. The shell hit-tests a tap and jumps (internal) or opens the URI.
     */
    external fun nativePageLinks(handle: Long, page: Int): ByteArray

    // ---- text selection + dictionary (RR11/RR12 / ADR-INKREAD-0009 D3) ----

    /** The word under normalized `(x, y)` on `page`; decode with [WireCodec.decodeSelection]. */
    external fun nativeWordAt(handle: Long, page: Int, x: Float, y: Float): ByteArray

    /** The text within the normalized rect on `page`; decode with [WireCodec.decodeSelection]. */
    external fun nativeTextInRect(handle: Long, page: Int, x0: Float, y0: Float, x1: Float, y1: Float): ByteArray

    /** Reading-order selection a drag sweeps from the start point (sx,sy) to the lift point (ex,ey)
     *  on `page` — the multi-line drag. Whole lines from the start line through the line before the
     *  lift; the lift line clipped to the word under ex; inter-line gaps filled. Decode with
     *  [WireCodec.decodeSelection]. */
    external fun nativeTextLineSpan(handle: Long, page: Int, sx: Float, sy: Float, ex: Float, ey: Float): ByteArray

    /** The reflow-stable [start,end] PinPosition pair a selection rect covers on a reflowable page —
     *  the Digest/highlight anchor (#46). Returns a JSON object `{"start":<pin>,"end":<pin>}`, or an
     *  EMPTY string for fixed-layout PDF / an empty selection (caller falls back to a page anchor). */
    external fun nativeSelectionPins(handle: Long, page: Int, x0: Float, y0: Float, x1: Float, y1: Float): String

    /**
     * Find [query] on `page` (RR2 in-document search), case-insensitive. Returns the search wire
     * (decode with [WireCodec.decodeSearch]): one match per occurrence with a snippet + highlight
     * boxes. Call page-by-page so the scan stays memory-bounded.
     */
    external fun nativeSearchPage(handle: Long, page: Int, query: String): ByteArray

    /** Open the dictionary corpus at [path]; returns an opaque handle (0 on failure → throws). */
    external fun nativeDictOpen(path: String): Long

    /** Free a dictionary handle. Callers zero their field on close (double-close safe). */
    external fun nativeDictClose(dictHandle: Long)

    /**
     * Look [word] up on-device, preferring the comma-separated [langsCsv] languages; decode with
     * [WireCodec.decodeDefinition]. A miss (found == false) is the shell's cue to try its online
     * source and cache the result with [nativeDictPut].
     */
    external fun nativeDefine(dictHandle: Long, word: String, langsCsv: String): ByteArray

    /** Cache a definition (e.g. an online result) into the corpus so the next lookup is instant. */
    external fun nativeDictPut(dictHandle: Long, lang: String, headword: String, defn: String)

    /**
     * Install a user StarDict folder (KOReader-style) into the open corpus [dictHandle], tagging
     * every entry with [lang] (also used as the source id). [syn] = true imports a Moby-style
     * thesaurus bundle (synonym lists) instead of definitions. Returns the record count; throws on a
     * malformed/unreadable bundle. Runs IO + gzip — call off the UI thread.
     */
    external fun nativeDictImport(dictHandle: Long, stardictDir: String, lang: String, syn: Boolean): Int

    /** Uninstall a user dictionary: drop every entry + synonym tagged [lang]. Returns rows removed. */
    external fun nativeDictForget(dictHandle: Long, lang: String): Int

    /** Set pinch-zoom factor ([zoom] >= 1; 1 = fit) and normalized pan [0,1] (RR5-FR3). The next
     *  [nativeRenderPage] renders the magnified/panned view. */
    external fun nativeSetZoom(handle: Long, zoom: Float, panX: Float, panY: Float)

    /** Set the reflow text scale ([scale]; 1.0 = default font size) for an EPUB, repaginating and
     *  preserving the chapter (RR2-FR5). Returns the new current page index, or -1 for a
     *  fixed-layout document (PDF) that does not reflow. Re-render after calling. */
    external fun nativeSetTextScale(handle: Long, scale: Float): Int

    /** Set the display-enhancement contrast [step] (0 = off; RR4). Applied as a post-render pixel
     *  remap (works on PDF + EPUB); re-render after calling. */
    external fun nativeSetContrast(handle: Long, step: Int)

    /** Update the render viewport after a surface resize / screen rotation (RR21-FR4). Required so
     *  the core renders into the new buffer size (PDF re-renders, EPUB repaginates). Re-render after. */
    external fun nativeSetViewport(handle: Long, width: Int, height: Int, dpi: Int)

    /** Set the page fit mode (0=Page/contain, 1=Width, 2=Height; RR4). Aspect-preserving (PDF);
     *  EPUB ignores it. Re-render after calling. */
    external fun nativeSetFit(handle: Long, mode: Int)

    /** Auto-crop white margins (RR4). [auto] != 0 enables it; [marginStep] (0..8, 1%-of-page each)
     *  keeps a margin around the detected content. PDF only; re-render after. */
    external fun nativeSetCrop(handle: Long, auto: Int, marginStep: Int)

    /** Render quality (0=low, 1=default, 2=high; RR4). High supersamples for smoother text.
     *  Re-render after calling. */
    external fun nativeSetRenderQuality(handle: Long, quality: Int)

    /** Reflow line-spacing multiplier (RR4); repaginates EPUB. Returns the new page, or -1 for a
     *  fixed-layout PDF. Re-render after. */
    external fun nativeSetLineSpacing(handle: Long, mult: Float): Int

    /** Reflow alignment (0=Left, 1=Justify, 2=Center, 3=Right; RR4); repaginates EPUB. Returns the
     *  new page, or -1 for a fixed-layout PDF. Re-render after. */
    external fun nativeSetAlignment(handle: Long, code: Int): Int

    /** Whether the open document can be reflowed — a text-layer PDF (ADR-INKREAD-0011). Use to
     *  enable/disable the Reflow control (false for scanned PDFs and for EPUB, already reflowable). */
    external fun nativeSupportsReflow(handle: Long): Boolean

    /** Whether the CURRENT view honors zoom — a fixed-layout page that is not reflowed (RR25-FR3).
     *  Gate every zoom entry point (pinch / +− / double-tap) on this so a gesture on a reflowable
     *  view (EPUB, or a reflowed PDF) can't strand the shell's zoom factor and skew tap hit-testing. */
    external fun nativeIsMagnifiable(handle: Long): Boolean

    /** Toggle reflow mode on a text-layer PDF (ADR-INKREAD-0011): reconstructs the page text and flows
     *  it like a book so font/spacing/alignment take effect; off restores the fixed page. Returns the
     *  new current page index (page count changes across the toggle), or -1 if reflow is unavailable.
     *  Re-render after. */
    external fun nativeSetReflow(handle: Long, on: Boolean): Int

    // ---- ink annotation, persisted by the core to a sidecar (RR6/RR10 / ADR-INKREAD-0010) ----

    /** Attach a `.inkread` sidecar store for the open document so strokes persist (RR10). */
    external fun nativeAttachInkStore(handle: Long, docPath: String)

    /** Export every page's ink into the PDF at [outPath] (ADR-INKREAD-0005). [flatten] bakes the
     *  ink into the page content (visible in any viewer); false writes editable Ink annotations.
     *  Throws on failure. */
    external fun nativeExportPdf(handle: Long, outPath: String, flatten: Boolean)

    /** Begin a stroke. [tool] is the CORE tool code (0=Pen, 1=Highlighter, 2=Eraser); for the
     *  eraser [width] is the erase radius. [colorRgba] packs `(r<<24|g<<16|b<<8|a)`. */
    external fun nativeInkBeginStroke(handle: Long, tool: Int, colorRgba: Int, width: Float, createdAtMs: Long)

    /** Add a sample to the in-progress stroke (ink) or erase at the point (eraser). NaN tilt = absent. */
    external fun nativeInkAddPoint(handle: Long, x: Float, y: Float, pressure: Float, tiltX: Float, tiltY: Float, timestampMs: Int)

    /** Batched [nativeInkAddPoint]: [xy] is packed `[x0,y0,x1,y1,…]` (pressure 1, no tilt/timestamp).
     *  One JNI crossing per stroke instead of per point — cheaper on the annotation hot path. */
    external fun nativeInkAddPoints(handle: Long, xy: FloatArray)

    /** Commit the in-progress stroke / eraser gesture; autosaves the page only if it changed. */
    external fun nativeInkEndStroke(handle: Long)

    /** Opt into deferred-autosave mode: edits mark the page dirty instead of fsyncing on every
     *  stroke-end, and the shell flushes via [nativeInkSave] on a trailing-edge debounce. Off by
     *  default (save-on-stroke-end durability). A power/flash-wear knob. */
    external fun nativeInkSetDeferredAutosave(handle: Long, deferred: Boolean)

    /** Strokes on [page] in the draw-wire (decode with [WireCodec.decodeStrokes]) — for baking. */
    external fun nativeInkStrokesForDraw(handle: Long, page: Int): ByteArray

    /** Undo / redo the last ink edit on the current page (autosaves). Returns whether it changed. */
    external fun nativeInkUndo(handle: Long): Boolean
    external fun nativeInkRedo(handle: Long): Boolean

    /** Explicit flush for pause/close (complements the per-edit autosave). */
    external fun nativeInkSave(handle: Long)

    // ---- lasso selection over the current page's strokes (ADR-INKREAD-0010) ----

    /** Select strokes a lasso [polygon] (flat normalized [x0,y0,x1,y1,…]) encloses/crosses under
     *  [mode] (0=Smart, 1=Freehand). Returns the selected stroke ids. */
    external fun nativeInkSelectInPolygon(handle: Long, polygon: FloatArray, mode: Int): IntArray

    /** Every stroke id on the current page ("Select All"). */
    external fun nativeInkSelectAll(handle: Long): IntArray

    /** Selection bounds `[x0,y0,x1,y1]` (normalized), or empty if the selection is empty. */
    external fun nativeInkSelectionBounds(handle: Long, ids: IntArray): FloatArray

    /** Move the selection by normalized (dx,dy) (clamped on-page); autosaves. Returns changed. */
    external fun nativeInkMoveSelection(handle: Long, ids: IntArray, dx: Float, dy: Float): Boolean

    /** Delete / cut the selection (cut also copies to the clipboard). Returns the removed ids. */
    external fun nativeInkDeleteSelection(handle: Long, ids: IntArray): IntArray
    external fun nativeInkCutSelection(handle: Long, ids: IntArray): IntArray

    /** Recolor the selection to [colorRgba]. Returns whether anything changed. */
    external fun nativeInkRecolorSelection(handle: Long, ids: IntArray, colorRgba: Int): Boolean

    /** Copy the selection into the cross-page clipboard; returns the count. */
    external fun nativeInkCopySelection(handle: Long, ids: IntArray): Int

    /** Paste the clipboard onto the current page offset by normalized (dx,dy). Returns new ids. */
    external fun nativeInkPaste(handle: Long, dx: Float, dy: Float): IntArray

    /** Whether the clipboard holds strokes to paste (gates the Paste control). */
    external fun nativeInkHasClipboard(handle: Long): Boolean
}

/**
 * One stroke decoded from the draw-wire (ADR-INKREAD-0010) for baking onto the page. [points] is
 * interleaved **normalized** `[x0,y0,x1,y1,…]` in `[0,1]`; color channels are 0–255; [coreTool] is
 * the core tool code (0=Pen, 1=Highlighter).
 */
data class InkStrokeDraw(
    val id: Int,
    val coreTool: Int,
    val r: Int,
    val g: Int,
    val b: Int,
    val a: Int,
    val width: Float,
    val points: FloatArray,
)

/** One flattened table-of-contents entry (RR11-FR2). [targetPage] is null for a label-only entry. */
data class TocItem(val depth: Int, val targetPage: Int?, val title: String)

/**
 * A clickable link region on a page (RR11-FR3), normalized to `[0,1]` with a top-left origin.
 * Exactly one of [targetPage] (internal jump) / [uri] (external) is non-null.
 */
data class LinkRect(
    val x0: Float,
    val y0: Float,
    val x1: Float,
    val y1: Float,
    val targetPage: Int?,
    val uri: String?,
) {
    /** Whether the normalized tap point `(nx, ny)` falls inside this link. */
    fun contains(nx: Float, ny: Float): Boolean = nx in x0..x1 && ny in y0..y1
}

/** A normalized highlight box `[0,1]` of a [Selection] (RR11 / D3). */
data class SelBox(val x0: Float, val y0: Float, val x1: Float, val y1: Float)

/** A text selection: the selected string + the boxes to highlight (RR11 / D3). */
data class Selection(val text: String, val boxes: List<SelBox>) {
    val isEmpty: Boolean get() = text.isEmpty()

    /** The selection's normalized bounding rect `[x0,y0,x1,y1]` (union of all boxes), or null when
     *  empty — the rect handed to the reflow-stable anchor lookup (#46). */
    fun boundsNorm(): FloatArray? {
        if (boxes.isEmpty()) return null
        return floatArrayOf(
            boxes.minOf { it.x0 }, boxes.minOf { it.y0 },
            boxes.maxOf { it.x1 }, boxes.maxOf { it.y1 },
        )
    }
}

/** One in-document search match on a page (RR2): highlight [boxes] + a context [snippet]. */
data class SearchMatch(val boxes: List<SelBox>, val snippet: String)

/** A search match resolved to its [page] (the shell pairs a page index with each [SearchMatch]). */
data class SearchHit(val page: Int, val match: SearchMatch)

/** A dictionary lookup result (RR12 / D3); [found] is false on a miss (try online next). */
data class WordDefinition(
    val found: Boolean,
    val headword: String,
    val lang: String,
    val senses: List<String>,
    val synonyms: List<String>,
)

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

    /**
     * Decode the flattened pre-order TOC wire from [NativeBridge.nativeToc] (RR11-FR2). Layout:
     * `[ver][count: u16]` then per entry `[depth: u8][flags: u8][page: u32][len: u16][title…]`,
     * `flags` bit 0 = resolved target. Mirrors `encode_toc_wire` in `reader-core/document`.
     */
    fun decodeToc(bytes: ByteArray): List<TocItem> {
        require(bytes.size >= TOC_HEADER_LEN) { "toc stream truncated" }
        require(bytes[0] == WIRE_VERSION) { "bad toc wire version ${bytes[0]}" }
        val buf = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
        val count = buf.getShort(1).toInt() and 0xFFFF
        val out = ArrayList<TocItem>(count)
        var off = TOC_HEADER_LEN
        repeat(count) {
            require(bytes.size >= off + TOC_RECORD_FIXED) { "toc record truncated" }
            val depth = bytes[off].toInt() and 0xFF
            val hasTarget = (bytes[off + 1].toInt() and 1) == 1
            val page = buf.getInt(off + 2)
            val len = buf.getShort(off + 6).toInt() and 0xFFFF
            val titleStart = off + TOC_RECORD_FIXED
            require(bytes.size >= titleStart + len) { "toc title overruns stream" }
            val title = String(bytes, titleStart, len, Charsets.UTF_8)
            out.add(TocItem(depth, if (hasTarget) page else null, title))
            off = titleStart + len
        }
        return out
    }

    private const val TOC_HEADER_LEN = 3
    private const val TOC_RECORD_FIXED = 8 // depth(1)+flags(1)+page(4)+len(2)

    /**
     * Decode the page-links wire from [NativeBridge.nativePageLinks] (RR11-FR3). Layout:
     * `[ver][count: u16]` then per link `[x0 f32][y0 f32][x1 f32][y1 f32][kind: u8]` + either
     * `[page: u32]` (kind 0) or `[len: u16][uri…]` (kind 1). Mirrors `encode_links_wire`.
     */
    fun decodeLinks(bytes: ByteArray): List<LinkRect> {
        require(bytes.size >= LINKS_HEADER_LEN) { "links stream truncated" }
        require(bytes[0] == WIRE_VERSION) { "bad links wire version ${bytes[0]}" }
        val buf = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
        val count = buf.getShort(1).toInt() and 0xFFFF
        val out = ArrayList<LinkRect>(count)
        var off = LINKS_HEADER_LEN
        repeat(count) {
            require(bytes.size >= off + LINKS_RECORD_FIXED) { "link record truncated" }
            val x0 = buf.getFloat(off)
            val y0 = buf.getFloat(off + 4)
            val x1 = buf.getFloat(off + 8)
            val y1 = buf.getFloat(off + 12)
            val kind = bytes[off + 16].toInt() and 0xFF
            off += LINKS_RECORD_FIXED
            if (kind == 0) {
                require(bytes.size >= off + 4) { "internal link target truncated" }
                val page = buf.getInt(off)
                off += 4
                out.add(LinkRect(x0, y0, x1, y1, page, null))
            } else {
                require(bytes.size >= off + 2) { "external link length truncated" }
                val len = buf.getShort(off).toInt() and 0xFFFF
                off += 2
                require(bytes.size >= off + len) { "external link uri truncated" }
                val uri = String(bytes, off, len, Charsets.UTF_8)
                off += len
                out.add(LinkRect(x0, y0, x1, y1, null, uri))
            }
        }
        return out
    }

    private const val LINKS_HEADER_LEN = 3
    private const val LINKS_RECORD_FIXED = 17 // x0,y0,x1,y1 (4×4) + kind(1)

    /**
     * Decode the selection wire from [NativeBridge.nativeWordAt]/[NativeBridge.nativeTextInRect]
     * (RR11 / D3): `[ver][textLen u16][text][boxCount u16]` then per box `[x0,y0,x1,y1 f32]`.
     * Mirrors `encode_selection_wire` in `reader-core/document`.
     */
    fun decodeSelection(bytes: ByteArray): Selection {
        require(bytes.size >= 3) { "selection stream truncated" }
        require(bytes[0] == WIRE_VERSION) { "bad selection wire version ${bytes[0]}" }
        val buf = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
        val tlen = buf.getShort(1).toInt() and 0xFFFF
        var off = 3
        require(bytes.size >= off + tlen) { "selection text overruns" }
        val text = String(bytes, off, tlen, Charsets.UTF_8)
        off += tlen
        require(bytes.size >= off + 2) { "selection box count truncated" }
        val count = buf.getShort(off).toInt() and 0xFFFF
        off += 2
        val boxes = ArrayList<SelBox>(count)
        repeat(count) {
            require(bytes.size >= off + 16) { "selection box truncated" }
            boxes.add(SelBox(buf.getFloat(off), buf.getFloat(off + 4), buf.getFloat(off + 8), buf.getFloat(off + 12)))
            off += 16
        }
        return Selection(text, boxes)
    }

    /**
     * Decode the search wire from [NativeBridge.nativeSearchPage] (RR2): `[ver][matchCount u16]`
     * then per match `[snippetLen u16][snippet][boxCount u16]` then per box `[x0,y0,x1,y1 f32]`.
     * Mirrors `encode_search_wire` in `reader-core/document`.
     */
    fun decodeSearch(bytes: ByteArray): List<SearchMatch> {
        require(bytes.size >= 3) { "search stream truncated" }
        require(bytes[0] == WIRE_VERSION) { "bad search wire version ${bytes[0]}" }
        val buf = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
        val matchCount = buf.getShort(1).toInt() and 0xFFFF
        var off = 3
        val out = ArrayList<SearchMatch>(matchCount)
        repeat(matchCount) {
            require(bytes.size >= off + 2) { "snippet length truncated" }
            val slen = buf.getShort(off).toInt() and 0xFFFF
            off += 2
            require(bytes.size >= off + slen) { "snippet overruns" }
            val snippet = String(bytes, off, slen, Charsets.UTF_8)
            off += slen
            require(bytes.size >= off + 2) { "box count truncated" }
            val count = buf.getShort(off).toInt() and 0xFFFF
            off += 2
            val boxes = ArrayList<SelBox>(count)
            repeat(count) {
                require(bytes.size >= off + 16) { "search box truncated" }
                boxes.add(SelBox(buf.getFloat(off), buf.getFloat(off + 4), buf.getFloat(off + 8), buf.getFloat(off + 12)))
                off += 16
            }
            out.add(SearchMatch(boxes, snippet))
        }
        return out
    }

    /**
     * Decode the definition wire from [NativeBridge.nativeDefine] (RR12 / D3): `[ver][found u8]`;
     * when found, `[hwLen u16][hw][langLen u8][lang][senseCount u16][senses…][synCount u16][syns…]`.
     * Mirrors `encode_definition_wire` in `reader-core/dict`.
     */
    fun decodeDefinition(bytes: ByteArray): WordDefinition {
        require(bytes.size >= 2) { "definition stream truncated" }
        require(bytes[0] == WIRE_VERSION) { "bad definition wire version ${bytes[0]}" }
        if (bytes[1].toInt() == 0) return WordDefinition(false, "", "", emptyList(), emptyList())
        val buf = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
        var off = 2
        fun str16(): String {
            require(bytes.size >= off + 2) { "string length truncated" }
            val n = buf.getShort(off).toInt() and 0xFFFF
            off += 2
            require(bytes.size >= off + n) { "string overruns" }
            val s = String(bytes, off, n, Charsets.UTF_8)
            off += n
            return s
        }
        fun list(): List<String> {
            require(bytes.size >= off + 2) { "list count truncated" }
            val c = buf.getShort(off).toInt() and 0xFFFF
            off += 2
            return ArrayList<String>(c).apply { repeat(c) { add(str16()) } }
        }
        val headword = str16()
        require(bytes.size >= off + 1) { "lang length truncated" }
        val llen = bytes[off].toInt() and 0xFF
        off += 1
        require(bytes.size >= off + llen) { "lang overruns" }
        val lang = String(bytes, off, llen, Charsets.UTF_8)
        off += llen
        val senses = list()
        val synonyms = list()
        return WordDefinition(true, headword, lang, senses, synonyms)
    }

    /**
     * Decode the ink draw-wire from [NativeBridge.nativeInkStrokesForDraw] (ADR-INKREAD-0010):
     * `[ver][count u16]` then per stroke `[id u32][tool u8][rgba u32][width f32][nPoints u16]` and
     * `nPoints × [x f32][y f32]`. Mirrors `encode_strokes_draw_wire` in `reader-core/ink_wire`.
     */
    fun decodeStrokes(bytes: ByteArray): List<InkStrokeDraw> {
        if (bytes.size < 3 || bytes[0] != WIRE_VERSION) return emptyList()
        val buf = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
        val count = buf.getShort(1).toInt() and 0xFFFF
        val out = ArrayList<InkStrokeDraw>(count)
        var off = 3
        repeat(count) {
            if (bytes.size < off + 11) return out // [id4][tool1][rgba4][width4][n2] = 15; guard below
            val id = buf.getInt(off); off += 4
            val tool = bytes[off].toInt() and 0xFF; off += 1
            val rgba = buf.getInt(off); off += 4
            val width = buf.getFloat(off); off += 4
            if (bytes.size < off + 2) return out
            val n = buf.getShort(off).toInt() and 0xFFFF; off += 2
            if (bytes.size < off + n * 8) return out
            val pts = FloatArray(n * 2)
            for (i in 0 until n) {
                pts[i * 2] = buf.getFloat(off); off += 4
                pts[i * 2 + 1] = buf.getFloat(off); off += 4
            }
            val r = (rgba ushr 24) and 0xFF
            val g = (rgba ushr 16) and 0xFF
            val b = (rgba ushr 8) and 0xFF
            val a = rgba and 0xFF
            out.add(InkStrokeDraw(id, tool, r, g, b, a, width, pts))
        }
        return out
    }
}
