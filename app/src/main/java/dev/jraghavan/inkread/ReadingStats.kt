package dev.jraghavan.inkread

import android.content.Context

/**
 * On-device reading stats for the home screen's streak + weekly chrome (the design's "12-day streak ·
 * 4h 20m this week · 320 pages"). All real, all local: a reading **streak** of consecutive calendar
 * days with activity, plus **minutes** and **pages** accumulated in the current week. The reader
 * reports a foreground session (time + pages advanced) when it backgrounds; the home reads the
 * rollups. No clock beyond the calendar day — nothing fabricated.
 */
object ReadingStats {

    private fun prefs(ctx: Context) = ctx.getSharedPreferences("stats", Context.MODE_PRIVATE)

    /** UTC calendar day (days since epoch) — the unit the streak counts in. */
    private fun today(): Long = System.currentTimeMillis() / 86_400_000L

    /** ISO-ish week bucket (7-day blocks since epoch) — the weekly accumulators reset when it changes. */
    private fun thisWeek(): Long = today() / 7

    /**
     * Record a finished foreground reading session: `minutes` read and `pages` advanced. Rolls the
     * weekly accumulators (resetting them when the week turns) and advances the streak for today.
     */
    fun record(ctx: Context, minutes: Int, pages: Int) {
        if (minutes <= 0 && pages <= 0) return
        val p = prefs(ctx)
        val week = thisWeek()
        val storedWeek = p.getLong("week", -1)
        val (baseMin, basePages) = if (week == storedWeek) {
            p.getInt("weekMin", 0) to p.getInt("weekPages", 0)
        } else {
            0 to 0 // a new week — start fresh
        }
        p.edit()
            .putLong("week", week)
            .putInt("weekMin", baseMin + minutes.coerceAtLeast(0))
            .putInt("weekPages", basePages + pages.coerceAtLeast(0))
            .apply()
        markRead(ctx)
    }

    /** Mark today as a reading day, advancing/continuing/resetting the streak. */
    private fun markRead(ctx: Context) {
        val p = prefs(ctx)
        val today = today()
        val last = p.getLong("lastDay", -1)
        if (last == today) return // already counted today
        val streak = p.getInt("streak", 0)
        val next = when (last) {
            today - 1 -> streak + 1 // consecutive day
            else -> 1 // first day, or a gap broke the streak
        }
        p.edit().putLong("lastDay", today).putInt("streak", next).apply()
    }

    /** Consecutive-day reading streak, or 0 if the last reading day was before yesterday. */
    fun streakDays(ctx: Context): Int {
        val p = prefs(ctx)
        val last = p.getLong("lastDay", -1)
        val today = today()
        return if (last == today || last == today - 1) p.getInt("streak", 0) else 0
    }

    /** Minutes read in the current week (0 once the week turns over). */
    fun weekMinutes(ctx: Context): Int =
        if (prefs(ctx).getLong("week", -1) == thisWeek()) prefs(ctx).getInt("weekMin", 0) else 0

    /** Pages advanced in the current week (0 once the week turns over). */
    fun weekPages(ctx: Context): Int =
        if (prefs(ctx).getLong("week", -1) == thisWeek()) prefs(ctx).getInt("weekPages", 0) else 0

    /** "4h 20m" / "35m" — a compact reading-time label for the weekly chip. */
    fun formatMinutes(min: Int): String {
        val h = min / 60
        val m = min % 60
        return if (h > 0) "${h}h ${m}m" else "${m}m"
    }
}
