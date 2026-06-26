#!/bin/bash
# =========================================================
# daily-preview.sh (#66) — HOST validation of the Daily pipeline.
# Reproduces the DEVICE issue: fetches ALL the seeded feeds, runs the REAL crate
# logic (parse → fetch articles → extract → assemble), then (a) dumps EVERY
# article's extracted text so each source's quality can be reviewed, and (b)
# renders the issue pages to PNGs. So we validate on a host, not blind on e-ink.
#
#   scripts/daily-preview.sh [PER_SOURCE]      (default 3 articles per feed)
# Output: target/daily-preview/dump.txt  +  target/daily-preview/page-NN.png
# =========================================================
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

TC=$(grep -oE 'channel = "[^"]+"' rust-toolchain.toml 2>/dev/null | cut -d'"' -f2 || true)
[ -n "${TC:-}" ] && export PATH="$HOME/.rustup/toolchains/$TC-aarch64-apple-darwin/bin:$PATH"

PER_SOURCE="${1:-3}"
out="target/daily-preview"
mkdir -p "$out"

# Build the CLI once so the per-feed parse calls are fast (not `cargo run` x10).
cargo build -q -p inkread-daily --example daily_cli
BIN="target/debug/examples/daily_cli"

python3 - "$BIN" "$PER_SOURCE" "$out" <<'PY'
import json, sys, subprocess, urllib.request, datetime
BIN, PER, out = sys.argv[1], int(sys.argv[2]), sys.argv[3]

# The exact feeds DailyController seeds on the device.
FEEDS = [
    ("Hacker News",       "https://hnrss.org/frontpage"),
    ("Lobsters",          "https://lobste.rs/rss"),
    ("Ars Technica",      "https://feeds.arstechnica.com/arstechnica/index"),
    ("The Verge",         "https://www.theverge.com/rss/index.xml"),
    ("TechCrunch",        "https://techcrunch.com/feed/"),
    ("BBC News",          "https://feeds.bbci.co.uk/news/rss.xml"),
    ("NPR News",          "https://feeds.npr.org/1001/rss.xml"),
    ("Quanta Magazine",   "https://api.quantamagazine.org/feed/"),
    ("Daring Fireball",   "https://daringfireball.net/feeds/main"),
    ("Smashing Magazine", "https://www.smashingmagazine.com/feed/"),
]

def fetch(url, timeout=20):
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0 (inkread-daily preview)"})
    return urllib.request.urlopen(req, timeout=timeout).read().decode("utf-8", "replace")

arts = []
for name, feed in FEEDS:
    try:
        xml = fetch(feed)
    except Exception as e:
        print(f"  FEED FAIL {name}: {e}", file=sys.stderr); continue
    items = json.loads(subprocess.run([BIN, "parse"], input=xml, capture_output=True, text=True).stdout or "[]")
    got = 0
    for it in items[:PER]:
        url = it.get("url", "")
        try:
            html = fetch(url)
        except Exception as e:
            print(f"  art FAIL {name} {url}: {e}", file=sys.stderr); continue
        arts.append({"title": it.get("title", ""), "source": name, "url": url, "html": html})
        got += 1
    print(f"  {name}: {got} articles", file=sys.stderr)

issue = {"title": "inkread daily",
         "date": datetime.date.today().strftime("%A, %B %d, %Y"),
         "articles": arts}
json.dump(issue, open(f"{out}/issue.json", "w"))
print(f"  TOTAL {len(arts)} articles from {len(FEEDS)} feeds", file=sys.stderr)
PY

echo "── dumping extracted text per article → $out/dump.txt ──"
"$BIN" dump < "$out/issue.json" > "$out/dump.txt"
"$BIN" assemble < "$out/issue.json" > "$out/issue.epub"
echo "issue.epub: $(wc -c < "$out/issue.epub") bytes; dump: $(wc -l < "$out/dump.txt") lines"

INKREAD_ISSUE_EPUB="$PWD/$out/issue.epub" cargo test -q -p reader-core daily_render_dump -- --ignored --nocapture 2>&1 | tail -2
echo "→ review $out/dump.txt and $out/page-*.png"
