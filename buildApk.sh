#!/bin/bash
set -euo pipefail

# =========================================================
# buildApk.sh — build the inkread Android APK
#
# Pipeline (see spec/SPEC-RUST-READER.md RR1 / RR29):
#   1. cargo-ndk → compile the Rust core (libreader.so) into app/src/main/jniLibs,
#                  then PRUNE stray dep dylibs cargo-ndk also copies (libpdfium_render*,
#                  libreader_core) and strip libreader.so — keep the APK lean.
#   2. pdfium    → stage the pinned vendored libpdfium.so into jniLibs (download +
#                  sha256-verify if absent) so the build is self-contained (RR5-FR5).
#   3. gradle    → assemble + sign the APK (bundles libreader.so + libpdfium.so + fonts)
#   4. (opt)     → adb install to the connected Supernote
#
# Conventions (color output, JDK guard, fail-fast) mirror sn-dictionary/buildPlugin.sh.
#
# Usage:
#   ./buildApk.sh                 # release build (signed via gradle signingConfig)
#   ./buildApk.sh --debug         # debug build
#   ./buildApk.sh --install       # build, then adb install to the device
#   ./buildApk.sh --device SN...  # target a specific adb serial
# =========================================================

# ---- config (override via env) ----
ABIS="${INKREAD_ABIS:-arm64-v8a}"        # RK3566 = arm64; add armeabi-v7a if needed
APP_MODULE="${INKREAD_APP_MODULE:-app}"  # gradle module that produces the APK
PKG_ID="${INKREAD_PKG_ID:-dev.jraghavan.inkread}"

# Vendored pdfium runtime (bblanchon prebuilt, BSD-3-Clause; see LICENSES-3RDPARTY.md,
# RR5-FR1/FR5). Pinned for a reproducible build. The arm64 binary is sha256-verified;
# other ABIs are ELF-sanity-checked only (v1 ships arm64-v8a).
PDFIUM_TAG="${INKREAD_PDFIUM_TAG:-chromium/7881}"   # = PDFium 151.0.7881.0
PDFIUM_SHA256_ARM64="${INKREAD_PDFIUM_SHA256_ARM64:-d043f50a7ab42c91b6dfa98a1bcbd64b77cf27532991f331318a357bcf7cb363}"

# =========================================================
# write_color_output — colored stderr message ($1 msg, $2 color)
# =========================================================
write_color_output() {
    local message="${1:-}" color="${2:-}"
    case "$color" in
        Red)    printf "\033[31m%s\033[0m\n" "$message" >&2 ;;
        Green)  printf "\033[32m%s\033[0m\n" "$message" >&2 ;;
        Yellow) printf "\033[33m%s\033[0m\n" "$message" >&2 ;;
        Blue)   printf "\033[34m%s\033[0m\n" "$message" >&2 ;;
        *)      printf "%s\n" "$message" >&2 ;;
    esac
}

die() { write_color_output "$1" "Red"; exit 1; }

# =========================================================
# select_jdk — Gradle 8.x needs JDK 17–21. If the active java is newer
# and JAVA_HOME isn't pinned, pick an installed JDK 21 (then 17) on macOS.
# (Mirrors sn-dictionary's guard: Java 24+ breaks Gradle with
# "Unsupported class file major version".)
# =========================================================
select_jdk() {
    [[ -n "${JAVA_HOME:-}" ]] && return 0
    command -v java >/dev/null 2>&1 || return 0
    local jmajor
    jmajor="$(java -version 2>&1 | sed -nE 's/.*version "([0-9]+).*/\1/p' | head -1)"
    [[ -z "$jmajor" || "$jmajor" -le 23 ]] && return 0
    if command -v /usr/libexec/java_home >/dev/null 2>&1; then
        local jh v
        for v in 21 17; do
            if jh="$(/usr/libexec/java_home -v "$v" 2>/dev/null)"; then
                export JAVA_HOME="$jh"
                write_color_output "Active JDK $jmajor too new for Gradle; selected JDK $v: $JAVA_HOME" "Yellow"
                return 0
            fi
        done
    fi
    die "Active JDK $jmajor is unsupported by Gradle (needs 17–21). Install JDK 21 ('brew install temurin@21') or set JAVA_HOME."
}

# =========================================================
# use_pinned_rust — force the rust-toolchain.toml-pinned toolchain onto PATH.
# A Homebrew `rust` (a standalone rustc/cargo, NOT a rustup shim) ignores
# rust-toolchain.toml and, if earlier on PATH, shadows the pinned toolchain —
# cargo-ndk then builds with it and fails ("rustc 1.85.0 is not supported").
# `rustup which cargo`, run in the repo, honours rust-toolchain.toml and returns
# the pinned toolchain's real cargo; prepend its bin dir so cargo-ndk's child
# cargo/rustc resolve to the pinned compiler. No-op if rustup isn't installed.
# =========================================================
use_pinned_rust() {
    local root="$1" cargo_path bindir
    command -v rustup >/dev/null 2>&1 || return 0
    cargo_path="$(cd "$root" && rustup which cargo 2>/dev/null || true)"
    [[ -n "$cargo_path" && -x "$cargo_path" ]] || return 0
    bindir="$(dirname "$cargo_path")"
    export PATH="$bindir:$PATH"
    write_color_output "rust: $("$cargo_path" --version 2>/dev/null) (pinned via rust-toolchain.toml)" "Blue"
}

# =========================================================
# build_rust — compile the Rust core into jniLibs via cargo-ndk (RR29-FR1)
# =========================================================
build_rust() {
    local root="$1" profile_flag="$2"
    use_pinned_rust "$root"
    command -v cargo >/dev/null 2>&1 || die "cargo not found — install Rust (https://rustup.rs)."
    cargo ndk --version >/dev/null 2>&1 || die "cargo-ndk not found — 'cargo install cargo-ndk' and install the Android NDK."
    [[ -f "$root/Cargo.toml" ]] || die "Cargo.toml not found at repo root — scaffold the Rust workspace first (spec RR1)."

    local jnilibs="$root/$APP_MODULE/src/main/jniLibs"
    local targets=()
    IFS=',' read -ra _abis <<< "$ABIS"; for a in "${_abis[@]}"; do targets+=("-t" "$a"); done

    # The Android build MUST enable the JNI bridge so libreader.so exports the
    # Java_dev_jraghavan_inkread_NativeBridge_* symbols (feature-gated; the host gate
    # builds WITHOUT it so jni stays out of the host graph — RR1-AC3 / IR-7).
    write_color_output "cargo ndk ${targets[*]} build $profile_flag --features jni-bridge → $jnilibs" "Blue"
    ( cd "$root" && cargo ndk "${targets[@]}" -o "$jnilibs" build $profile_flag -p reader-core --features jni-bridge ) \
        && write_color_output "Rust core built (libreader.so staged in jniLibs)" "Green" \
        || die "cargo-ndk build failed"

    prune_stray_libs "$jnilibs"
    strip_reader_lib "$jnilibs"
}

# =========================================================
# prune_stray_libs — cargo-ndk's -o copies EVERY .so it builds, including Rust
# dep dylibs (libpdfium_render*.so, ~33 MB) and the stale pre-rename
# libreader_core.so. The APK needs only our cdylib (libreader.so) + the vendored
# runtime (libpdfium.so). Drop everything else so the APK stays lean (RR29).
# =========================================================
prune_stray_libs() {
    local jnilibs="$1" abi dir f
    IFS=',' read -ra _abis <<< "$ABIS"
    for abi in "${_abis[@]}"; do
        dir="$jnilibs/$abi"
        [[ -d "$dir" ]] || continue
        while IFS= read -r f; do
            [[ -n "$f" ]] && write_color_output "pruned stray lib: $(basename "$f")" "Yellow"
        done < <(find "$dir" -maxdepth 1 -name '*.so' ! -name 'libreader.so' ! -name 'libpdfium.so' -print)
        find "$dir" -maxdepth 1 -name '*.so' ! -name 'libreader.so' ! -name 'libpdfium.so' -delete
    done
}

# =========================================================
# strip_reader_lib — best-effort strip of libreader.so via the NDK's llvm-strip
# (a debug-profile lib is large with debuginfo; release is already stripped).
# Never fails the build — if no NDK/strip tool is found, leave the lib as-is.
# =========================================================
strip_reader_lib() {
    local jnilibs="$1" ndk="" strip_bin="" abi lib
    ndk="${ANDROID_NDK_HOME:-}"
    if [[ -z "$ndk" && -n "${ANDROID_HOME:-}" ]]; then
        ndk="$(ls -d "$ANDROID_HOME"/ndk/* 2>/dev/null | sort -V | tail -1)"
    fi
    [[ -n "$ndk" ]] && strip_bin="$(ls "$ndk"/toolchains/llvm/prebuilt/*/bin/llvm-strip 2>/dev/null | head -1)"
    [[ -n "$strip_bin" ]] || return 0
    IFS=',' read -ra _abis <<< "$ABIS"
    for abi in "${_abis[@]}"; do
        lib="$jnilibs/$abi/libreader.so"
        [[ -f "$lib" ]] || continue
        if "$strip_bin" --strip-unneeded "$lib" 2>/dev/null; then
            write_color_output "stripped libreader.so ($abi)" "Green"
        fi
    done
}

# =========================================================
# pdfium_arch_for_abi — map an Android ABI to bblanchon's asset arch suffix.
# =========================================================
pdfium_arch_for_abi() {
    case "$1" in
        arm64-v8a)   echo "arm64" ;;
        armeabi-v7a) echo "arm" ;;
        x86_64)      echo "x64" ;;
        x86)         echo "x86" ;;
        *)           echo "" ;;
    esac
}

# =========================================================
# stage_pdfium — ensure jniLibs/<abi>/libpdfium.so exists. If absent, download the
# pinned bblanchon prebuilt (cached under build/), extract, sha256-verify (arm64),
# and stage it. Keeps the 6 MB binary out of git while making the build
# self-contained + reproducible (RR5-FR5 / RR29). (RR18: pdfium is BSD, not the
# private research material — fine to vendor at build time.)
# =========================================================
stage_pdfium() {
    local root="$1"
    local jnilibs="$root/$APP_MODULE/src/main/jniLibs"
    local cache="$root/build/pdfium-vendor/${PDFIUM_TAG//\//-}"
    local abi dest arch tgz url tmp so got
    IFS=',' read -ra _abis <<< "$ABIS"
    for abi in "${_abis[@]}"; do
        dest="$jnilibs/$abi/libpdfium.so"
        if [[ -f "$dest" ]]; then
            write_color_output "pdfium present: $dest" "Green"; continue
        fi
        arch="$(pdfium_arch_for_abi "$abi")"
        [[ -n "$arch" ]] || die "No pdfium asset mapping for ABI '$abi'"
        command -v curl >/dev/null 2>&1 || die "curl not found (needed to fetch pdfium $PDFIUM_TAG)"
        mkdir -p "$cache" "$jnilibs/$abi"
        tgz="$cache/pdfium-android-$arch.tgz"
        if [[ ! -f "$tgz" ]]; then
            url="https://github.com/bblanchon/pdfium-binaries/releases/download/${PDFIUM_TAG//\//%2F}/pdfium-android-$arch.tgz"
            write_color_output "fetch pdfium $PDFIUM_TAG ($arch) → $tgz" "Blue"
            curl -sSL -o "$tgz" "$url" || die "pdfium download failed: $url"
        fi
        tmp="$(mktemp -d)"
        tar -xzf "$tgz" -C "$tmp" || { rm -rf "$tmp"; die "pdfium extract failed: $tgz"; }
        so="$(find "$tmp" -name libpdfium.so -print -quit)"
        [[ -n "$so" ]] || { rm -rf "$tmp"; die "libpdfium.so not found inside $tgz"; }
        if [[ "$arch" == "arm64" && -n "$PDFIUM_SHA256_ARM64" ]]; then
            got="$(shasum -a 256 "$so" | awk '{print $1}')"
            [[ "$got" == "$PDFIUM_SHA256_ARM64" ]] || { rm -rf "$tmp"; \
                die "pdfium sha256 mismatch ($arch): got $got, expected $PDFIUM_SHA256_ARM64"; }
        fi
        cp "$so" "$dest"; rm -rf "$tmp"
        write_color_output "pdfium staged: $dest ($PDFIUM_TAG, $arch)" "Green"
    done
}

# =========================================================
# build_apk — assemble + sign the APK via gradle (RR29-FR1/FR2)
# =========================================================
build_apk() {
    local root="$1" gradle_task="$2"
    local android_dir="$root"                  # single-module layout: gradlew at repo root
    [[ -f "$root/settings.gradle" || -f "$root/settings.gradle.kts" ]] || \
        die "No Gradle project found — scaffold the Android app ($APP_MODULE/) first (spec RR1/RR29)."

    local gradlew="$root/gradlew"
    select_jdk
    write_color_output "gradle: $gradle_task" "Blue"
    ( cd "$android_dir"
        if [[ -f "$gradlew" ]]; then chmod +x "$gradlew"; "$gradlew" "$gradle_task"
        elif command -v gradle >/dev/null 2>&1; then gradle "$gradle_task"
        else die "gradle/gradlew not found"; fi
    ) && write_color_output "APK assembled" "Green" || die "APK build failed"
}

# =========================================================
# find_apk — locate the built APK
# =========================================================
find_apk() {
    local root="$1" variant="$2"
    find "$root/$APP_MODULE/build/outputs/apk/$variant" -type f -name '*.apk' -print -quit 2>/dev/null
}

# =========================================================
# main
# =========================================================
main() {
    local root; root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    local variant="release" profile_flag="--release" install=0 device=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --debug)   variant="debug"; profile_flag="" ;;
            --install) install=1 ;;
            --device)  device="$2"; shift ;;
            -h|--help) sed -n '3,22p' "${BASH_SOURCE[0]}"; exit 0 ;;
            *) die "Unknown arg: $1" ;;
        esac
        shift
    done

    write_color_output "Running on: $(uname -s)  |  inkread APK build ($variant)" "Blue"

    # Not scaffolded yet? Explain instead of failing opaquely.
    if [[ ! -f "$root/Cargo.toml" && ! -f "$root/settings.gradle" && ! -f "$root/settings.gradle.kts" ]]; then
        write_color_output "This repo isn't scaffolded yet (no Cargo workspace / Gradle project)." "Yellow"
        write_color_output "Scaffold per spec RR1 (reader-core cdylib + $APP_MODULE/ Android module), then re-run." "Yellow"
        exit 2
    fi

    build_rust "$root" "$profile_flag"
    stage_pdfium "$root"
    # Capitalize the first letter portably (macOS ships bash 3.2, which lacks ${var^}).
    local task                              # assembleRelease / assembleDebug
    case "$variant" in
        release) task="assembleRelease" ;;
        debug)   task="assembleDebug" ;;
        *)       die "Unknown variant: $variant" ;;
    esac
    build_apk "$root" "$task"

    local apk; apk="$(find_apk "$root" "$variant")"
    [[ -n "$apk" ]] || die "Built, but no APK found under $APP_MODULE/build/outputs/apk/$variant"
    write_color_output "APK: $apk" "Green"

    if [[ "$install" == "1" ]]; then
        command -v adb >/dev/null 2>&1 || die "adb not found"
        local dev_flag=(); [[ -n "$device" ]] && dev_flag=(-s "$device")
        write_color_output "adb install -r $apk" "Blue"
        adb "${dev_flag[@]}" install -r "$apk" \
            && write_color_output "Installed $PKG_ID on the device" "Green" \
            || die "adb install failed (is the Supernote connected with USB debugging on?)"
    fi
}

main "$@"
