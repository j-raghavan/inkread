package dev.jraghavan.inkread

/**
 * Parses WordNet StarDict definition text — the format shipped in `dict.db` (sametypesequence='m',
 * plain UTF-8) — into POS-tagged, numbered senses with extracted examples and synonyms, so the
 * dictionary popup can lay each sense out in its own block instead of dumping a wall of text. Pure
 * (no Android types); unit-testable.
 *
 * Format observed in WordNet 2.x (dict.org / huzheng mirror), e.g. for *anatomy*:
 * ```
 *   n 1: the branch of morphology that deals with the structure of
 *        animals [syn: {general anatomy}]
 *   2: alternative names for the body...; "Leonardo studied the human body" [syn: {human body}, ...]
 *   v 1: ...
 * ```
 *
 * inkread's core ([`inkread-dict`]) splits the stored entry on newlines and trims each line, so —
 * unlike the original indentation-based parser this is ported from — a sense start is recognised by
 * the POS-token / number-colon pattern alone. Anything not classifiable into a sense leaves
 * [WordNetEntry.parseFailed] true so the caller can fall back to rendering the raw lines (e.g.
 * Wiktionary online results, which aren't WordNet-shaped).
 */
data class WordNetSense(
    /** WordNet POS abbreviation (`n` | `v` | `adj` | `adv` | `a` | `r`); null for bare numbered senses. */
    val pos: String?,
    /** 1-based sense number within its POS block. */
    val index: Int,
    /** Definition text with the `[syn:]` block and `"example"` quotes stripped out. */
    val definition: String,
    /** Quoted-string usage examples lifted from the text. */
    val examples: List<String>,
    /** `[syn: {a}, {b}]` entries, with braces stripped. */
    val synonyms: List<String>,
)

/** A parsed WordNet entry; [parseFailed] when nothing matched (caller falls back to raw lines). */
data class WordNetEntry(val senses: List<WordNetSense>, val parseFailed: Boolean)

object WordNet {
    private val POS_TOKENS = setOf("n", "v", "adj", "adv", "a", "r")

    // Sense-line shapes (leading whitespace optional — inkread's core trims each line):
    //   "n 1: text" -> first sense of a POS block      "2: text" -> subsequent sense (inherits POS)
    //   "n : text"  -> single-sense entry (treat as 1)
    private val SENSE_POS_AND_NUM = Regex("""^\s*([a-z]+)\s+(\d+):\s*(.*)$""")
    private val SENSE_POS_ONLY = Regex("""^\s*([a-z]+)\s+:\s*(.*)$""")
    private val SENSE_NUM_ONLY = Regex("""^\s*(\d+):\s*(.*)$""")

    private val SYN_BLOCK = Regex("""\[syn:\s*([^\]]+)\]""")
    private val BRACE = Regex("""\{([^}]+)\}""")
    private val QUOTED = Regex(""""([^"]+)"""")
    private val WS = Regex("""\s+""")

    private val POS_LABELS = mapOf(
        "n" to "noun", "v" to "verb", "adj" to "adjective",
        "adv" to "adverb", "a" to "adjective", "r" to "adverb",
    )

    /** Human-readable part of speech for a WordNet abbreviation (e.g. `n` → "noun"). */
    fun labelForPos(pos: String?): String = pos?.let { POS_LABELS[it] ?: it } ?: ""

    private data class Start(val pos: String?, val index: Int, val rest: String)

    private fun matchStart(line: String): Start? {
        SENSE_POS_AND_NUM.matchEntire(line)?.let { m ->
            val pos = m.groupValues[1]
            if (pos in POS_TOKENS) return Start(pos, m.groupValues[2].toIntOrNull() ?: 1, m.groupValues[3])
        }
        SENSE_POS_ONLY.matchEntire(line)?.let { m ->
            val pos = m.groupValues[1]
            if (pos in POS_TOKENS) return Start(pos, 1, m.groupValues[2])
        }
        SENSE_NUM_ONLY.matchEntire(line)?.let { m ->
            return Start(null, m.groupValues[1].toIntOrNull() ?: 1, m.groupValues[2])
        }
        return null
    }

    private fun synonymsOf(text: String): List<String> {
        val block = SYN_BLOCK.find(text) ?: return emptyList()
        return BRACE.findAll(block.groupValues[1]).map { it.groupValues[1].replace(WS, " ").trim() }.toList()
    }

    private fun examplesOf(text: String): List<String> =
        QUOTED.findAll(text).map { it.groupValues[1].trim() }.toList()

    private fun stripSynAndExamples(text: String): String =
        text.replace(SYN_BLOCK, "")
            .replace(Regex(""";?\s*"[^"]+""""), "")
            .replace(WS, " ")
            .replace(Regex("""\s*;\s*$"""), "")
            .trim()

    /**
     * Parse the body lines of a WordNet entry (inkread's `WordDefinition.senses`). A line that starts
     * a sense opens a new block; any other non-blank line continues the current block. The leading
     * headword line (which matches no sense pattern) is harmlessly ignored.
     */
    fun parse(lines: List<String>): WordNetEntry {
        data class Working(val pos: String?, val index: Int, val chunks: MutableList<String>)
        val working = ArrayList<Working>()
        var currentPos: String? = null
        for (line in lines) {
            if (line.isBlank()) continue
            val s = matchStart(line)
            if (s != null) {
                if (s.pos != null) currentPos = s.pos
                working.add(Working(currentPos, s.index, mutableListOf(s.rest)))
            } else if (working.isNotEmpty()) {
                working.last().chunks.add(line.trim())
            }
        }
        val senses = working.map { w ->
            val flat = w.chunks.joinToString(" ")
            WordNetSense(w.pos, w.index, stripSynAndExamples(flat), examplesOf(flat), synonymsOf(flat))
        }
        return WordNetEntry(senses, senses.isEmpty())
    }
}
