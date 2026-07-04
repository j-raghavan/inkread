# Spec index (public)

The canonical design documents live in `spec/`, which is **gitignored** — it contains private
on-device research that is not distributable. Source comments, commits, and PRs cite those
documents by ID. This index is the public, redacted map of every citable ID, so an external
contributor can resolve any citation without access to `spec/`. If a summary here is too thin for
the change you're making, ask in the issue/PR — a maintainer will quote the relevant contract.

## How to read a citation

- **`RRn`** — a top-level requirement. `RRn-FRm` is a functional requirement inside it, `RRn-ACm`
  an acceptance criterion, `RRn-NFRm` a non-functional requirement.
- **Two RR series exist** (a historical hazard, load-bearing for every reader of this codebase):
  the **golden spec** (`SPEC-INKREAD.md`, RR1–RR23) is the canonical product spec, and the
  **device annex** (`SPEC-RUST-READER.md`, RR1–RR29) is the older Supernote-first spec whose
  numbering does **not** match. **In-code `RR…` citations refer to the device annex** unless
  prefixed `IR-RR…`, which marks a golden-spec reference (per ADR-INKREAD-0000, Decision 1).
- **`IR-n`** — an invariant (see below). **`S-n`** — a superiority amendment. **`W-n`** — an
  owner waiver. **`Amendment n`** — an M0 JNI-boundary review decision (see below).
- **`ADR-INKREAD-nnnn`** (often shortened to `ADR-nnnn`) — an architecture decision record.

## Golden spec — `SPEC-INKREAD.md` (RR1–RR23)

Statuses are coarse (per the ADR-0000 conformance ledger) and dated 2026-06; the ledger is the
authority and moves with the code.

| RR | Title | One-line summary | Status |
|----|-------|------------------|--------|
| RR1 | Project scaffold & build system | Cargo workspace + Android shell + JNI bridge; core builds and tests host-only. | Partial (desktop emulator waived, W-1) |
| RR2 | Document model & format backends | `Document` trait; PDF/EPUB shipped, search shipped; CBZ/TXT partial, MD/HTML pending. | Partial |
| RR3 | E-ink display & refresh policy | Content-aware refresh decisions as vendor-neutral intents/commands. | Conformant+ (S-1) |
| RR4 | Rendering pipeline | `PixelBuffer`, grayscale, dithering, bounded caches, streaming render. | Conformant |
| RR5 | Input, stylus & palm rejection | Normalized input events, pen samples, palm rejection. | Partial (palm filter is shell-side) |
| RR6 | Ink engine & low-latency handwriting | Rust vector-stroke model: smoothing, undo, eraser, pressure-width. | Partial (live render is platform fast-path, S-2) |
| RR7 | PDF annotation | Ink + highlights on PDF with autosave, persistence, and export. | Partial |
| RR8 | EPUB highlights, notes & linked handwriting | Reflow-anchored annotations that survive typography changes. | Partial |
| RR9 | Native notebooks | Standalone ink notebooks with templates. | Planned |
| RR10 | Annotation storage & sidecars | `.inkread/` sidecar, binary ink codec, document identity, atomic writes. | Conformant |
| RR11 | Export system | MD/JSON/PDF/SVG/PNG export of annotations and notes. | Planned (PDF export shipped) |
| RR12 | Rust-native built-in workflows | Reading progress, dictionary, vocabulary, export as core services. | Partial |
| RR13 | Lua plugin runtime | Embedded Lua for user plugins (L1 shipped: logging API). | Partial |
| RR14 | KOReader compatibility shim | Loader for selected `.koplugin` plugins. | Planned |
| RR15 | Plugin security & permissions | Capability manifest + per-plugin storage sandbox. | Planned |
| RR16 | Reader, annotation, notebook & plugin UI | Reader view, tool palette (ADR-0010), notebook/plugin surfaces. | Partial |
| RR17 | Library, metadata & reading progress | Library browser, metadata, resume-where-you-left. | Partial |
| RR18 | Sync & interoperability | Sync-friendly sidecar layout; external tool interop. | Planned |
| RR19 | Performance & resource budgets | Bounded caches, memory ceiling, trim hooks, no busy loops. | Conformant |
| RR20 | Reliability & data safety | Autosave, atomic crash-safe writes, original document never modified. | Conformant |
| RR21 | Accessibility & usability | High-contrast e-ink UI, handedness, hardware buttons, font scaling. | Partial |
| RR22 | Licensing & dependency policy | AGPL-3.0 product, license-gated dependencies, DRM detected but never bypassed. | Conformant |
| RR23 | Testing & observability | Host unit + golden-image tests, CI gates, coverage. | Conformant |

## Device annex — `SPEC-RUST-READER.md` (RR1–RR29)

The Supernote-first spec. **This is the series in-code `RR…` comments cite.** Retained for its
device contracts; its scope and numbering are superseded by the golden spec.

| RR | Title / summary |
|----|-----------------|
| RR1 | Project scaffold: Android shell + Rust `cdylib` + JNI bridge; host-only core build (RR1-AC3). |
| RR2 | `RefreshCommand` + `DeviceCapabilities` contract; one Kotlin `EinkAdapter` behind a kept interface. |
| RR3 | Content-aware refresh-mode policy state machine. |
| RR4 | Rendering pipeline: `PixelBuffer`, Surface handoff, grayscale + dithering. |
| RR5 | Fixed-layout PDF backend (`pdfium-render`). |
| RR6 | Position model: `PinPosition` + page ranges. |
| RR7 | Reflowable engine integration (EPUB) + the license gate. |
| RR8 | Reflowable pagination (fork-and-walk) + disk pagination cache. |
| RR9 | Typesetting: `ReaderTextStyle` + `GlobalConfig` contract. |
| RR10 | Text shaping & fonts + CJK/fallback. |
| RR11 | Navigation, TOC, search, selection & hit-testing. |
| RR12 | Annotations, bookmarks, reading-position persistence (SQLite). |
| RR13 | Library, metadata, storage layers. |
| RR14 | Input & gestures. |
| RR15 | Supernote device adapter: e-ink execution path. |
| RR16 | Settings & reader UI. |
| RR17 | Test strategy, golden images, e-ink metrics. |
| RR18 | Clean-room / licensing compliance: reimplement from documented contracts only; never copy decompiled code. |
| RR19 | Stylus capture & low-latency ink path (`PenAdapter`). |
| RR20 | Ink annotation layer: model, anchoring, persistence, export. |
| RR21 | Threading, JNI boundary & Android lifecycle contract (RR21-FR3: never panic across JNI). |
| RR22 | Storage layout, file import & scoped storage. |
| RR23 | Settings & config schema. |
| RR24 | Performance & resource budget (bounded caches, memory trim). |
| RR25 | Reader UX & interaction model. |
| RR26 | Library, home & import UX. |
| RR27 | Session restore & crash recovery. |
| RR28 | Fonts & i18n shipping. |
| RR29 | Build, packaging, signing, CI & observability. |

> **`RR30` is a stale alias**: a few annex FRs and one code comment cite “RR30” for logging; it
> resolves to **RR29-FR4** (the observability/logging facade). There is no RR30.

## Invariants (`IR-n`)

Like the RRs, **both specs define an IR series and code comments cite both**. The collision is
mostly harmless — the load-bearing ideas (core purity, vendor-neutrality, host-testability) appear
in each — but check both tables when resolving a citation.

Golden spec (`SPEC-INKREAD.md`):

| ID | Invariant |
|----|-----------|
| IR-1 | An ADR must select the first hardware target before implementation begins. |
| IR-2 | The target must expose enough input/display control for reading and basic handwriting. |
| IR-3 | A desktop emulator backend for dev/CI. **Waived (W-1)** — the host-test half of its intent is kept. |
| IR-4 | No framebuffer, JNI, vendor SDK, or raw device APIs leak into the core crates. |
| IR-5 | Platform-specific display and input code lives in platform adapters. |
| IR-6 | The Rust core is unit-testable without physical hardware. |

Device annex (`SPEC-RUST-READER.md`):

| ID | Invariant |
|----|-----------|
| IR-1 | The core renders into a `PixelBuffer` and never knows which panel it is on; refresh commands are plain data; the core holds no device/JNI handle. |
| IR-2 | The refresh **policy** (decide) is pure Rust and identical across devices; only the adapter **execution** (map to panel) differs. Host-testable via the mock recorder. |
| IR-3 | Positions, annotations, and ink anchor to `PinPosition` (reflow) or page+rect (fixed) — never bare pixels — so they survive re-layout. |
| IR-4 | Pagination is invalidated iff a layout-affecting setting changes (the `layout_digest`); color/theme changes don't re-paginate. |
| IR-5 | No DRM circumvention and no copied decompiled code — clean-room, distributable under the chosen license. |
| IR-6 | Single target v1 (Supernote), device-agnostic core retained; degrade, don't disable, when a capability is absent. |
| IR-7 | **No vendor name in `reader-core`** — all device code lives in the Kotlin adapters + JNI bridge. Adding a device = adding an adapter, never editing the core. |
| IR-8 | Every milestone gate is hardware-validated on the device; "works in the mock" is necessary, not sufficient. |
| IR-9 | Handwriting is first-class and vendor-neutral: the core consumes normalized ink points via a peer adapter path; low-latency feel is capability-gated. |

## M0 JNI-boundary decisions (`Amendment n`)

Decisions from the M0 boundary review, cited directly in `reader-core` comments:

| # | Decision |
|---|----------|
| 1 | The `jni` dependency is feature-gated (`jni-bridge`); the default host build never resolves it (RR1-AC3). |
| 2 | Handle model: the JNI `long` points at the `ReaderSession`; created by `open`, freed only by `close`; every entry point null/range-checks the handle. |
| 3 | Pixel channel order is fixed and locked by a golden test (byte order across the JNI buffer). |
| 4 | M0 scope fence: the `Document` trait is metadata + page count + render-page only; everything else arrives with its own RR. |
| 5 | Render writes into a caller-provided direct `ByteBuffer`; the session never stores a `PixelBuffer`. |
| 6 | One wire codec for commands/gestures — no hand-rolled byte streams; gestures delegate to the refresh policy. |

## Superiority amendments (`S-n`) and waivers (`W-n`)

Recorded in ADR-INKREAD-0000. An **amendment** = the implementation exceeds the golden spec; a
**waiver** = the owner chose not to implement part of it. Neither is silent drift.

| ID | Summary |
|----|---------|
| S-1 | Refresh: intent/command split instead of the spec's core-side `EinkDisplay` I/O trait — keeps the policy pure and host-testable. |
| S-2 | Ink fast path: the platform may render live ink (firmware overlay) as a valid realization of RR6-FR4/FR5; the Rust stroke model is unchanged. |
| S-3 | Host-testability hardened: platform deps are feature-gated out of the default build, not merely "host-buildable". |
| S-4 | Persistence: SQLite WAL behind a `ReaderStore` port with versioned migrations — a concrete realization of "atomic or journaled writes". |
| W-1 | Desktop emulator de-scoped; host tests + CI keep IR-3's intent. |
| W-2 | Single target family: Supernote (Manta / A5 X / Nomad / A6 X). Platform seams are kept so other devices remain drop-in later. |

## ADRs

| ID | Title |
|----|-------|
| ADR-INKREAD-0000 | Golden spec adoption, ADR governance, naming, and the RR conformance ledger. |
| ADR-INKREAD-0001 | First hardware target & the platform boundary. |
| ADR-INKREAD-0002 | Crate decomposition roadmap. |
| ADR-INKREAD-0003 | E-ink refresh & rendering architecture (intent/command over `EinkDisplay`). |
| ADR-INKREAD-0004 | Normalized input, the Rust ink engine, and the firmware fast-path. |
| ADR-INKREAD-0005 | Annotation storage, sidecars, reliability, and the export engine. |
| ADR-INKREAD-0006 | Extensibility: Lua runtime, plugin security, native built-ins, KOReader shim. |
| ADR-INKREAD-0007 | Document model trait & format-backend roadmap. |
| ADR-INKREAD-0008 | UI architecture, portability, and accessibility. |
| ADR-INKREAD-0009 | Dictionary, thesaurus & text selection. |
| ADR-INKREAD-0010 | Annotation tool model & the floating tool palette. |
| ADR-INKREAD-0011 | PDF reflow over the existing reflow layout engine. |
| ADR-INKREAD-0012 | Threading a source anchor through the EPUB pipeline to mint `PinPosition`s. |
| ADR-INKREAD-0013 | Reflow pagination: layout digest, `PageRange` boundaries, disk cache. |
| ADR-INKREAD-0014 | In-app GitHub self-update. |
| ADR-INKREAD-0015 | Any-script text rendering (system-font fallback + line breaking; shaping/BiDi next). |
| ADR-RUST-READER | Reflow engine, product license, and distribution model (annex ADR). |
| ADR-SUPERNOTE-INK | Handwriting ink path & the Rust-vs-Lua architecture question (annex ADR). |

---

*If an ID you encounter in code or a PR is missing here, open an issue — that's an index bug.*
