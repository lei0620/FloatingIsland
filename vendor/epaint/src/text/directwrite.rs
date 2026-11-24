//! Local Windows patch: grid-fit real font faces with DirectWrite, then retain egui's
//! existing grayscale atlas, layout, caches and renderer. No synthetic font weight.
use ab_glyph::{Font, FontArc, GlyphId};
use std::{
    cell::RefCell,
    collections::HashMap,
    mem::ManuallyDrop,
    sync::atomic::{AtomicU64, Ordering},
};
use windows::{
    core::{IUnknown, Interface},
    Win32::{Foundation::BOOL, Graphics::DirectWrite::*},
};

static NATIVE: AtomicU64 = AtomicU64::new(0);
static FALLBACK: AtomicU64 = AtomicU64::new(0);
pub(super) fn stats() -> (u64, u64) {
    (
        NATIVE.load(Ordering::Relaxed),
        FALLBACK.load(Ordering::Relaxed),
    )
}

pub(super) struct Bitmap {
    pub width: usize,
    pub height: usize,
    pub left: i32,
    pub top: i32,
    pub alpha: Vec<u8>,
    pub rgb: Vec<[u8;3]>,
}
struct Cache {
    base: IDWriteFactory,
    analysis: IDWriteFactory2,
    loader: IDWriteInMemoryFontFileLoader,
    bgr:bool,
    // Retain the FontArc so source pointers used as keys cannot be recycled.
    faces: HashMap<(usize, usize, u32), (FontArc, IDWriteFontFace)>,
}
impl Cache {
    fn new() -> windows::core::Result<Self> {
        unsafe {
            let factory: IDWriteFactory5 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_ISOLATED)?;
            let base: IDWriteFactory = factory.cast()?;
            let analysis: IDWriteFactory2 = factory.cast()?;
            let loader = factory.CreateInMemoryFontFileLoader()?;
            base.RegisterFontFileLoader(&loader)?;
            let bgr=base.CreateRenderingParams().is_ok_and(|p|p.GetPixelGeometry()==DWRITE_PIXEL_GEOMETRY_BGR);
            Ok(Self {
                base,
                analysis,
                loader,
                bgr,
                faces: HashMap::new(),
            })
        }
    }
    fn face(&mut self, font: &FontArc, index: u32) -> windows::core::Result<IDWriteFontFace> {
        unsafe {
            let bytes = font.font_data();
            let key = (bytes.as_ptr() as usize, bytes.len(), index);
            if let Some((_, face)) = self.faces.get(&key) {
                return Ok(face.clone());
            }
            // With no owner object DirectWrite copies and owns this bounded font buffer.
            let file = self.loader.CreateInMemoryFontFileReference(
                &self.base,
                bytes.as_ptr().cast(),
                u32::try_from(bytes.len()).map_err(|_| windows::core::Error::from_win32())?,
                None::<&IUnknown>,
            )?;
            let (mut supported, mut file_type, mut face_type, mut count) = (
                BOOL(0),
                DWRITE_FONT_FILE_TYPE_UNKNOWN,
                DWRITE_FONT_FACE_TYPE_UNKNOWN,
                0,
            );
            file.Analyze(
                &mut supported,
                &mut file_type,
                Some(&mut face_type),
                &mut count,
            )?;
            if !supported.as_bool() || index >= count {
                return Err(windows::core::Error::from_win32());
            }
            let face = self.base.CreateFontFace(
                face_type,
                &[Some(file)],
                index,
                DWRITE_FONT_SIMULATIONS_NONE,
            )?;
            if self.faces.len() < 32 {
                self.faces.insert(key, (font.clone(), face.clone()));
            }
            Ok(face)
        }
    }
    fn glyph(
        &mut self,
        font: &FontArc,
        index: u32,
        glyph: GlyphId,
        pixels: f32,
    ) -> windows::core::Result<Bitmap> {
        unsafe {
            let face = self.face(font, index)?;
            let em = pixels * font.units_per_em().unwrap_or(2048.0) / font.height_unscaled();
            let glyph_index = glyph.0;
            let mut run = DWRITE_GLYPH_RUN {
                fontFace: ManuallyDrop::new(Some(face)),
                fontEmSize: em,
                glyphCount: 1,
                glyphIndices: &glyph_index,
                ..Default::default()
            };
            let result = self.analysis.CreateGlyphRunAnalysis(
                &run,
                None,
                DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
                DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_GRID_FIT_MODE_ENABLED,
                DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE,
                0.0,
                0.0,
            );
            let color_result=self.analysis.CreateGlyphRunAnalysis(&run,None,
                DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_GRID_FIT_MODE_ENABLED,DWRITE_TEXT_ANTIALIAS_MODE_CLEARTYPE,0.0,0.0);
            // DWRITE_GLYPH_RUN uses ManuallyDrop; release its temporary COM reference on errors too.
            ManuallyDrop::drop(&mut run.fontFace);
            let analysis = result?;
            let bounds = analysis.GetAlphaTextureBounds(DWRITE_TEXTURE_ALIASED_1x1)?;
            let color=color_result.ok().and_then(|a|{
                let b=a.GetAlphaTextureBounds(DWRITE_TEXTURE_CLEARTYPE_3x1).ok()?;
                (b.right>b.left&&b.bottom>b.top).then_some((a,b))
            });
            // Keep the existing glyph paint bounds. LCD filtering can add an
            // outer fringe; do not let that change spacing, scrolling or the
            // bounds of grayscale glyphs used by transparent viewports.
            let width = (bounds.right - bounds.left).max(0) as usize;
            let height = (bounds.bottom - bounds.top).max(0) as usize;
            if width > 4096 || height > 4096 {
                return Err(windows::core::Error::from_win32());
            }
            let mut alpha = vec![0; width * height];
            if !alpha.is_empty() {
                analysis.CreateAlphaTexture(DWRITE_TEXTURE_ALIASED_1x1, &bounds, &mut alpha)?;
            }
            let mut rgb=Vec::new();
            if let Some((color,_))=color{
                let mut bytes=vec![0;width*height*3];
                if !bytes.is_empty()&&color.CreateAlphaTexture(DWRITE_TEXTURE_CLEARTYPE_3x1,&bounds,&mut bytes).is_ok(){
                    rgb=bytes.chunks_exact(3).map(|c|if self.bgr{[c[2],c[1],c[0]]}else{[c[0],c[1],c[2]]}).collect();
                }
            }
            Ok(Bitmap {
                width,
                height,
                left: bounds.left,
                top: bounds.top,
                alpha,
                rgb,
            })
        }
    }
}
impl Drop for Cache {
    fn drop(&mut self) {
        self.faces.clear();
        unsafe {
            let _ = self.base.UnregisterFontFileLoader(&self.loader);
        }
    }
}
thread_local! {static CACHE:RefCell<Option<Cache>>=RefCell::new(Cache::new().ok());}

pub(super) fn rasterize(font: &FontArc, index: u32, glyph: GlyphId, pixels: f32) -> Option<Bitmap> {
    // Explicit fallback is useful for visual regression comparisons, never changes OS settings.
    if std::env::var_os("DAOZAI_LEGACY_TEXT").is_some() {
        return None;
    }
    let result = CACHE.with(|cache| {
        cache
            .borrow_mut()
            .as_mut()?
            .glyph(font, index, glyph, pixels)
            .ok()
    });
    if result.is_some() {
        NATIVE.fetch_add(1, Ordering::Relaxed);
    } else {
        FALLBACK.fetch_add(1, Ordering::Relaxed);
    }
    result
}
