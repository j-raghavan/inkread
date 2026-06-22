package dev.jraghavan.inkread.penspike

import java.io.File

/**
 * Rolling nib-to-ink latency distribution for one route (RR19-FR4b / RR24-FR4).
 *
 * Records `deltaMs = postTime - eventTime` per drawn sample and reports median / p90 / max —
 * e-ink waveform timing is bimodal, so a single mean would lie (runbook §What to measure).
 * NOTE: this is the SOFTWARE-OBSERVABLE delta (input sample time → surface-post return). It
 * does NOT include the panel's physical settle — that needs the high-speed camera ground
 * truth (see README). Treat these as a lower bound / relative comparison across routes.
 */
class LatencyStats(private val capacity: Int = 4096) {
    private val samples = ArrayList<Double>(capacity)

    @Synchronized
    fun add(deltaMs: Double) {
        if (samples.size >= capacity) samples.removeAt(0)
        samples.add(deltaMs)
    }

    @Synchronized fun count(): Int = samples.size

    @Synchronized
    fun summary(): String {
        if (samples.isEmpty()) return "n=0"
        val sorted = samples.sorted()
        val median = percentile(sorted, 50.0)
        val p90 = percentile(sorted, 90.0)
        val max = sorted.last()
        return "n=%d med=%.1f p90=%.1f max=%.1f ms".format(sorted.size, median, p90, max)
    }

    @Synchronized fun clear() = samples.clear()

    private fun percentile(sorted: List<Double>, p: Double): Double {
        if (sorted.isEmpty()) return 0.0
        val idx = ((p / 100.0) * (sorted.size - 1)).toInt().coerceIn(0, sorted.size - 1)
        return sorted[idx]
    }
}

/**
 * Append-only CSV sink at `getExternalFilesDir(null)/pen-latency.csv`.
 * Columns: route,eventTimeMs,postTimeMs,deltaMs (RR19-FR4b deliverable).
 */
class CsvLogger(dir: File?) {
    private val file: File? = dir?.let { File(it, "pen-latency.csv") }

    init {
        file?.let {
            if (!it.exists()) {
                runCatching { it.writeText("route,eventTimeMs,postTimeMs,deltaMs\n") }
            }
        }
    }

    fun path(): String = file?.absolutePath ?: "(no external files dir)"

    fun append(route: String, eventTimeMs: Long, postTimeMs: Long, deltaMs: Double) {
        val f = file ?: return
        runCatching {
            f.appendText("%s,%d,%d,%.3f\n".format(route, eventTimeMs, postTimeMs, deltaMs))
        }
    }
}
