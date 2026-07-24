package dev.jraghavan.inkread.eink

import android.graphics.Rect
import android.os.Build
import android.view.SurfaceView
import com.onyx.android.sdk.api.device.epd.EpdController
import com.onyx.android.sdk.data.note.TouchPoint
import com.onyx.android.sdk.pen.RawInputCallback
import com.onyx.android.sdk.pen.TouchHelper
import com.onyx.android.sdk.pen.data.TouchPointList
import com.onyx.android.sdk.rx.RxManager
import org.lsposed.hiddenapibypass.HiddenApiBypass

/**
 * BOOX firmware/native wet-ink client.
 *
 * [TouchHelper] owns the live stroke so handwriting follows the nib without waiting for the
 * application's render loop. The same raw samples are copied out through [Listener] after pen-up;
 * the reader can then map them into its vendor-neutral Rust ink model.
 *
 * Vendor code deliberately stays in the Android shell, matching [SupernoteInk].
 */
class BooxInk(
    private val surfaceView: SurfaceView,
    private val listener: Listener,
) {
    data class Sample(
        val x: Float,
        val y: Float,
        val pressure: Float,
    )

    interface Listener {
        fun onBooxPenStroke(samples: List<Sample>)
        fun onBooxEraserGesture(samples: List<Sample>)
        fun onBooxInkStatus(message: String) {}
    }

    private var touchHelper: TouchHelper? = null
    private var started = false

    private val pendingPenPoints = ArrayList<TouchPoint>(2048)
    private var batchedPenPoints: List<TouchPoint>? = null
    private var penFromTouch = false

    private val pendingEraserPoints = ArrayList<TouchPoint>(2048)
    private var batchedEraserPoints: List<TouchPoint>? = null
    private var eraserFromTouch = false

    private val callback = object : RawInputCallback() {
        override fun onBeginRawDrawing(fromTouch: Boolean, touchPoint: TouchPoint?) {
            pendingPenPoints.clear()
            batchedPenPoints = null
            penFromTouch = fromTouch
            if (fromTouch) return
            touchPoint?.let { pendingPenPoints += TouchPoint(it) }
            listener.onBooxInkStatus("BOOX pen down")
        }

        override fun onRawDrawingTouchPointMoveReceived(touchPoint: TouchPoint?) {
            if (penFromTouch) return
            // Some BOOX firmware versions intermittently omit the final cumulative-list callback.
            // Keep per-move samples as a fallback, but prefer the SDK list when present.
            touchPoint?.let { pendingPenPoints += TouchPoint(it) }
        }

        override fun onRawDrawingTouchPointListReceived(touchPointList: TouchPointList?) {
            if (penFromTouch) return
            val raw = touchPointList?.points.orEmpty()
            if (raw.isNotEmpty()) batchedPenPoints = raw.map { TouchPoint(it) }
        }

        override fun onEndRawDrawing(fromTouch: Boolean, touchPoint: TouchPoint?) {
            val ignore = penFromTouch || fromTouch
            penFromTouch = false
            if (ignore) {
                pendingPenPoints.clear()
                batchedPenPoints = null
                return
            }

            val snapshot = mergedSequence(pendingPenPoints, batchedPenPoints, touchPoint)
            pendingPenPoints.clear()
            batchedPenPoints = null
            if (snapshot.isEmpty()) return

            surfaceView.post {
                listener.onBooxPenStroke(snapshot.map(::toSample))
            }
        }

        override fun onBeginRawErasing(fromTouch: Boolean, touchPoint: TouchPoint?) {
            pendingEraserPoints.clear()
            batchedEraserPoints = null
            eraserFromTouch = fromTouch
            if (fromTouch) return
            touchPoint?.let { pendingEraserPoints += TouchPoint(it) }
            listener.onBooxInkStatus("BOOX eraser down")
        }

        override fun onRawErasingTouchPointMoveReceived(touchPoint: TouchPoint?) {
            if (eraserFromTouch) return
            touchPoint?.let { pendingEraserPoints += TouchPoint(it) }
        }

        override fun onRawErasingTouchPointListReceived(touchPointList: TouchPointList?) {
            if (eraserFromTouch) return
            val raw = touchPointList?.points.orEmpty()
            if (raw.isNotEmpty()) batchedEraserPoints = raw.map { TouchPoint(it) }
        }

        override fun onEndRawErasing(fromTouch: Boolean, touchPoint: TouchPoint?) {
            val ignore = eraserFromTouch || fromTouch
            eraserFromTouch = false
            if (ignore) {
                pendingEraserPoints.clear()
                batchedEraserPoints = null
                return
            }

            val snapshot = mergedSequence(pendingEraserPoints, batchedEraserPoints, touchPoint)
            pendingEraserPoints.clear()
            batchedEraserPoints = null
            if (snapshot.isEmpty()) return

            surfaceView.post {
                listener.onBooxEraserGesture(snapshot.map(::toSample))
            }
        }

        override fun onPenActive(touchPoint: TouchPoint?) = Unit
    }

    /**
     * Build one portable sequence without losing SDK begin/end samples.
     *
     * BOOX can deliver a cumulative list for the middle of the stroke, while begin/end arrive in
     * separate callbacks. When the list is present we use it as the authoritative middle, but still
     * prepend the begin sample and append the pen-up sample when they are not already present.
     */
    private fun mergedSequence(
        pending: List<TouchPoint>,
        batch: List<TouchPoint>?,
        end: TouchPoint?,
    ): List<TouchPoint> {
        val middle = batch.orEmpty()
        val out = ArrayList<TouchPoint>((if (middle.isNotEmpty()) middle.size else pending.size) + 2)

        if (middle.isNotEmpty()) {
            pending.firstOrNull()?.let { appendDistinct(out, it) }
            middle.forEach { appendDistinct(out, it) }
        } else {
            pending.forEach { appendDistinct(out, it) }
        }
        end?.let { appendDistinct(out, it) }
        return out
    }

    private fun appendDistinct(out: MutableList<TouchPoint>, point: TouchPoint) {
        val copy = TouchPoint(point)
        val last = out.lastOrNull()
        if (last == null || last.x != copy.x || last.y != copy.y || last.pressure != copy.pressure) {
            out += copy
        }
    }

    /** Start BOOX raw drawing over the visible reader surface. Safe to call more than once. */
    fun setup(): Boolean {
        if (started) return true
        val limit = Rect()
        if (!surfaceView.getLocalVisibleRect(limit) || limit.isEmpty) {
            listener.onBooxInkStatus("BOOX ink surface is not laid out yet")
            return false
        }

        return try {
            // Current Onyx SDK code reflects hidden framework APIs. Install the exemption before
            // TouchHelper initialization, matching known-good BOOX implementations.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                runCatching { HiddenApiBypass.addHiddenApiExemptions("") }
            }
            runCatching { RxManager.Builder.initAppContext(surfaceView.context.applicationContext) }

            val helper = TouchHelper.create(surfaceView, callback)
            helper.setRawDrawingEnabled(false)
            helper.closeRawDrawing()
            helper.setStrokeWidth(STROKE_WIDTH_PX)
            helper.setLimitRect(mutableListOf(limit))
                .setExcludeRect(emptyList())
                .openRawDrawing()
            helper.enableSideBtnErase(true)
            helper.setBrushRawDrawingEnabled(true)
            helper.setEraserRawDrawingEnabled(true, ERASER_WIDTH)
            helper.setStrokeStyle(TouchHelper.STROKE_STYLE_FOUNTAIN)
            helper.setRawDrawingRenderEnabled(true)
            runCatching { EpdController.enablePost(1) }
            helper.setRawDrawingEnabled(true)

            touchHelper = helper
            started = true
            listener.onBooxInkStatus("BOOX TouchHelper active: native render + portable capture")
            true
        } catch (t: Throwable) {
            listener.onBooxInkStatus("BOOX TouchHelper failed: ${t.javaClass.simpleName}: ${t.message}")
            false
        }
    }

    /** Enable/disable BOOX firmware drawing without destroying the adapter. */
    fun setWritable(enabled: Boolean) {
        runCatching { touchHelper?.setRawDrawingEnabled(enabled) }
            .onFailure { listener.onBooxInkStatus("BOOX ink toggle failed: ${it.message}") }
    }

    /** Release the firmware/native drawing path. */
    fun teardown() {
        if (!started) return
        runCatching { touchHelper?.setRawDrawingEnabled(false) }
        runCatching { touchHelper?.closeRawDrawing() }
            .onFailure { listener.onBooxInkStatus("BOOX ink close failed: ${it.message}") }
        touchHelper = null
        started = false
    }

    private fun toSample(point: TouchPoint): Sample {
        val rawPressure = point.pressure
        val maxPressure = runCatching { EpdController.getMaxTouchPressure().toFloat() }
            .getOrDefault(DEFAULT_MAX_PRESSURE)
            .coerceAtLeast(1f)
        val pressure = if (rawPressure > 1f) rawPressure / maxPressure else rawPressure
        return Sample(
            x = point.x,
            y = point.y,
            pressure = pressure.coerceIn(0f, 1f),
        )
    }

    companion object {
        private const val STROKE_WIDTH_PX = 3f
        private const val ERASER_WIDTH = 5
        private const val DEFAULT_MAX_PRESSURE = 4095f

        /** BOOX firmware commonly reports Onyx/BOOX across manufacturer/brand fields. */
        fun isBooxDevice(): Boolean {
            val identity = "${Build.MANUFACTURER} ${Build.BRAND} ${Build.DEVICE}".lowercase()
            return "onyx" in identity || "boox" in identity
        }
    }
}
