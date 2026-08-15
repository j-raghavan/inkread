//! Repair a book's text encoding before the EPUB parser sees it (#159).
//!
//! EPUB requires UTF-8 (or UTF-16) for its XML, and rbook enforces that strictly: *any* resource it
//! reads as a string must be valid UTF-8, or the whole book fails to open with `InvalidUtf8Resource`
//! — not just the offending chapter, but the package document and table of contents too.
//!
//! Real libraries are less tidy than the spec. A great many Russian EPUBs — especially the ones
//! converted from FB2 — are **windows-1251**, and a reader that refuses them is simply a reader that
//! cannot open Russian books. The same is true of koi8-r, and of Shift-JIS/Big5/EUC-KR for books
//! from those traditions.
//!
//! So this module runs first: it decodes any text resource that is not valid UTF-8, using the
//! encoding the document itself declares, and rewrites the container with UTF-8 throughout. A
//! well-formed book is returned **byte-identical and untouched** — the cost is one scan of the
//! archive's text entries, and nothing else.
//!
//! Pure and host-testable: bytes in, bytes out, no IO.

use std::io::{Cursor, Read, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// Return `bytes` with every non-UTF-8 text resource transcoded to UTF-8.
///
/// Returns the input unchanged when nothing needs repair, when the input is not a readable ZIP (the
/// EPUB parser will report that far better than we can), or when rebuilding fails for any reason —
/// this is a repair pass, and it must never be the thing that breaks a book that would have opened.
#[must_use]
pub(crate) fn to_utf8(bytes: Vec<u8>) -> Vec<u8> {
    match read_repaired(&bytes) {
        Some(entries) => rebuild(&entries).unwrap_or(bytes),
        // The overwhelmingly common case: a conforming book, handed back untouched.
        None => bytes,
    }
}

/// Read every entry, transcoding the text resources that need it.
///
/// `None` means "hand back the original bytes": either nothing needed repair, or the container
/// could not be read at all — in which case the EPUB parser's own error is the useful one.
fn read_repaired(bytes: &[u8]) -> Option<Vec<(String, Vec<u8>, bool)>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).ok()?;
    let mut entries: Vec<(String, Vec<u8>, bool)> = Vec::with_capacity(archive.len());
    let mut repaired_any = false;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).ok()?;
        if !file.is_file() {
            continue;
        }
        let name = file.name().to_string();
        let deflate = file.compression() != CompressionMethod::Stored;
        let mut data = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut data).ok()?;
        if is_text_resource(&name) {
            if let Some(fixed) = repair(&data) {
                data = fixed;
                repaired_any = true;
            }
        }
        entries.push((name, data, deflate));
    }
    repaired_any.then_some(entries)
}

/// Resources whose bytes are read as text. Images and fonts are copied through verbatim — running a
/// decoder over a JPEG would be both pointless and destructive.
fn is_text_resource(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        ".xhtml", ".html", ".htm", ".xml", ".opf", ".ncx", ".css", ".svg",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

/// Decode `data` to UTF-8 if it is not already valid UTF-8, returning `None` when no repair is
/// needed. The declared encoding is honoured where the document states one; otherwise windows-1252
/// stands in, being the label the WHATWG standard maps the unlabelled legacy case onto.
fn repair(data: &[u8]) -> Option<Vec<u8>> {
    if std::str::from_utf8(data).is_ok() {
        return None;
    }
    let label = declared_encoding(data);
    let encoding = label
        .as_deref()
        .and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        .unwrap_or(encoding_rs::WINDOWS_1252);
    // `decode` never fails: unmappable bytes become U+FFFD, which is the right trade here. A page
    // with a few replacement characters is a page the reader can still read; refusing the book is not.
    let (text, _, _) = encoding.decode(data);
    Some(rewrite_declaration(&text).into_bytes())
}

/// The encoding a document declares — an XML prolog `encoding="…"`, else an HTML
/// `<meta charset="…">` or `<meta http-equiv … content="text/html; charset=…">`.
///
/// Scanned over the *bytes*, since the document is by definition not decodable yet, and only over
/// the head of the file where a declaration is permitted to appear. The label itself is always
/// ASCII in every encoding this matters for.
fn declared_encoding(data: &[u8]) -> Option<String> {
    const HEAD: usize = 2048;
    let head = &data[..data.len().min(HEAD)];
    let text: String = head.iter().map(|&b| b as char).collect();
    let lower = text.to_ascii_lowercase();

    // `encoding="…"` (XML prolog) or `charset=…` (HTML meta), quoted or bare.
    for key in ["encoding=", "charset="] {
        let mut from = 0;
        while let Some(at) = lower[from..].find(key) {
            let start = from + at + key.len();
            let rest = &text[start..];
            let value: String = match rest.chars().next() {
                Some(q @ ('"' | '\'')) => rest[1..].chars().take_while(|&c| c != q).collect(),
                _ => rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                    .collect(),
            };
            if !value.trim().is_empty() {
                return Some(value.trim().to_string());
            }
            from = start;
        }
    }
    None
}

/// Point any encoding declaration at UTF-8, now that the bytes are UTF-8.
///
/// This is not cosmetic: leaving `encoding="windows-1251"` in the prolog of a document that is now
/// UTF-8 invites the next XML parser down the line to decode it a second time, back into mojibake.
fn rewrite_declaration(text: &str) -> String {
    let Some(end) = text.find("?>") else {
        return text.to_string();
    };
    let (prolog, rest) = text.split_at(end);
    let lower = prolog.to_ascii_lowercase();
    let Some(at) = lower.find("encoding=") else {
        return text.to_string();
    };
    let after = at + "encoding=".len();
    let value_len = match prolog[after..].chars().next() {
        Some(q @ ('"' | '\'')) => prolog[after + 1..].chars().take_while(|&c| c != q).count() + 2, // the quotes
        _ => prolog[after..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .count(),
    };
    format!(
        "{}\"UTF-8\"{}{}",
        &prolog[..after],
        &prolog[after + value_len..],
        rest
    )
}

/// Write the entries back into a fresh EPUB container.
///
/// `mimetype` must come first and be stored uncompressed for the file to be a valid EPUB, so it is
/// emitted ahead of everything else regardless of where it sat in the original.
fn rebuild(entries: &[(String, Vec<u8>, bool)]) -> Option<Vec<u8>> {
    let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    if let Some((name, data, _)) = entries.iter().find(|(n, _, _)| n == "mimetype") {
        zw.start_file(name, stored).ok()?;
        zw.write_all(data).ok()?;
    }
    for (name, data, deflate) in entries {
        if name == "mimetype" {
            continue;
        }
        zw.start_file(name, if *deflate { deflated } else { stored })
            .ok()?;
        zw.write_all(data).ok()?;
    }
    Some(zw.finish().ok()?.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_utf8_is_returned_byte_identical() {
        // The common case must cost nothing and change nothing — not even a re-zip, which would
        // churn every book's bytes on every open.
        let original = crate::tests::sample_epub();
        assert_eq!(to_utf8(original.clone()), original);
    }

    #[test]
    fn a_non_zip_is_handed_back_untouched() {
        // The EPUB parser reports "not a container" far better than a repair pass can.
        let junk = b"not a zip at all".to_vec();
        assert_eq!(to_utf8(junk.clone()), junk);
    }

    #[test]
    fn declared_encoding_reads_the_xml_prolog() {
        let xml = br#"<?xml version="1.0" encoding="windows-1251"?><html/>"#;
        assert_eq!(declared_encoding(xml).as_deref(), Some("windows-1251"));
    }

    #[test]
    fn declared_encoding_reads_an_html_meta_charset() {
        let html = br#"<html><head><meta charset="koi8-r"></head></html>"#;
        assert_eq!(declared_encoding(html).as_deref(), Some("koi8-r"));
        let legacy =
            br#"<html><head><meta http-equiv="Content-Type" content="text/html; charset=koi8-r"/></head></html>"#;
        assert_eq!(declared_encoding(legacy).as_deref(), Some("koi8-r"));
    }

    #[test]
    fn declared_encoding_is_none_when_undeclared() {
        assert_eq!(declared_encoding(b"<html><body>hi</body></html>"), None);
    }

    #[test]
    fn the_rewritten_declaration_says_utf8() {
        let out = rewrite_declaration(r#"<?xml version="1.0" encoding="windows-1251"?><p/>"#);
        assert_eq!(out, r#"<?xml version="1.0" encoding="UTF-8"?><p/>"#);
        // Single quotes and a missing declaration must both survive the rewrite unharmed.
        assert_eq!(
            rewrite_declaration(r#"<?xml version='1.0' encoding='koi8-r'?><p/>"#),
            r#"<?xml version='1.0' encoding="UTF-8"?><p/>"#,
        );
        assert_eq!(rewrite_declaration("<p/>"), "<p/>");
    }

    #[test]
    fn repair_is_a_no_op_on_utf8_and_decodes_otherwise() {
        assert_eq!(repair("Первая глава".as_bytes()), None, "already UTF-8");

        let cp1251 = crate::tests::to_cp1251(
            r#"<?xml version="1.0" encoding="windows-1251"?><p>Первая глава</p>"#,
        );
        let fixed = repair(&cp1251).expect("cp1251 needs repair");
        let text = String::from_utf8(fixed).expect("repaired to valid UTF-8");
        assert!(text.contains("Первая глава"), "decoded to {text:?}");
        assert!(
            text.contains(r#"encoding="UTF-8""#),
            "declaration rewritten"
        );
    }

    #[test]
    fn an_undeclared_legacy_document_still_decodes_rather_than_failing() {
        // No declaration and not UTF-8 — the WHATWG fallback keeps the book openable instead of
        // letting one unlabelled chapter take the whole thing down.
        let bytes = vec![0xE9, 0xE8, b'!']; // windows-1252 "éè!"
        let fixed = repair(&bytes).expect("needs repair");
        assert!(String::from_utf8(fixed).is_ok());
    }
}
