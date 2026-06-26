#!/bin/bash
# =========================================================
# daily-preview.sh (#66) — HOST preview of the Daily pipeline.
# Fetches a feed, runs the REAL crate logic (parse → fetch articles → extract →
# assemble issue EPUB), then renders the issue's pages to PNGs you can actually
# look at — so the reading experience is verified on a host, not blind on e-ink.
#
#   scripts/daily-preview.sh [FEED_URL] [N_ARTICLES]
# Defaults: Hacker News, 4 articles. Output: target/daily-preview/page-NN.png
# =========================================================
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# Use the rustup toolchain (Homebrew rustc shadows it — repo gotcha).
TC=$(grep -oE 'channel = "[^"]+"' rust-toolchain.toml 2>/dev/null | cut -d'"' -f2 || true)
[ -n "${TC:-}" ] && export PATH="$HOME/.rustup/toolchains/$TC-aarch64-apple-darwin/bin:$PATH"

FEED="${1:-https://news.ycombinator.com/rss}"
N="${2:-4}"
tmp="$(mktemp -d)"
echo "feed: $FEED  (top $N articles)  tmp: $tmp"

curl -fsSL -A "Mozilla/5.0 (inkread-daily preview)" "$FEED" > "$tmp/feed.xml"
cargo run -q -p inkread-daily --example daily_cli -- parse < "$tmp/feed.xml" > "$tmp/items.json"

python3 - "$tmp" "$N" <<'PY'
import json, sys, urllib.request, datetime
tmp, n = sys.argv[1], int(sys.argv[2])
items = json.load(open(f"{tmp}/items.json"))[:n]
arts = []
for it in items:
    url = it.get("url", "")
    html = ""
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
        html = urllib.request.urlopen(req, timeout=15).read().decode("utf-8", "replace")
    except Exception as e:
        print(f"  fetch FAIL {url}: {e}", file=sys.stderr)
    arts.append({"title": it.get("title", ""), "source": "Hacker News", "url": url, "html": html})
issue = {"title": "inkread daily",
         "date": datetime.date.today().strftime("%A, %B %d, %Y"),
         "articles": arts}
json.dump(issue, open(f"{tmp}/issue.json", "w"))
print(f"  built issue with {len(arts)} articles", file=sys.stderr)
PY

cargo run -q -p inkread-daily --example daily_cli -- assemble < "$tmp/issue.json" > "$tmp/issue.epub"
echo "issue.epub: $(wc -c < "$tmp/issue.epub") bytes"

INKREAD_ISSUE_EPUB="$tmp/issue.epub" cargo test -q -p reader-core daily_render_dump -- --ignored --nocapture 2>&1 | tail -3
echo "→ open target/daily-preview/page-*.png"
