package dev.jraghavan.inkread

/**
 * Navigation derived from a decoded TOC: chapter prev/next stops (1.7) and the Daily per-article
 * jump. Pure, so it is host-testable apart from the reader.
 */
internal object TocNav {

    /**
     * Chapter starts as `(page, title)`, sorted and de-duplicated by page.
     *
     * A book's TOC is an outline in reading order, and its top-level entries are its chapters (or
     * parts). A Daily issue's TOC is grouped by source (#269) while its pages round-robin the
     * sources, so its top level is a list of sources: stepping through it would visit each source's
     * first article and then report "Last chapter" partway through. So with [perArticle] (a Daily
     * issue), or whenever the TOC's targets read in TOC order step backwards, every leaf entry is a
     * chapter — one stop per article. A flat TOC uses all its targets, as before.
     */
    fun chapterStarts(toc: List<TocItem>, perArticle: Boolean = false): List<Pair<Int, String>> {
        val targeted = toc.filter { it.targetPage != null }
        val inReadingOrder = targeted.zipWithNext().all { (a, b) -> a.targetPage!! <= b.targetPage!! }
        val tops = targeted.filter { it.depth == 0 }
        val stops = when {
            !perArticle && inReadingOrder && tops.isNotEmpty() -> tops
            !perArticle && inReadingOrder -> targeted
            else -> toc.filterIndexed { i, item ->
                item.targetPage != null && toc.getOrNull(i + 1)?.let { it.depth <= item.depth } != false
            }
        }
        return stops.map { it.targetPage!! to it.title }.distinctBy { it.first }.sortedBy { it.first }
    }

    /**
     * The start page of article [index] (0-based, in reading order) of a Daily issue, or null.
     *
     * The issue's first TOC entry is the cover; every article has its own document and appears in
     * the TOC, so the distinct article targets, sorted, are the article starts in reading order. This
     * holds for the grouped TOC (#269) and for back issues compiled with the flat one.
     */
    fun dailyArticlePage(toc: List<TocItem>, index: Int): Int? {
        if (index < 0) return null
        val cover = toc.firstOrNull()?.targetPage
        return toc.drop(1).mapNotNull { it.targetPage }.filter { it != cover }.distinct().sorted().getOrNull(index)
    }
}
