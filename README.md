<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset=".github/assets/inkread-icon-dark.png">
    <img src=".github/assets/inkread-icon-light.png" alt="inkread" width="120" height="120">
  </picture>
</p>

<h1 align="center">inkread</h1>

<p align="center">
  <strong>A Rust-core, e-ink-first document reader with first-class handwriting.</strong><br>
  KOReader-class reading meets Supernote-class inking — open source, in a clean Rust core.
</p>

<p align="center">
  <a href="./.github/workflows/ci.yml"><img alt="CI" src="https://github.com/j-raghavan/inkread/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://codecov.io/gh/j-raghavan/inkread"><img alt="coverage" src="https://codecov.io/gh/j-raghavan/inkread/branch/master/graph/badge.svg"></a>
  <img alt="core: Rust" src="https://img.shields.io/badge/core-Rust-orange?logo=rust&logoColor=white">
  <a href="./LICENSE"><img alt="License: AGPL-3.0" src="https://img.shields.io/badge/license-AGPL--3.0-blue.svg"></a>
  <img alt="status: early (M0)" src="https://img.shields.io/badge/status-early%20(M0)-orange.svg">
</p>

---

inkread is a document reader and writing platform for tablet-class **e-ink** devices, targeting the
**Supernote** (Ratta, RK3566, Android 11) first. A Kotlin/Android shell wraps a Rust `cdylib` (over
JNI) that owns parsing, layout, rendering, the refresh policy, and the **ink model** — so the hard
parts are memory-safe, vendor-neutral, and testable on your laptop with no device.

## Why inkread?

On e-ink today you usually pick one of two compromises:

- **KOReader** reads beautifully and has a huge plugin ecosystem — but it's reading-first; handwriting
  on documents is an afterthought.
- **Supernote's built-in reader** writes beautifully on native hardware — but it's closed, locks your
  annotations into a proprietary format, and the document-reading features (reflow, dictionary,
  plugins) are thin.

inkread aims at the **gap between them**: real reading *and* real handwriting, with your annotations
**written back into the PDF** (editable or flattened) so they're portable to any other app — all on
an open [AGPL-3.0](./LICENSE) Rust core you can audit and extend.

## How it compares

| | **inkread** | **KOReader** | **Supernote reader** |
|---|:---:|:---:|:---:|
| Handwriting on documents | **First-class** (core ink model) | Minimal | **First-class** |
| Annotation portability | **Written into the PDF** + portable sidecar | Sidecar metadata | Proprietary, locked-in |
| Document reading (PDF reflow, dictionary) | Yes | **Mature** | Limited |
| E-ink refresh control | Vendor-neutral policy in core | **Excellent** | Native / vendor-optimal |
| Extensibility | Native Lua API + selected KOReader-shim | **Huge Lua ecosystem** | None (closed) |
| Architecture | Rust core, host-testable | C + Lua | Closed-source |
| Open source | **AGPL-3.0** | **AGPL-3.0** | Proprietary |
| Devices | Supernote family (RK3566) | **Broad** (Kindle/Kobo/Android…) | Supernote only |

> Honest take: KOReader is the more mature *reader* and runs on far more hardware; the Supernote
> reader is the more polished *native* experience. inkread is the **only one of the three that's both
> open and built handwriting-first**, and it's **early** — see status below.

inkread is **not** a KOReader clone. KOReader is prior art and compatibility inspiration; inkread
reuses its plugin *style* (a selected `.koplugin` shim) but ships its own Rust-native engine.

## Status

Pre-1.0, **milestone M0**. The Rust workspace (parse · reflow · ink · refresh policy · dictionary ·
Lua runtime) builds and tests green on the host; device bring-up on the Supernote is in progress.
APIs and formats will change. Roadmap milestones run M0 → M2 (*Daily Driver v1*) and beyond.

## Quick start

The entire Rust core builds **on your machine with no Android SDK** — that's a hard design rule:

```bash
git clone https://github.com/j-raghavan/inkread.git
cd inkread
cargo test --workspace      # green with no device, no Android toolchain
```

Build & sideload the Android APK (needs JDK 17–21, the Android NDK, and `cargo-ndk`):

```bash
./buildApk.sh              # cargo-ndk → pdfium → dictionary → Gradle assemble
./buildApk.sh --install    # ...and adb install to a connected Supernote
```

Prebuilt APKs are attached to each [GitHub Release](https://github.com/j-raghavan/inkread/releases).

## Architecture

```
app/  (Kotlin/Android shell)  ──JNI──▶  reader-core/  (Rust cdylib, libreader.so)
  UI · EPD adapter · pen/touch          parse · layout · render · refresh policy · ink
  speaks vendor waveforms               speaks RefreshIntent — never names a vendor (IR-7)
```

The core never names a vendor and never leaks Android types — device specifics live in the Kotlin
adapter and the feature-gated JNI bridge. Supporting crates: `inkread-pdftext`, `inkread-epub`,
`inkread-ink`, `inkread-dict`, `inkread-lua`, and the vendor-neutral `device-eink`.

## Contributing

Contributions are very welcome — you don't need a Supernote to help. Start with
**[CONTRIBUTING.md](./CONTRIBUTING.md)** and look for **`good first issue`** labels. Please also read
the [Code of Conduct](./CODE_OF_CONDUCT.md) and [Security Policy](./SECURITY.md).

## About & disclaimer

inkread is an **independent, community project** built by a Supernote Manta owner and fan. It exists
because the itch was personal — I wanted reading and handwriting to work *together* on my own device,
the way I needed them to, and built the reader I wished existed.

> It is **not affiliated with, authorized by, sponsored by, or endorsed by Ratta or Supernote**.
> "Supernote", "Manta", and related names are trademarks of their respective owners and are used here
> only descriptively (for interoperability and identification). inkread is a clean-room implementation
> and contains no decompiled or vendor-proprietary code. It is provided "as is", without warranty;
> sideloading and use are at your own risk.

## License

[AGPL-3.0-only](./LICENSE). Third-party components are listed in
[LICENSES-3RDPARTY.md](./LICENSES-3RDPARTY.md).
