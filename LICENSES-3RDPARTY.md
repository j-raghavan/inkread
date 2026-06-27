# Third-party license manifest (RR18-AC2)

Product license: **AGPL-3.0-only** (ADR Decision 2). Every dependency below is
AGPL-3.0-compatible; `scripts/check-licenses.sh` enforces this in CI (RR29-FR3).
This manifest is generated from `cargo metadata --all-features`; regenerate it when
the dependency set changes.

## Native binaries (vendored separately, not in the crate graph)

- **pdfium** (`libpdfium.so`/`.dylib`, bblanchon prebuilt, pinned `chromium/7881` = PDFium 151.0.7881.0) — **BSD-3-Clause** (PDFium) — clean; bundled under `app/src/main/jniLibs/` on device and bound via `PDFIUM_DYNAMIC_LIB_PATH` on the host (RR5-FR1/FR5). Attribution: see the PDFium and bblanchon/pdfium-binaries notices.

## Bundled data (generated build artifacts, not committed to git)

- **WordNet 3.0** (Princeton University) — the English dictionary corpus (definitions + synsets) imported into `app/src/main/assets/dict.db` by `scripts/stage-dict.sh`, sourced from the Hu Zheng StarDict packaging (`download.huzheng.org`). **WordNet License** — permissive: free use/redistribution provided the Princeton copyright + license notice is retained (ADR-INKREAD-0009 / RR22-FR5). The `dict.db` artifact is gitignored and regenerated at build time. Other languages are looked up online (opt-in) and cached, not bundled.

## Rust dependencies (163 crates, all features)

| Crate | Version | License (SPDX) |
|---|---|---|
| ab_glyph | 0.2.32 | Apache-2.0 |
| ab_glyph_rasterizer | 0.1.10 | Apache-2.0 |
| adler2 | 2.0.1 | 0BSD OR MIT OR Apache-2.0 |
| ahash | 0.8.12 | MIT OR Apache-2.0 |
| android_system_properties | 0.1.5 | MIT/Apache-2.0 |
| arrayref | 0.3.9 | BSD-2-Clause |
| arrayvec | 0.7.6 | MIT OR Apache-2.0 |
| autocfg | 1.5.1 | Apache-2.0 OR MIT |
| bitflags | 1.3.2 | MIT/Apache-2.0 |
| bitflags | 2.13.0 | MIT OR Apache-2.0 |
| bstr | 1.12.1 | MIT OR Apache-2.0 |
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
| cssparser | 0.37.0 | MPL-2.0 |
| cssparser-macros | 0.7.0 | MPL-2.0 |
| derive_more | 2.1.1 | MIT |
| derive_more-impl | 2.1.1 | MIT |
| dtoa | 1.0.11 | MIT OR Apache-2.0 |
| dtoa-short | 0.3.5 | MPL-2.0 |
| ego-tree | 0.11.0 | ISC |
| either | 1.16.0 | MIT OR Apache-2.0 |
| env_home | 0.1.0 | MIT OR Apache-2.0 |
| equivalent | 1.0.2 | Apache-2.0 OR MIT |
| errno | 0.3.14 | MIT OR Apache-2.0 |
| fallible-iterator | 0.3.0 | MIT/Apache-2.0 |
| fallible-streaming-iterator | 0.1.9 | MIT/Apache-2.0 |
| fastrand | 2.4.1 | Apache-2.0 OR MIT |
| fdeflate | 0.3.7 | MIT OR Apache-2.0 |
| find-msvc-tools | 0.1.9 | MIT OR Apache-2.0 |
| flate2 | 1.1.9 | MIT OR Apache-2.0 |
| futures-core | 0.3.32 | MIT OR Apache-2.0 |
| futures-task | 0.3.32 | MIT OR Apache-2.0 |
| futures-util | 0.3.32 | MIT OR Apache-2.0 |
| hashbrown | 0.14.5 | MIT OR Apache-2.0 |
| hashbrown | 0.17.1 | MIT OR Apache-2.0 |
| hashlink | 0.9.1 | MIT OR Apache-2.0 |
| html5ever | 0.39.0 | MIT OR Apache-2.0 |
| iana-time-zone | 0.1.65 | MIT OR Apache-2.0 |
| iana-time-zone-haiku | 0.1.2 | MIT OR Apache-2.0 |
| image | 0.25.10 | MIT OR Apache-2.0 |
| indexmap | 2.14.0 | Apache-2.0 OR MIT |
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
| linux-raw-sys | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| lock_api | 0.4.14 | MIT OR Apache-2.0 |
| log | 0.4.32 | MIT OR Apache-2.0 |
| lua-src | 547.0.0 | MIT |
| luajit-src | 210.5.12+a4f56a4 | MIT |
| markup5ever | 0.39.0 | MIT OR Apache-2.0 |
| maybe-owned | 0.3.4 | MIT OR Apache-2.0 |
| memchr | 2.8.1 | Unlicense OR MIT |
| miniz_oxide | 0.8.9 | MIT OR Zlib OR Apache-2.0 |
| mlua | 0.10.5 | MIT |
| mlua-sys | 0.6.8 | MIT |
| moxcms | 0.8.1 | BSD-3-Clause OR Apache-2.0 |
| new_debug_unreachable | 1.0.6 | MIT |
| num-traits | 0.2.19 | MIT OR Apache-2.0 |
| once_cell | 1.21.4 | MIT OR Apache-2.0 |
| owned_ttf_parser | 0.25.1 | Apache-2.0 |
| parking_lot | 0.12.5 | MIT OR Apache-2.0 |
| parking_lot_core | 0.9.12 | MIT OR Apache-2.0 |
| pdfium-render | 0.9.1 | MIT OR Apache-2.0 |
| percent-encoding | 2.3.2 | MIT OR Apache-2.0 |
| phf | 0.13.1 | MIT |
| phf_codegen | 0.13.1 | MIT |
| phf_generator | 0.13.1 | MIT |
| phf_macros | 0.13.1 | MIT |
| phf_shared | 0.13.1 | MIT |
| pin-project-lite | 0.2.17 | Apache-2.0 OR MIT |
| piston-float | 1.0.1 | MIT |
| pkg-config | 0.3.33 | MIT OR Apache-2.0 |
| png | 0.17.16 | MIT OR Apache-2.0 |
| precomputed-hash | 0.1.1 | MIT |
| proc-macro2 | 1.0.106 | MIT OR Apache-2.0 |
| pxfm | 0.1.29 | BSD-3-Clause OR Apache-2.0 |
| quick-xml | 0.40.1 | MIT |
| quote | 1.0.45 | MIT OR Apache-2.0 |
| rbook | 0.7.9 | Apache-2.0 |
| redox_syscall | 0.5.18 | MIT |
| rusqlite | 0.32.1 | MIT |
| rustc-hash | 2.1.2 | Apache-2.0 OR MIT |
| rustc_version | 0.4.1 | MIT OR Apache-2.0 |
| rustix | 1.1.4 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| rustversion | 1.0.22 | MIT OR Apache-2.0 |
| same-file | 1.0.6 | Unlicense/MIT |
| scopeguard | 1.2.0 | MIT OR Apache-2.0 |
| scraper | 0.27.0 | ISC |
| selectors | 0.38.0 | MPL-2.0 |
| semver | 1.0.28 | MIT OR Apache-2.0 |
| serde | 1.0.228 | MIT OR Apache-2.0 |
| serde_core | 1.0.228 | MIT OR Apache-2.0 |
| serde_derive | 1.0.228 | MIT OR Apache-2.0 |
| serde_json | 1.0.150 | MIT OR Apache-2.0 |
| servo_arc | 0.4.3 | MIT OR Apache-2.0 |
| shlex | 2.0.1 | MIT OR Apache-2.0 |
| simd-adler32 | 0.3.9 | MIT |
| simd_cesu8 | 1.1.1 | Apache-2.0 OR MIT |
| simdutf8 | 0.1.5 | MIT OR Apache-2.0 |
| siphasher | 1.0.3 | MIT/Apache-2.0 |
| slab | 0.4.12 | MIT |
| smallvec | 1.15.2 | MIT OR Apache-2.0 |
| stable_deref_trait | 1.2.1 | MIT OR Apache-2.0 |
| strict-num | 0.1.1 | MIT |
| string_cache | 0.9.0 | MIT OR Apache-2.0 |
| string_cache_codegen | 0.6.1 | MIT OR Apache-2.0 |
| syn | 2.0.117 | MIT OR Apache-2.0 |
| tendril | 0.5.0 | MIT OR Apache-2.0 |
| thiserror | 2.0.18 | MIT OR Apache-2.0 |
| thiserror-impl | 2.0.18 | MIT OR Apache-2.0 |
| tiny-skia | 0.11.4 | BSD-3-Clause |
| tiny-skia-path | 0.11.4 | BSD-3-Clause |
| ttf-parser | 0.25.1 | MIT OR Apache-2.0 |
| typed-path | 0.12.3 | MIT OR Apache-2.0 |
| unicode-ident | 1.0.24 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| utf-8 | 0.7.6 | MIT OR Apache-2.0 |
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
| web_atoms | 0.2.5 | MIT OR Apache-2.0 |
| which | 7.0.3 | MIT |
| winapi-util | 0.1.11 | Unlicense OR MIT |
| windows-core | 0.62.2 | MIT OR Apache-2.0 |
| windows-implement | 0.60.2 | MIT OR Apache-2.0 |
| windows-interface | 0.59.3 | MIT OR Apache-2.0 |
| windows-link | 0.2.1 | MIT OR Apache-2.0 |
| windows-result | 0.4.1 | MIT OR Apache-2.0 |
| windows-strings | 0.5.1 | MIT OR Apache-2.0 |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 |
| winsafe | 0.0.19 | MIT |
| zerocopy | 0.8.52 | BSD-2-Clause OR Apache-2.0 OR MIT |
| zerocopy-derive | 0.8.52 | BSD-2-Clause OR Apache-2.0 OR MIT |
| zip | 8.6.0 | MIT |
| zlib-rs | 0.6.3 | Zlib |
| zmij | 1.0.21 | MIT |
| zune-core | 0.5.1 | MIT OR Apache-2.0 OR Zlib |
| zune-jpeg | 0.5.15 | MIT OR Apache-2.0 OR Zlib |


## Bundled fonts

| Font | Source | License |
|------|--------|---------|
| Pinyon Script | github.com/SorkinType/Pinyon (Google Fonts) | SIL Open Font License 1.1 |
| Spectral | github.com/productiontype/Spectral (Google Fonts) | SIL Open Font License 1.1 |
| Noto Music | github.com/notofonts/music | SIL Open Font License 1.1 |
| Noto Serif | github.com/koreader/koreader-fonts (notofonts) | SIL Open Font License 1.1 |
| Noto Sans | github.com/koreader/koreader-fonts (notofonts) | SIL Open Font License 1.1 |
| Droid Sans Mono | github.com/koreader/koreader-fonts | Apache License 2.0 |
| FreeSerif | github.com/koreader/koreader-fonts (GNU FreeFont) | GPL-3.0 with font exception |
| FreeSans | github.com/koreader/koreader-fonts (GNU FreeFont) | GPL-3.0 with font exception |

Pinyon Script is bundled at `app/src/main/assets/fonts/pinyon_script.ttf` for the home-screen
"Library" heading (Spencerian-inspired). © The Pinyon Script Project Authors; SIL OFL 1.1
(http://scripts.sil.org/OFL) — free to use, bundle, and redistribute with attribution.

Spectral (Regular) is bundled at `inkread-epub/fonts/Spectral-Regular.ttf` and embedded in the
reflow renderer (`inkread-epub::render`) as the default EPUB reading face. © Production Type; SIL
OFL 1.1 — the full license ships alongside the font at `inkread-epub/fonts/OFL.txt`.

The selectable reading faces (font picker) + the glyph fallback are also bundled under
`inkread-epub/fonts/` and embedded in the reflow renderer: **Noto Serif / Noto Sans / Noto Music**
(© The Noto Project Authors, SIL OFL 1.1), **Droid Sans Mono** (© Google, Apache-2.0), and
**FreeSerif / FreeSans** (© the GNU FreeFont contributors, GPL-3.0 with the font embedding
exception — one-way compatible with this project's AGPL-3.0). All are the open-source faces KOReader
ships; each redistributed with attribution per its license.
