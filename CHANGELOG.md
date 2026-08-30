# Changelog

All notable changes to inkread are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Versions are the workspace version in
`Cargo.toml`, which the release workflow keeps in step with the tag; each release also ships a signed
APK on the [Releases page](https://github.com/j-raghavan/inkread/releases).

Entries link the pull request that landed the change. Where a PR closed a reported issue, the issue
number is given in parentheses.

## [Unreleased]

### Changed

- Cleartext HTTP is no longer allowed to arbitrary hosts. `HttpFetch` permits it only when the host
  is on the local network — a private/loopback/link-local IP literal, `localhost`, a reserved local
  suffix, or a single-label name — which is what a calibre or Calibre-Web server on the LAN needs,
  and refuses it everywhere else. A network security config additionally pins GitHub (the
  self-updater) and Wiktionary to HTTPS at the platform level.
- `reader-core/src/jni.rs` (1,927 lines) is split into a `jni/` module by the domain each export
  serves — document, display, ink, lasso, text, services — with the shared throw/handle/array
  helpers in `mod.rs`. All 79 exported symbols and their bodies are byte-identical, so the ABI the
  Kotlin side links against is unchanged.
- `reader-core/src/session.rs` (1,630 lines, one `impl` with 102 methods) is split into a
  `session/` module by what each method is for — open, render, view, navigate, ink, select — with
  the struct, its lifecycle and the plain accessors in `mod.rs`.
- `reader-core/src/document/text_select.rs` (1,634 lines) is split into a `text_select/` module by
  what a query is — search, word, columns, span — with the geometry types and shared predicates in
  `mod.rs`. The module's public surface is unchanged.
- `device-eink` and `reader-core` no longer name a vendor.
  `DeviceCapabilities::supernote_baseline` / `supernote_full` are now `flashing_epd` /
  `controllable_epd`, and `ResourceBudget::default_supernote` is `default_tablet_epd` — the
  capability, not the manufacturer. `scripts/check-vendor-neutral.sh` now enforces IR-7 in CI.

### Added

- Property-based tests (`proptest`) for `PinPosition` round-trip and ordering totality, the
  `device-eink` capability and command wire codecs, and the `.inkbin` stroke codec — the RR6-AC1 /
  RR17-FR3 acceptance criteria, and what `CONTRIBUTING.md` already claimed the suite had.

### Fixed

- Module headers that still described the M0 milestone now describe the shipped code: the `Document`
  trait has four backends and defaulted `toc`/`search`/`hint_page` rather than "the PDF backend is
  the one implementation", the render pipeline has a cache and prefetch, the refresh policy has
  scroll suppression and a night cadence, and `ReaderActivity` is no longer marked
  `DEVICE-UNVERIFIED`.
- Production `unwrap`/`expect` removed from the PDF backend, `PinPosition` serialization, text
  selection, and the PDF text-block splitter — paths the JNI bridge reaches, where an unwind would
  have violated RR21-FR3. A missing symbol-fallback face or hyphenation pattern set now degrades
  (lost glyph coverage, whole-word wrapping) instead of taking the reader down.

## [1.3.2] — 2026-08-29

### Added

- Imported font files are grouped into families, so a reader's own font keeps its real bold and
  italic faces instead of one entry per file with both synthesized
  ([#249](https://github.com/j-raghavan/inkread/pull/249), closes
  [#248](https://github.com/j-raghavan/inkread/issues/248)).

## [1.3.1] — 2026-08-29

### Added

- **Menu Size** scales the bottom bar and its panels, not just their text, and a touch-target floor
  keeps controls hittable at the smaller steps
  ([#246](https://github.com/j-raghavan/inkread/pull/246), closes
  [#245](https://github.com/j-raghavan/inkread/issues/245)).

### Fixed

- EPUB honours CSS `font-style`, so a stylesheet italic renders italic
  ([#244](https://github.com/j-raghavan/inkread/pull/244)).

## [1.3.0] — 2026-08-28

### Added

- Real bold and italic faces for the bundled families, and **import your own reading fonts**
  ([#239](https://github.com/j-raghavan/inkread/pull/239)).
- Configurable EPUB page margins ([#237](https://github.com/j-raghavan/inkread/pull/237), closes
  [#167](https://github.com/j-raghavan/inkread/issues/167)).
- A feed's URL can be edited in Daily ([#243](https://github.com/j-raghavan/inkread/pull/243)).
- An explicit **Quit** in settings ([#242](https://github.com/j-raghavan/inkread/pull/242)).
- After adding a selection to the Digest, the app offers to open the Digest app
  ([#241](https://github.com/j-raghavan/inkread/pull/241)).

### Fixed

- Paginations made stale by [#239](https://github.com/j-raghavan/inkread/pull/239) are invalidated,
  and the fallout is reported accurately ([#240](https://github.com/j-raghavan/inkread/pull/240)).
- Tool options open beside the toolbar, and Menu Size reaches the palette
  ([#238](https://github.com/j-raghavan/inkread/pull/238), closes
  [#200](https://github.com/j-raghavan/inkread/issues/200)).
- Refresh settings survive a rotation ([#236](https://github.com/j-raghavan/inkread/pull/236),
  closes [#206](https://github.com/j-raghavan/inkread/issues/206)).

## [1.2.8] — 2026-08-23

### Added

- Table rows, and a corner-docked horizontal tool palette
  ([#230](https://github.com/j-raghavan/inkread/pull/230), closes
  [#200](https://github.com/j-raghavan/inkread/issues/200)).

### Fixed

- Defining a word split across a line break no longer looks up `well--`
  ([#231](https://github.com/j-raghavan/inkread/pull/231)).

## [1.2.7] — 2026-08-22

### Changed

- Shelf and Home UX pass ([#229](https://github.com/j-raghavan/inkread/pull/229), closes
  [#227](https://github.com/j-raghavan/inkread/issues/227)).

### Fixed

- The tool palette remembers where it was parked
  ([#228](https://github.com/j-raghavan/inkread/pull/228), closes
  [#200](https://github.com/j-raghavan/inkread/issues/200)).

## [1.2.6] — 2026-08-22

### Added

- The display adapter is chosen by capability rather than by device string
  ([#224](https://github.com/j-raghavan/inkread/pull/224)).
- Daily opens each issue with an **In This Issue** contents page
  ([#218](https://github.com/j-raghavan/inkread/pull/218)) and a curated quotation
  ([#219](https://github.com/j-raghavan/inkread/pull/219)).

### Fixed

- Reflow keeps the reading position within the chapter across a layout change
  ([#226](https://github.com/j-raghavan/inkread/pull/226)).
- Pen strokes bake at the firmware nib width
  ([#225](https://github.com/j-raghavan/inkread/pull/225)).

## [1.2.5] — 2026-08-21

### Added

- Two-column newspaper layout ([#216](https://github.com/j-raghavan/inkread/pull/216), closes
  [#194](https://github.com/j-raghavan/inkread/issues/194)).

### Fixed

- Reflowable documents get a working size control
  ([#217](https://github.com/j-raghavan/inkread/pull/217)).

## [1.2.4] — 2026-08-21

### Added

- Selectable pen stroke thickness ([#205](https://github.com/j-raghavan/inkread/pull/205)).
- EPUB illustrations render instead of `[image]` placeholders
  ([#204](https://github.com/j-raghavan/inkread/pull/204), closes
  [#187](https://github.com/j-raghavan/inkread/issues/187)).
- Per-feed article limits in Daily ([#210](https://github.com/j-raghavan/inkread/pull/210), closes
  [#193](https://github.com/j-raghavan/inkread/issues/193)).

### Changed

- EPUB CSS selectors are matched with `simplecss` instead of by hand
  ([#203](https://github.com/j-raghavan/inkread/pull/203)).

### Performance

- About 2.6s removed from every book open
  ([#207](https://github.com/j-raghavan/inkread/pull/207), closes
  [#186](https://github.com/j-raghavan/inkread/issues/186)).

### Fixed

- The bottom bar stays open for zoom ([#211](https://github.com/j-raghavan/inkread/pull/211)).
- The expanded toolbar pill stays on screen with the grip under the finger
  ([#209](https://github.com/j-raghavan/inkread/pull/209)).
- EPUB honours the book's stylesheet, so title pages render as designed
  ([#202](https://github.com/j-raghavan/inkread/pull/202), closes
  [#188](https://github.com/j-raghavan/inkread/issues/188)).

## [1.2.3] — 2026-08-17

### Performance

- EPUB chapters parse on demand, and a book no longer opens on a black screen
  ([#189](https://github.com/j-raghavan/inkread/pull/189), closes
  [#186](https://github.com/j-raghavan/inkread/issues/186)).

### Fixed

- A paragraph's first line is indented, not every line
  ([#185](https://github.com/j-raghavan/inkread/pull/185), closes
  [#163](https://github.com/j-raghavan/inkread/issues/163)).
- The Calibre-Web path is complete: search, failure reporting, and sizes
  ([#184](https://github.com/j-raghavan/inkread/pull/184)).

## [1.2.2] — 2026-08-16

### Fixed

- The OPDS catalog client works against a real Calibre-Web
  ([#183](https://github.com/j-raghavan/inkread/pull/183), closes
  [#175](https://github.com/j-raghavan/inkread/issues/175)).

## [1.2.1] — 2026-08-16

### Added

- Browse a calibre / Calibre-Web library over OPDS and download onto the shelf
  ([#181](https://github.com/j-raghavan/inkread/pull/181), closes
  [#175](https://github.com/j-raghavan/inkread/issues/175)).
- See every book on the device, and remove one
  ([#182](https://github.com/j-raghavan/inkread/pull/182)).

### Fixed

- EPUBs whose text is a legacy encoding, not UTF-8, now open
  ([#180](https://github.com/j-raghavan/inkread/pull/180), closes
  [#159](https://github.com/j-raghavan/inkread/issues/159)).
- The eraser no longer corrupts the page
  ([#179](https://github.com/j-raghavan/inkread/pull/179), closes
  [#158](https://github.com/j-raghavan/inkread/issues/158)).

## [1.2.0] — 2026-08-15

### Performance

- A book is paginated once and cached, and the reader can cancel a reflow
  ([#178](https://github.com/j-raghavan/inkread/pull/178)).

### Fixed

- The firmware EMR ink claim is released on background via the working binder
  ([#172](https://github.com/j-raghavan/inkread/pull/172), closes
  [#157](https://github.com/j-raghavan/inkread/issues/157)).

## [1.1.0] — 2026-08-02

### Added

- Full refresh every N page-turns, plus a manual **Refresh now**
  ([#156](https://github.com/j-raghavan/inkread/pull/156), closes
  [#99](https://github.com/j-raghavan/inkread/issues/99)).
- A **Menu Size** setting that scales the reader chrome on large panels
  ([#155](https://github.com/j-raghavan/inkread/pull/155), closes
  [#133](https://github.com/j-raghavan/inkread/issues/133)).
- Kotlin coverage via Kover, reported to Codecov under its own flag
  ([#154](https://github.com/j-raghavan/inkread/pull/154)).

### Fixed

- CBZ and plain-text files appear in the picker, shelf, and import
  ([#153](https://github.com/j-raghavan/inkread/pull/153), closes
  [#125](https://github.com/j-raghavan/inkread/issues/125)).
- Editable-annotation PDF ink renders in Adobe and Preview
  ([#137](https://github.com/j-raghavan/inkread/pull/137), closes
  [#136](https://github.com/j-raghavan/inkread/issues/136)).

## [1.0.1] — 2026-07-12

### Fixed

- Lasso and drag text selection stay within the column they cover
  ([#134](https://github.com/j-raghavan/inkread/pull/134), closes
  [#128](https://github.com/j-raghavan/inkread/issues/128)).
- Cancelling the Open-Document picker returns to Home instead of stranding a blank page
  ([#127](https://github.com/j-raghavan/inkread/pull/127)).

## [1.0.0] — 2026-07-05

First stable release: a Supernote daily driver.

### Changed

- Stylus/eraser capture and ink commit extracted from `ReaderActivity`
  ([#121](https://github.com/j-raghavan/inkread/pull/121)); lasso and Define selection likewise
  ([#120](https://github.com/j-raghavan/inkread/pull/120)).
- Review Feedback R2 landed: a public spec index, a CI packaging check, `ReaderActivity`
  decomposition, a release smoke checklist, and `docs/EINK-LIMITS.md`
  ([#118](https://github.com/j-raghavan/inkread/pull/118)).

## [0.8.0] — 2026-07-03

### Fixed

- Daily interleaves articles round-robin across sources
  ([#111](https://github.com/j-raghavan/inkread/pull/111), closes
  [#107](https://github.com/j-raghavan/inkread/issues/107)).

## [0.7.0] — 2026-07-03

### Added

- Any-script text rendering: a system-font fallback chain and UAX #14 line breaking
  ([#108](https://github.com/j-raghavan/inkread/pull/108)).
- A **Compiling…** notice while a Daily issue builds
  ([#109](https://github.com/j-raghavan/inkread/pull/109)).

## [0.6.0] — 2026-06-28

### Added

- In-app GitHub self-updater, pinned to the release signing certificate
  ([#96](https://github.com/j-raghavan/inkread/pull/96)).

## [0.5.0] — 2026-06-27

The large feature release: **InkRead Daily**, reflow-stable positions, and the reading UX.

### Added

- **InkRead Daily** end to end — the `inkread-daily` crate and issue model
  ([#71](https://github.com/j-raghavan/inkread/pull/71)), the front-page screen and Home entry
  ([#72](https://github.com/j-raghavan/inkread/pull/72)), the fetch pipeline
  ([#73](https://github.com/j-raghavan/inkread/pull/73)), a fresh issue compiled each morning
  ([#74](https://github.com/j-raghavan/inkread/pull/74)), read/unread headlines
  ([#75](https://github.com/j-raghavan/inkread/pull/75)), and `feed-rs` as the feed parser
  ([#78](https://github.com/j-raghavan/inkread/pull/78)) — all closing
  [#66](https://github.com/j-raghavan/inkread/issues/66).
- Reflow-stable reading-position resume via `PinPosition`
  ([#65](https://github.com/j-raghavan/inkread/pull/65), closes
  [#46](https://github.com/j-raghavan/inkread/issues/46)), with a locator and EPUB source-anchor
  threading ([#47](https://github.com/j-raghavan/inkread/pull/47)) and a persisted anchor for EPUB
  digests ([#68](https://github.com/j-raghavan/inkread/pull/68)).
- A CBZ comic-archive backend ([#70](https://github.com/j-raghavan/inkread/pull/70), closes
  [#36](https://github.com/j-raghavan/inkread/issues/36)), a plain-text backend
  ([#45](https://github.com/j-raghavan/inkread/pull/45), closes
  [#35](https://github.com/j-raghavan/inkread/issues/35)), and format detection by magic bytes
  ([#43](https://github.com/j-raghavan/inkread/pull/43), closes
  [#34](https://github.com/j-raghavan/inkread/issues/34)).
- Font-family selection ([#93](https://github.com/j-raghavan/inkread/pull/93), closes
  [#92](https://github.com/j-raghavan/inkread/issues/92)), flexible line spacing and larger font
  sizes ([#90](https://github.com/j-raghavan/inkread/pull/90), closes
  [#89](https://github.com/j-raghavan/inkread/issues/89)), font-size presets
  ([#63](https://github.com/j-raghavan/inkread/pull/63), closes
  [#55](https://github.com/j-raghavan/inkread/issues/55)), reading-style presets and night mode
  ([#88](https://github.com/j-raghavan/inkread/pull/88), closes
  [#87](https://github.com/j-raghavan/inkread/issues/87)).
- The handwritten-notes annotations list
  ([#86](https://github.com/j-raghavan/inkread/pull/86), closes
  [#85](https://github.com/j-raghavan/inkread/issues/85)) and reading-bar chapter navigation with
  progress ([#83](https://github.com/j-raghavan/inkread/pull/83), closes
  [#82](https://github.com/j-raghavan/inkread/issues/82) and
  [#84](https://github.com/j-raghavan/inkread/issues/84)).
- Double-tap-to-zoom at the tap focal point
  ([#62](https://github.com/j-raghavan/inkread/pull/62), closes
  [#54](https://github.com/j-raghavan/inkread/issues/54)), swipe to turn pages and a translucent
  tool puck ([#76](https://github.com/j-raghavan/inkread/pull/76)), and a collapsed circular inkwell
  puck by default ([#91](https://github.com/j-raghavan/inkread/pull/91)).
- `scripts/gate.sh`, to run the CI gates locally with honest exit codes
  ([#77](https://github.com/j-raghavan/inkread/pull/77)).

### Performance

- Next-page read-ahead into the render cache
  ([#80](https://github.com/j-raghavan/inkread/pull/80)), a reused scratch buffer for fit/crop
  render with a real-PDF golden test ([#81](https://github.com/j-raghavan/inkread/pull/81)), and
  zoom preserved across page turns ([#60](https://github.com/j-raghavan/inkread/pull/60)).

### Fixed

- EPUB multi-line drag-selection ([#64](https://github.com/j-raghavan/inkread/pull/64)) and
  glyph-center containment for lasso/drag selection
  ([#59](https://github.com/j-raghavan/inkread/pull/59), closes
  [#51](https://github.com/j-raghavan/inkread/issues/51)).
- In-progress strokes and erases survive a page turn
  ([#58](https://github.com/j-raghavan/inkread/pull/58), closes
  [#50](https://github.com/j-raghavan/inkread/issues/50)).
- Palm rejection and colour-swatch ink persistence
  ([#56](https://github.com/j-raghavan/inkread/pull/56), closes
  [#49](https://github.com/j-raghavan/inkread/issues/49) and
  [#57](https://github.com/j-raghavan/inkread/issues/57)).
- Zoom entry points are gated on a magnifiable view
  ([#67](https://github.com/j-raghavan/inkread/pull/67), closes
  [#61](https://github.com/j-raghavan/inkread/issues/61)).
- Article HTML is parsed with `scraper`, not a hand-rolled scanner
  ([#94](https://github.com/j-raghavan/inkread/pull/94)).

## [0.4.0] — 2026-06-23

### Fixed

- Pinch-zoom is suppressed while the pen is active
  ([#33](https://github.com/j-raghavan/inkread/pull/33)).

## [0.3.0] — 2026-06-23

### Fixed

- First-palm rejection, with host tests
  ([#32](https://github.com/j-raghavan/inkread/pull/32)).

## [0.2.0] — 2026-06-22

### Added

- The inkread MVP on master ([#19](https://github.com/j-raghavan/inkread/pull/19)): the Rust
  workspace (parse · layout · render · refresh policy · ink model), the Kotlin shell, and the JNI
  bridge.

### Fixed

- An in-window colour palette, so changing ink no longer drops annotations
  ([#28](https://github.com/j-raghavan/inkread/pull/28)).

[Unreleased]: https://github.com/j-raghavan/inkread/compare/v1.3.2...HEAD
[1.3.2]: https://github.com/j-raghavan/inkread/compare/v1.3.1...v1.3.2
[1.3.1]: https://github.com/j-raghavan/inkread/compare/v1.3.0...v1.3.1
[1.3.0]: https://github.com/j-raghavan/inkread/compare/v1.2.8...v1.3.0
[1.2.8]: https://github.com/j-raghavan/inkread/compare/v1.2.7...v1.2.8
[1.2.7]: https://github.com/j-raghavan/inkread/compare/v1.2.6...v1.2.7
[1.2.6]: https://github.com/j-raghavan/inkread/compare/v1.2.5...v1.2.6
[1.2.5]: https://github.com/j-raghavan/inkread/compare/v1.2.4...v1.2.5
[1.2.4]: https://github.com/j-raghavan/inkread/compare/v1.2.3...v1.2.4
[1.2.3]: https://github.com/j-raghavan/inkread/compare/v1.2.2...v1.2.3
[1.2.2]: https://github.com/j-raghavan/inkread/compare/v1.2.1...v1.2.2
[1.2.1]: https://github.com/j-raghavan/inkread/compare/v1.2.0...v1.2.1
[1.2.0]: https://github.com/j-raghavan/inkread/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/j-raghavan/inkread/compare/v1.0.1...v1.1.0
[1.0.1]: https://github.com/j-raghavan/inkread/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/j-raghavan/inkread/compare/v0.8.0...v1.0.0
[0.8.0]: https://github.com/j-raghavan/inkread/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/j-raghavan/inkread/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/j-raghavan/inkread/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/j-raghavan/inkread/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/j-raghavan/inkread/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/j-raghavan/inkread/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/j-raghavan/inkread/releases/tag/v0.2.0
