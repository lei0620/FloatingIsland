//! 玻璃材质的数值，逐条取自 oyenza-studio（MIT）。
//!
//! 对应关系见 `CHANGES-v0.5.5.md` 的对照表：
//! - `crates/poni-core/src/glass.rs`：烟熏不透明度、最小/最大着色、对比度目标、
//!   厚度、阴影；
//! - `crates/poni/src/glass.rs`：霜层模糊、rim/glow 强度、饱和度；
//! - `crates/poni/res/glass.hlsl`：`dark = mat * (1 - tint)`、rim 与 glow 的叠加方式；
//! - `crates/poni-core/src/look.rs`：玻璃底色与描边。
//!
//! 浅色岛体先对后方像素做高斯模糊，再叠加浅色材质，最后绘制清晰的前景。

/// 看不到屏幕时的烟熏玻璃不透明度（oyenza `glass.rs:21`）。
pub const SMOKED_OPACITY: f32 = 0.82;
/// 深色玻璃的着色范围（`crates/poni/src/glass.rs:673`）。
pub const TINT_MIN: f32 = 0.34;
pub const TINT_MAX: f32 = 0.85;
/// 文字要达到的对比度（`poni-core/src/glass.rs:173`）。
pub const TEXT_CONTRAST: f32 = 4.8;
/// 玻璃最薄/最厚的高度（DIP，`poni-core/src/glass.rs:25-26`）。
const THIN_H: f32 = 37.0;
const THICK_H: f32 = 220.0;
/// 边缘高光与内发光的强度（`crates/poni/src/glass.rs:693-694`）。
pub const RIM_STRENGTH: f32 = 0.80;
pub const RIM_FACING_MIN: f32 = 0.30;
pub const GLOW_STRENGTH: f32 = 0.09;
/// 两层霜的模糊半径，DIP（`crates/poni/src/glass.rs:29-30`）。
pub const FROST_LO: f32 = 3.0;
pub const FROST_HI: f32 = 9.0;
/// 颜色饱和度与色散（`crates/poni/src/glass.rs:699-700, 725`）。
pub const SATURATION: f32 = 1.45;
pub const DISPERSION: f32 = 0.03;
/// 深色玻璃的底色与描边（`poni-core/src/look.rs:139, 142`）。
pub const GROUND_DARK: (u8, u8, u8) = (0x1C, 0x1C, 0x1E);
pub const BORDER_ALPHA: f32 = 0.10;

/// 玻璃厚度：0 = 收起的小药丸，1 = 展开的面板。
pub fn thickness(height: f32) -> f32 {
    ((height - THIN_H) / (THICK_H - THIN_H)).clamp(0.0, 1.0)
}

/// 阴影强度（`poni-core/src/glass.rs:238-240`）。
pub fn shadow_alpha(busy: f32, thick: f32) -> f32 {
    (0.12 + 0.06 * thick + 1.1 * busy).clamp(0.12, 0.34)
}

/// WCAG 相对亮度（`poni-core/src/glass.rs:120-123`）。
pub fn rel_luminance(c: [f32; 3]) -> f32 {
    fn lin(v: f32) -> f32 {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

/// WCAG 对比度，1..21。
pub fn contrast(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (la, lb) = (rel_luminance(a), rel_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// 让 `ink` 在「底色 `material` 混入 `toward`」上至少达到 `target` 对比度，
/// 取满足条件的最小混合比例；范围是 `min..=max`（`poni-core/src/glass.rs:139-157`）。
pub fn tint_for_contrast(
    material: [f32; 3],
    ink: [f32; 3],
    toward: [f32; 3],
    min: f32,
    max: f32,
    target: f32,
) -> f32 {
    let ok = |t: f32| contrast(mix3(material, toward, t), ink) >= target;
    if ok(min) {
        return min;
    }
    if !ok(max) {
        return max;
    }
    let (mut lo, mut hi) = (min, max);
    for _ in 0..20 {
        let mid = (lo + hi) / 2.0;
        if ok(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
}

/// 深色玻璃在给定背景上的着色量。没有背景样本时用 oyenza 的兜底值 `0.34`
/// （`crates/poni/src/glass.rs:678`）。
pub fn dark_tint(backdrop: Option<[f32; 3]>) -> f32 {
    match backdrop {
        Some(b) => tint_for_contrast(b, [1.0; 3], [0.0; 3], TINT_MIN, TINT_MAX, TEXT_CONTRAST),
        None => TINT_MIN,
    }
}

/// 边缘高光的基础强度：rim 0.80 × 背光面 0.30（`glass.hlsl:149`）。
pub fn rim_alpha() -> f32 {
    RIM_STRENGTH * RIM_FACING_MIN
}

use eframe::egui::{self, Color32, Painter, Pos2, Rect, Shape};
use crate::{paint, shape::Rounded};

fn light_surface_image() -> egui::ColorImage {
    egui::ColorImage { size: [1, 2], pixels: vec![Color32::WHITE, Color32::from_rgb(247, 250, 255)] }
}

#[cfg(any(windows, test))]
pub(crate) const BACKDROP_SIGMA: f32 = 5.5;

/// 可分离高斯滤波只作用于桌面样本，岛内文字和图标始终按原分辨率绘制。
#[cfg(any(windows, test))]
pub(crate) fn blur_backdrop([width, height]: [usize; 2], pixels: &[[u8; 3]], sigma: f32) -> egui::ColorImage {
    let radius = (3.0 * sigma).ceil() as isize;
    let mut weights: Vec<_> = (-radius..=radius).map(|d| (-0.5 * (d as f32 / sigma).powi(2)).exp()).collect();
    let sum: f32 = weights.iter().sum();
    weights.iter_mut().for_each(|weight| *weight /= sum);

    let mut horizontal = vec![[0.0; 3]; width * height];
    for y in 0..height {
        for x in 0..width {
            let dest = &mut horizontal[y * width + x];
            for (d, &weight) in (-radius..=radius).zip(&weights) {
                let sx = (x as isize + d).clamp(0, width as isize - 1) as usize;
                let source = pixels[y * width + sx];
                for c in 0..3 { dest[c] += f32::from(source[c]) * weight; }
            }
        }
    }
    let mut blurred = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let mut rgb = [0.0; 3];
            for (d, &weight) in (-radius..=radius).zip(&weights) {
                let sy = (y as isize + d).clamp(0, height as isize - 1) as usize;
                let source = horizontal[sy * width + x];
                for c in 0..3 { rgb[c] += source[c] * weight; }
            }
            // 让背景的色彩透出，同时维持暗色桌面上的文字对比度。
            let level = (rgb[0] * 0.21 + rgb[1] * 0.72 + rgb[2] * 0.07) / 255.0;
            let backdrop_weight = 0.24 + level * 0.25;
            let [r, g, b] = rgb.map(|v| (255.0 + (v - 255.0) * backdrop_weight).round() as u8);
            blurred.push(Color32::from_rgb(r, g, b));
        }
    }
    egui::ColorImage { size: [width, height], pixels: blurred }
}

#[derive(Clone)]
struct Backdrop {
    texture: egui::TextureHandle,
    rect: Rect,
}

#[cfg(any(windows, test))]
pub(crate) fn set_backdrop(ctx: &egui::Context, image: Option<(Rect, egui::ColorImage)>) {
    let id = egui::Id::new(("glass-backdrop", ctx.viewport_id()));
    let Some((rect, image)) = image else {
        ctx.data_mut(|data| data.remove::<Backdrop>(id));
        return;
    };
    let old = ctx.data(|data| data.get_temp::<Backdrop>(id));
    let texture = if let Some(mut backdrop) = old {
        backdrop.texture.set(image, egui::TextureOptions::LINEAR);
        backdrop.texture
    } else {
        ctx.load_texture("glass-backdrop", image, egui::TextureOptions::LINEAR)
    };
    ctx.data_mut(|data| data.insert_temp(id, Backdrop { texture, rect }));
}

pub(crate) fn surface(painter: &Painter, shape: Rounded, light: bool) {
    if !light {
        shape.fill(painter, Color32::BLACK);
        return;
    }
    let ctx = painter.ctx();
    let first = mark(painter);
    let backdrop = ctx.data(|data| data.get_temp::<Backdrop>(egui::Id::new(("glass-backdrop", ctx.viewport_id()))));
    if let Some(backdrop) = backdrop.filter(|b| b.rect.contains_rect(shape.rect())) {
        let point = |p: Pos2| Pos2::new((p.x - backdrop.rect.left()) / backdrop.rect.width(),
            (p.y - backdrop.rect.top()) / backdrop.rect.height());
        shape.glass(painter, backdrop.texture.id(), Rect::from_min_max(point(shape.rect().min), point(shape.rect().max)));
    } else {
        let id = egui::Id::new("island-light-glass-texture");
        let texture = match ctx.data(|data| data.get_temp::<egui::TextureHandle>(id)) {
            Some(texture) => texture,
            None => {
                let texture = ctx.load_texture("island-light-glass", light_surface_image(), egui::TextureOptions::LINEAR);
                ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
                texture
            }
        };
        shape.glass(painter, texture.id(), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)));
    }
    // 白色高光属于材质，跳过前景的深色配色转换。
    keep_palette(painter, first);
}

pub(crate) const INK: Color32 = Color32::from_rgb(17, 24, 39);
pub(crate) const MUTED: Color32 = Color32::from_rgb(51, 65, 85);
pub(crate) const CONTROL_HIGHLIGHT: Color32 = Color32::WHITE;
pub(crate) const CONTROL_SELECTED: Color32 = Color32::from_rgb(25, 81, 161);

pub(crate) fn control_ink(light: bool, selected: bool) -> Color32 {
    if light && !selected { INK } else { Color32::WHITE }
}

/// A secondary or selected control on light glass, with its own highlight and
/// a short shadow so its white surface stands out against bright wallpaper.
pub(crate) fn light_capsule<'a>(
    c: &paint::Card<'a>, [x, y, w, h]: [f32; 4], id: &str, selected: Option<Color32>,
) -> (egui::Response, paint::Card<'a>) {
    let response = c.hit(x, y, w, h, id);
    let (hover, press) = c.hover_t(&response);
    let body = c.scaled(x + w * 0.5, y + h * 0.5, 1.0 - 0.05 * press);
    let first = mark(&c.painter);
    let shadow = (22.0 + 10.0 * hover).round() as u8;
    let fill = selected.map_or(Color32::WHITE, |accent| paint::mix(accent, Color32::WHITE, hover * 0.10));
    body.fill(x, y + 1.4, w, h, h * 0.5, Color32::from_black_alpha(shadow));
    body.fill(x, y, w, h, h * 0.5, fill);
    body.stroke(x, y, w, h, h * 0.5, 1.0, CONTROL_HIGHLIGHT);
    if response.has_focus() && !response.hovered() {
        body.seg((x + w * 0.4, y + h - 3.0), (x + w * 0.6, y + h - 3.0), 1.0, control_ink(true, selected.is_some()));
    }
    keep_palette(&c.painter, first);
    (response, body)
}

pub(crate) fn secondary_capsule<'a>(
    c: &paint::Card<'a>, bounds: [f32; 4], id: &str, light: bool,
) -> (egui::Response, paint::Card<'a>) {
    if light {
        light_capsule(c, bounds, id, None)
    } else {
        let [x, y, w, h] = bounds;
        paint::capsule(c, x, y, w, h, id, crate::theme::BTN, crate::theme::BTN_HOVER)
    }
}

pub(crate) fn divider_ink(light: bool) -> Color32 {
    if light { Color32::from_rgba_unmultiplied(51, 65, 85, 44) } else { crate::theme::TRACK }
}

pub(crate) fn divider(c: &paint::Card, light: bool, from: (f32, f32), to: (f32, f32)) {
    let first = light.then(|| mark(&c.painter));
    c.seg(from, to, 1.0, divider_ink(light));
    if let Some(first) = first { keep_palette(&c.painter, first); }
}

fn recolor(color: &mut Color32, text: bool) {
    if *color == Color32::PLACEHOLDER || color.a() == 0 {
        return;
    }
    let [r, g, b, alpha] = color.to_srgba_unmultiplied();
    let high = r.max(g).max(b);
    let low = r.min(g).min(b);
    let neutral = high - low < 28;
    // 透明黑是投影，保留原本的深色。
    if high <= 4 && alpha < 90 {
        return;
    }
    let rgb = Color32::from_rgb(r, g, b);
    let mapped = if neutral && (text || high >= 104) {
        paint::mix(INK, MUTED, (255 - high) as f32 / 150.0)
    } else if neutral {
        paint::mix(Color32::from_rgb(245, 248, 255), Color32::from_rgb(218, 230, 248), high as f32 / 104.0)
    } else if text {
        paint::mix(rgb, INK, 0.78)
    } else if high < 145 {
        paint::mix(rgb, Color32::from_rgb(240, 248, 255), 0.62)
    } else {
        rgb
    };
    *color = Color32::from_rgba_unmultiplied(mapped.r(), mapped.g(), mapped.b(), alpha);
}

fn light_shape(shape: &mut Shape) {
    use egui::epaint::shape_transform::adjust_colors;
    match shape {
        Shape::Vec(children) => children.iter_mut().for_each(light_shape),
        // 保留专辑封面、图片和玻璃材质本身的颜色。
        Shape::Rect(rect) if rect.brush.is_some() => {}
        Shape::Mesh(mesh) if crate::skin_paint::is_monochrome(mesh.texture_id) => {
            adjust_colors(shape, |color| recolor(color, true));
        }
        Shape::Mesh(mesh) if mesh.texture_id != egui::TextureId::default() => {}
        Shape::Text(_) => adjust_colors(shape, |color| recolor(color, true)),
        _ => adjust_colors(shape, |color| recolor(color, false)),
    }
}

/// 只转换岛体这次绘制添加的图形，设置页和其他窗口保留独立主题。
pub(crate) fn mark(painter: &Painter) -> usize {
    painter.ctx().graphics_mut(|graphics| graphics.entry(painter.layer_id()).next_idx().0)
}

/// 保留显式指定的图形配色。
pub(crate) fn keep_palette(painter: &Painter, first: usize) {
    let range = first..mark(painter);
    let id = egui::Id::new(("island-native-palette", painter.layer_id()));
    painter.ctx().data_mut(|data| data.get_temp_mut_or_default::<Vec<std::ops::Range<usize>>>(id).push(range));
}

pub(crate) fn take_palette_ranges(painter: &Painter) -> Vec<std::ops::Range<usize>> {
    let id = egui::Id::new(("island-native-palette", painter.layer_id()));
    painter.ctx().data_mut(|data| data.remove_temp::<Vec<std::ops::Range<usize>>>(id)).unwrap_or_default()
}

pub(crate) fn apply_light(painter: &Painter, first: usize, excluded: &[(usize, usize)]) {
    let native = take_palette_ranges(painter);
    painter.ctx().graphics_mut(|graphics| {
        let list = graphics.entry(painter.layer_id());
        for index in first..list.next_idx().0 {
            if !native.iter().any(|range| range.contains(&index)) && !excluded.iter().any(|&(a,b)| (a..b).contains(&index)) {
                list.mutate_shape(egui::layers::ShapeIdx(index), |clipped| light_shape(&mut clipped.shape));
            }
        }
    });
}

/// 毛玻璃皮肤的着色量（叠在系统模糊之上的深色玻璃底，0..1）。
///
/// - 开启系统模糊：背景已被抹平，按对比度刚好够白字 4.8:1 即可，再留 0.03 余量
///   应对采样点之外的亮斑；最浅 0.20，深色桌面上尽量通透。
/// - 没有模糊：背后细节仍然清楚，需要更深的烟熏（不低于 0.62）才压得住纹理。
/// - 还没有采样结果时按中等亮度背景估计，宁可略深也不让文字发虚。
pub fn glass_tint(backdrop: Option<[f32; 3]>, blurred: bool) -> f32 {
    // 按真实的玻璃底色（不是纯黑）计算，白字刚好达到目标对比度。
    let ground = [GROUND_DARK.0 as f32 / 255.0, GROUND_DARK.1 as f32 / 255.0, GROUND_DARK.2 as f32 / 255.0];
    let need = tint_for_contrast(backdrop.unwrap_or([0.6; 3]), [1.0; 3], ground, 0.0, 0.95, TEXT_CONTRAST);
    if blurred {
        (need + 0.03).clamp(0.20, 0.95)
    } else {
        (need + 0.20).clamp(SMOKED_OPACITY - 0.20, 0.90)
    }
}

/// 浅色毛玻璃的着色量（叠在模糊之上的白色玻璃底，0..1），让深色文字达到 4.8:1。
/// 深色背景对深色文字最不利，所以没有采样时按偏暗的背景估计。
pub fn light_tint(backdrop: Option<[f32; 3]>, blurred: bool) -> f32 {
    let ink = [22.0 / 255.0, 22.0 / 255.0, 26.0 / 255.0];
    let need = tint_for_contrast(backdrop.unwrap_or([0.35; 3]), ink, LIGHT_GROUND, 0.0, 0.95, TEXT_CONTRAST);
    if blurred { (need + 0.03).clamp(0.12, 0.95) } else { (need + 0.20).clamp(0.72, 0.96) }
}

/// 浅色玻璃的底色。
pub const LIGHT_GROUND: [f32; 3] = [248.0 / 255.0, 248.0 / 255.0, 250.0 / 255.0];

/// 把岛体四周的若干屏幕采样合成一个「背景色」：取平均色与最亮样本的中点，
/// 偏向亮处，因为白字最怕的是亮背景。
pub fn backdrop_estimate(samples: &[[f32; 3]]) -> Option<[f32; 3]> {
    if samples.is_empty() {
        return None;
    }
    let n = samples.len() as f32;
    let mean = samples.iter().fold([0.0f32; 3], |a, c| [a[0] + c[0] / n, a[1] + c[1] / n, a[2] + c[2] / n]);
    let bright = samples
        .iter()
        .copied()
        .max_by(|a, b| rel_luminance(*a).total_cmp(&rel_luminance(*b)))?;
    Some(mix3(mean, bright, 0.5))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_glass_rim_stays_white_after_foreground_recoloring() {
        let ctx = egui::Context::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            let first = mark(&painter);
            let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(120.0, 32.0));
            surface(&painter, Rounded::capsule(rect), true);
            apply_light(&painter, first, &[]);
        });
        let rim = out.shapes.iter().find_map(|item| match &item.shape {
            Shape::Rect(rect) if !rect.stroke.is_empty() => Some(rect.stroke.color.to_srgba_unmultiplied()),
            _ => None,
        });
        assert_eq!(rim, Some([255, 255, 255, 245]));
    }


    #[test]
    fn light_glass_softens_the_backdrop_and_keeps_text_readable() {
        let pixels: Vec<_> = (0..63).map(|i| if i % 21 < 10 { [22, 30, 42] } else { [112, 190, 255] }).collect();
        let image = blur_backdrop([21, 3], &pixels, BACKDROP_SIGMA / 4.0);
        assert!(image.pixels[10].b() > image.pixels[0].b());
        assert!(image.pixels[10].b() < image.pixels[20].b());
        assert!(image.pixels[10].g().abs_diff(image.pixels[9].g()) < 18);
        assert!(image.pixels[20].g().abs_diff(image.pixels[0].g()) >= 25);
        for mut ink in [Color32::WHITE, crate::theme::GRAY, crate::theme::CYAN, crate::theme::GREEN] {
            recolor(&mut ink, true);
            let foreground = [ink.r(), ink.g(), ink.b()].map(|x| f32::from(x) / 255.0);
            for sample in &image.pixels {
                let background = [sample.r(), sample.g(), sample.b()].map(|x| f32::from(x) / 255.0);
                assert!(contrast(foreground, background) >= 4.5, "{ink:?} over {sample:?}");
            }
        }
    }

    #[test]
    fn glass_tint_keeps_white_text_readable_on_any_backdrop() {
        for b in [[1.0; 3], [0.95, 0.9, 0.7], [0.5; 3], [0.05; 3]] {
            for blurred in [true, false] {
                let t = glass_tint(Some(b), blurred);
                let ground = [GROUND_DARK.0 as f32 / 255.0, GROUND_DARK.1 as f32 / 255.0, GROUND_DARK.2 as f32 / 255.0];
                assert!(contrast(mix3(b, ground, t), [1.0; 3]) >= 4.5, "{b:?} {blurred} {t}");
            }
        }
        // 深色背景更通透，浅色背景更深；无模糊始终比有模糊更深
        assert!(glass_tint(Some([0.05; 3]), true) < glass_tint(Some([1.0; 3]), true));
        assert!(glass_tint(Some([0.05; 3]), false) > glass_tint(Some([0.05; 3]), true));
        assert!(glass_tint(None, true) >= 0.20);
    }

    #[test]
    fn light_glass_keeps_dark_text_readable() {
        let ink = [22.0 / 255.0, 22.0 / 255.0, 26.0 / 255.0];
        for b in [[0.0; 3], [0.1, 0.2, 0.5], [0.5; 3], [1.0; 3]] {
            for blurred in [true, false] {
                let t = light_tint(Some(b), blurred);
                assert!(contrast(mix3(b, LIGHT_GROUND, t), ink) >= 4.5, "{b:?} {blurred} {t}");
            }
        }
        assert!(light_tint(Some([1.0; 3]), true) < light_tint(Some([0.0; 3]), true), "亮背景更通透");
    }

    #[test]
    fn backdrop_estimate_leans_toward_bright_samples() {
        assert_eq!(backdrop_estimate(&[]), None);
        let e = backdrop_estimate(&[[0.0; 3], [0.0; 3], [0.0; 3], [1.0; 3]]).unwrap();
        assert!((e[0] - 0.625).abs() < 1e-5, "{e:?}");
        assert_eq!(backdrop_estimate(&[[0.2, 0.4, 0.6]]), Some([0.2, 0.4, 0.6]));
    }

    #[test]
    fn contrast_matches_wcag() {
        assert!((contrast([1.0; 3], [0.0; 3]) - 21.0).abs() < 0.01);
        assert!((contrast([0.5; 3], [0.5; 3]) - 1.0).abs() < 1e-5);
        let g = 0x76 as f32 / 255.0;
        assert!((contrast([g; 3], [1.0; 3]) - 4.54).abs() < 0.02);
    }

    #[test]
    fn tint_stops_where_text_becomes_readable() {
        let orange = [0.98, 0.70, 0.45];
        let t = tint_for_contrast(orange, [1.0; 3], [0.0; 3], TINT_MIN, TINT_MAX, 4.5);
        assert!(t > TINT_MIN && t < TINT_MAX, "{t}");
        assert!(contrast(mix3(orange, [0.0; 3], t), [1.0; 3]) >= 4.5);
        assert!(contrast(mix3(orange, [0.0; 3], t - 0.02), [1.0; 3]) < 4.5);
        // 深色背景：最小着色就够
        assert_eq!(tint_for_contrast([0.10, 0.10, 0.12], [1.0; 3], [0.0; 3], TINT_MIN, TINT_MAX, 4.5), TINT_MIN);
        // 白底：着色抬到刚够 4.8:1，而不是无脑拉满
        let white = dark_tint(Some([1.0; 3]));
        assert!(white > TINT_MIN && white < TINT_MAX, "{white}");
        assert!(contrast(mix3([1.0; 3], [0.0; 3], white), [1.0; 3]) >= TEXT_CONTRAST);
    }

    #[test]
    fn dark_tint_falls_back_to_oyenza_minimum() {
        assert_eq!(dark_tint(None), TINT_MIN);
        // 深色页面按最小着色，浅色页面抬到能读
        assert!(dark_tint(Some([0.12, 0.12, 0.14])) < dark_tint(Some([0.9, 0.9, 0.9])));
    }

    #[test]
    fn busy_backdrops_get_deeper_shadows() {
        assert!(shadow_alpha(0.0, 0.0) < shadow_alpha(0.1, 0.0));
        assert!(shadow_alpha(0.0, 0.0) < shadow_alpha(0.0, 1.0));
        assert_eq!(shadow_alpha(5.0, 1.0), 0.34);
        assert!((shadow_alpha(0.0, 0.0) - 0.12).abs() < 1e-6);
    }

    #[test]
    fn thickness_and_rim_follow_oyenza() {
        assert_eq!(thickness(37.0), 0.0);
        assert_eq!(thickness(220.0), 1.0);
        assert!(thickness(140.0) > 0.5 && thickness(140.0) < 0.6);
        assert!((rim_alpha() - 0.24).abs() < 1e-6);
    }
}
