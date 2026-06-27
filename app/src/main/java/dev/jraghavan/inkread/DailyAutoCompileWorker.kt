package dev.jraghavan.inkread

import android.content.Context
import android.util.Log
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.NetworkType
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.Worker
import androidx.work.WorkerParameters
import java.util.Calendar
import java.util.concurrent.TimeUnit

/**
 * Compiles a fresh daily issue in the background each morning (#66), so an issue is waiting when the
 * device is picked up instead of needing a manual Regenerate. A WorkManager periodic job: it survives
 * reboots, waits for a network, and backs off on transient failure. The first run is aligned to the
 * next [MORNING_HOUR]; WorkManager then repeats it about once a day.
 *
 * The compile pipeline itself lives in [DailyController] (fetch → core extract/assemble); this worker
 * only schedules and drives it. Idempotent: if today's issue already exists (e.g. the user compiled
 * manually) it does nothing.
 */
class DailyAutoCompileWorker(context: Context, params: WorkerParameters) :
    Worker(context, params) {

    override fun doWork(): Result {
        val daily = DailyController(applicationContext)
        if (daily.todayIssue() != null) {
            Log.i(TAG, "today's issue already compiled — skipping")
            return Result.success()
        }
        return try {
            // No network / no enabled sources yet → retry with backoff (WorkManager honours the
            // network constraint, so this mainly covers a feed being briefly unreachable).
            if (daily.compileSync()) Result.success() else Result.retry()
        } catch (e: Exception) {
            Log.e(TAG, "auto-compile failed", e)
            Result.retry()
        }
    }

    companion object {
        private const val TAG = "DailyAutoCompile"
        private const val UNIQUE = "daily-auto-compile"
        private const val MORNING_HOUR = 5

        /** Enqueue (or keep) the daily auto-compile. Safe to call on every app start — KEEP leaves an
         *  already-scheduled job untouched, so the morning cadence isn't reset each launch. */
        fun schedule(context: Context) {
            val constraints = Constraints.Builder()
                .setRequiredNetworkType(NetworkType.CONNECTED)
                .build()
            val request = PeriodicWorkRequestBuilder<DailyAutoCompileWorker>(1, TimeUnit.DAYS)
                .setConstraints(constraints)
                .setInitialDelay(delayToNextMorning(), TimeUnit.MILLISECONDS)
                .setBackoffCriteria(BackoffPolicy.LINEAR, 30, TimeUnit.MINUTES)
                .build()
            WorkManager.getInstance(context)
                .enqueueUniquePeriodicWork(UNIQUE, ExistingPeriodicWorkPolicy.KEEP, request)
            Log.i(TAG, "scheduled daily auto-compile (first run in ${delayToNextMorning() / 60000} min)")
        }

        /** Millis from now until the next [MORNING_HOUR] o'clock, local time. */
        private fun delayToNextMorning(): Long {
            val now = Calendar.getInstance()
            val next = (now.clone() as Calendar).apply {
                set(Calendar.HOUR_OF_DAY, MORNING_HOUR)
                set(Calendar.MINUTE, 0)
                set(Calendar.SECOND, 0)
                set(Calendar.MILLISECOND, 0)
                if (!after(now)) add(Calendar.DAY_OF_MONTH, 1)
            }
            return next.timeInMillis - now.timeInMillis
        }
    }
}
