package dev.jraghavan.inkread.penspike

import android.app.Activity
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.RectF
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View

const val TAG = "PenSpike"

/** Let DrawService's async onServiceConnected land before the consolidated self-test verdict. */
private const val DRAWSERVICE_SETTLE_MS = 1500L

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
        R5("R5 service_myservice (firmware ink)"),
    }

    private lateinit var surfaceView: SurfaceView
    private var holder: SurfaceHolder? = null
    private var hasSurface = false

    // Default to R5 (firmware ink): it is the only route whose output reaches the panel on a
    // sideloaded app, and it needs no legend tap to activate. Tap the top band to cycle to R1–R4.
    private var route = Route.R5
    private var snAutoSetupDone = false
    private val stats = HashMap<Route, LatencyStats>().apply {
        Route.values().forEach { put(it, LatencyStats()) }
    }
    private lateinit var csv: CsvLogger
    private var drawServiceProbe: DrawServiceProbe? = null
    private var supernoteInkProbe: SupernoteInkProbe? = null
    private var r3Open = false
    private var einkForegroundTested = false

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
        supernoteInkProbe = SupernoteInkProbe(this)

        // One-shot reachability self-test (RR19-FR4b). The make-or-break facts — can a
        // sideloaded untrusted_app open()+ioctl /dev/ebc, and does DrawService bind — need
        // no stylus and no input injection (which this device blocks for adb). Drawing-based
        // latency still needs the pen + a high-speed camera; this just settles reachability.
        runReachabilitySelfTest()
    }

    private fun runReachabilitySelfTest() {
        Log.i(TAG, "SELFTEST begin (R2 DrawService bind + R3 /dev/ebc) ==========")
        drawServiceProbe?.bind() // descriptor arrives async via onServiceConnected

        // The device's real driver is Ratta ht_eink (private 'HT' ioctl family), so the stock
        // ebc-dev 0x70xx probes (discoverAbi/probeA2) return EINVAL and CANNOT put a pixel up.
        // The decisive SAFE direct-path datum is the read-only HT GETINFO verdict; the blind 'HT'
        // write cmds reboot the device, so we never fire them here.
        val htVerdict = if (EbcNative.available) {
            Log.i(TAG, "SELFTEST R3 canOpen() rc=${EbcNative.canOpen()} (0=OK, negative=-errno)")
            EbcNative.htGetInfo().trim().also { Log.i(TAG, "SELFTEST R3 htGetInfo:\n$it") }
                .also {
                    // Read-only FB readback: is mmap offset 0 the live panel? (camera-free truth)
                    Log.i(TAG, "SELFTEST R3 htDumpFb: ${EbcNative.htDumpFb()}")
                }
        } else {
            Log.e(TAG, "SELFTEST R3 native lib penspike_ebc not loaded")
            "HT-GETINFO: native lib penspike_ebc NOT loaded"
        }

        // Route 0: the EinkManager reflection path (KOReader's RK3566 refresh; clean-room). Safe —
        // dumps the real EinkManager API surface and invokes only the no-arg sendOneFullFrame().
        val einkVerdict = EinkManagerProbe.run(this).also { Log.i(TAG, "SELFTEST R0 EinkManager:\n$it") }
        // Route 0b: camera-free confirmation — getMode() round-trip proves setMode works; the
        // getSeqNum(0) frame counter advancing across sendOneFullFrame proves a refresh fired.
        val einkConfirm = EinkManagerProbe.confirmRefresh(this).also { Log.i(TAG, "SELFTEST R0b confirm:\n$it") }

        // Route 5: the firmware HandWrite binder (service_myservice). This is the make-or-break
        // sideload reachability test for low-latency ink — the route the other four never tried.
        val snVerdict = supernoteInkProbe?.probe() ?: "ROUTE 5 (service_myservice): probe unavailable"

        // DrawService binds async — give onServiceConnected time to land, then print the single
        // consolidated VERDICT block the sideload paths turn on (grep for "SELFTEST VERDICT").
        Handler(Looper.getMainLooper()).postDelayed({
            Log.i(TAG, "──── SELFTEST VERDICT (sideload refresh paths) ────")
            Log.i(TAG, "SELFTEST VERDICT  route0(EM) : ${einkVerdict.substringBefore('\n')}")
            Log.i(TAG, "SELFTEST VERDICT  route0b(EM): ${einkConfirm.substringBefore('\n')}")
            Log.i(TAG, "SELFTEST VERDICT  direct(HT) : ${htVerdict.substringBefore('\n')}")
            Log.i(TAG, "SELFTEST VERDICT  route2(DS) : ${drawServiceProbe?.report()}")
            Log.i(TAG, "SELFTEST VERDICT  route5(SN) : ${snVerdict.substringBefore('\n')}")
            Log.i(TAG, "SELFTEST end ==========")
        }, DRAWSERVICE_SETTLE_MS)
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

    // ---- DIAG (RR19-FR4b): catch EVERY event reaching the window, on every channel, so we can
    // see exactly what Android delivers for the Wacom pen (tool type + source + action). These
    // dispatch hooks fire before any view-level handling. Remove once the channel is known.
    private fun diag(tag: String, e: MotionEvent) {
        Log.i(TAG, "$tag act=${e.actionMasked} tool=${e.getToolType(0)} " +
            "src=0x${Integer.toHexString(e.source)} x=${e.x} y=${e.y} press=${e.pressure} n=${e.pointerCount}")
    }

    override fun dispatchTouchEvent(event: MotionEvent): Boolean {
        diag("DISPATCH-TOUCH", event)
        return super.dispatchTouchEvent(event)
    }

    override fun dispatchGenericMotionEvent(event: MotionEvent): Boolean {
        diag("DISPATCH-GENERIC", event)
        return super.dispatchGenericMotionEvent(event)
    }

    override fun onGenericMotionEvent(event: MotionEvent): Boolean {
        diag("GENERIC", event)
        return super.onGenericMotionEvent(event)
    }

    private val legendHeight = 150
    private fun inLegend(y: Float) = y < legendHeight

    override fun onTouchEvent(event: MotionEvent): Boolean {
        diag("TOUCH", event)
        // Tapping the legend band cycles routes (and runs the route's one-shot setup probe).
        if (event.actionMasked == MotionEvent.ACTION_DOWN && inLegend(event.y)) {
            cycleRoute()
            return true
        }

        // DIAG: temporarily accept ALL tool types so any pointer draws (the RR19-FR7
        // stylus-only filter is restored once the pen's delivery channel/tool-type is known).
        @Suppress("UNUSED_VARIABLE")
        val ti = event.getToolType(0)

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

        // R5: the firmware paints the live stroke onto the EPDC overlay itself — the app draws
        // NOTHING per sample. What you see under the nib is pure firmware ink; that is the verdict.
        if (route != Route.R5) {
            // Draw into the locked dirty rect (preserves the rest of the surface content).
            val canvas: Canvas = h.lockCanvas(dirty) ?: return
            try {
                canvas.drawLine(x0, y0, x1, y1, strokePaint)
            } finally {
                h.unlockCanvasAndPost(canvas)
            }
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
        if (route == Route.R5) supernoteInkProbe?.teardown()

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
            Route.R5 -> {
                val ok = supernoteInkProbe?.setup() ?: false
                Log.i(TAG, "ROUTE 5 setup: firmware ink claim=$ok — ${supernoteInkProbe?.report()}")
            }
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
        Route.R5 -> supernoteInkProbe?.report() ?: "service_myservice: not attempted"
    }

    override fun onResume() {
        super.onResume()
        // Retry setMode("12") now that our window is FOREGROUND — the system likely honors a
        // per-app eink mode only for the focused window (the onCreate attempt ran too early).
        if (!einkForegroundTested) {
            einkForegroundTested = true
            Log.i(TAG, "SELFTEST R0c (foreground):\n${EinkManagerProbe.foregroundModeTest(this)}")
        }

        // Auto-activate R5 firmware ink while we're foreground — no legend tap required. The
        // firmware then paints ink under the nib directly to the EPDC overlay; just draw.
        if (route == Route.R5 && !snAutoSetupDone) {
            snAutoSetupDone = true
            val ok = supernoteInkProbe?.setup() ?: false
            Log.i(TAG, "AUTO R5 setup (resume): firmware ink claim=$ok — ${supernoteInkProbe?.report()}")
            // Push the legend/instructions onto the panel so the screen isn't blank.
            clearAndDrawLegend()
            EinkManagerProbe.fullRefresh(this)
        }
    }

    override fun onPause() {
        super.onPause()
        // Flush a summary per route so a quick run still yields numbers in logcat.
        Route.values().forEach { r ->
            Log.i(TAG, "SUMMARY ${r.label}: ${stats[r]!!.summary()}")
        }
        if (route == Route.R3 && r3Open) { EbcNative.closeEbc(); r3Open = false }
        drawServiceProbe?.unbind()
        supernoteInkProbe?.teardown()
    }
}
