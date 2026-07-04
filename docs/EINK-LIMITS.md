# E-ink refresh & pen latency on Supernote: what a sideloaded app can and cannot do

Short version: **pen latency is firmware-fast and refresh tuning is platform-capped.** Where
inkread differs from KOReader-on-Kobo or the native Supernote reader on refresh behavior, it is a
device-policy boundary, not missing code. This page states those limits plainly so you can tell a
platform constraint apart from an unbuilt feature.

## What the platform gives a sideloaded app

The Supernote family (RK3566, Android 11) exposes different display capabilities to **system**
apps (the firmware reader, Notes) than to **sideloaded** apps like inkread:

| Capability | System apps | Sideloaded apps |
|---|---|---|
| Waveform selection (A2/DU-class fast modes vs quality modes) | Yes | **No** — no public API, and the privileged paths are blocked by SELinux for third-party apps |
| Partial (dirty-rect) refresh | Yes | **No** — the panel path a sideloaded app can reach performs full-screen updates only |
| Full-screen refresh | Yes | Yes — the firmware auto-refreshes on drawing, and a full flash can be requested |
| Live pen ink | Firmware overlay | **Yes, same firmware overlay** — see below |

## Pen latency: solved by the platform, kept by design

inkread does **not** render live pen strokes itself. The firmware's ink service draws the wet ink
on its overlay at sub-frame latency — the same path the vendor's own Notes app uses — and inkread
feeds it stroke geometry and consumes the results into its own Rust ink model (undo, persistence,
export, lasso all run on our side). This is recorded as a deliberate design decision
(spec amendment S-2, ADR-INKREAD-0004): on this hardware, a firmware overlay beats any app-side
render path, and the portable Rust stroke model is unchanged beneath it.

So "pen latency work" is not deferred — nib-to-ink is already at the native app's latency. What
an app-side path would add is nothing; what it would risk (fighting the EPD controller from
userspace) is real.

## Refresh: what inkread does within the cap

- **Policy over waveforms.** The Rust core runs a content-aware refresh policy (RR3 / spec
  amendment S-1) that emits vendor-neutral intents — flash here, avoid flashing there, clear
  ghosts after N partials, night-mode handling. On Supernote the adapter maps these onto what the
  platform allows: cooperate with the firmware's auto-refresh, and issue full refreshes at policy
  boundaries. On a future device with real waveform control, the same policy drives it with no
  core change.
- **Ghosting** is managed by full-refresh cadence, not per-region waveform tricks — the platform
  offers nothing finer to a sideloaded app.
- **Page-turn speed** is won in software instead: a pagination cache plus next-page read-ahead
  keeps the render off the critical path, so a page turn costs roughly one panel update.

## What this means for feature requests

- *"Add A2/fast mode like KOReader on Kobo"* — not possible sideloaded on this device; KOReader's
  own Supernote port has the same ceiling (full-refresh via the public path).
- *"Partial refresh for the ink/UI"* — same answer; the firmware pen overlay already covers the
  case that matters most.
- *"Configurable full-refresh interval"* (every N pages, manual refresh) — possible within the
  cap and planned (#99).

If Ratta exposes finer refresh control to third-party apps in a future firmware, the adapter is
the only layer that changes (IR-7: the core names no vendor and speaks intents).
