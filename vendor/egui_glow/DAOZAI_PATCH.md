# Daozai native text coverage patch

Upstream egui_glow 0.31.1, MIT OR Apache-2.0 (license texts included).

On Windows, epaint retains DirectWrite RGB coverage alongside the grayscale mask.
OpenGL 3.3+ uses dual-source blending: the second shader output supplies separate
RGB destination attenuation, preserving ordinary source-over order even when a
mesh mixes text and solid geometry. Only opaque viewports enable RGB text.
Transparent viewports, older OpenGL drivers and `DAOZAI_DISABLE_SUBPIXEL=1` use
the grayscale mask. `DAOZAI_DISABLE_DUAL_SOURCE=1` exercises the old-driver
shader variant. No system font-smoothing settings are changed.

Font coverage is uploaded as linear RGBA8 data; ordinary image textures retain
the upstream sRGB path. epaint neutralizes the legacy 0.55 FontImage weight curve;
the painter then applies foreground-aware correction once. `DAOZAI_LEGACY_TEXT`
retains the previous rasterizer and weight behavior for comparisons.

The preblend follows the perceptual-inverse destination estimate used in Skia's
grayscale mask correction, with gamma 1.4 and contrast 1.0. This does not reproduce
all of Chromium's layout, font selection, subpixel positioning or system overrides.
The Rust `font_preblend_alpha` function is used by Daozai's existing software
preview renderer, which also applies component-wise destination attenuation
when its opaque-text mode is enabled.

Relevant primary sources:
- https://github.com/google/skia/blob/main/src/core/SkMaskGamma.cpp
- https://github.com/chromium/chromium/blob/main/ui/gfx/font_render_params_win.cc
- https://registry.khronos.org/OpenGL/extensions/ARB/ARB_blend_func_extended.txt

Only painter.rs, shader/fragment.glsl and lib.rs have runtime changes. Cargo.toml
uses local license paths and omits the example target not included in this subset.
