package dev.jraghavan.inkread

/**
 * The bottom bar's "time left" decision (#98), extracted from [BottomBarController] so the
 * when-to-show and how-to-word rules are unit-tested on the host, like [ReaderGestures].
 *
 * The estimate's arithmetic lives in the Rust core (`reader::minutes_left`, reached over
 * [NativeBridge.nativeReadingMinutesLeft]); this object owns only the shell-side policy: a page
 * count of one or less means the total is unknown (or there is nothing to pace), and the estimate
 * is hidden rather than shown as a misleading "~0 min".
 */
object ReadingEstimate {

    /**
     * The label to show beside the page indicator, or `null` to show nothing. `minutesOf` maps the
     * pages still ahead to an estimate (the Rust bridge in production; a stub in tests), and is only
     * consulted when there is genuinely something left to read.
     */
    fun label(currentPage: Int, pageCount: Int, minutesOf: (pagesLeft: Int) -> Int): String? {
        if (pageCount <= 1) return null
        val pagesLeft = pageCount - currentPage - 1
        if (pagesLeft <= 0) return null
        val minutes = minutesOf(pagesLeft)
        if (minutes <= 0) return null
        return "~$minutes min left"
    }
}
