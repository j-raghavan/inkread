# Contributing to inkread

First off — **thank you** for taking the time to look at inkread. 🖋️

inkread is a Rust-core e-ink document reader with **first-class handwriting**, targeted at the
**Supernote** (Ratta, RK3566, Android 11). It pairs a Kotlin/Android shell with a Rust `cdylib`
(over JNI) that owns parsing, layout, rendering, the refresh policy, and the ink model.

You do **not** need a Supernote to contribute. The entire Rust core builds and tests **on your
host machine with no Android SDK** — that's a hard design rule (RR1-AC3), and it means most of the
interesting work (parsing, reflow, the ink model, the refresh policy, dictionary, Lua plugins) is a
`cargo test` away. This guide gets you from clone to merged PR.

---

## Table of contents

- [Code of conduct](#code-of-conduct)
- [Ways to contribute](#ways-to-contribute)
- [Architecture in 60 seconds](#architecture-in-60-seconds)
- [Development setup](#development-setup)
- [The build & test loop](#the-build--test-loop)
- [Engineering principles (non-negotiable)](#engineering-principles-non-negotiable)
- [Coding standards](#coding-standards)
- [Commit messages](#commit-messages)
- [Opening a pull request](#opening-a-pull-request)
- [Licensing & the clean-room rule](#licensing--the-clean-room-rule)
- [Reporting bugs & requesting features](#reporting-bugs--requesting-features)
- [Where to ask questions](#where-to-ask-questions)

---

## Code of conduct

This project ships a [Code of Conduct](./CODE_OF_CONDUCT.md). By participating you agree to uphold
it. Be kind, assume good faith, and keep critique about the code.

---

## Ways to contribute

You don't have to write Rust to help:

| Contribution                | Examples                                                              |
| --------------------------- | --------------------------------------------------------------------- |
| 🐛 **Bug reports**          | Crashes, wrong layout, ink that doesn't render, a refresh that ghosts |
| 💡 **Feature ideas**        | See [`spec/NEOREADER-FEATURE-BACKLOG.md`] for the roadmap             |
| 📝 **Docs**                 | Clarify a confusing README/spec, fix a typo, improve this guide       |
| 🧪 **Tests / fixtures**     | A PDF/EPUB that breaks reflow is a *gift* — add it as a golden test   |
| 🦀 **Rust core**            | Parsing, reflow, ink, refresh policy, dictionary, Lua plugin API      |
| 🤖 **Kotlin/Android shell** | The device adapter, the JNI bridge surface, the UI                    |

New here? Look for issues labelled **`good first issue`** and **`help wanted`**.

---

## Architecture in 60 seconds

```
┌─────────────────────────────────────────────────────────┐
│  app/  (Kotlin/Android shell)                            │
│    • UI, the device EPD adapter, the pen/touch input     │
│    • Speaks vendor-specific waveforms (EBC/A2/GC)        │
└───────────────────────────┬─────────────────────────────┘
                            │  JNI  (NativeBridge_*)
┌───────────────────────────┴─────────────────────────────┐
│  reader-core/  (Rust cdylib — libreader.so)              │
│    • document parse · layout · render · refresh policy   │
│    • ink model · session                                 │
│    • NEVER names a vendor — speaks RefreshIntent only    │
└──────────────────────────────────────────────────────────┘
   inkread-epub · inkread-pdftext · inkread-ink · inkread-dict
   inkread-lua  · device-eink (vendor-neutral refresh contracts)
```

Two rules fall out of this split and they shape almost every review comment:

1. **The Rust core never names a vendor (IR-7).** All device/EPD/pen specifics live in the Kotlin
   adapter + the JNI bridge. The core emits `RefreshIntent`/`RefreshCommand`, never an `EBC`/`A2`/`GC`
   waveform. If you find yourself typing a vendor name in `reader-core/`, stop — it belongs in `app/`.
2. **The core builds host-only.** No Android types leak into `reader-core`. `cargo test -p reader-core`
   must resolve **without** `jni` in the dependency graph (the JNI bridge is feature-gated behind
   `--features jni-bridge`).

> The canonical design lives in `spec/` (gitignored, not in this repo): `SPEC-INKREAD.md` plus the
> `ADR-INKREAD-*` set and the `RR` requirement ledger. PRs reference requirements by number
> (e.g. *RR5-FR1*, *ADR-0010*). If you're an external contributor and need a requirement clarified,
> ask in the issue/PR — a maintainer will quote the relevant contract.

---

## Development setup

### Prerequisites

| Tool                    | Version            | Why                                              |
| ----------------------- | ------------------ | ------------------------------------------------ |
| **Rust**                | pinned (see below) | the core + all crates                            |
| A C toolchain           | any recent clang   | `rusqlite`/`mlua`/`pdfium` compile vendored C    |
| **JDK**                 | 17–21              | Gradle (only if you build the APK)               |
| **Android SDK + NDK**   | NDK r26+           | only if you build the APK (`cargo-ndk`)          |
| `cargo-ndk`             | latest             | only if you build the APK                        |

The Rust toolchain is **pinned** in [`rust-toolchain.toml`](./rust-toolchain.toml) (currently
`1.90.0`, with `rustfmt` + `clippy` + `llvm-tools-preview`). If you use `rustup`, it picks this up
automatically — you don't install anything by hand.

> **macOS gotcha:** a Homebrew `rust` on your `PATH` is a standalone `rustc`/`cargo` that *ignores*
> `rust-toolchain.toml` and will shadow the pinned toolchain (you'll see "rustc 1.85 is not
> supported"). Prefer `rustup`. If you must keep Homebrew rust, prepend the pinned toolchain:
> `export PATH="$(rustup which cargo | xargs dirname):$PATH"`. (`buildApk.sh` already does this for
> you when it shells out.)

### Clone & verify the host build

```bash
git clone https://github.com/j-raghavan/inkread.git
cd inkread
cargo test --workspace      # should be green with no Android SDK installed
```

That's it — if that passes, you're set up to work on the core.

---

## The build & test loop

Run these **before every commit** — CI enforces all of them and a PR won't merge until they're green:

```bash
cargo fmt --all                      # format
cargo clippy --all -- -D warnings    # lint — warnings are errors
cargo test --workspace               # unit + property + golden-image tests (host)
cargo llvm-cov --workspace           # coverage — must hold the RR17 gate
./scripts/check-licenses.sh          # every dep's SPDX license is AGPL-compatible
```

Building the actual Android APK (only needed for device-facing changes):

```bash
cargo ndk -t arm64-v8a -o app/src/main/jniLibs build --release   # build libreader.so
./buildApk.sh                                                     # assemble + sign the APK
./buildApk.sh --install                                          # ...and adb install to a connected Supernote
```

`buildApk.sh` is self-contained: it builds the Rust core via `cargo-ndk`, downloads &
sha256-verifies the pinned vendored `libpdfium.so`, stages the dictionary corpus, and runs Gradle.

### A note on the PDF render tests

The pdfium render tests **skip** unless `PDFIUM_DYNAMIC_LIB_PATH` points at a `libpdfium.so`. CI
fetches the BSD bblanchon prebuilt and re-runs them for real, so the render path actually executes.
Locally they skip cleanly — that's expected, not a failure. To run them yourself, set the env var to
a downloaded `libpdfium.so`.

---

## Engineering principles (non-negotiable)

These come straight from [`CLAUDE.md`](./CLAUDE.md) and they're applied in review:

- **SOLID / DRY / KISS** and **DDD** — model the domain, don't sprinkle logic.
- **Reuse first** — prefer an existing function/utility/pattern over new code. Adding a new *file*
  needs justification; prefer editing an existing one. Keep files focused (target **< ~500 lines**).
- **Principal-engineer quality**, not intern quality. Small, reviewable, well-named diffs.
- **Do what's asked — nothing more, nothing less.** Scope creep gets split into its own PR.
- **Validate at the boundary; never panic across JNI** (RR21-FR3). The bridge catches panics and
  converts them to Java exceptions — keep `panic = "unwind"`.

---

## Coding standards

**Rust**
- `rustfmt` is the source of truth — don't hand-format.
- `clippy` runs with `-D warnings`. Fix the lint; reach for `#[allow(...)]` only with a comment that
  says why.
- Public items get doc comments. Reference the requirement they satisfy where it helps
  (`// RR5-FR1: …`).
- New behaviour ships with a test. A bug fix ships with a regression test that fails before the fix.
- Prefer total functions and explicit error types at the system boundary; no `unwrap()` on
  untrusted input.

**Kotlin/Android**
- Device/EPD/pen specifics live here, never in the core.
- Handle reader taps on `ACTION_DOWN` (the Supernote's gesture layer can swallow finger `ACTION_UP`).

---

## Commit messages

We use **[Conventional Commits](https://www.conventionalcommits.org/)** with a scope. Look at
`git log` for the house style:

```
feat(ui): pass-2 dialog polish + 1.5x tool palette
perf(reader): trim software work off the page-flip critical path
fix(reflow): clamp column width so RTL pages don't overflow
```

- **type**: `feat` · `fix` · `perf` · `refactor` · `docs` · `test` · `chore` · `build` · `ci`
- **scope** (optional but encouraged): the subsystem — `reader`, `reflow`, `ink`, `dict`, `lua`,
  `ui`, `jni`, `device`, `review`…
- Subject in the **imperative mood**, lower-case, no trailing period, ideally ≤ 72 chars.
- The commit-lint CI check enforces this on every PR.

> **Do not add `Co-Authored-By` / "Generated by" trailers, or attribute any AI tool in commits, PRs,
> or code.** Tools are facilitators, not authors. This is a hard project rule.

---

## Opening a pull request

1. **Branch** off `master`: `feat/<short-topic>`, `fix/<short-topic>`, etc.
2. Keep the PR **focused** — one logical change. Split unrelated cleanups out.
3. Run the full local gate (fmt, clippy, test, coverage, licenses) — see above.
4. Fill in the **PR template** (it auto-populates). The reviewer checklist mirrors the
   [Commit Criteria](./CLAUDE.md#commit-criteria): no format errors, no clippy warnings, no type
   errors, tests pass, coverage holds, and the core still builds **host-only**.
5. Reference the issue (`Closes #123`) and any requirement IDs (`RR…` / `ADR-…`) you implemented.
6. CI runs the host gate, cross-checks the JNI bridge for the device target, lints commits, and
   verifies the license manifest. Green CI + one maintainer approval merges.

Small, well-scoped PRs get reviewed *fast*. A 2,000-line PR sits for a week — please don't.

---

## Licensing & the clean-room rule

- inkread is **AGPL-3.0-only**. By contributing, you agree your contribution is licensed under it.
- Every dependency's license must be on the **AGPL-compatible allowlist** — `check-licenses.sh`
  fails the build for anything unvetted (RR18-AC2). Adding a dep? Expect to justify its license.
- **Clean room (RR18):** never copy decompiled Onyx/Ratta/vendor code into source. Reimplement from
  documented contracts only. The reverse-engineering research is private and **must not** appear in
  this repo (no decompiled APKs, `.so`, or jadx output in a PR).
- See [`LICENSE`](./LICENSE) and [`LICENSES-3RDPARTY.md`](./LICENSES-3RDPARTY.md).

---

## Reporting bugs & requesting features

Use the **issue templates** (New issue → pick *Bug report* or *Feature request*). A good bug report
includes the document that triggers it (or a minimal fixture), the device/host, and what you
expected vs. saw. For anything sensitive, see [`SECURITY.md`](./SECURITY.md) — please don't file
security issues in the public tracker.

---

## Where to ask questions

- **A specific bug or proposal** → open an issue.
- **"How does X work / is this the right approach?"** → start a
  [GitHub Discussion](https://github.com/j-raghavan/inkread/discussions) (or open a draft PR and ask
  inline).

Welcome aboard — we're glad you're here. 🚀

[`spec/NEOREADER-FEATURE-BACKLOG.md`]: ./spec/NEOREADER-FEATURE-BACKLOG.md
