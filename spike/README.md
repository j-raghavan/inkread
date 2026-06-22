# penspike — RR19-FR4b pen-latency spike

A **separate, minimal measurement APK** (package `dev.jraghavan.inkread.penspike`) — **not the
reader**. It answers the product go/no-go gating ADR Decision 3: *can a sideloaded app reach
~20 ms nib-to-ink on the Supernote, and by which route?* See the runbook:
`docs/spike-rr19-fr4b-pen-latency.md`.

This module is intentionally device-specific and names the vendor (Ratta/Rockchip). IR-7
constrains only `reader-core`; this throwaway tool is exempt. It depends on nothing in
`reader-core` / `device-eink`.

## Build

```bash
# From repo root. Standalone module, no cargo-ndk needed (uses a C native helper via CMake).
./gradlew :spike:assembleDebug
# Output:
#   spike/build/outputs/apk/debug/spike-debug.apk
```

The native `/dev/ebc` helper (`src/main/cpp/ebc_jni.c`) is compiled by the Android NDK via
`externalNativeBuild` (CMake). No host Rust toolchain involved.

## Install & run

```bash
adb install -r spike/build/outputs/apk/debug/spike-debug.apk
adb shell am start -n dev.jraghavan.inkread.penspike/.PenSpikeActivity
# Watch the route probes + latency stats:
adb logcat -s PenSpike PenSpike-ebc
```

The CSV lands at (pull it after a run):

```bash
adb shell run-as dev.jraghavan.inkread.penspike cat \
  /sdcard/Android/data/dev.jraghavan.inkread.penspike/files/pen-latency.csv
# or
adb pull /sdcard/Android/data/dev.jraghavan.inkread.penspike/files/pen-latency.csv
```

## How to read the verdict

The on-screen legend band (top of screen) shows the **active route**; **tap the band to cycle**
R1 → R2 → R3 → R4. Draw below it **with the stylus** (finger/palm are ignored, RR19-FR7).

Each route logs to logcat tag `PenSpike` (the native helper logs `PenSpike-ebc`):

| Route | Reachable when logcat shows… | What it proves |
|---|---|---|
| **R1** auto-A2 | always (`ROUTE 1 ... reachable=true`) | A plain third-party SurfaceView draws; whether the einkhwc auto-applies A2 is a **camera/quality** question, not a reachability one. |
| **R2** DrawService | `ROUTE 2 (DrawService): BINDS (onServiceConnected) descriptor='…'` | The exported service **binds** from a sideload. The logged `descriptor` is the AIDL to develop later. `onNullBinding` or `bindService()=false` = red. |
| **R3** /dev/ebc | `ROUTE 3 (/dev/ebc): reachable=true (open+ioctl OK)` + per-stroke `SEND_BUFFER(A2)=OK` | The make-or-break: **open()+ioctl succeed under the untrusted_app SELinux domain**. `open(/dev/ebc)=FAILED errno=13(EACCES)` = red (SELinux blocks it) — that is a RESULT, not a bug. |
| **R4** baseline | always | The `pen_low_latency=false` floor for comparison. |
| **R5** service_myservice | `ROUTE 5 (service_myservice): reachable=true alive=true desc='…'` | **The make-or-break ink route.** The firmware HandWrite binder is reachable from a sideload, so the firmware paints ink under the nib at sub-frame latency (the app draws nothing — see it appear). `reachable=false … hidden-API blocked` = the Java-reflection lookup is blocked; production moves the `getService` lookup into the JNI native helper (not subject to hidden-API enforcement). |

> **R5 is special:** it is the only route where the firmware renders the stroke, not the app. When
> R5 is active the app draws nothing per sample — *whatever ink you see under the pen is the
> firmware's*. If ink appears under the nib with no lag, R5 is GREEN and is the path inkread ships
> (binder client in the Kotlin adapter, ink model in Rust). See `spec/adr/ADR-SUPERNOTE-INK.md`.

The `latency …` line (median/p90/max ms) is the **software-observable** delta
(`event.getEventTime()` → surface-post return). It is a **lower bound / relative comparison**
only — it does **not** capture the panel's physical settle. The true nib-to-ink number comes
from the camera (below).

`SUMMARY <route>: n=… med=… p90=… max=…` lines are emitted on `onPause` (e.g. when you press
home), so a short session still yields numbers.

## Camera ground-truth procedure (the real nib-to-ink ms)

The on-device timestamp **misses the panel's physical waveform settle** — that is exactly the
part that decides ~20 ms. Measure it optically:

1. **Rig.** Phone/camera at **240 fps minimum** (960 fps if available — iPhone slow-mo or a
   Galaxy "Super Slow-mo"; or a dedicated high-speed cam). Mount it on a tripod looking
   **straight down** at the Supernote so you can see **both the pen nib and the screen ink**
   in one frame. Bright, even, flicker-free lighting (daylight or DC LED; avoid PWM-flickering
   bulbs that beat against the frame rate).
2. **Per route.** Cycle to the route under test (tap the legend), confirm logcat shows it
   active. Make **single, deliberate, fast strokes** (a quick dot-then-dash) with a clear gap
   between them so frames are countable.
3. **Count frames.** Scrub the footage frame-by-frame. Mark frame **A** = the nib contacts the
   glass (or starts moving), frame **B** = ink first becomes visible at that point. Latency =
   `(B − A) / fps × 1000` ms. At 240 fps each frame = 4.17 ms; at 960 fps = 1.04 ms.
4. **Distribution, not a single number.** Repeat ≥10 strokes per route; e-ink waveform timing
   is bimodal. Report median / p90 / max, and note any strokes where the ink **flickers/ghosts**
   or gets **overwritten by a full refresh** (especially R3 — does our A2 region survive the
   compositor's next post?).
5. **Cross-check.** Compare the camera median against the logged CSV median for the same route.
   A large gap = the software timestamp under-counts the panel settle (expected); the **camera
   number is the verdict** against the ~20 ms target (RR24-FR4).
6. **Record** per route: camera median/p90/max, the software median, and a one-line quality
   note (clean / ghosting / overwritten). Green on any route (≤ ~20 ms, clean) ⇒
   `pen_low_latency=true`, proceed (ADR Decision 3 stands). All red ⇒ escalate per the runbook
   verdict table.

## Clean-room note (RR18)

The EBC ioctl ABI in `ebc_jni.c` is reimplemented **only** from the public GPL Rockchip
`ebc-dev` kernel UAPI header (ioctls `0x7000–0x7007`, the 44-byte `ebc_buf_info`, the
`panel_refresh_mode` enum where `EPD_A2=12`). Sources are cited in the file banner. No
decompiled Ratta/Onyx code was copied. **Enum caveat:** values `0–12` are identical across the
known kernel forks (so `EPD_A2=12` is safe); values `≥13` (DU/RESET/…) diverge between forks —
the helper deliberately uses only A2/PART_GC16/FULL_GC16 (`≤12`). If the vendor kernel re-pinned
the enum, A2 is still 12; verify higher modes against the device's GPL kernel drop before use.
