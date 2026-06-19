//! The `Document` trait — the M0 subset every format implements (RR5-FR2).
//!
//! M0 needs only metadata + page count + render-page (Amendment 4 / scope fence): no
//! `text_runs`/`toc`/`search`/`hint_page`/`page_hash` (those are M1+). The PDF backend
//! ([`fixed::PdfBackend`]) is the one implementation in M0.

pub mod fixed;
pub mod text_select;

pub use text_select::{CharBox, NormRect, TextSelection};

use crate::error::CoreResult;
use crate::render::PixelBuffer;

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
}
