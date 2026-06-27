//! Host preview CLI for the Daily pipeline (#66) — exercises the REAL crate logic so the output can
//! be inspected on a host instead of blind on the e-ink device.
//!
//!   daily_cli parse     < feed.xml    > items.json   (RSS/Atom → article links)
//!   daily_cli assemble  < issue.json  > issue.epub   (fetched issue JSON → issue EPUB bytes)
//!
//! `scripts/daily-preview.sh` ties these together with curl + a render dump.

use std::io::{Read, Write};

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .expect("read stdin");
    let text = String::from_utf8_lossy(&input);

    match mode.as_str() {
        "parse" => println!("{}", inkread_daily::parse_feed_json(&text)),
        "dump" => print!("{}", inkread_daily::debug_dump_issue(&text)),
        "assemble" => match inkread_daily::assemble_issue_from_json(&text) {
            Ok(bytes) => std::io::stdout().write_all(&bytes).expect("write epub"),
            Err(e) => {
                eprintln!("assemble error: {e}");
                std::process::exit(1);
            }
        },
        other => {
            eprintln!("usage: daily_cli parse|dump|assemble   (got {other:?})");
            std::process::exit(2);
        }
    }
}
