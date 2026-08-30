#!/usr/bin/env bash
# Run the CI gates locally with HONEST exit codes, before commit/push.
#
# Why this exists: piping a gate to `tail`/`grep` (e.g. `cargo fmt --check | tail -1 && echo OK`)
# reports the PIPE's exit status (the last command), not cargo's — so a real failure reads as green.
# This script runs each gate bare, captures `$?`, and fails loudly. Mirrors .github/workflows/ci.yml.
#
# Usage: scripts/gate.sh [--fast]   (--fast skips the Android NDK cross-check + gradle unit tests)
set -u

cd "$(dirname "$0")/.."

# Pin to the rustup toolchain (Homebrew's cargo can shadow it and format differently).
TC=$(grep -oE 'channel = "[^"]+"' rust-toolchain.toml 2>/dev/null | cut -d'"' -f2)
[ -n "${TC:-}" ] && export PATH="$HOME/.rustup/toolchains/$TC-aarch64-apple-darwin/bin:$PATH"
export JAVA_HOME="${JAVA_HOME:-/Library/Java/JavaVirtualMachines/temurin-21.jdk/Contents/Home}"

fail=0
run() { # run "<label>" cmd...
  local label="$1"; shift
  printf '── %s\n' "$label"
  if "$@"; then
    printf '   ✓ %s\n' "$label"
  else
    printf '   ✗ %s (exit %d)\n' "$label" "$?"
    fail=1
  fi
}

run "fmt --check"      cargo fmt --all --check
run "clippy -D warnings" cargo clippy --all -- -D warnings
run "test --workspace" cargo test --workspace

# The pdfium render tests SKIP when no libpdfium is bound, and a skip still passes — so a green
# suite can silently be missing a thousand-line file. Harmless for `cargo test`; not harmless for
# `cargo llvm-cov`, where it drops the workspace figure by ~5 points and looks like a real
# regression. Warn rather than fail: a contributor without the library is expected (CONTRIBUTING).
# 2>&1, not 2>/dev/null: the skip notices are `eprintln!`, so they arrive on stderr — the
# obvious redirect discards exactly the lines being counted and reports a clean run.
skips=$(cargo test -p reader-core document::fixed::pdf -- --nocapture 2>&1 | grep -c "SKIP" || true)
if [ "${skips:-0}" -gt 0 ]; then
  printf '── pdfium render tests\n'
  printf '   ! %s pdfium test(s) SKIPPED — set PDFIUM_DYNAMIC_LIB_PATH to exercise the render path.\n' "$skips"
  printf '     Coverage measured now will read far below CI. See CONTRIBUTING.md.\n'
else
  printf '── pdfium render tests\n   ✓ executing (no skips)\n'
fi
run "IR-7 vendor-neutral core" ./scripts/check-vendor-neutral.sh
run "dependency licenses"      ./scripts/check-licenses.sh

if [ "${1:-}" != "--fast" ]; then
  NDK=$(ls -d "$HOME"/Library/Android/sdk/ndk/* 2>/dev/null | sort -V | tail -1)
  if [ -n "$NDK" ]; then
    TCBIN="$NDK/toolchains/llvm/prebuilt/darwin-x86_64/bin"
    export CC_aarch64_linux_android="$TCBIN/aarch64-linux-android24-clang"
    export AR_aarch64_linux_android="$TCBIN/llvm-ar"
    export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$TCBIN/aarch64-linux-android24-clang"
    run "jni-bridge (aarch64-android)" \
      cargo check -p reader-core --features jni-bridge --target aarch64-linux-android
  else
    printf '── jni-bridge: SKIP (no Android NDK found)\n'
  fi
  run "android unit tests (host JVM)" ./gradlew :app:testReleaseUnitTest -q
fi

if [ "$fail" -eq 0 ]; then
  printf '\nALL GATES GREEN\n'
else
  printf '\nGATE FAILURE — do not commit/push\n'
fi
exit "$fail"
