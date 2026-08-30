package dev.jraghavan.inkread

/**
 * Which page, if any, to warm into the core's render cache behind the current one (RR24), extracted
 * from [ReaderActivity]'s render path.
 *
 * Read-ahead is the single biggest win available on the page-turn critical path: a turn that hits
 * the cache renders in about 10ms against 90–150ms for a cold pdfium render, which on e-ink is the
 * difference between a turn that feels immediate and one that visibly waits. But every condition
 * below exists to stop it doing harm, and each is easy to get subtly wrong:
 *
 * - **Only at fit.** A zoomed render is viewport-specific, so prefetching the next page at the
 *   current zoom caches something the reader will never see at that magnification.
 * - **In the direction of travel.** Prefetching backwards while reading forwards evicts the page
 *   about to be needed.
 * - **Never twice for the same page.** Chrome repaints — opening the bar, drawing a selection,
 *   toggling a bookmark — all re-enter the render path on the *same* page. Without the dedupe each
 *   one would re-enqueue the same prefetch onto the serial engine thread, queueing work behind the
 *   reader's next real action.
 */
object PrefetchPolicy {

    /**
     * The page to warm, or `null` for none.
     *
     * @param current        the page just rendered.
     * @param direction      +1 reading forward, -1 backward.
     * @param pageCount      pages in the document.
     * @param zoom           current zoom; anything above fit disables read-ahead.
     * @param lastPrefetched the page most recently handed to read-ahead (`-1` = none yet).
     */
    fun nextPage(
        current: Int,
        direction: Int,
        pageCount: Int,
        zoom: Float,
        lastPrefetched: Int,
    ): Int? {
        if (!ZoomPolicy.isFit(zoom)) return null
        val ahead = current + direction
        if (ahead < 0 || ahead >= pageCount) return null
        if (ahead == lastPrefetched) return null
        return ahead
    }
}
