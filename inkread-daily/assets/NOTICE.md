# Daily assets

- `vintage-masthead.png`: the Vintage style's nameplate, "The Daily", a grayscale raster of
  the words set in UnifrakturCook (SIL Open Font License 1.1). The image is a document produced with
  the font, not the font software, so it carries inkread's own AGPL-3.0 license; the font itself is
  not redistributed here. The reflow engine has no per-block font selection, so the nameplate ships
  as an image rather than live text.

- Display equations (#66 math): rendered to grayscale PNGs at assembly time by the `latex-rust`
  crate (MIT OR Apache-2.0), which bundles the **STIX Two Math** font (SIL Open Font License 1.1).
  The font software ships inside the `latex-rust` dependency, not in this repository; the equation
  images the daily embeds are documents produced with that font, carrying inkread's own AGPL-3.0
  license. STIX Two Math is (c) the STIX Fonts project; the OFL permits this embedding and bundling.
  Because the font software is compiled into the shipped binary, its OFL 1.1 text travels with the
  distribution in `STIXTwoMath-OFL.txt` (this directory).
