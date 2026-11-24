# Daozai Windows text patch

Upstream: egui/epaint 0.31.1, MIT OR Apache-2.0 (license texts alongside this file).

The patch adds `src/text/directwrite.rs` and the Windows dependency in Cargo.toml.
`fonts.rs` carries the existing FontData collection index into FontImpl; `font.rs`
uses DirectWrite glyph alpha and bounds in the existing atlas. Layout metrics,
kerning, font fallback selection, texture uploads and rendering stay upstream.

The native raster size and its cache key retain fractional precision. Coverage
neutralizes FontImage's legacy 0.55 weight curve before the patched egui_glow
painter applies foreground-aware correction. Alternative painters need the same
coverage convention when using this local patch.

DirectWrite uses real faces with `DWRITE_FONT_SIMULATIONS_NONE`, natural symmetric
grayscale and ClearType masks with enabled grid fitting. Its em size is converted from
ab_glyph's height scale. Font collection indices are preserved (including YaHei UI).
The per-thread cache retains the source FontArc and native face; DirectWrite owns
a copy of font bytes because the in-memory loader receives no owner object.
The cache retains at most 32 faces. The loader is unregistered on thread teardown.

The in-memory loader requires IDWriteFactory5 (Windows 10 version 1703 or newer).
Factory, font or glyph errors fall back to the original ab_glyph path; other OSes
use the unchanged path. FontImage carries RGB coverage separately, preserving
the original grayscale paint bounds and metrics. LCD filter tails outside those
bounds are cropped, so existing scrolling and layout do not expand. The patched
egui_glow painter uses RGB masks only on opaque windows; grayscale remains available.
`DAOZAI_LEGACY_TEXT=1` selects the original rasterizer for controlled comparisons.
`epaint::text::directwrite_rasterization_stats()` reports native/fallback counts.

Regression entry: `cargo test --locked --lib windows_text_uses_native_grid_fitting -- --nocapture`.
It verifies real CJK/Latin glyph generation, native font weights, grayscale coverage
and 100/125/150/200% scales without silent font fallback. The settings gallery test
also covers every setting tab in both themes with the existing software renderer.

Microsoft API references:
- https://learn.microsoft.com/en-us/windows/win32/api/dwrite_2/nf-dwrite_2-idwritefactory2-createglyphrunanalysis
- https://learn.microsoft.com/en-us/windows/win32/api/dwrite_3/nf-dwrite_3-idwriteinmemoryfontfileloader-createinmemoryfontfilereference

Packaging-only adjustment: Cargo.toml includes local license paths and omits the
benchmark target whose source was not included in this vendored subset.
