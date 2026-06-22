#!/bin/bash
set -euo pipefail
# =========================================================
# stage-dict.sh — build the on-device English dictionary corpus (ADR-INKREAD-0009 D2).
#
# Imports free, public-domain WordNet (definitions + tight inline `[syn:]` synonyms) into
# app/src/main/assets/dict.db via the build-dict tool. The app opens this SQLite read-only at
# runtime; other languages fall back to (cached) online lookup. dict.db is a gitignored build
# artifact — regenerate it here; it is staged into the APK by buildApk.sh.
#
#   ./scripts/stage-dict.sh          # build the corpus if app/src/main/assets/dict.db is absent
#   ./scripts/stage-dict.sh --force  # rebuild it
# =========================================================
cd "$(dirname "${BASH_SOURCE[0]}")/.."

ASSETS="app/src/main/assets"
OUT="$ASSETS/dict.db"
CACHE="build/dict-vendor"
# WordNet 3.0 (Princeton), StarDict packaging by Hu Zheng — definitions + synsets, free to use.
WN_URL="http://download.huzheng.org/dict.org/stardict-dictd_www.dict.org_wn-2.4.2.tar.bz2"

[[ "${1:-}" == "--force" ]] && rm -f "$OUT"
if [[ -f "$OUT" ]]; then
    echo "dict.db present ($(du -h "$OUT" | cut -f1)) — pass --force to rebuild."
    exit 0
fi

mkdir -p "$CACHE" "$ASSETS"
tgz="$CACHE/$(basename "$WN_URL")"
if [[ ! -f "$tgz" ]]; then
    command -v curl >/dev/null 2>&1 || { echo "curl not found (needed to fetch WordNet)"; exit 1; }
    echo "fetching WordNet → $tgz"
    curl -sSL --max-time 300 -o "$tgz" "$WN_URL"
fi
dir="$CACHE/wn"
if [[ ! -d "$dir" ]]; then
    mkdir -p "$dir"
    tar -xjf "$tgz" -C "$dir" --strip-components=1
fi

# Use the rust-toolchain.toml-pinned cargo if rustup is available (a Homebrew rust would fail MSRV).
CARGO="cargo"
if command -v rustup >/dev/null 2>&1; then
    CARGO="$(dirname "$(rustup which cargo)")/cargo"
fi
"$CARGO" build -p build-dict --release
rm -f "$OUT"
./target/release/build-dict "$dir" "$OUT" en
command -v sqlite3 >/dev/null 2>&1 && sqlite3 "$OUT" "VACUUM;" || true
echo "staged $OUT ($(du -h "$OUT" | cut -f1))"
