# Contributing to inkread

## Clean-room boundary (RR18, IR-5) — non-negotiable

inkread is a **clean-room** reimplementation. The reverse-engineering research
(decompiled Onyx/NeoReader/Ratta sources, jadx output, vendor `.so`/`.apk`) is used
**only** to understand architecture, public-API shapes, and behavior — it is **never**
copied into this repository.

- **Do not** paste decompiled code, decompiled snippets, or vendor headers into source.
- Reimplement interfaces and policies from the documented contracts, not from disassembly.
- The research material stays out of the tree (see `.gitignore`: `neoreader/`,
  `supernote/`, `*.apk`, `*.so`).
- The Rust core (`reader-core`) **never names a vendor** (IR-7); all device specifics live
  in the Kotlin `Supernote*Adapter` classes + the JNI bridge.

## License (RR18-FR3, ADR Decision 2)

The product is **AGPL-3.0-only**. Every dependency must be AGPL-3.0-compatible;
`scripts/check-licenses.sh` enforces this in CI against the allowlist, and
`LICENSES-3RDPARTY.md` is the generated manifest. When you add a dependency, regenerate the
manifest and ensure the license is on the allowlist (or extend it deliberately, with a note).
No DRM code and no DRM-protected files are ever opened.

## The commit gate (CLAUDE.md)

Before every commit, the full gate must pass:

```bash
cargo fmt --all --check
cargo clippy --all -- -D warnings
cargo test --workspace
```

The Rust core must keep building **host-only** (no Android SDK); `jni` stays behind the
`jni-bridge` feature so a bare `cargo test -p reader-core` never resolves it (RR1-AC3 / IR-7).
