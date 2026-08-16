# Release smoke checklist (on-device)

Run this pass on a real Supernote before tagging a release. Device behavior is the product and
cannot be simulated: a generic AVD has no EMR pen, no EPD, and `adb screencap` returns black on
this panel (EPD compositing), so every step below is verified **by eye on the device** — there is
no screenshot automation to lean on.

**Devices:** run the full list on one primary device (Nomad or Manta); when a step is marked
*both*, repeat it on the second device — those steps have per-panel behavior (touch metrics,
panel size).

**Setup:** `./buildApk.sh --install` (or sideload the release-candidate APK), with at least one
PDF with a text layer, one scanned PDF, and one EPUB on the device.

Tick a step only when the expected result is observed. Any failure blocks the tag.

## 1. Open & render

| # | Action | Expect | Devices |
|---|--------|--------|---------|
| 1.1 | Launch inkread → Home shows the shelf; open a text-layer PDF | Page renders crisp; no loading freeze (a "Loading…" frame at worst) | primary |
| 1.2 | Kill the app (swipe away), relaunch | Reopens the same book at the same page (RR27) | primary |
| 1.3 | Open an EPUB from Home | Renders reflowed; TOC available from the bottom bar | primary |

## 2. Page turn & navigation

| # | Action | Expect | Devices |
|---|--------|--------|---------|
| 2.1 | Tap right / left page edges repeatedly (10+ pages) | Page turns keep up (prefetch); no stuck refresh, no ghost pile-up | both |
| 2.2 | Centre-tap → bottom bar; drag the page slider, release | Jumps to the page; slider label tracked the drag | primary |
| 2.3 | Bottom bar → Contents; tap a chapter | Jumps to the chapter start | primary |
| 2.4 | Tap an internal PDF link (TOC page or cross-reference) | Follows the link — single tap, on finger-DOWN feel (no double-tap needed) | primary |

## 3. Ink (firmware pen path)

| # | Action | Expect | Devices |
|---|--------|--------|---------|
| 3.1 | With Pen tool active, write a few words | Ink appears under the nib at firmware latency (no visible lag) | both |
| 3.2 | Turn the page and come back | Strokes persisted and re-render baked on the page | primary |
| 3.3 | Undo, then redo from the selection toolbar / palette | Last stroke disappears, then returns | primary |
| 3.4 | Eraser tool: scrub across a stroke | Grey swept band follows the nib (no black firmware ink); stroke vanishes; surrounding strokes untouched | primary |
| 3.5 | After 3.4, turn the page and come back | Stroke still gone; **no** eraser-shaped scribble anywhere on the page (#158) | primary |
| 3.6 | With the **Pen** tool active, flip the pen to its eraser end and scrub a stroke | Erases — does not ink. Turn the page and back: stroke stays gone, nothing new drawn (#158) | primary |
| 3.7 | Rest a palm while writing | No stray finger input fires (palm rejection); writing unaffected | both |
| 3.8 | Kill the app right after a stroke; relaunch | The stroke survived (autosave + crash-safe sidecar, RR20) | primary |

## 4. Lasso & selection

| # | Action | Expect | Devices |
|---|--------|--------|---------|
| 4.1 | Lasso tool: circle some strokes | **Dashed** marching-ants loop draws (no stray firmware ink); selection box + toolbar appear | both |
| 4.2 | Drag the selection to a new spot | Strokes move; nothing left behind | primary |
| 4.3 | Copy, then paste | Duplicate lands slightly offset beside the source | primary |
| 4.4 | Delete the selection | Selected strokes gone; undo restores them | primary |
| 4.5 | Lasso over printed text (Smart mode) | Text selection result with Copy/Define/Digest actions | primary |
| 4.6 | Exit the Lasso tool | The pen writes again immediately | both |

## 5. Reading tools

| # | Action | Expect | Devices |
|---|--------|--------|---------|
| 5.1 | Hold the pen still on a word | Definition card pops (on-device corpus); no stroke committed | primary |
| 5.2 | Bottom bar → Search; search a word; step through hits | Hits highlight on-page; jump lands on the right page | primary |
| 5.3 | Tap the top-right corner | Dog-ear bookmark toggles; Marks lists it; tap the entry to jump | primary |
| 5.4 | Highlighter tool: drag across a paragraph | Translucent band bakes over the text lines; persists after a page turn | primary |
| 5.5 | Adjust sheet: Font size A+ on the EPUB | Repaginates at the larger size; position roughly held | primary |
| 5.6 | Adjust sheet: Reflow **On** for the text-layer PDF; then on the scanned PDF | Text PDF reflows ("Reflowing…" only if slow); scanned PDF toasts "no text layer" and stays put | primary |
| 5.7 | Pinch on a fixed-layout PDF; double-tap | Live zoom preview, crisp re-render on release; double-tap toggles zoom | both |

## 6. Export & integrations

| # | Action | Expect | Devices |
|---|--------|--------|---------|
| 6.1 | Export → editable annotations; open the PDF in the Supernote firmware reader | Ink visible as annotations in the other app (written into the PDF, not a sidecar) | primary |
| 6.2 | Export → flattened; reopen the exported file | Ink baked into the page image | primary |
| 6.3 | Lasso a passage → Digest | Entry appears in the firmware Digest app (Knowledge provider write-through) | primary |

## 7. Daily & self-update

| # | Action | Expect | Devices |
|---|--------|--------|---------|
| 7.1 | Daily screen → compile today's issue (needs Wi-Fi) | "Compiling…" notice, then the issue opens; articles interleaved across sources | primary |
| 7.2 | Open a Daily article → bottom bar → "Daily" | Returns to the issue's front page | primary |
| 7.3 | Settings → check for updates (on a production-signed install) | Finds/declines the latest release correctly; a debug-signed build reports the updater inert (signer gate fails closed) | primary |

## 7b. Calibre library (OPDS, needs a calibre content server or Calibre-Web on the LAN)

| # | Action | Expect | Devices |
|---|--------|--------|---------|
| 7b.1 | Settings → Library; enter a bare host (e.g. `192.168.1.20:8080`); save | Home grows a "Your library" card showing that address | primary |
| 7b.2 | Tap the card | The library's top-level feed lists rows; navigation rows read BROWSE →, books read DOWNLOAD EPUB | primary |
| 7b.3 | Walk into a category, then press Back | Returns to the previous feed, not out of the library | primary |
| 7b.4 | Search for a title with a space in it | Matches come back (percent-encoded path, not `+`) | primary |
| 7b.5 | Download a book, then "Read now" | Opens in the reader; it is also on the Home shelf afterwards | primary |
| 7b.6 | Re-download the same book, then kill Wi-Fi mid-transfer | The already-shelved copy still opens — a failed download costs nothing (#175) | primary |
| 7b.7 | Point at a server that is off / wrong | Says it could not reach the library and names `--auth-mode=basic`; no crash, no empty-looking library | primary |
| 7b.8 | On a Calibre-Web instance with a login, set username + password | Catalog loads (Basic auth); wrong credentials fail with the same clear notice | primary |

## 8. Lifecycle & stability

| # | Action | Expect | Devices |
|---|--------|--------|---------|
| 8.1 | Rotate the device (Adjust → Rotate 90°) | Re-renders at the new orientation; ink/highlights anchored correctly | primary |
| 8.2 | Sleep the device mid-book (cover / power), wake | Reader resumes where it was; pen still writes | both |
| 8.3 | Background inkread, open the firmware Notes app, ink there, return | inkread unaffected; its pen path re-claims correctly | primary |

---

*When a release ships with a step intentionally skipped (e.g. only one device on hand), note it
in the release PR under "How it was tested".*
