#!/bin/bash
set -euo pipefail

# =========================================================
# buildApk.sh — build the inkread Android APK
#
# Pipeline (see spec/SPEC-RUST-READER.md RR1 / RR29):
#   1. cargo-ndk → compile the Rust core (libreader.so) into app/src/main/jniLibs
#   2. gradle    → assemble + sign the APK (bundles libreader.so + libpdfium.so + fonts)
#   3. (opt)     → adb install to the connected Supernote
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
# build_rust — compile the Rust core into jniLibs via cargo-ndk (RR29-FR1)
# =========================================================
build_rust() {
    local root="$1" profile_flag="$2"
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
    local task="assemble${variant^}"        # assembleRelease / assembleDebug
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
