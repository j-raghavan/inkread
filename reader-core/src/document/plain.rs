//! Plain-text (`.txt`) backend behind the [`Document`] trait (RR2-FR5).
//!
//! Plain text has no layout of its own, so this backend does the cheapest possible thing: split the
//! file into paragraphs and hand them to the **existing reflow engine** ([`ReflowView`]) as a single
//! reflowable unit. Pagination, font-size / line-spacing / alignment, text selection, and search all
//! come from the shared `inkread-epub` layout pipeline verbatim — there is **no new layout code**
//! here (REUSE-FIRST). The only plain-text-specific logic is the paragraph splitter.
//!
//! Blank lines separate paragraphs; a single newline inside a paragraph is a soft wrap (joined with a
//! space and re-wrapped to the viewport), matching how a reader expects flowed prose to behave.

use inkread_epub::{Block, Inline, TextRun};

use crate::document::reflow_view::ReflowView;
use crate::document::text_select::{self, NormRect, TextSelection};
use crate::document::{Document, DocumentMetadata, SearchMatch};
use crate::error::{CoreError, CoreResult};
use crate::render::{PixelBuffer, Viewport};

/// The plain-text backend: a reflow view over the file's paragraphs. Title/author are unknown for a
/// bare `.txt`, so the metadata is empty.
pub struct PlainBackend {
    view: ReflowView,
    meta: DocumentMetadata,
}

impl PlainBackend {
    /// Decode `bytes` as UTF-8, split into paragraphs, and build a reflowable view for `viewport`.
    /// Invalid UTF-8 is rejected at the boundary with a typed error — never a panic (RR21-FR3).
    pub fn open(bytes: Vec<u8>, viewport: Viewport) -> CoreResult<Self> {
        let text = String::from_utf8(bytes)
            .map_err(|_| CoreError::InvalidArgument("text file is not valid UTF-8".into()))?;
        let blocks = paragraphs_to_blocks(&text);
        // One reflowable unit = the whole file; the engine paginates it to the viewport.
        let view = ReflowView::new(vec![blocks], viewport.width, viewport.height);
        Ok(Self {
            view,
            meta: DocumentMetadata::default(),
        })
    }
}

impl Document for PlainBackend {
    fn page_count(&self) -> usize {
        self.view.page_count()
    }

    fn metadata(&self) -> DocumentMetadata {
        self.meta.clone()
    }

    fn render_page(&self, index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
        self.view.render(index, buf)
    }

    fn word_at(&self, page: usize, x: f32, y: f32) -> Option<TextSelection> {
        text_select::word_at(&self.view.page_chars(page), x, y)
    }

    fn text_in_rect(&self, page: usize, rect: NormRect) -> TextSelection {
        text_select::text_in_rect(&self.view.page_chars(page), rect)
    }

    fn search_page(&self, page: usize, query: &str) -> Vec<SearchMatch> {
        text_select::find_matches(&self.view.page_chars(page), query)
    }

    fn set_text_scale(&self, scale: f32, current_page: usize) -> Option<usize> {
        Some(self.view.set_scale(scale, current_page))
    }

    fn set_line_spacing(&self, mult: f32, current_page: usize) -> Option<usize> {
        Some(self.view.set_line_spacing(mult, current_page))
    }

    fn set_alignment(&self, align_code: i32, current_page: usize) -> Option<usize> {
        Some(self.view.set_alignment(align_code, current_page))
    }
}

/// Split plain text into paragraph [`Block`]s. A blank line ends a paragraph; the lines within a
/// paragraph are joined with a space (soft wrap). Whitespace-only input yields no blocks — the
/// reflow engine still renders a single blank page (no panic).
///
/// Iterating with [`str::lines`] keeps peak memory at ~1× the input (it borrows slices and strips a
/// trailing `\r`, so both `\n` and `\r\n` endings work) rather than allocating normalized copies of
/// an up-to-2 GiB file. Trade-off: a classic-Mac file with bare-`\r` line endings (extinct since
/// ~2001) is treated as a single line. Per-line [`str::trim`] also normalizes away leading
/// indentation and Unicode-space-only lines — acceptable for flowed prose, lossy for ASCII art /
/// code listings (out of scope for a `.txt` reader).
fn paragraphs_to_blocks(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut para = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            push_paragraph(&mut blocks, &mut para);
        } else {
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(line.trim());
        }
    }
    push_paragraph(&mut blocks, &mut para);
    blocks
}

/// Flush the accumulated paragraph text into a [`Block::Paragraph`] (dropping an empty one) and reset
/// the buffer.
fn push_paragraph(blocks: &mut Vec<Block>, para: &mut String) {
    let text = para.trim();
    if !text.is_empty() {
        blocks.push(Block::Paragraph {
            content: vec![Inline::Run(TextRun {
                text: text.to_string(),
                bold: false,
                italic: false,
                href: None,
            })],
        });
    }
    para.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport() -> Viewport {
        Viewport {
            width: 1404,
            height: 1872,
            dpi: 226,
        }
    }

    /// Concatenate a paragraph block's text runs (test helper).
    fn block_text(b: &Block) -> String {
        match b {
            Block::Paragraph { content } => content
                .iter()
                .filter_map(|i| match i {
                    Inline::Run(r) => Some(r.text.as_str()),
                    _ => None,
                })
                .collect(),
            _ => String::new(),
        }
    }

    #[test]
    fn paragraphs_split_on_blank_lines() {
        let blocks = paragraphs_to_blocks("First para.\n\nSecond para.");
        assert_eq!(blocks.len(), 2);
        assert_eq!(block_text(&blocks[0]), "First para.");
        assert_eq!(block_text(&blocks[1]), "Second para.");
    }

    #[test]
    fn single_newline_is_a_soft_wrap() {
        let blocks = paragraphs_to_blocks("line one\nline two");
        assert_eq!(blocks.len(), 1);
        assert_eq!(block_text(&blocks[0]), "line one line two");
    }

    #[test]
    fn crlf_and_multiple_blank_lines_collapse() {
        assert_eq!(paragraphs_to_blocks("a\r\n\r\nb").len(), 2);
        assert_eq!(paragraphs_to_blocks("a\n\n\n\nb").len(), 2);
    }

    #[test]
    fn leading_and_trailing_blank_lines_make_no_empty_paragraphs() {
        let blocks = paragraphs_to_blocks("\n\nbody\n\n");
        assert_eq!(blocks.len(), 1);
        assert_eq!(block_text(&blocks[0]), "body");
    }

    #[test]
    fn a_whitespace_only_line_separates_paragraphs() {
        // A line that is only spaces/tabs acts as a blank-line separator.
        let blocks = paragraphs_to_blocks("a\n   \nb");
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn bare_cr_is_treated_as_a_single_line() {
        // `str::lines()` does not split on a classic-Mac bare `\r`; documented trade-off.
        let blocks = paragraphs_to_blocks("one\rtwo");
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn whitespace_only_yields_no_blocks() {
        assert!(paragraphs_to_blocks("   \n  \n\t").is_empty());
        assert!(paragraphs_to_blocks("").is_empty());
    }

    #[test]
    fn open_rejects_non_utf8() {
        // `matches!` (not `unwrap_err`) so the test needs no `Debug` on the backend.
        assert!(matches!(
            PlainBackend::open(vec![0xff, 0xfe, 0x00], viewport()),
            Err(CoreError::InvalidArgument(_))
        ));
    }

    #[test]
    fn open_empty_file_is_one_blank_page() {
        let doc = PlainBackend::open(Vec::new(), viewport()).unwrap();
        assert_eq!(doc.page_count(), 1);
        assert_eq!(doc.metadata(), DocumentMetadata::default());
        // Render must succeed (blank page), and an out-of-range page is a typed error, not a panic.
        let mut bytes = vec![0u8; 1404 * 1872 * 4];
        let mut buf = PixelBuffer::from_rgba(&mut bytes, 1404, 1872).unwrap();
        assert!(doc.render_page(0, &mut buf).is_ok());
        assert!(doc.render_page(99, &mut buf).is_err());
    }

    #[test]
    fn metadata_is_empty_for_bare_txt() {
        let doc = PlainBackend::open(b"hello".to_vec(), viewport()).unwrap();
        assert_eq!(doc.metadata().title, None);
        assert_eq!(doc.metadata().author, None);
    }

    #[test]
    fn reflow_controls_are_supported() {
        let doc = PlainBackend::open(b"some flowing text".to_vec(), viewport()).unwrap();
        // Reflowable backend: each control returns a preserved-position page (Some), not None.
        assert!(doc.set_text_scale(1.5, 0).is_some());
        assert!(doc.set_line_spacing(1.7, 0).is_some());
        assert!(doc.set_alignment(1, 0).is_some());
    }

    #[test]
    fn search_finds_text_through_the_reused_pipeline() {
        let doc = PlainBackend::open(b"the quick brown fox jumps".to_vec(), viewport()).unwrap();
        assert!(
            !doc.search_page(0, "brown").is_empty(),
            "a present word should be found via the shared search path"
        );
        assert!(
            doc.search_page(0, "zebra").is_empty(),
            "an absent word should yield no matches"
        );
    }

    #[test]
    fn text_in_rect_selects_the_page_body() {
        let doc = PlainBackend::open(b"alpha beta gamma".to_vec(), viewport()).unwrap();
        let whole_page = NormRect {
            x0: 0.0,
            y0: 0.0,
            x1: 1.0,
            y1: 1.0,
        };
        let sel = doc.text_in_rect(0, whole_page);
        assert!(
            sel.text.contains("alpha") && sel.text.contains("gamma"),
            "a full-page rect should select the body text, got {:?}",
            sel.text
        );
    }

    #[test]
    fn word_at_returns_the_word_under_a_glyph() {
        let doc = PlainBackend::open(b"hello world".to_vec(), viewport()).unwrap();
        // Aim at the centre of the first glyph (the `view` field is reachable from the same module).
        let chars = doc.view.page_chars(0);
        let first = chars.first().expect("laid-out text has glyphs");
        let cx = (first.rect.x0 + first.rect.x1) / 2.0;
        let cy = (first.rect.y0 + first.rect.y1) / 2.0;
        let sel = doc.word_at(0, cx, cy).expect("a glyph centre hits a word");
        assert!(!sel.text.is_empty());
    }
}
