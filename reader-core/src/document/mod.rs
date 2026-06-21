//! The `Document` trait — the M0 subset every format implements (RR5-FR2).
//!
//! M0 needs only metadata + page count + render-page (Amendment 4 / scope fence): no
//! `text_runs`/`toc`/`search`/`hint_page`/`page_hash` (those are M1+). The PDF backend
//! ([`fixed::PdfBackend`]) is the one implementation in M0.

pub mod fixed;
pub mod reflow;
pub mod text_select;

pub use text_select::{CharBox, NormRect, SearchMatch, TextSelection};

use crate::error::CoreResult;
use crate::render::PixelBuffer;

/// One ink stroke to write into the PDF on export (ADR-INKREAD-0005). Points are normalized page
/// space `[0,1]` (top-left origin, y-down) exactly like the ink model; the backend maps them to PDF
/// points. `width` is normalized to the page width. RGBA is the true stroke colour.
#[derive(Debug, Clone)]
pub struct ExportStroke {
    /// Stroke path as normalized `(x, y)` pairs.
    pub points: Vec<(f32, f32)>,
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
    /// Normalized stroke width (fraction of page width).
    pub width: f32,
}

/// The ink to write onto one page on export.
#[derive(Debug, Clone)]
pub struct PageInk {
    /// 0-based page index.
    pub page: usize,
    /// The strokes on that page (paint order).
    pub strokes: Vec<ExportStroke>,
}

/// How an export writes the ink into the PDF (ADR-INKREAD-0005).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportMode {
    /// Editable PDF **Ink annotations** (selectable/removable in standard viewers; colour preserved).
    Annotations,
    /// **Flatten** the ink into the page content (visible in every viewer; not editable afterward).
    Flatten,
}

/// How a fixed-layout page is fit to the viewport (RR4 — KOReader's "Fit"). All modes preserve the
/// page's aspect ratio (unlike a raw stretch); the difference is which dimension is filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FitMode {
    /// Fit the whole page within the viewport (contain), centered with white letterbox. Default.
    #[default]
    Page,
    /// Scale so the page **width** fills the viewport; taller pages overflow vertically (pannable).
    Width,
    /// Scale so the page **height** fills the viewport; wider pages overflow horizontally (pannable).
    Height,
}

impl FitMode {
    /// Decode the wire integer (`0=Page, 1=Width, 2=Height`); unknown → `Page`.
    #[must_use]
    pub fn from_code(code: i32) -> FitMode {
        match code {
            1 => FitMode::Width,
            2 => FitMode::Height,
            _ => FitMode::Page,
        }
    }
}

/// Document metadata (title/author) — the M0 subset (RR5-FR2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentMetadata {
    /// Document title, if present.
    pub title: Option<String>,
    /// Document author, if present.
    pub author: Option<String>,
}

/// One table-of-contents entry; nested `children` form the outline tree (RR11-FR2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    /// Display title for the entry.
    pub title: String,
    /// Target page index for a fixed-layout document, or `None` for a label-only/unresolved
    /// entry (the UI shows it but tapping it does not navigate).
    pub target_page: Option<usize>,
    /// Nested child entries (sub-sections).
    pub children: Vec<TocEntry>,
}

/// Where a [`PageLink`] navigates (RR11-FR3): an internal page jump or an external URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// Jump to a 0-based page index within this document.
    Page(usize),
    /// Open an external URI (e.g. `https://…`).
    Uri(String),
}

/// A clickable link region on a page (RR11-FR3).
///
/// The rect is **normalized to the rendered page**: `[0,1]` on both axes with a **top-left**
/// origin, matching the stretched-to-viewport render (`set_target_size`). So the shell
/// hit-tests a tap as `(tap_x / view_w, tap_y / view_h)` with no scale/DPI/letterbox math.
#[derive(Debug, Clone, PartialEq)]
pub struct PageLink {
    /// Left edge, normalized `[0,1]`.
    pub x0: f32,
    /// Top edge, normalized `[0,1]`.
    pub y0: f32,
    /// Right edge, normalized `[0,1]`.
    pub x1: f32,
    /// Bottom edge, normalized `[0,1]`.
    pub y1: f32,
    /// Where the link goes.
    pub target: LinkTarget,
}

/// Wire-format version for the page-links the JNI bridge ships to the shell (RR11-FR3).
const LINKS_WIRE_VERSION: u8 = 0x01;

/// Encode a page's links for the shell to hit-test (RR11-FR3). Pure marshaling (no device/JNI
/// types) so it is host-tested. Layout (little-endian):
/// `[ver=1][count: u16]` then, per link, `[x0 f32][y0 f32][x1 f32][y1 f32][kind: u8]` followed
/// by either `[page: u32]` (kind 0, internal) or `[uri_len: u16][uri: utf-8 × uri_len]`
/// (kind 1, external). Counts/lengths saturate rather than panic (RR21-FR3).
#[must_use]
pub fn encode_links_wire(links: &[PageLink]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(LINKS_WIRE_VERSION);
    let count = u16::try_from(links.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&count.to_le_bytes());
    for link in links.iter().take(count as usize) {
        out.extend_from_slice(&link.x0.to_le_bytes());
        out.extend_from_slice(&link.y0.to_le_bytes());
        out.extend_from_slice(&link.x1.to_le_bytes());
        out.extend_from_slice(&link.y1.to_le_bytes());
        match &link.target {
            LinkTarget::Page(page) => {
                out.push(0u8);
                out.extend_from_slice(&u32::try_from(*page).unwrap_or(u32::MAX).to_le_bytes());
            }
            LinkTarget::Uri(uri) => {
                out.push(1u8);
                let bytes = uri.as_bytes();
                let len = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
                out.extend_from_slice(&len.to_le_bytes());
                out.extend_from_slice(&bytes[..len as usize]);
            }
        }
    }
    out
}

/// Wire-format version for the flattened TOC the JNI bridge ships to the shell (RR11-FR2).
const TOC_WIRE_VERSION: u8 = 0x01;

/// Encode a TOC tree as a flat **pre-order** list for the shell to render as an indented
/// list (RR11-FR2). Pure marshaling (no device/JNI types) so it is host-tested.
///
/// Layout (little-endian, mirroring the other wire codecs):
/// `[ver=1][count: u16]` then, per entry, `[depth: u8][flags: u8][target_page: u32]
/// [title_len: u16][title: utf-8 × title_len]`. `flags` bit 0 = the entry has a resolved
/// `target_page` (an unresolved/label-only entry carries `0` and bit 0 clear). Depth, count,
/// page, and title length all saturate rather than panic on pathological input (RR21-FR3).
#[must_use]
pub fn encode_toc_wire(entries: &[TocEntry]) -> Vec<u8> {
    fn walk(out: &mut Vec<u8>, count: &mut u16, entries: &[TocEntry], depth: u8) {
        for e in entries {
            let (flags, page) = match e.target_page {
                Some(p) => (1u8, u32::try_from(p).unwrap_or(u32::MAX)),
                None => (0u8, 0u32),
            };
            let title = e.title.as_bytes();
            let len = u16::try_from(title.len()).unwrap_or(u16::MAX);
            out.push(depth);
            out.push(flags);
            out.extend_from_slice(&page.to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&title[..len as usize]);
            *count = count.saturating_add(1);
            walk(out, count, &e.children, depth.saturating_add(1));
        }
    }
    let mut body = Vec::new();
    let mut count = 0u16;
    walk(&mut body, &mut count, entries, 0);
    let mut out = Vec::with_capacity(3 + body.len());
    out.push(TOC_WIRE_VERSION);
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Wire-format version for a [`TextSelection`] the JNI bridge ships to the shell (RR11 / D1).
const SELECTION_WIRE_VERSION: u8 = 0x01;

/// Encode a text selection (the selected string + its highlight boxes) for the shell — pure
/// marshaling (no device/JNI types), so it is host-tested. Layout (little-endian):
/// `[ver=1][text_len: u16][text: utf-8 × text_len][box_count: u16]` then per box
/// `[x0 f32][y0 f32][x1 f32][y1 f32]`. Lengths saturate rather than panic (RR21-FR3).
#[must_use]
pub fn encode_selection_wire(sel: &TextSelection) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(SELECTION_WIRE_VERSION);
    let bytes = sel.text.as_bytes();
    let tlen = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&tlen.to_le_bytes());
    out.extend_from_slice(&bytes[..tlen as usize]);
    let bcount = u16::try_from(sel.boxes.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&bcount.to_le_bytes());
    for b in sel.boxes.iter().take(bcount as usize) {
        out.extend_from_slice(&b.x0.to_le_bytes());
        out.extend_from_slice(&b.y0.to_le_bytes());
        out.extend_from_slice(&b.x1.to_le_bytes());
        out.extend_from_slice(&b.y1.to_le_bytes());
    }
    out
}

/// Wire-format version for a page's [`SearchMatch`]es the JNI bridge ships to the shell (RR2 search).
const SEARCH_WIRE_VERSION: u8 = 0x01;

/// Encode one page's search matches for the shell — pure marshaling (no device/JNI types), so it is
/// host-tested. Layout (little-endian): `[ver=1][match_count: u16]` then, per match,
/// `[snippet_len: u16][snippet: utf-8 × snippet_len][box_count: u16]` followed by `box_count` ×
/// `[x0 f32][y0 f32][x1 f32][y1 f32]`. Counts/lengths saturate rather than panic (RR21-FR3).
#[must_use]
pub fn encode_search_wire(matches: &[SearchMatch]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(SEARCH_WIRE_VERSION);
    let count = u16::try_from(matches.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&count.to_le_bytes());
    for m in matches.iter().take(count as usize) {
        let bytes = m.snippet.as_bytes();
        let slen = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&slen.to_le_bytes());
        out.extend_from_slice(&bytes[..slen as usize]);
        let bcount = u16::try_from(m.boxes.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&bcount.to_le_bytes());
        for b in m.boxes.iter().take(bcount as usize) {
            out.extend_from_slice(&b.x0.to_le_bytes());
            out.extend_from_slice(&b.y0.to_le_bytes());
            out.extend_from_slice(&b.x1.to_le_bytes());
            out.extend_from_slice(&b.y1.to_le_bytes());
        }
    }
    out
}

/// The core trait every format implements (M0 subset).
///
/// Render targets a borrowed [`PixelBuffer`] (Fork 4); the backend white-fills before
/// rasterizing (RR4-FR3) and resolves the channel order so the buffer ends up RGBA
/// (Amendment 3).
pub trait Document {
    /// Total page count (fixed-layout: a trivial integer model — RR5-FR2).
    fn page_count(&self) -> usize;

    /// Title/author metadata.
    fn metadata(&self) -> DocumentMetadata;

    /// Render page `index` into `buf` at the buffer's resolution.
    ///
    /// Returns [`CoreError::PageOutOfRange`](crate::error::CoreError::PageOutOfRange) for a
    /// bad index and a typed backend error on render failure — never panics (RR21-FR3).
    fn render_page(&self, index: usize, buf: &mut PixelBuffer<'_>) -> CoreResult<()>;

    /// Render a magnified, panned view of `index` for pinch-zoom (RR5-FR3). The page is rendered as
    /// if [`Self::render_page`] (stretched to the buffer) then scaled by `zoom` (≥1, buffer-relative)
    /// and the `buf`-sized window at `(offset_x, offset_y)` scaled-buffer pixels is shown. So at
    /// `zoom == 1, offset == 0` it is identical to [`Self::render_page`], and the shell's normalized
    /// ink overlay transforms by the same `(zoom, offset)`. Default: ignores zoom → render_page.
    fn render_zoom(
        &self,
        index: usize,
        buf: &mut PixelBuffer<'_>,
        _zoom: f32,
        _offset_x: i32,
        _offset_y: i32,
    ) -> CoreResult<()> {
        self.render_page(index, buf)
    }

    /// Render page `index` fit to `buf` per [`FitMode`], preserving the page aspect ratio (RR4).
    /// `pan_x`/`pan_y` are normalized `[0,1]` scroll positions used only when a mode overflows the
    /// viewport (e.g. `Width` on a tall page); centered when the page fits. Default: ignores fit and
    /// falls back to [`Self::render_page`] — correct for reflowable backends (EPUB), which already
    /// fill the viewport. Fixed-layout backends (PDF) override this.
    fn render_fit(
        &self,
        index: usize,
        buf: &mut PixelBuffer<'_>,
        _mode: FitMode,
        _pan_x: f32,
        _pan_y: f32,
    ) -> CoreResult<()> {
        self.render_page(index, buf)
    }

    /// The page's **content bounding box** in normalized page coords `[0,1]` (RR4 — KOReader's
    /// auto Crop): the tight rectangle around the non-white content, used to trim white margins.
    /// `None` if undetectable or not applicable (blank page / reflowable backend). Never panics.
    fn content_bbox(&self, _index: usize) -> Option<NormRect> {
        None
    }

    /// Render the normalized `crop` sub-rect of page `index` fit to `buf` per [`FitMode`] (RR4 —
    /// Crop). Like [`Self::render_fit`] but only the cropped region is shown (margins trimmed), so
    /// the content fills the screen. `pan_x`/`pan_y` scroll an overflowing axis. Default: ignores
    /// the crop and falls back to [`Self::render_fit`] (reflowable backends don't crop).
    fn render_cropped(
        &self,
        index: usize,
        buf: &mut PixelBuffer<'_>,
        _crop: NormRect,
        mode: FitMode,
        pan_x: f32,
        pan_y: f32,
    ) -> CoreResult<()> {
        self.render_fit(index, buf, mode, pan_x, pan_y)
    }

    /// Adjust the reflow **text scale** (`1.0` = the backend default size) and repaginate (RR2-FR5
    /// font-size control). `current_page` is the page the reader is on *before* the change; the
    /// backend returns `Some(new_page)` to jump to so the reading position is preserved across the
    /// reflow (anchored to the chapter), or `None` for a **fixed-layout** format that has no reflow
    /// (PDF — the shell leaves the page unchanged). Default: unsupported (`None`). Interior
    /// mutability lets this stay `&self` like the render path.
    fn set_text_scale(&self, _scale: f32, _current_page: usize) -> Option<usize> {
        None
    }

    /// Set the reflow **line spacing** multiplier (e.g. `1.2`/`1.4`/`1.7`) and repaginate, preserving
    /// the chapter (RR4 — KOReader's "Line Spacing"). Returns the new page, or `None` for a
    /// fixed-layout format (PDF). Default: unsupported.
    fn set_line_spacing(&self, _mult: f32, _current_page: usize) -> Option<usize> {
        None
    }

    /// Set the reflow **alignment** (`0=Left, 1=Justify, 2=Center, 3=Right`) and repaginate,
    /// preserving the chapter (RR4 — KOReader's "Alignment"). Returns the new page, or `None` for a
    /// fixed-layout format (PDF). Default: unsupported.
    fn set_alignment(&self, _align_code: i32, _current_page: usize) -> Option<usize> {
        None
    }

    /// Prefetch hint (RR4-FR7): the core may call this after rendering the current page so a
    /// backend can warm an internal handle for the likely-next page, making a page turn blit a
    /// ready buffer. Default: a no-op (backends opt in). Must never panic on a bad index.
    fn hint_page(&self, _next: usize) {}

    /// The document outline as a nested tree (RR5-FR2 / RR11-FR2). Default: empty — a format
    /// with no outline (or a backend that hasn't implemented it) returns no entries, never an
    /// error.
    fn toc(&self) -> Vec<TocEntry> {
        Vec::new()
    }

    /// The clickable links on `page` (RR11-FR3), normalized to the rendered page (see
    /// [`PageLink`]). Default: empty — a format without links (or an out-of-range page) returns
    /// no links, never an error or panic (RR21-FR3).
    fn page_links(&self, _page: usize) -> Vec<PageLink> {
        Vec::new()
    }

    /// The word under the normalized point `(x, y)` on `page` (RR11 / dictionary tap, D1).
    /// Default: `None` — a format without a text layer has no selection (never panics).
    fn word_at(&self, _page: usize, _x: f32, _y: f32) -> Option<TextSelection> {
        None
    }

    /// The text whose glyphs fall within the normalized `rect` on `page` (RR11 / drag-highlight,
    /// D1). Default: an empty selection.
    fn text_in_rect(&self, _page: usize, _rect: NormRect) -> TextSelection {
        TextSelection::default()
    }

    /// Find `query` on `page` (case-insensitive, whitespace-normalized substring) — RR2 in-document
    /// search. Returns one [`SearchMatch`] per occurrence (highlight boxes + context snippet) in
    /// reading order. The shell drives this page-by-page so the scan stays memory-bounded (RR19).
    /// Default: empty — a format with no text layer is not searchable (never panics, RR21-FR3).
    fn search_page(&self, _page: usize, _query: &str) -> Vec<SearchMatch> {
        Vec::new()
    }

    /// Write `page_ink` into the document and save it to `out_path` (ADR-INKREAD-0005). [`ExportMode`]
    /// chooses editable annotations vs. flattened page content. Default: unsupported (a format/back-
    /// end without write support returns an error, never panics — RR21-FR3).
    fn export_pdf(
        &mut self,
        _out_path: &str,
        _page_ink: &[PageInk],
        _mode: ExportMode,
    ) -> CoreResult<()> {
        Err(crate::error::CoreError::RenderBackend(
            "PDF export not supported by this backend".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Plain;
    impl Document for Plain {
        fn page_count(&self) -> usize {
            1
        }
        fn metadata(&self) -> DocumentMetadata {
            DocumentMetadata::default()
        }
        fn render_page(&self, _index: usize, _buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
            Ok(())
        }
    }

    // RR4-FR7: a backend that does not override hint_page gets the no-op default (no panic).
    #[test]
    fn hint_page_default_is_noop() {
        Plain.hint_page(0);
        Plain.hint_page(99); // out-of-range hint must not panic either
    }

    // RR11-FR2: the default toc is empty (a format with no outline), never an error.
    #[test]
    fn toc_default_is_empty() {
        assert!(Plain.toc().is_empty());
    }

    // RR11-FR2: TocEntry nests into a tree; an unresolved entry carries target_page = None.
    #[test]
    fn toc_entry_tree_nests() {
        let tree = TocEntry {
            title: "Part I".into(),
            target_page: Some(0),
            children: vec![
                TocEntry {
                    title: "Chapter 1".into(),
                    target_page: Some(3),
                    children: vec![],
                },
                TocEntry {
                    title: "(unresolved)".into(),
                    target_page: None,
                    children: vec![],
                },
            ],
        };
        assert_eq!(tree.children.len(), 2);
        assert_eq!(tree.children[0].target_page, Some(3));
        assert_eq!(tree.children[1].target_page, None);
    }

    // RR11-FR2: the flattened TOC wire is pre-order with depth, resolves the target flag, and
    // round-trips titles. Decoded here the same way the Kotlin shell does.
    #[test]
    fn encode_toc_wire_flattens_preorder_with_depth_and_targets() {
        let tree = vec![TocEntry {
            title: "Part I".into(),
            target_page: Some(0),
            children: vec![
                TocEntry {
                    title: "Chapter 1".into(),
                    target_page: Some(3),
                    children: vec![],
                },
                TocEntry {
                    title: "(unresolved)".into(),
                    target_page: None,
                    children: vec![],
                },
            ],
        }];
        let bytes = encode_toc_wire(&tree);
        assert_eq!(bytes[0], TOC_WIRE_VERSION);
        let count = u16::from_le_bytes([bytes[1], bytes[2]]);
        assert_eq!(count, 3, "Part I + 2 children, pre-order");

        // Walk the records and collect (depth, has_target, page, title).
        let mut off = 3usize;
        let mut got = Vec::new();
        for _ in 0..count {
            let depth = bytes[off];
            let flags = bytes[off + 1];
            let page = u32::from_le_bytes(bytes[off + 2..off + 6].try_into().unwrap());
            let len = u16::from_le_bytes([bytes[off + 6], bytes[off + 7]]) as usize;
            let title = String::from_utf8(bytes[off + 8..off + 8 + len].to_vec()).unwrap();
            got.push((depth, flags & 1 == 1, page, title));
            off += 8 + len;
        }
        assert_eq!(off, bytes.len(), "no trailing bytes");
        assert_eq!(got[0], (0, true, 0, "Part I".to_string()));
        assert_eq!(got[1], (1, true, 3, "Chapter 1".to_string()));
        assert_eq!(got[2], (1, false, 0, "(unresolved)".to_string()));
    }

    // RR11-FR2: an empty outline encodes to just the header (version + zero count).
    #[test]
    fn encode_toc_wire_empty_is_header_only() {
        let bytes = encode_toc_wire(&[]);
        assert_eq!(bytes, vec![TOC_WIRE_VERSION, 0, 0]);
    }

    // RR11-FR3: page links encode with the normalized rect + internal/external target, decoded
    // here the same way the Kotlin shell does.
    #[test]
    fn encode_links_wire_roundtrips_internal_and_external() {
        let links = vec![
            PageLink {
                x0: 0.1,
                y0: 0.2,
                x1: 0.3,
                y1: 0.25,
                target: LinkTarget::Page(7),
            },
            PageLink {
                x0: 0.5,
                y0: 0.6,
                x1: 0.7,
                y1: 0.65,
                target: LinkTarget::Uri("https://example.com".into()),
            },
        ];
        let b = encode_links_wire(&links);
        assert_eq!(b[0], LINKS_WIRE_VERSION);
        assert_eq!(u16::from_le_bytes([b[1], b[2]]), 2);

        let mut off = 3usize;
        let read_f32 = |b: &[u8], o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        // link 0: internal Page(7)
        assert!((read_f32(&b, off) - 0.1).abs() < 1e-6);
        assert!((read_f32(&b, off + 12) - 0.25).abs() < 1e-6);
        assert_eq!(b[off + 16], 0, "kind internal");
        assert_eq!(
            u32::from_le_bytes(b[off + 17..off + 21].try_into().unwrap()),
            7
        );
        off += 21;
        // link 1: external URI
        assert_eq!(b[off + 16], 1, "kind external");
        let len = u16::from_le_bytes([b[off + 17], b[off + 18]]) as usize;
        let uri = String::from_utf8(b[off + 19..off + 19 + len].to_vec()).unwrap();
        assert_eq!(uri, "https://example.com");
        assert_eq!(off + 19 + len, b.len(), "no trailing bytes");
    }

    // RR11-FR3: a page with no links encodes to just the header.
    #[test]
    fn encode_links_wire_empty_is_header_only() {
        assert_eq!(encode_links_wire(&[]), vec![LINKS_WIRE_VERSION, 0, 0]);
    }

    // RR4-FR7: a backend that overrides hint_page receives the requested page.
    #[test]
    fn hint_page_override_is_called() {
        use std::cell::Cell;
        struct Hinter {
            last: Cell<Option<usize>>,
        }
        impl Document for Hinter {
            fn page_count(&self) -> usize {
                3
            }
            fn metadata(&self) -> DocumentMetadata {
                DocumentMetadata::default()
            }
            fn render_page(&self, _index: usize, _buf: &mut PixelBuffer<'_>) -> CoreResult<()> {
                Ok(())
            }
            fn hint_page(&self, next: usize) {
                self.last.set(Some(next));
            }
        }
        let h = Hinter {
            last: Cell::new(None),
        };
        h.hint_page(2);
        assert_eq!(h.last.get(), Some(2));
    }

    #[test]
    fn encode_selection_wire_carries_text_and_boxes() {
        let sel = TextSelection {
            text: "hello".into(),
            boxes: vec![NormRect {
                x0: 0.1,
                y0: 0.2,
                x1: 0.3,
                y1: 0.25,
            }],
        };
        let w = encode_selection_wire(&sel);
        assert_eq!(w[0], SELECTION_WIRE_VERSION);
        assert_eq!(u16::from_le_bytes([w[1], w[2]]), 5); // text len
        assert_eq!(&w[3..8], b"hello");
        assert_eq!(u16::from_le_bytes([w[8], w[9]]), 1); // box count
        assert_eq!(f32::from_le_bytes([w[10], w[11], w[12], w[13]]), 0.1);
    }

    #[test]
    fn encode_selection_wire_empty_is_header_only() {
        let w = encode_selection_wire(&TextSelection::default());
        // ver + text_len(0) + box_count(0)
        assert_eq!(w, vec![SELECTION_WIRE_VERSION, 0, 0, 0, 0]);
    }

    // RR2 search: a page's matches encode snippet + boxes, decoded here the way the Kotlin shell does.
    #[test]
    fn encode_search_wire_roundtrips_matches() {
        let matches = vec![
            SearchMatch {
                boxes: vec![NormRect {
                    x0: 0.1,
                    y0: 0.2,
                    x1: 0.4,
                    y1: 0.25,
                }],
                snippet: "…the needle here…".into(),
            },
            SearchMatch {
                boxes: vec![
                    NormRect {
                        x0: 0.0,
                        y0: 0.5,
                        x1: 0.3,
                        y1: 0.55,
                    },
                    NormRect {
                        x0: 0.0,
                        y0: 0.56,
                        x1: 0.2,
                        y1: 0.61,
                    },
                ],
                snippet: "two-line match".into(),
            },
        ];
        let b = encode_search_wire(&matches);
        assert_eq!(b[0], SEARCH_WIRE_VERSION);
        assert_eq!(u16::from_le_bytes([b[1], b[2]]), 2);

        let mut off = 3usize;
        let read_f32 = |b: &[u8], o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        // match 0
        let slen = u16::from_le_bytes([b[off], b[off + 1]]) as usize;
        off += 2;
        assert_eq!(
            String::from_utf8(b[off..off + slen].to_vec()).unwrap(),
            "…the needle here…"
        );
        off += slen;
        assert_eq!(u16::from_le_bytes([b[off], b[off + 1]]), 1);
        off += 2;
        assert!((read_f32(&b, off) - 0.1).abs() < 1e-6);
        off += 16;
        // match 1: 2 boxes
        let slen = u16::from_le_bytes([b[off], b[off + 1]]) as usize;
        off += 2 + slen;
        assert_eq!(u16::from_le_bytes([b[off], b[off + 1]]), 2);
        off += 2 + 32;
        assert_eq!(off, b.len(), "no trailing bytes");
    }

    #[test]
    fn encode_search_wire_empty_is_header_only() {
        assert_eq!(encode_search_wire(&[]), vec![SEARCH_WIRE_VERSION, 0, 0]);
    }
}
