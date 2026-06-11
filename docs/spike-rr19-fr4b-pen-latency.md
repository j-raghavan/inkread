# Spike runbook — RR19-FR4b pen-latency (the product go/no-go)

> **Status:** runbook only — to be executed by the owner on a physical Supernote.
> **Gates:** ADR Decision 3 (distribution model) and the Supernote-only thesis. Runs
> **alongside M0**, independently of the M0 reader bring-up.

## Why this exists

The ~20 ms ink path is the product's differentiator (RR19, IR-9). On-device investigation
found Ratta's *own* instant ink is **system-privileged** (SurfaceFlinger transaction +
`EinkManager`, `ACCESS_SURFACE_FLINGER`-gated). A plain sideloaded app cannot use that path.
This spike answers the make-or-break question: **can a sideloaded app reach ~20 ms nib-to-ink
on the Supernote, and by which route?** The answer decides whether v1 ships as a normal
sideload (ADR Decision 3 as written) or must become a privileged/rooted install — or whether
the Supernote-only bet itself needs reconsidering (RR19-FR4b, ADR §Decision 3).

This is a **separate measurement APK**, not the reader. Keep it minimal so the measurement is
trustworthy.

## What to measure

**Nib-to-ink latency**: the time from a stylus `MotionEvent` sample to that ink appearing on
the e-ink panel. Target **≤ ~20 ms** (RR24-FR4). Report the distribution (median, p90, max),
not just a single number — e-ink waveform timing is bimodal.

## The measurement APK (build it minimal)

A single full-screen `Activity` with a `SurfaceView` that:

1. Captures stylus `MotionEvent`s (`getToolType == TOOL_TYPE_STYLUS`), including
   `getHistorical*` batched samples; ignores finger/palm (RR19-FR7).
2. Draws each new segment into the `SurfaceView`.
3. Timestamps `event.getEventTime()` (input sample time) and the moment the panel post
   completes, and logs the delta.
4. Has a visible counter overlay (median/p90/max ms) and dumps a CSV to
   `getExternalFilesDir(null)/pen-latency.csv` for offline analysis.

> Measure with a high-speed camera too (240–960 fps) as ground truth — the on-device
> timestamp misses the panel's physical settle. Film the nib + screen, count frames
> nib→ink, cross-check against the logged deltas.

## Routes to test — IN ORDER, stop at the first green

Test each route and record the latency distribution as-is. **Green on any ⇒ stop, that's the
winner.**

1. **Automatic SurfaceView + touch fast path (preferred).**
   Render the stroke in a plain `SurfaceView` and rely on the EBC hwcomposer's
   *automatic* A2-on-touch classification (`handle_clr_touch`) — **no API call**.
   - **The critical unknown:** does a *third-party* (non-Ratta) `SurfaceView` get this fast
     classification, or is it reserved to Ratta `wrt_app`/`winType` packages? Measure.
2. **`com.ratta.DrawService` bridge.**
   Bind the exported `com.ratta.DrawService` (no permission, runs as system) and feed it
   strokes / ask it to refresh. Measure whether it accepts our strokes and what latency
   results.
3. **Direct `/dev/ebc` A2 (last resort).**
   Open `/dev/ebc` (SELinux-permitted from the app sandbox) and issue an A2 partial update for
   the stroke's bounding box. **The unknown:** does our region update survive the
   compositor's next post, or get overwritten? Measure.
4. **Standard refresh (the floor).**
   `MotionEvent` → normal `Fast`/`Ui` refresh. This is the `pen_low_latency = false` fallback;
   handwriting still works, just with visible latency (RR19-FR5). Record this as the baseline.

## Verdict → decision

| Outcome | `pen_low_latency` | Action |
|---|---|---|
| **Green** on route 1/2/3 (≤ ~20 ms sideloaded) | `true` | **Proceed.** ADR Decision 3 stands (normal sideload). The winning route becomes `SupernotePenAdapter`'s primary path (RR19 DoD). |
| **Red** on all sideloaded routes | `false` | **Escalate.** Either pursue the **privileged/system-app install** (RR19-FR3b — unlocks `EinkManager` directly, changes the distribution model) or accept standard stylus and **reconsider the Supernote-only thesis** (the 20 ms ink *is* the differentiator). |

## Record the result (this is the deliverable)

When the spike runs, record in the ADR and the spec Decision log:

- The latency distribution per route (median / p90 / max), with the high-speed-camera
  cross-check.
- The **winning route** (or "all red").
- The resulting `pen_low_latency` value and the ADR Decision 3 disposition (accept as written
  vs flip to privileged-primary).

Until then, M0 ships advertising `pen_low_latency = false` (the honest baseline,
`DeviceCapabilities.supernoteBaseline()`), and the reader degrades correctly (RR19-AC2).
