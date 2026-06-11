package dev.jraghavan.inkread

import android.app.Activity
import android.graphics.Bitmap
import android.graphics.Canvas
import android.os.Bundle
import android.util.Log
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import dev.jraghavan.inkread.eink.EinkAdapter
import dev.jraghavan.inkread.eink.SupernoteEinkAdapter
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * The M0 reader Activity (RR1-FR2, RR21) — **DEVICE-UNVERIFIED**.
 *
 * Owns the [SurfaceView], drives the JNI round-trip (init → open → render → blit), and on a
 * tap forwards a [Gesture] to the core, then hands the returned [RefreshCommand] stream to
 * the [EinkAdapter] for execution. The Rust core owns all document/policy logic; this shell
 * only marshals + presents (IR-1/IR-2).
 *
 * Handle discipline (Amendment 2): [docHandle] is zeroed on close so a double-close is safe.
 */
class ReaderActivity : Activity(), SurfaceHolder.Callback {

    private lateinit var surfaceView: SurfaceView
    private val adapter: EinkAdapter = SupernoteEinkAdapter()

    private var docHandle: Long = 0L
    private var bitmap: Bitmap? = null
    private var renderBuffer: ByteBuffer? = null
    private var viewW = 0
    private var viewH = 0

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Prove the JNI boundary up front (RR1-AC2).
        Log.i(TAG, "core: ${NativeBridge.nativeHello()}")

        surfaceView = SurfaceView(this)
        surfaceView.holder.addCallback(this)
        surfaceView.setOnTouchListener { _, event -> onSurfaceTouch(event) }
        setContentView(surfaceView)
    }

    // ---- SurfaceHolder lifecycle → core (RR21-FR4) ----

    override fun surfaceCreated(holder: SurfaceHolder) { /* size arrives in surfaceChanged */ }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        viewW = width
        viewH = height
        bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        // A direct, tightly-packed RGBA buffer the core renders into (Fork 4 / Amendment 5).
        renderBuffer = ByteBuffer.allocateDirect(width * height * 4).order(ByteOrder.LITTLE_ENDIAN)

        openDocumentIfNeeded()
        renderAndBlit()
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) { /* keep the doc open across resizes */ }

    override fun onDestroy() {
        closeDocument()
        super.onDestroy()
    }

    // ---- the round-trip ----

    private fun openDocumentIfNeeded() {
        if (docHandle != 0L) return
        val caps = adapter.capabilities()
        val capsBytes = WireCodec.encodeCapabilities(caps)
        NativeBridge.nativeInit(capsBytes)

        // M0 opens a fixed sample placed on-device; the SAF/library path is RR22 (M1a).
        val sample = File(getExternalFilesDir(null), "sample.pdf")
        if (!sample.exists()) {
            Log.w(TAG, "no sample.pdf in ${sample.parent}; nothing to open (M0 bring-up)")
            return
        }
        docHandle = try {
            NativeBridge.nativeOpenDocument(sample.absolutePath, capsBytes, viewW, viewH, DPI)
        } catch (e: RuntimeException) {
            Log.e(TAG, "open failed: ${e.message}")
            0L
        }
        if (docHandle != 0L) {
            Log.i(TAG, "opened ${sample.name}: ${NativeBridge.nativePageCount(docHandle)} pages")
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
        // Copy the rendered RGBA into the bitmap and blit to the surface.
        buf.rewind()
        bmp.copyPixelsFromBuffer(buf)
        val canvas: Canvas = surfaceView.holder.lockCanvas() ?: return
        try {
            canvas.drawBitmap(bmp, 0f, 0f, null)
        } finally {
            surfaceView.holder.unlockCanvasAndPost(canvas)
        }
    }

    private fun onSurfaceTouch(event: MotionEvent): Boolean {
        if (event.action != MotionEvent.ACTION_UP) return true
        if (docHandle == 0L) return true
        // Left third = prev, right two-thirds = next (RR25-FR3 tap zones, simplified for M0).
        val gesture = if (event.x < viewW / 3f) Gesture.PREV_PAGE else Gesture.NEXT_PAGE

        val commandBytes = try {
            NativeBridge.nativeOnGesture(docHandle, gesture.code)
        } catch (e: RuntimeException) {
            Log.e(TAG, "gesture failed: ${e.message}")
            return true
        }
        renderAndBlit()
        // Execute the policy's refresh stream on the panel (RR2-FR3).
        adapter.executeAll(WireCodec.decodeCommands(commandBytes))
        return true
    }

    private fun closeDocument() {
        val h = docHandle
        docHandle = 0L // zero BEFORE the call so a re-entrant close is a no-op (Amendment 2)
        if (h != 0L) NativeBridge.nativeCloseDocument(h)
    }

    private companion object {
        const val TAG = "ReaderActivity"
        const val DPI = 226 // Supernote-class panel density (approx); refined per device.
    }
}
