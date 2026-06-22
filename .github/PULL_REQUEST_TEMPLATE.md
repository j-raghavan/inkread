<!--
  Thanks for contributing to inkread! Keep PRs focused on one logical change.
  Fill in the sections below — delete any that genuinely don't apply.
-->

## What & why

<!-- What does this change do, and why? Link the issue it closes. -->

Closes #

## Requirements / design refs

<!-- Spec/ADR/requirement IDs this implements or touches, e.g. RR5-FR1, ADR-INKREAD-0010.
     External contributor without spec access? Write "n/a" and describe the behaviour instead. -->

-

## How it was tested

<!-- Commands you ran, fixtures you added, device(s) you verified on (host-only is fine). -->

- [ ] `cargo test --workspace` is green
- [ ] Added/updated a test that fails without this change (for bugs/new behaviour)
- [ ] Verified on a device (Supernote) — _or_ host-only change, N/A

## Pre-merge checklist

<!-- All must be true. CI enforces them; tick to confirm you ran them locally. -->

- [ ] `cargo fmt --all` — no format diff
- [ ] `cargo clippy --all -- -D warnings` — no warnings
- [ ] `cargo test --workspace` — passes
- [ ] `cargo llvm-cov --workspace` — coverage holds the RR17 gate
- [ ] `./scripts/check-licenses.sh` — no new/unvetted dependency licenses
- [ ] The Rust core still builds **host-only** — no Android types leaked into `reader-core`,
      and `jni` is absent from its default dependency graph (RR1-AC3 / IR-7)
- [ ] The core names **no vendor** — device/EPD/pen specifics stay in `app/` + the JNI bridge (IR-7)
- [ ] Commits follow Conventional Commits (`type(scope): subject`) with **no AI/Co-Authored-By
      attribution**
- [ ] No secrets, signing keys, `.env`, or private research material (decompiled APK/`.so`/jadx)

## Notes for the reviewer

<!-- Anything to look at first, known trade-offs, follow-ups intentionally left out of scope. -->
