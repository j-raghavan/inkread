package dev.jraghavan.inkread

/**
 * Navigation derived from a decoded TOC: chapter prev/next stops (1.7) and the Daily per-article
 * jump. Pure, so it is host-testable apart from the reader.
 */
internal object TocNav {

    /**
     * Chapter starts as `(page, title)`, sorted and de-duplicated by page: a document's top-level
     * entries (all its targets for a flat TOC). With [perArticle] (a Daily issue), every leaf entry
     * instead — the issue's TOC is grouped by source (#269) while its pages round-robin the sources,
     * so its top level would stop at each source's first article and nowhere else.
     */
    fun chapterStarts(toc: List<TocItem>, perArticle: Boolean = false): List<Pair<Int, String>> {
        val targeted = toc.filter { it.targetPage != null }
        val stops = if (perArticle) leaves(toc) else targeted.filter { it.depth == 0 }.ifEmpty { targeted }
        return stops.map { it.targetPage!! to it.title }.distinctBy { it.first }.sortedBy { it.first }
    }

    /**
     * The start page of article [index] (0-based, in reading order) of a Daily issue, or null: the
     * per-article chapter stops after the cover. Holds for the grouped TOC (#269) and for back issues
     * compiled with the flat one.
     */
    fun dailyArticlePage(toc: List<TocItem>, index: Int): Int? =
        if (index < 0) null else chapterStarts(toc, perArticle = true).drop(1).getOrNull(index)?.first

    /** Targeted entries none of whose descendants has a target. */
    private fun leaves(toc: List<TocItem>): List<TocItem> =
        toc.filterIndexed { i, item ->
            val descendants = toc.drop(i + 1).takeWhile { it.depth > item.depth }
            item.targetPage != null && descendants.none { it.targetPage != null }
        }
}
