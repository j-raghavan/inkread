# InkBridge native bridge proof

## Goal

Keep each vendor's native reading/annotation environment and make the two systems exchange editable ink rather than replacing NeoReader or Supernote DOC with a common reader.

## BOOX proof result (Note Air 4C, 2026-07-24)

A test PDF containing four externally-created standard PDF `/Ink` annotations was opened in NeoReader, edited, and saved with **Embed Data to PDF**.

Inspection of the returned PDF shows:

- Externally-created standard `/Ink` annotations are editable in NeoReader.
- NeoReader preserves their `/NM` annotation IDs and adds `/onyxtag` metadata with a BOOX UUID and `type: PencilStroke`.
- NeoReader can transform those imported `/Ink` annotations (for example scaling/translating them) while leaving them as standard `/Ink` objects.
- Deleting an imported stroke removes it from the page's active annotation list.
- Native NeoReader handwriting embedded into the PDF is stored as `/Subtype /Stamp`, `/Name /#ONYX-STROKE`, with `/onyxtag` `type: BrushStroke` and a stable UUID.
- Native BOOX strokes also carry an `/onyxpoints` stream. The observed stream is structured binary rather than a raster-only appearance: an 8-byte header followed by fixed five-float records. X/Y and elapsed-time fields are directly visible; pressure-like sample values are also present. The remaining per-sample field must be mapped before relying on it.

This is sufficient evidence that BOOX-to-bridge extraction can be implemented without replacing NeoReader.

## Supernote official API capability

Ratta's official plugin API exposes native NOTE/DOC elements, including handwritten strokes with:

- UUID
- page/layer
- thickness
- pen type/color
- EMR points
- pressure samples

The API supports reading, inserting, modifying, replacing, and deleting elements, plus reloading the currently-open file.

Relevant official documentation:

- https://github.com/Supernote-Ratta/docs-plugin
- `PluginFileAPI.getElements`
- `PluginFileAPI.insertElements`
- `PluginFileAPI.modifyElements`
- `PluginCommAPI.reloadFile`

## Next proof

Build the smallest possible official Supernote plugin that runs inside NOTE/DOC and proves that plugin-created stroke data becomes an ordinary native editable Supernote stroke.

Acceptance test:

1. Open a PDF/DOC containing at least one handwritten native Supernote stroke.
2. Tap an `InkBridge Test` toolbar button.
3. Plugin reads current file path/page and enumerates page elements.
4. Plugin takes one existing stroke, duplicates its geometry with a small X/Y offset, assigns a new element identity as required by the API, and inserts it via `PluginFileAPI.insertElements`.
5. Plugin calls `PluginCommAPI.reloadFile`.
6. The duplicated stroke must be selectable with native lasso, movable, erasable, and otherwise behave like ordinary Supernote ink.

If this passes, both device-native environments have a viable editable-ink bridge.

## Architecture after both proofs

InkBridge should become a lightweight translation/synchronization layer:

- BOOX side: NeoReader remains the reader. A companion bridge watches/merges PDF annotations and understands standard `/Ink` plus BOOX `/onyxtag` + `/onyxpoints` strokes.
- Supernote side: official plugin remains inside native NOTE/DOC and translates native `Element/Stroke` data.
- Portable identity/journal: small InkBridge sidecar for stable cross-device IDs, tombstones, origin metadata, and conflict resolution.
- PDF remains the document carrier/interoperability surface, but not the sole multiwriter conflict database.

The existing Inkread-based BOOX reader work remains a useful fallback and SDK reference, but PR #2 should stay draft while the native-bridge proof is evaluated.
