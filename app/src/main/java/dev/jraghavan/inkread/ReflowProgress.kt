package dev.jraghavan.inkread

import android.app.Activity
import android.app.Dialog
import android.os.Handler
import android.os.Looper
import android.view.Gravity
import android.view.ViewGroup
import android.view.Window
import android.widget.LinearLayout
import android.widget.TextView

/**
 * The "Reflowing…" notice shown while a repagination runs on the engine thread, for every path that
 * can trigger one (#55, #161).
 *
 * Two things repaginate a whole document: toggling reflow on a text-layer PDF, and changing
 * typography on a reflowable book. Both are quick on a small document and slow on a large one, and
 * neither gives the reader anything to look at meanwhile — so both route through here rather than
 * each growing its own notice.
 *
 * The dialog is **armed, not shown**: nothing appears unless the work outlasts [SHOW_AFTER_MS]. A
 * dialog that flashes up and vanishes costs an e-ink refresh and reads as a glitch, and most
 * reflows finish well inside that window. For the same reason the label is only repainted when the
 * chapter count actually changes.
 *
 * Where the core reports chapter-level progress (a reflowable book) the label counts chapters off
 * and a Cancel button is offered; cancelling leaves the reader on the pagination they already had,
 * which the core guarantees. Where it does not ([cancellable] = false, the PDF reflow toggle) this
 * is a plain "Reflowing…" notice — the behaviour that path already had.
 *
 * UI thread only. Progress is read through lock-free process-wide counters rather than the document
 * handle, because the engine thread is inside a native call on that handle throughout.
 */
class ReflowProgress(private val activity: Activity, private val cancellable: Boolean = true) {

    private val handler = Handler(Looper.getMainLooper())
    private var dialog: Dialog? = null
    private var label: TextView? = null
    private var shownChapters = -1
    private var cancelling = false
    private var finished = false

    /** Arm the notice and clear any previous run's cancel flag. Call before starting the reflow. */
    fun begin() {
        // Only meaningful for a core-driven repagination; the PDF path has no use for the flag.
        if (cancellable) runCatching { NativeBridge.nativeCancelPagination(false) }
        handler.postDelayed({ if (!finished) show() }, SHOW_AFTER_MS)
    }

    /** The reflow returned (finished or cancelled): disarm and dismiss. */
    fun end() {
        finished = true
        handler.removeCallbacksAndMessages(null)
        dialog?.let { runCatching { it.dismiss() } }
        dialog = null
        label = null
    }

    private fun show() {
        if (activity.isFinishing) return

        val message = TextView(activity).apply {
            text = REFLOWING
            textSize = Ink.sp(16f)
            setTextColor(Ink.ink)
        }
        val content = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setPadding(Ink.dp(28), Ink.dp(22), Ink.dp(28), Ink.dp(22))
            addView(message)
            if (cancellable) {
                addView(
                    Ink.pillButton(activity, "Cancel", primary = false) {
                        // Asks; the core stops at its next chapter boundary and keeps the old
                        // pagination. The notice stays up until the engine thread returns.
                        runCatching { NativeBridge.nativeCancelPagination(true) }
                        cancelling = true
                        message.text = CANCELLING
                    },
                    LinearLayout.LayoutParams(
                        ViewGroup.LayoutParams.WRAP_CONTENT,
                        ViewGroup.LayoutParams.WRAP_CONTENT,
                    ).apply { topMargin = Ink.dp(16) },
                )
            }
        }

        dialog = Dialog(activity).apply {
            requestWindowFeature(Window.FEATURE_NO_TITLE)
            // Not dismissable by tapping away: the only ways out are finishing and Cancel, so the
            // reader is never left wondering whether the reflow is still running.
            setCancelable(false)
            setContentView(content)
            window?.setBackgroundDrawable(Ink.cardBg())
            runCatching { show() }
        }
        label = message
        if (cancellable) poll()
    }

    private fun poll() {
        if (finished) return
        val packed = runCatching { NativeBridge.nativePaginationProgress() }.getOrDefault(0L)
        val done = NativeBridge.paginationDone(packed)
        val total = NativeBridge.paginationTotal(packed)
        // Repaint only on a real change — every update is an e-ink refresh — and never over the
        // "Cancelling…" acknowledgement.
        if (total > 0 && done != shownChapters && !cancelling) {
            shownChapters = done
            label?.text = "$REFLOWING $done/$total"
        }
        handler.postDelayed({ poll() }, POLL_MS)
    }

    private companion object {
        const val REFLOWING = "Reflowing…"
        const val CANCELLING = "Cancelling…"

        /** Reflows shorter than this never show a notice at all (#55). */
        const val SHOW_AFTER_MS = 250L

        /** Slow enough to keep e-ink repaints rare, fast enough to look live. */
        const val POLL_MS = 500L
    }
}
