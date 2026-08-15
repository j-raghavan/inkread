package dev.jraghavan.inkread

import android.app.Activity
import android.app.Dialog
import android.graphics.Color
import android.graphics.drawable.GradientDrawable
import android.os.Handler
import android.os.Looper
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.Window
import android.widget.LinearLayout
import android.widget.TextView

/**
 * Progress + cancel for a repagination running on the engine thread (#161).
 *
 * Changing typography re-lays-out the whole book. That is fast for most books and slow for a very
 * long one, and there is nothing the reader can do about it — so show what is happening and offer a
 * way out. The reader is left on the pagination they already had if they cancel; the core owns that
 * guarantee, this only asks for it.
 *
 * The dialog is **armed, not shown**: nothing appears unless the reflow is still running after
 * [SHOW_AFTER_MS]. A dialog that flashes up and vanishes costs an e-ink refresh and reads as a
 * glitch, and most reflows finish well inside that window. For the same reason the label is only
 * repainted when the chapter count actually changes.
 *
 * UI thread only. Progress is read through lock-free process-wide counters rather than the document
 * handle, because the engine thread is inside a native call on that handle for the whole time.
 */
class ReflowProgress(private val activity: Activity) {

    private val handler = Handler(Looper.getMainLooper())
    private var dialog: Dialog? = null
    private var label: TextView? = null
    private var shownChapters = -1
    private var finished = false

    /** Arm the dialog and clear any previous run's cancel flag. Call before starting the reflow. */
    fun begin() {
        try {
            NativeBridge.nativeCancelPagination(false)
        } catch (e: RuntimeException) {
            return // no native bridge: silently skip the progress UI, the reflow still runs
        }
        handler.postDelayed({ if (!finished) show() }, SHOW_AFTER_MS)
    }

    /** The reflow returned (completed or cancelled): disarm and dismiss. */
    fun end() {
        finished = true
        handler.removeCallbacksAndMessages(null)
        dialog?.let { if (it.isShowing) runCatching { it.dismiss() } }
        dialog = null
        label = null
    }

    private fun show() {
        val d = activity.resources.displayMetrics.density
        fun dp(v: Int) = (v * d).toInt()

        val message = TextView(activity).apply {
            setTextColor(Color.BLACK); textSize = Ink.sp(16f); typeface = Ink.serif
            text = "Reflowing…"
        }
        val cancel = TextView(activity).apply {
            text = "Cancel"
            setTextColor(Color.BLACK); textSize = Ink.sp(16f); gravity = Gravity.CENTER
            setPadding(dp(18), dp(10), dp(18), dp(10)); isClickable = true
            background = GradientDrawable().apply {
                setColor(Color.WHITE); cornerRadius = dp(20).toFloat()
                setStroke(maxOf(1, dp(1)), Color.parseColor("#9E9E9E"))
            }
            setOnClickListener {
                // Asks; the core stops at its next chapter boundary and keeps the old pagination.
                runCatching { NativeBridge.nativeCancelPagination(true) }
                text = "Cancelling…"
                isClickable = false
            }
        }
        val content = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setPadding(dp(24), dp(20), dp(24), dp(16))
            setBackgroundColor(Color.WHITE)
            addView(message)
            addView(cancel, LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT,
            ).apply { topMargin = dp(16) })
        }

        dialog = Dialog(activity).apply {
            requestWindowFeature(Window.FEATURE_NO_TITLE)
            setContentView(content)
            // Not dismissable by tapping away: the only ways out are finishing and Cancel, so the
            // reader can never be left wondering whether the reflow is still going.
            setCancelable(false)
            window?.setBackgroundDrawable(GradientDrawable().apply {
                setColor(Color.WHITE); cornerRadius = dp(8).toFloat()
            })
            runCatching { show() }
        }
        label = message
        poll()
    }

    private fun poll() {
        if (finished) return
        val packed = try {
            NativeBridge.nativePaginationProgress()
        } catch (e: RuntimeException) {
            0L
        }
        val done = NativeBridge.paginationDone(packed)
        val total = NativeBridge.paginationTotal(packed)
        // Repaint only on a real change — every update is an e-ink refresh.
        if (total > 0 && done != shownChapters) {
            shownChapters = done
            label?.text = "Reflowing… $done/$total"
        }
        handler.postDelayed({ poll() }, POLL_MS)
    }

    private companion object {
        /** Reflows shorter than this never show a dialog at all. */
        const val SHOW_AFTER_MS = 400L

        /** Slow enough to keep e-ink repaints rare, fast enough to look live. */
        const val POLL_MS = 500L
    }
}
