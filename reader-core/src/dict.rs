//! Dictionary integration (RR12 / ADR-INKREAD-0009 D3): re-exports the `inkread-dict` engine and
//! the wire codec the JNI bridge uses to ship a [`Definition`] to the shell. The engine itself is
//! the separate `inkread-dict` crate (offline-testable); this module is just the reader-core-side
//! glue + marshaling, kept pure so it is host-tested.

pub use inkread_dict::{Definition, Dict, DictError, DictResult, OnlineEntry, OnlineSource};

/// Wire-format version for a [`Definition`] (RR12 / D3).
const DEFINITION_WIRE_VERSION: u8 = 0x01;

/// Encode a (possibly absent) definition for the shell. Layout (little-endian):
/// `[ver=1][found: u8]`; when `found == 1`:
/// `[headword_len: u16][headword][lang_len: u8][lang][sense_count: u16]` then per sense
/// `[len: u16][utf-8]`, then `[syn_count: u16]` then per synonym `[len: u16][utf-8]`.
/// Pure marshaling; lengths saturate rather than panic (RR21-FR3).
#[must_use]
pub fn encode_definition_wire(def: Option<&Definition>) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(DEFINITION_WIRE_VERSION);
    let Some(d) = def else {
        out.push(0); // not found
        return out;
    };
    out.push(1);
    put_str16(&mut out, &d.headword);
    let lang = d.lang.as_bytes();
    let llen = u8::try_from(lang.len()).unwrap_or(u8::MAX);
    out.push(llen);
    out.extend_from_slice(&lang[..llen as usize]);
    put_list(&mut out, &d.senses);
    put_list(&mut out, &d.synonyms);
    out
}

/// Append a `u16`-length-prefixed UTF-8 string.
fn put_str16(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let len = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&bytes[..len as usize]);
}

/// Append a `u16` count followed by that many `u16`-length-prefixed strings.
fn put_list(out: &mut Vec<u8>, items: &[String]) {
    let count = u16::try_from(items.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&count.to_le_bytes());
    for s in items.iter().take(count as usize) {
        put_str16(out, s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_found_definition() {
        let def = Definition {
            headword: "run".into(),
            lang: "en".into(),
            senses: vec!["to move quickly".into(), "to operate".into()],
            synonyms: vec!["sprint".into()],
        };
        let w = encode_definition_wire(Some(&def));
        assert_eq!(w[0], DEFINITION_WIRE_VERSION);
        assert_eq!(w[1], 1, "found");
        // headword len = 3 (LE u16) at [2..4]
        assert_eq!(u16::from_le_bytes([w[2], w[3]]), 3);
        assert_eq!(&w[4..7], b"run");
        // the lang, sense text, and synonym are all present in the payload
        let s = String::from_utf8_lossy(&w);
        assert!(s.contains("en"));
        assert!(s.contains("to move quickly") && s.contains("to operate"));
        assert!(s.contains("sprint"));
    }

    #[test]
    fn encodes_a_miss() {
        let w = encode_definition_wire(None);
        assert_eq!(w, vec![DEFINITION_WIRE_VERSION, 0]);
    }

    #[test]
    fn empty_senses_and_synonyms_round_to_zero_counts() {
        let def = Definition {
            headword: "x".into(),
            lang: "en".into(),
            senses: vec![],
            synonyms: vec![],
        };
        let w = encode_definition_wire(Some(&def));
        // ver + found + hw_len(2)+ "x" + lang_len(1)+"en" + sense_count(2)=0 + syn_count(2)=0
        assert_eq!(w.len(), 1 + 1 + 2 + 1 + 1 + 2 + 2 + 2);
    }
}
