package dev.jraghavan.inkread

import android.app.Activity
import android.app.AlertDialog
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.os.Bundle
import android.util.Log
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.widget.Toast
import dev.jraghavan.inkread.eink.EinkAdapter
import dev.jraghavan.inkread.eink.SupernoteEinkAdapter
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.Executors

/**
 * The M0/M1a reader Activity (RR1-FR2, RR21) — **DEVICE-UNVERIFIED**.
 *
 * Owns the [SurfaceView], drives the JNI round-trip (init → open → render → blit), and on a
 * tap forwards a [Gesture] to the core, then hands the returned [RefreshCommand] stream to
 * the [EinkAdapter] for execution. The Rust core owns all document/policy logic; this shell
 * only marshals + presents (IR-1/IR-2).
 *
 * ## Threading (RR21-FR4 / RR24): engine calls off the UI thread
 * Opening + rendering a PDF (pdfium) can take seconds on an image-heavy page; doing it on the
 * main thread froze the app (ANR-precursor). All document/engine work runs on a **single**
 * background executor (`engine`) — single-threaded so pdfium access is serialized (the core
 * assumes one worker thread). The UI thread only enqueues tasks and shows a quick "Loading…"
 * frame. Engine-thread-only fields ([docHandle], [bitmap], [renderBuffer], [viewW]/[viewH]) are
 * touched solely on that thread; a re-entrant close is safe (Amendment 2).
 */
class ReaderActivity : Activity(), SurfaceHolder.Callback {

    private lateinit var surfaceView: SurfaceView
    private val adapter: EinkAdapter = SupernoteEinkAdapter()

    /** Single worker thread for all engine/JNI/document work (serialized per RR21). */
    private val engine = Executors.newSingleThreadExecutor { r -> Thread(r, "inkread-engine") }

    // ---- engine-thread-only state ----
    private var docHandle: Long = 0L
    private var bitmap: Bitmap? = null
    private var renderBuffer: ByteBuffer? = null
    private var viewW = 0
    private var viewH = 0

    private val loadingBg = Paint().apply { color = Color.WHITE }
    private val loadingText = Paint().apply {
        color = Color.DKGRAY
        textSize = 48f
        isAntiAlias = true
        textAlign = Paint.Align.CENTER
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Prove the JNI boundary up front (RR1-AC2). Cheap; fine on the UI thread.
        Log.i(TAG, "core: ${NativeBridge.nativeHello()}")

        surfaceView = SurfaceView(this)
        surfaceView.holder.addCallback(this)
        surfaceView.setOnTouchListener { _, event -> onSurfaceTouch(event) }
        setContentView(surfaceView)
    }

    // ---- SurfaceHolder lifecycle → core (RR21-FR4) ----

    override fun surfaceCreated(holder: SurfaceHolder) { /* size arrives in surfaceChanged */ }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        // Hand the slow open+render to the engine thread; show feedback immediately.
        engine.execute { onSurfaceSized(width, height) }
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) { /* keep the doc open across resizes */ }

    override fun onPause() {
        super.onPause()
        // Persist the reading position when backgrounded (RR27) — async on the engine thread.
        engine.execute { savePosition() }
    }

    override fun onDestroy() {
        engine.execute { closeDocument() }
        engine.shutdown() // lets the queued close run, then stops the worker
        super.onDestroy()
    }

    // ---- engine-thread work ----

    private fun onSurfaceSized(width: Int, height: Int) {
        viewW = width
        viewH = height
        bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        // A direct, tightly-packed RGBA buffer the core renders into (Fork 4 / Amendment 5).
        renderBuffer = ByteBuffer.allocateDirect(width * height * 4).order(ByteOrder.LITTLE_ENDIAN)

        drawLoading() // quick feedback while the (slow) open runs
        openDocumentIfNeeded()
        renderAndBlit()
    }

    private fun openDocumentIfNeeded() {
        if (docHandle != 0L) return
        val caps = adapter.capabilities()
        val capsBytes = WireCodec.encodeCapabilities(caps)
        NativeBridge.nativeInit(capsBytes)

        // M0 opens a fixed sample placed on-device; the SAF/library path is RR22 (M1a.6).
        // External files dir first, then internal filesDir — the latter is adb/`run-as`-writable
        // under Android 11 scoped storage, so a bring-up PDF can be placed without SAF.
        val sample = listOf(
            File(getExternalFilesDir(null), "sample.pdf"),
            File(filesDir, "sample.pdf"),
        ).firstOrNull(File::exists)
        if (sample == null) {
            Log.w(TAG, "no sample.pdf in external/internal files dir; nothing to open (M0 bring-up)")
            return
        }
        // Open with a SQLite store so the reading position + e-ink settings persist and the
        // saved page resumes on reopen (RR12 / RR27). The store lives under app storage; the
        // book identity is the file name (the library will key by a real id in RR26).
        val dbPath = File(filesDir, "reader.db").absolutePath
        docHandle = try {
            NativeBridge.nativeOpenDocumentWithStore(
                sample.absolutePath, capsBytes, viewW, viewH, DPI, dbPath, sample.name,
            )
        } catch (e: RuntimeException) {
            Log.e(TAG, "open failed: ${e.message}")
            0L
        }
        if (docHandle != 0L) {
            Log.i(
                TAG,
                "opened ${sample.name}: ${NativeBridge.nativePageCount(docHandle)} pages, " +
                    "resumed at page ${NativeBridge.nativeCurrentPage(docHandle)}",
            )
        }
    }

    private fun renderAndBlit() {
        val handle = docHandle
        val buf = renderBuffer ?: return
        val bmp = bitmap ?: return
        if (handle == 0L) return

        try {
            buf.clear()
            NativeBridge.nativeRenderPage(handle, buf)
        } catch (e: RuntimeException) {
            Log.e(TAG, "render failed: ${e.message}")
            return
        }
        buf.rewind()
        bmp.copyPixelsFromBuffer(buf)
        blit { canvas -> canvas.drawBitmap(bmp, 0f, 0f, null) }
    }

    /** Draw a centered "Loading…" frame so the open doesn't look like a freeze. */
    private fun drawLoading() {
        blit { canvas ->
            canvas.drawColor(Color.WHITE)
            canvas.drawRect(0f, 0f, canvas.width.toFloat(), canvas.height.toFloat(), loadingBg)
            canvas.drawText("Loading…", canvas.width / 2f, canvas.height / 2f, loadingText)
        }
    }

    /** Lock the surface, run [draw], and post — null-safe across surface destroy (any thread). */
    private inline fun blit(draw: (Canvas) -> Unit) {
        val holder = surfaceView.holder
        val canvas: Canvas =
            try {
                holder.lockCanvas() ?: return
            } catch (_: IllegalStateException) {
                return // surface went away mid-blit
            }
        try {
            draw(canvas)
        } finally {
            runCatching { holder.unlockCanvasAndPost(canvas) }
        }
    }

    // ---- input (UI thread) → engine ----

    private fun onSurfaceTouch(event: MotionEvent): Boolean {
        if (event.action != MotionEvent.ACTION_UP) return true
        // Tap zones (RR25-FR3): left third = prev, right third = next, center third = contents.
        val third = surfaceView.width / 3f
        when {
            event.x < third -> postGesture(Gesture.PREV_PAGE)
            event.x > 2 * third -> postGesture(Gesture.NEXT_PAGE)
            else -> openTocMenu()
        }
        return true
    }

    /** Apply a page-turn gesture on the engine thread, then render + refresh (RR25). */
    private fun postGesture(gesture: Gesture) {
        engine.execute {
            if (docHandle == 0L) return@execute
            val commandBytes = try {
                NativeBridge.nativeOnGesture(docHandle, gesture.code)
            } catch (e: RuntimeException) {
                Log.e(TAG, "gesture failed: ${e.message}")
                return@execute
            }
            renderAndBlit()
            // Execute the policy's refresh stream on the panel (RR2-FR3).
            adapter.executeAll(WireCodec.decodeCommands(commandBytes))
        }
    }

    /** Fetch the TOC on the engine thread, then show the chooser on the UI thread (RR11-FR2). */
    private fun openTocMenu() {
        engine.execute {
            if (docHandle == 0L) return@execute
            val toc = try {
                WireCodec.decodeToc(NativeBridge.nativeToc(docHandle))
            } catch (e: RuntimeException) {
                Log.e(TAG, "toc failed: ${e.message}")
                return@execute
            }
            runOnUiThread { showTocDialog(toc) }
        }
    }

    private fun showTocDialog(toc: List<TocItem>) {
        if (toc.isEmpty()) {
            Toast.makeText(this, "No contents in this document", Toast.LENGTH_SHORT).show()
            return
        }
        // Indent by depth; show the 1-based page for resolvable entries (RR11-FR2).
        val labels = toc.map { item ->
            val indent = "    ".repeat(item.depth)
            val page = item.targetPage?.let { "  ·  p${it + 1}" } ?: ""
            indent + item.title + page
        }.toTypedArray()

        AlertDialog.Builder(this)
            .setTitle("Contents")
            .setItems(labels) { _, which ->
                val page = toc[which].targetPage ?: return@setItems // label-only: no jump
                engine.execute {
                    if (docHandle == 0L) return@execute
                    val commandBytes = try {
                        NativeBridge.nativeJumpToPage(docHandle, page)
                    } catch (e: RuntimeException) {
                        Log.e(TAG, "jump failed: ${e.message}")
                        return@execute
                    }
                    renderAndBlit()
                    adapter.executeAll(WireCodec.decodeCommands(commandBytes))
                }
            }
            .show()
    }

    /** Persist the current reading position (RR12-FR3 / RR27); store-less / closed = no-op. */
    private fun savePosition() {
        if (docHandle == 0L) return
        try {
            NativeBridge.nativeSavePosition(docHandle)
        } catch (e: RuntimeException) {
            Log.e(TAG, "save position failed: ${e.message}")
        }
    }

    private fun closeDocument() {
        val h = docHandle
        docHandle = 0L // zero BEFORE the call so a re-entrant close is a no-op (Amendment 2)
        if (h == 0L) return
        try {
            NativeBridge.nativeSavePosition(h) // last-chance save before teardown (RR27)
        } catch (e: RuntimeException) {
            Log.e(TAG, "save position failed: ${e.message}")
        }
        NativeBridge.nativeCloseDocument(h)
    }

    private companion object {
        const val TAG = "ReaderActivity"
        const val DPI = 226 // Supernote-class panel density (approx); refined per device.
    }
}
