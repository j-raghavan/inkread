# Third-party license manifest (RR18-AC2)

Product license: **AGPL-3.0-only** (ADR Decision 2). Every dependency below is
AGPL-3.0-compatible; `scripts/check-licenses.sh` enforces this in CI (RR29-FR3).
This manifest is generated from `cargo metadata --all-features`; regenerate it when
the dependency set changes.

## Native binaries (vendored separately, not in the crate graph)

- **pdfium** (`libpdfium.so`/`.dylib`, bblanchon prebuilt, pinned `chromium/7881` = PDFium 151.0.7881.0) — **BSD-3-Clause** (PDFium) — clean; bundled under `app/src/main/jniLibs/` on device and bound via `PDFIUM_DYNAMIC_LIB_PATH` on the host (RR5-FR1/FR5). Attribution: see the PDFium and bblanchon/pdfium-binaries notices.

## Rust dependencies (106 crates, all features)

| Crate | Version | License (SPDX) |
|---|---|---|
| adler2 | 2.0.1 | 0BSD OR MIT OR Apache-2.0 |
| ahash | 0.8.12 | MIT OR Apache-2.0 |
| android_system_properties | 0.1.5 | MIT/Apache-2.0 |
| arrayref | 0.3.9 | BSD-2-Clause |
| arrayvec | 0.7.6 | MIT OR Apache-2.0 |
| autocfg | 1.5.1 | Apache-2.0 OR MIT |
| bitflags | 1.3.2 | MIT/Apache-2.0 |
| bitflags | 2.13.0 | MIT OR Apache-2.0 |
| bumpalo | 3.20.3 | MIT OR Apache-2.0 |
| bytemuck | 1.25.0 | Zlib OR Apache-2.0 OR MIT |
| byteorder | 1.5.0 | Unlicense OR MIT |
| byteorder-lite | 0.1.0 | Unlicense OR MIT |
| bytes | 1.11.1 | MIT |
| cc | 1.2.63 | MIT OR Apache-2.0 |
| cfg-if | 1.0.4 | MIT OR Apache-2.0 |
| chrono | 0.4.45 | MIT OR Apache-2.0 |
| combine | 4.6.7 | MIT |
| console_error_panic_hook | 0.1.7 | Apache-2.0/MIT |
| console_log | 1.0.0 | MIT/Apache-2.0 |
| core-foundation-sys | 0.8.7 | MIT OR Apache-2.0 |
| crc32fast | 1.5.0 | MIT OR Apache-2.0 |
| either | 1.16.0 | MIT OR Apache-2.0 |
| fallible-iterator | 0.3.0 | MIT/Apache-2.0 |
| fallible-streaming-iterator | 0.1.9 | MIT/Apache-2.0 |
| fdeflate | 0.3.7 | MIT OR Apache-2.0 |
| find-msvc-tools | 0.1.9 | MIT OR Apache-2.0 |
| flate2 | 1.1.9 | MIT OR Apache-2.0 |
| futures-core | 0.3.32 | MIT OR Apache-2.0 |
| futures-task | 0.3.32 | MIT OR Apache-2.0 |
| futures-util | 0.3.32 | MIT OR Apache-2.0 |
| hashbrown | 0.14.5 | MIT OR Apache-2.0 |
| hashlink | 0.9.1 | MIT OR Apache-2.0 |
| iana-time-zone | 0.1.65 | MIT OR Apache-2.0 |
| iana-time-zone-haiku | 0.1.2 | MIT OR Apache-2.0 |
| image | 0.25.10 | MIT OR Apache-2.0 |
| itertools | 0.14.0 | MIT OR Apache-2.0 |
| itoa | 1.0.18 | MIT OR Apache-2.0 |
| jni | 0.22.4 | MIT OR Apache-2.0 |
| jni-macros | 0.22.4 | MIT OR Apache-2.0 |
| jni-sys | 0.4.1 | MIT OR Apache-2.0 |
| jni-sys-macros | 0.4.1 | MIT OR Apache-2.0 |
| js-sys | 0.3.100 | MIT OR Apache-2.0 |
| libc | 0.2.186 | MIT OR Apache-2.0 |
| libloading | 0.9.0 | ISC |
| libsqlite3-sys | 0.30.1 | MIT |
| log | 0.4.32 | MIT OR Apache-2.0 |
| maybe-owned | 0.3.4 | MIT OR Apache-2.0 |
| memchr | 2.8.1 | Unlicense OR MIT |
| miniz_oxide | 0.8.9 | MIT OR Zlib OR Apache-2.0 |
| moxcms | 0.8.1 | BSD-3-Clause OR Apache-2.0 |
| num-traits | 0.2.19 | MIT OR Apache-2.0 |
| once_cell | 1.21.4 | MIT OR Apache-2.0 |
| pdfium-render | 0.9.1 | MIT OR Apache-2.0 |
| pin-project-lite | 0.2.17 | Apache-2.0 OR MIT |
| piston-float | 1.0.1 | MIT |
| pkg-config | 0.3.33 | MIT OR Apache-2.0 |
| png | 0.17.16 | MIT OR Apache-2.0 |
| proc-macro2 | 1.0.106 | MIT OR Apache-2.0 |
| pxfm | 0.1.29 | BSD-3-Clause OR Apache-2.0 |
| quote | 1.0.45 | MIT OR Apache-2.0 |
| rusqlite | 0.32.1 | MIT |
| rustc_version | 0.4.1 | MIT OR Apache-2.0 |
| rustversion | 1.0.22 | MIT OR Apache-2.0 |
| same-file | 1.0.6 | Unlicense/MIT |
| semver | 1.0.28 | MIT OR Apache-2.0 |
| serde | 1.0.228 | MIT OR Apache-2.0 |
| serde_core | 1.0.228 | MIT OR Apache-2.0 |
| serde_derive | 1.0.228 | MIT OR Apache-2.0 |
| serde_json | 1.0.150 | MIT OR Apache-2.0 |
| shlex | 2.0.1 | MIT OR Apache-2.0 |
| simd-adler32 | 0.3.9 | MIT |
| simd_cesu8 | 1.1.1 | Apache-2.0 OR MIT |
| simdutf8 | 0.1.5 | MIT OR Apache-2.0 |
| slab | 0.4.12 | MIT |
| smallvec | 1.15.2 | MIT OR Apache-2.0 |
| strict-num | 0.1.1 | MIT |
| syn | 2.0.117 | MIT OR Apache-2.0 |
| thiserror | 2.0.18 | MIT OR Apache-2.0 |
| thiserror-impl | 2.0.18 | MIT OR Apache-2.0 |
| tiny-skia | 0.11.4 | BSD-3-Clause |
| tiny-skia-path | 0.11.4 | BSD-3-Clause |
| unicode-ident | 1.0.24 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| utf16string | 0.2.0 | MIT OR Apache-2.0 |
| vcpkg | 0.2.15 | MIT/Apache-2.0 |
| vecmath | 1.0.0 | MIT |
| version_check | 0.9.5 | MIT/Apache-2.0 |
| walkdir | 2.5.0 | Unlicense/MIT |
| wasm-bindgen | 0.2.123 | MIT OR Apache-2.0 |
| wasm-bindgen-futures | 0.4.73 | MIT OR Apache-2.0 |
| wasm-bindgen-macro | 0.2.123 | MIT OR Apache-2.0 |
| wasm-bindgen-macro-support | 0.2.123 | MIT OR Apache-2.0 |
| wasm-bindgen-shared | 0.2.123 | MIT OR Apache-2.0 |
| web-sys | 0.3.100 | MIT OR Apache-2.0 |
| winapi-util | 0.1.11 | Unlicense OR MIT |
| windows-core | 0.62.2 | MIT OR Apache-2.0 |
| windows-implement | 0.60.2 | MIT OR Apache-2.0 |
| windows-interface | 0.59.3 | MIT OR Apache-2.0 |
| windows-link | 0.2.1 | MIT OR Apache-2.0 |
| windows-result | 0.4.1 | MIT OR Apache-2.0 |
| windows-strings | 0.5.1 | MIT OR Apache-2.0 |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 |
| zerocopy | 0.8.52 | BSD-2-Clause OR Apache-2.0 OR MIT |
| zerocopy-derive | 0.8.52 | BSD-2-Clause OR Apache-2.0 OR MIT |
| zmij | 1.0.21 | MIT |
| zune-core | 0.5.1 | MIT OR Apache-2.0 OR Zlib |
| zune-jpeg | 0.5.15 | MIT OR Apache-2.0 OR Zlib |

