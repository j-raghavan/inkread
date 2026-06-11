package dev.jraghavan.inkread.penspike

import android.app.Activity
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.RectF
import android.os.Bundle
import android.os.SystemClock
import android.util.Log
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View

const val TAG = "PenSpike"

/**
 * RR19-FR4b pen-latency spike — the standalone measurement Activity.
 *
 * A full-screen [SurfaceView]: capture stylus [MotionEvent]s (incl. getHistorical* batched
 * samples), ignore finger/palm (RR19-FR7), draw each segment, and measure the
 * software-observable nib-to-ink delta (eventTime → surface-post return). Cycle through the
 * four routes; each logs `ROUTE x: <reachable|FAILED reason>` + latency stats to logcat
 * (tag "PenSpike") and appends to pen-latency.csv.
 *
 * Routes (RR19-FR3 / runbook §Routes):
 *   R1 auto compositor A2 — plain Canvas draw, no API; rely on einkhwc A2-on-touch.
 *   R2 DrawService        — bind com.ratta.DrawService, report descriptor.
 *   R3 /dev/ebc           — native open + A2 partial ioctl per stroke (the highest-value proof).
 *   R4 standard baseline  — plain draw with no fast intent (the pen_low_latency=false floor).
 */
class PenSpikeActivity : Activity(), SurfaceHolder.Callback {

    private enum class Route(val label: String) {
        R1("R1 auto-A2 (SurfaceView, no API)"),
        R2("R2 DrawService bind"),
        R3("R3 /dev/ebc A2 ioctl"),
        R4("R4 standard baseline"),
    }

    private lateinit var surfaceView: SurfaceView
    private var holder: SurfaceHolder? = null
    private var hasSurface = false

    private var route = Route.R1
    private val stats = HashMap<Route, LatencyStats>().apply {
        Route.values().forEach { put(it, LatencyStats()) }
    }
    private lateinit var csv: CsvLogger
    private var drawServiceProbe: DrawServiceProbe? = null
    private var r3Open = false

    // Stroke geometry for the inked path + the dirty bbox we hand to the panel.
    private var lastX = -1f
    private var lastY = -1f
    private val strokePaint = Paint().apply {
        color = Color.BLACK
        strokeWidth = 3f
        style = Paint.Style.STROKE
        isAntiAlias = false // 1-bit-friendly for A2
        strokeCap = Paint.Cap.ROUND
    }
    private val overlayBg = Paint().apply { color = Color.WHITE }
    private val overlayText = Paint().apply {
        color = Color.BLACK
        textSize = 34f
        isAntiAlias = true
    }
    private val legendDivider = Paint().apply { color = Color.LTGRAY; strokeWidth = 1f }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        csv = CsvLogger(getExternalFilesDir(null))
        Log.i(TAG, "=== PenSpike start === csv=${csv.path()} ebcLib=${EbcNative.available}")

        surfaceView = SurfaceView(this)
        surfaceView.holder.addCallback(this)
        // Tap the top-of-screen legend band to cycle routes; draw below it.
        surfaceView.setOnClickListener(null)
        setContentView(surfaceView)
        drawServiceProbe = DrawServiceProbe(this)
    }

    override fun surfaceCreated(h: SurfaceHolder) {
        holder = h; hasSurface = true
        clearAndDrawLegend()
        Log.i(TAG, "surfaceCreated; active=${route.label}")
    }

    override fun surfaceChanged(h: SurfaceHolder, format: Int, width: Int, height: Int) {
        holder = h; hasSurface = true
        clearAndDrawLegend()
    }

    override fun surfaceDestroyed(h: SurfaceHolder) {
        hasSurface = false; holder = null
    }

    private val legendHeight = 150
    private fun inLegend(y: Float) = y < legendHeight

    override fun onTouchEvent(event: MotionEvent): Boolean {
        // Tapping the legend band cycles routes (and runs the route's one-shot setup probe).
        if (event.actionMasked == MotionEvent.ACTION_DOWN && inLegend(event.y)) {
            cycleRoute()
            return true
        }

        // RR19-FR7: only stylus inks; ignore finger/palm.
        val ti = event.getToolType(0)
        if (ti != MotionEvent.TOOL_TYPE_STYLUS && ti != MotionEvent.TOOL_TYPE_ERASER) {
            return true
        }

        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                lastX = event.x; lastY = event.y
                drawAndMeasure(event, event.x, event.y, event.x, event.y, event.eventTime)
            }
            MotionEvent.ACTION_MOVE -> {
                // Replay batched historical samples first (RR19-FR4b: getHistorical*).
                val hist = event.historySize
                for (i in 0 until hist) {
                    val hx = event.getHistoricalX(i)
                    val hy = event.getHistoricalY(i)
                    val ht = event.getHistoricalEventTime(i)
                    if (lastX >= 0) drawAndMeasure(event, lastX, lastY, hx, hy, ht)
                    lastX = hx; lastY = hy
                }
                if (lastX >= 0) drawAndMeasure(event, lastX, lastY, event.x, event.y, event.eventTime)
                lastX = event.x; lastY = event.y
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                lastX = -1f; lastY = -1f
            }
        }
        return true
    }

    /**
     * Draw one segment and record the software-observable latency: eventTime → the moment the
     * surface post (unlockCanvasAndPost) returns. For R3 we additionally fire a native A2
     * ioctl for the segment's bbox.
     */
    private fun drawAndMeasure(
        event: MotionEvent, x0: Float, y0: Float, x1: Float, y1: Float, sampleEventTime: Long
    ) {
        val h = holder ?: return
        if (!hasSurface || y0 < legendHeight && y1 < legendHeight) return

        val left = (minOf(x0, x1) - 4).toInt().coerceAtLeast(0)
        val top = (minOf(y0, y1) - 4).toInt().coerceAtLeast(legendHeight)
        val right = (maxOf(x0, x1) + 4).toInt()
        val bottom = (maxOf(y0, y1) + 4).toInt()
        val dirty = Rect(left, top, right, bottom)

        // Draw into the locked dirty rect (preserves the rest of the surface content).
        val canvas: Canvas = h.lockCanvas(dirty) ?: return
        try {
            canvas.drawLine(x0, y0, x1, y1, strokePaint)
        } finally {
            h.unlockCanvasAndPost(canvas)
        }

        // R3: direct /dev/ebc A2 update for the same bbox (after the surface post).
        if (route == Route.R3 && r3Open) {
            val rc = EbcNative.sendA2(left, top, right, bottom)
            if (rc != 0) Log.w(TAG, "R3 sendA2 rc=$rc (-errno) rect=$dirty")
        }

        // postTime: best software proxy for "ink committed" — when the post call returned.
        val postTimeMs = SystemClock.uptimeMillis()
        val deltaMs = (postTimeMs - sampleEventTime).toDouble()
        if (deltaMs in 0.0..2000.0) { // drop absurd outliers (clock edge cases)
            stats[route]!!.add(deltaMs)
            csv.append(route.label, sampleEventTime, postTimeMs, deltaMs)
        }
    }

    private fun cycleRoute() {
        // Tear down the route we're leaving.
        if (route == Route.R2) drawServiceProbe?.unbind()
        if (route == Route.R3 && r3Open) { EbcNative.closeEbc(); r3Open = false }

        route = Route.values()[(route.ordinal + 1) % Route.values().size]
        Log.i(TAG, "──── switched to ${route.label} ────")

        // Run the route's one-shot setup/reachability probe.
        when (route) {
            Route.R1 -> Log.i(TAG, "ROUTE 1 (auto-A2): reachable=true (trivially — plain SurfaceView; " +
                "panel-speed truth = camera). Draw to measure.")
            Route.R2 -> {
                val ok = drawServiceProbe?.bind() ?: false
                Log.i(TAG, "ROUTE 2 setup: bindService immediate=$ok (descriptor arrives async)")
            }
            Route.R3 -> {
                if (!EbcNative.available) {
                    Log.e(TAG, "ROUTE 3: FAILED native lib penspike_ebc not loaded")
                } else {
                    // One-shot full diagnostic over a representative center bbox.
                    val report = EbcNative.probeA2(800, 800, 1000, 1000)
                    Log.i(TAG, "ROUTE 3 probe: $report")
                    val open = EbcNative.openEbc()
                    r3Open = (open == 0)
                    if (r3Open) {
                        Log.i(TAG, "ROUTE 3 (/dev/ebc): reachable=true (open+ioctl OK); per-stroke A2 armed")
                    } else {
                        Log.e(TAG, "ROUTE 3 (/dev/ebc): FAILED openEbc rc=$open (-errno) — " +
                            "likely SELinux EACCES under untrusted_app. This is a RESULT.")
                    }
                }
            }
            Route.R4 -> Log.i(TAG, "ROUTE 4 (baseline): plain draw, no fast intent — the pen_low_latency=false floor.")
        }
        clearAndDrawLegend()
    }

    /** Repaint the whole surface white and draw the legend band + DRAW HERE hint. */
    private fun clearAndDrawLegend() {
        val h = holder ?: return
        if (!hasSurface) return
        val canvas = h.lockCanvas() ?: return
        try {
            canvas.drawColor(Color.WHITE)
            // Legend band
            canvas.drawRect(0f, 0f, canvas.width.toFloat(), legendHeight.toFloat(), overlayBg)
            canvas.drawLine(0f, legendHeight.toFloat(), canvas.width.toFloat(), legendHeight.toFloat(), legendDivider)
            canvas.drawText("ACTIVE: ${route.label}   (tap here to cycle route)", 20f, 44f, overlayText)
            canvas.drawText(routeStatusLine(), 20f, 88f, overlayText)
            canvas.drawText("latency ${stats[route]!!.summary()}   csv=pen-latency.csv", 20f, 130f, overlayText)
            canvas.drawText("DRAW HERE  ↓  (stylus only)", 20f, legendHeight + 60f, overlayText)
        } finally {
            h.unlockCanvasAndPost(canvas)
        }
    }

    private fun routeStatusLine(): String = when (route) {
        Route.R1 -> "reachable: yes (auto compositor classification — measure latency/quality)"
        Route.R2 -> drawServiceProbe?.report() ?: "DrawService: not attempted"
        Route.R3 -> if (!EbcNative.available) "native lib missing"
            else if (r3Open) "/dev/ebc: OPEN ok — per-stroke A2 armed"
            else "/dev/ebc: not open (see logcat for errno)"
        Route.R4 -> "reachable: yes (standard refresh floor)"
    }

    override fun onPause() {
        super.onPause()
        // Flush a summary per route so a quick run still yields numbers in logcat.
        Route.values().forEach { r ->
            Log.i(TAG, "SUMMARY ${r.label}: ${stats[r]!!.summary()}")
        }
        if (route == Route.R3 && r3Open) { EbcNative.closeEbc(); r3Open = false }
        drawServiceProbe?.unbind()
    }
}
