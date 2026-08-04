use super::font_backend::{
    create_gpu_resources, empty_glyph_region, upload_bitmap, AtlasGlyphKey, DirtyRect, FontBackend,
    GlyphRegion, ShapedGlyph, GLYPH_PADDING, INITIAL_ATLAS_SIZE, MAX_ATLAS_SIZE,
};
use lru::LruCache;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

/// Cache key for a shaped run: run text + style + subpixel bin.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ShapeCacheKey {
    text: String,
    bold: bool,
    subpixel_offset: u8,
}

/// Check if character is CJK or other wide script that shouldn't use subpixel binning.
fn is_cjk_or_wide(ch: char) -> bool {
    matches!(ch as u32,
        0x2E80..=0x2EFF |     // CJK Radicals Supplement
        0x3000..=0x303F |     // CJK Symbols and Punctuation
        0x3040..=0x309F |     // Hiragana
        0x30A0..=0x30FF |     // Katakana
        0x3100..=0x312F |     // Bopomofo
        0x3130..=0x318F |     // Hangul Compatibility Jamo
        0x3190..=0x319F |     // Kanbun
        0x31A0..=0x31BF |     // Bopomofo Extended
        0x31C0..=0x31EF |     // CJK Strokes
        0x31F0..=0x31FF |     // Katakana Phonetic Extensions
        0x3200..=0x32FF |     // Enclosed CJK Letters and Months
        0x3300..=0x33FF |     // CJK Compatibility
        0x4E00..=0x9FFF |     // CJK Unified Ideographs
        0xF900..=0xFAFF |     // CJK Compatibility Ideographs
        0x20000..=0x2A6DF    // CJK Unified Ideographs Extension B+
    )
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct GidGlyphKey {
    gid: u16,
    bold: bool,
    subpixel_offset: u8,
}

/// Raster data handed to atlas placement: whole-pixel grayscale coverage, or
/// fontdue's true 3x-horizontal subpixel RGB coverage (`width*3` bytes/row).
enum RasterInput<'a> {
    Gray(&'a [u8]),
    Subpixel(&'a [u8]),
}

fn raster_input(use_subpixel: bool, bitmap: &[u8]) -> RasterInput<'_> {
    if use_subpixel {
        RasterInput::Subpixel(bitmap)
    } else {
        RasterInput::Gray(bitmap)
    }
}

/// FreeType's default 5-tap LCD FIR filter (`FIR5 0x08 0x4D 0x56 0x4D 0x08`),
/// run along the subpixel stream to suppress color fringing. The filter spills
/// coverage one full pixel to each side, so the output is `width + 2` pixels
/// wide (RGB triplets); callers must shift `bearing_x` left by one pixel.
fn lcd_filter_expand(src: &[u8], width: usize, height: usize, weight_boost: f32) -> Vec<u8> {
    const FIR: [u32; 5] = [8, 77, 86, 77, 8];
    let out_w = width + 2;
    let mut out = vec![0u8; out_w * 3 * height];
    for y in 0..height {
        let src_row = &src[y * width * 3..(y + 1) * width * 3];
        let dst_row = &mut out[y * out_w * 3..(y + 1) * out_w * 3];
        for (i, dst) in dst_row.iter_mut().enumerate() {
            // Output subpixel i sits one pixel (3 subpixels) left of the input.
            let center = i as i32 - 3;
            let mut acc = 0u32;
            for (k, &wt) in FIR.iter().enumerate() {
                let s = center + k as i32 - 2;
                if s >= 0 && (s as usize) < width * 3 {
                    acc += wt * src_row[s as usize] as u32;
                }
            }
            let coverage = ((acc + 128) / 256) as f32 / 255.0;
            let boosted = (coverage * weight_boost).min(1.0);
            *dst = (boosted * 255.0 + 0.5) as u8;
        }
    }
    out
}

/// Owned shaping output kept after releasing the immutable font-face borrow.
type ShapedGlyphData = (u16, u32, f32, f32, f32);

/// Parsed HarfRust shaping data backed by immutable owned font bytes.
///
/// `ShaperData` caches the expensive font-derived tables without borrowing the
/// source. A lightweight `FontRef` is recreated for each shape call, avoiding
/// the self-referential forged `'static` lifetime the old RustyBuzz wrapper
/// required.
struct ShapingFont {
    data: Arc<Vec<u8>>,
    shaper_data: harfrust::ShaperData,
    has_gsub: bool,
}

impl ShapingFont {
    fn new(data: Arc<Vec<u8>>) -> Option<Self> {
        let font = harfrust::FontRef::from_index(data.as_slice(), 0).ok()?;
        let has_gsub = font.table_data(harfrust::Tag::new(b"GSUB")).is_some();
        let shaper_data = harfrust::ShaperData::new(&font);
        Some(Self {
            data,
            shaper_data,
            has_gsub,
        })
    }

    fn shape(&self, mut buffer: harfrust::UnicodeBuffer) -> (harfrust::GlyphBuffer, f32) {
        // `new` parsed these same immutable bytes successfully. Rebuilding the
        // borrowed view cannot fail unless that invariant is broken.
        let font = harfrust::FontRef::from_index(self.data.as_slice(), 0)
            .expect("validated immutable shaping font");
        let shaper = self.shaper_data.shaper(&font).build();
        let units_per_em = shaper.units_per_em() as f32;
        // RustyBuzz's free `shape` function guessed direction/script/language
        // internally. HarfRust exposes shaping as a method and requires the
        // caller to establish those segment properties before plan creation.
        buffer.guess_segment_properties();
        (shaper.shape(buffer, &[]), units_per_em)
    }
}

pub struct FontdueAtlas {
    font_regular: fontdue::Font,
    font_bold: Option<fontdue::Font>,
    fallback_fonts: Vec<fontdue::Font>,
    font_size_px: f32,
    font_weight: f32,
    bitmap: Vec<u8>,
    width: u32,
    height: u32,
    shelf_x: u32,
    shelf_y: u32,
    shelf_height: u32,
    ascii_cache: HashMap<AtlasGlyphKey, GlyphRegion>,
    unicode_cache: LruCache<AtlasGlyphKey, GlyphRegion>,
    /// 整形字形(gid)→图集区域缓存。用 LRU 而非裸 HashMap,避免大量不同
    /// 连字/PUA 图标长期累积导致无界增长。淘汰项遗留的货架死空间会在图集填满
    /// 触发 reset 重建时一并回收(见 needs_compaction)。
    gid_cache: LruCache<GidGlyphKey, GlyphRegion>,
    /// Cache of fully shaped runs, keyed by text+style. Holds atlas UVs, so it is
    /// cleared whenever the atlas grows or resets (regions would otherwise be stale).
    shape_cache: LruCache<ShapeCacheKey, Arc<Vec<ShapedGlyph>>>,
    /// Incremented on every atlas grow/reset; used to detect a grow that happens
    /// mid-run so we don't cache regions captured against the old atlas size.
    atlas_generation: u64,
    /// Set when a glyph cannot be placed even after growth (atlas at max size).
    /// The next ensure_uploaded compacts the atlas by rebuilding from scratch.
    needs_compaction: bool,
    dirty_rects: Vec<DirtyRect>,
    needs_full_upload: bool,
    needs_rebind: bool,
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
    cached_ascent: f32,
    cached_descent: f32,
    cached_advance_width: f32,
    /// HarfRust's cached shaping tables. The borrowed `FontRef` itself is cheap
    /// to reconstruct; the expensive derived state lives in [`ShapingFont`].
    shape_regular: Option<ShapingFont>,
    shape_bold: Option<ShapingFont>,
    shaping_enabled: bool,
    /// Reusable HarfRust buffer to avoid allocating one per shape miss. Held
    /// in an Option so we can `take` it across the consume-by-shape boundary.
    shape_buffer: Option<harfrust::UnicodeBuffer>,
    // Subpixel rendering
    subpixel_rendering: bool,
}

impl FontdueAtlas {
    // This constructor intentionally mirrors the independent GPU, font-family, and
    // rendering settings needed to create an atlas; grouping them would only move
    // the same one-time initialization fields into an otherwise unused wrapper.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        font_data_regular: &[u8],
        font_data_bold: Option<&[u8]>,
        fallback_font_data: &[Vec<u8>],
        font_size_px: f32,
        font_weight: f32,
        subpixel_rendering: bool,
    ) -> Self {
        let settings = fontdue::FontSettings {
            ..Default::default()
        };
        let font_regular = fontdue::Font::from_bytes(font_data_regular, settings)
            .expect("failed to load regular font");
        let font_bold = font_data_bold.map(|data| {
            fontdue::Font::from_bytes(data, settings).expect("failed to load bold font")
        });
        let fallback_fonts: Vec<fontdue::Font> = fallback_font_data
            .iter()
            .filter_map(|data| fontdue::Font::from_bytes(data.as_slice(), settings).ok())
            .collect();

        let width = INITIAL_ATLAS_SIZE;
        let height = INITIAL_ATLAS_SIZE;
        let bitmap = vec![0u8; (width * height * 4) as usize];

        let (texture, view, sampler) = create_gpu_resources(device, width, height);
        upload_bitmap(queue, &texture, &bitmap, width, height);

        let (cached_ascent, cached_descent) = Self::compute_metrics(&font_regular, font_size_px);
        let cached_advance_width = font_regular.metrics('0', font_size_px).advance_width;

        // Parse and cache HarfRust's font-derived shaping data once. Preserve
        // the prior behavior of enabling the ligature path only for GSUB fonts.
        let font_data_arc = Arc::new(font_data_regular.to_vec());
        let font_data_bold_arc = font_data_bold.map(|d| Arc::new(d.to_vec()));
        let shape_regular = ShapingFont::new(font_data_arc);
        let shape_bold = font_data_bold_arc.and_then(ShapingFont::new);
        let shaping_enabled = shape_regular.as_ref().is_some_and(|font| font.has_gsub);

        let mut atlas = FontdueAtlas {
            shape_regular,
            shape_bold,
            shape_buffer: Some(harfrust::UnicodeBuffer::new()),
            font_regular,
            font_bold,
            fallback_fonts,
            font_size_px,
            font_weight,
            bitmap,
            width,
            height,
            shelf_x: 0,
            shelf_y: 0,
            shelf_height: 0,
            ascii_cache: HashMap::with_capacity(1024),
            unicode_cache: LruCache::new(NonZeroUsize::new(8192).unwrap()),
            gid_cache: LruCache::new(NonZeroUsize::new(8192).unwrap()),
            shape_cache: LruCache::new(NonZeroUsize::new(2048).unwrap()),
            atlas_generation: 0,
            needs_compaction: false,
            dirty_rects: Vec::new(),
            needs_full_upload: false,
            needs_rebind: false,
            texture,
            view,
            sampler,
            cached_ascent,
            cached_descent,
            cached_advance_width,
            shaping_enabled,
            subpixel_rendering,
        };

        atlas.prepopulate_ascii();
        atlas
    }

    fn compute_metrics(font: &fontdue::Font, font_size_px: f32) -> (f32, f32) {
        if let Some(lm) = font.horizontal_line_metrics(font_size_px) {
            (lm.ascent, lm.descent)
        } else {
            (font_size_px * 0.8, -(font_size_px * 0.2))
        }
    }

    fn prepopulate_ascii(&mut self) {
        // Single subpixel bin: positioning is done via fractional cell origin +
        // linear sampling at render time, so per-bin glyph copies are not needed.
        for ch in ' '..='~' {
            self.get_or_rasterize(ch, false, 0);
        }
        for ch in ' '..='~' {
            self.get_or_rasterize(ch, true, 0);
        }
    }

    fn allocate_shelf(&mut self, w: u32, h: u32) -> bool {
        if self.shelf_x + w <= self.width && self.shelf_y + h.max(self.shelf_height) <= self.height
        {
            self.shelf_x += w;
            if h > self.shelf_height {
                self.shelf_height = h;
            }
            return true;
        }

        let new_shelf_y = self.shelf_y + self.shelf_height;
        if w <= self.width && new_shelf_y + h <= self.height {
            self.shelf_y = new_shelf_y;
            self.shelf_x = w;
            self.shelf_height = h;
            return true;
        }

        false
    }

    fn grow(&mut self) -> bool {
        let new_size = self.width * 2;
        if new_size > MAX_ATLAS_SIZE {
            return false;
        }

        let mut new_bitmap = vec![0u8; (new_size * new_size * 4) as usize];
        for y in 0..self.height {
            let src_start = (y * self.width * 4) as usize;
            let src_end = src_start + (self.width * 4) as usize;
            let dst_start = (y * new_size * 4) as usize;
            new_bitmap[dst_start..dst_start + (self.width * 4) as usize]
                .copy_from_slice(&self.bitmap[src_start..src_end]);
        }

        self.bitmap = new_bitmap;
        let scale_x = self.width as f32 / new_size as f32;
        let scale_y = self.height as f32 / new_size as f32;
        for region in self.ascii_cache.values_mut() {
            region.u0 *= scale_x;
            region.u1 *= scale_x;
            region.v0 *= scale_y;
            region.v1 *= scale_y;
        }
        for (_, region) in self.unicode_cache.iter_mut() {
            region.u0 *= scale_x;
            region.u1 *= scale_x;
            region.v0 *= scale_y;
            region.v1 *= scale_y;
        }
        // gid_cache holds shaped-glyph UVs and must be rescaled too, otherwise
        // ligatures/shaped runs sample the wrong atlas region after a grow.
        for (_, region) in self.gid_cache.iter_mut() {
            region.u0 *= scale_x;
            region.u1 *= scale_x;
            region.v0 *= scale_y;
            region.v1 *= scale_y;
        }

        // Shaped-run cache stores UVs; drop it and bump the generation so any
        // in-flight run does not cache regions captured against the old size.
        self.shape_cache.clear();
        self.atlas_generation = self.atlas_generation.wrapping_add(1);

        self.width = new_size;
        self.height = new_size;
        self.needs_full_upload = true;
        self.dirty_rects.clear();
        true
    }

    /// Insert region into the appropriate cache (ASCII or Unicode)
    fn cache_insert(&mut self, key: AtlasGlyphKey, region: GlyphRegion) {
        if (key.ch as u32) < 128 {
            self.ascii_cache.insert(key, region);
        } else {
            self.unicode_cache.put(key, region);
        }
    }

    fn rasterize_and_place(
        &mut self,
        metrics: &fontdue::Metrics,
        input: RasterInput<'_>,
        bold: bool,
        key: AtlasGlyphKey,
    ) -> GlyphRegion {
        let weight_boost = if bold { 1.0 } else { self.font_weight };
        let region = self.place_glyph(metrics, input, weight_boost, key.subpixel_offset);
        self.cache_insert(key, region);
        region
    }

    /// Shared placement path for character and shaped-gid glyphs: filter (for
    /// subpixel input), allocate atlas space, write raw coverage pixels and
    /// compute the glyph region. The atlas stores *unboosted* linear coverage;
    /// perceptual weight is restored by the luminance-corrected blend in the
    /// fragment shader.
    fn place_glyph(
        &mut self,
        metrics: &fontdue::Metrics,
        input: RasterInput<'_>,
        weight_boost: f32,
        subpixel_offset: u8,
    ) -> GlyphRegion {
        if metrics.width == 0 || metrics.height == 0 {
            return empty_glyph_region();
        }

        let glyph_h = metrics.height as u32;
        // The LCD FIR widens subpixel glyphs by one pixel on each side.
        let (glyph_w, filtered): (u32, Option<Vec<u8>>) = match input {
            RasterInput::Gray(_) => (metrics.width as u32, None),
            RasterInput::Subpixel(src) => (
                metrics.width as u32 + 2,
                Some(lcd_filter_expand(
                    src,
                    metrics.width,
                    metrics.height,
                    weight_boost,
                )),
            ),
        };

        let padded_w = glyph_w + GLYPH_PADDING * 2;
        let padded_h = glyph_h + GLYPH_PADDING * 2;

        if !self.allocate_shelf(padded_w, padded_h) {
            if !self.grow() {
                // Atlas is at max size and full. Request a compaction (rebuild) on
                // the start of the next frame so churned/evicted glyphs reclaim
                // their shelf space. Bump the generation immediately: the current
                // CPU instance build must be discarded before a callback can paint
                // it against the subsequently rebuilt atlas.
                self.needs_compaction = true;
                self.shape_cache.clear();
                self.atlas_generation = self.atlas_generation.wrapping_add(1);
                return empty_glyph_region();
            }
            if !self.allocate_shelf(padded_w, padded_h) {
                return empty_glyph_region();
            }
        }

        let atlas_x = self.shelf_x - padded_w;
        let atlas_y = self.shelf_y;
        let bx = atlas_x + GLYPH_PADDING;
        let by = atlas_y + GLYPH_PADDING;

        match (&input, &filtered) {
            (_, Some(rgb)) => {
                for gy in 0..glyph_h {
                    for gx in 0..glyph_w {
                        let dst_x = bx + gx;
                        let dst_y = by + gy;
                        if dst_x < self.width && dst_y < self.height {
                            let src_idx = ((gy * glyph_w + gx) * 3) as usize;
                            let r = rgb[src_idx];
                            let g = rgb[src_idx + 1];
                            let b = rgb[src_idx + 2];
                            let pixel = [r, g, b, r.max(g).max(b)];
                            let dst_idx = ((dst_y * self.width + dst_x) * 4) as usize;
                            self.bitmap[dst_idx..dst_idx + 4].copy_from_slice(&pixel);
                        }
                    }
                }
            }
            (RasterInput::Gray(src), _) => {
                for gy in 0..glyph_h {
                    for gx in 0..glyph_w {
                        let src_idx = (gy * glyph_w + gx) as usize;
                        let dst_x = bx + gx;
                        let dst_y = by + gy;
                        if dst_x < self.width && dst_y < self.height {
                            let coverage = src[src_idx] as f32 / 255.0;
                            let boosted = (coverage * weight_boost).min(1.0);
                            let a8 = (boosted * 255.0 + 0.5) as u8;
                            let pixel = [a8, a8, a8, a8];
                            let dst_idx = ((dst_y * self.width + dst_x) * 4) as usize;
                            self.bitmap[dst_idx..dst_idx + 4].copy_from_slice(&pixel);
                        }
                    }
                }
            }
            (RasterInput::Subpixel(_), None) => unreachable!("subpixel input is always filtered"),
        }

        // Record dirty rectangle (with padding)
        self.dirty_rects
            .push(DirtyRect::new(atlas_x, atlas_y, padded_w, padded_h));

        let subpixel_shift = match subpixel_offset {
            1 => 0.25,
            2 => 0.5,
            3 => 0.75,
            _ => 0.0,
        };
        // Subpixel glyphs were widened one pixel to the left by the FIR.
        let lcd_expand = if filtered.is_some() { 1.0 } else { 0.0 };
        let bearing_x = metrics.xmin as f32 - lcd_expand + subpixel_shift;
        let bearing_y = self.cached_ascent - (metrics.ymin as f32 + metrics.height as f32);

        GlyphRegion {
            u0: bx as f32 / self.width as f32,
            v0: by as f32 / self.height as f32,
            u1: (bx + glyph_w) as f32 / self.width as f32,
            v1: (by + glyph_h) as f32 / self.height as f32,
            width_px: glyph_w as f32,
            height_px: glyph_h as f32,
            bearing_x,
            bearing_y,
        }
    }

    fn rasterize_gid(&mut self, gid: u16, bold: bool, subpixel_offset: u8) -> GlyphRegion {
        let key = GidGlyphKey {
            gid,
            bold,
            subpixel_offset,
        };
        if let Some(&region) = self.gid_cache.get(&key) {
            return region;
        }

        let font = if bold {
            self.font_bold.as_ref().unwrap_or(&self.font_regular)
        } else {
            &self.font_regular
        };

        let use_subpixel = self.subpixel_rendering;
        let (metrics, glyph_bitmap) = if use_subpixel {
            font.rasterize_indexed_subpixel(gid, self.font_size_px)
        } else {
            font.rasterize_indexed(gid, self.font_size_px)
        };

        if glyph_bitmap.is_empty() || metrics.width == 0 || metrics.height == 0 {
            let region = empty_glyph_region();
            self.gid_cache.put(key, region);
            return region;
        }

        let weight_boost = if bold { 1.0 } else { self.font_weight };
        let input = if use_subpixel {
            RasterInput::Subpixel(&glyph_bitmap)
        } else {
            RasterInput::Gray(&glyph_bitmap)
        };
        let region = self.place_glyph(&metrics, input, weight_boost, subpixel_offset);
        self.gid_cache.put(key, region);
        region
    }
}

impl FontBackend for FontdueAtlas {
    fn get_or_rasterize(&mut self, ch: char, bold: bool, subpixel_offset: u8) -> GlyphRegion {
        // Force CJK characters to always use subpixel bin 0 (no subpixel variation needed)
        let effective_subpixel = if is_cjk_or_wide(ch) {
            0
        } else {
            subpixel_offset
        };
        let key = AtlasGlyphKey {
            ch,
            bold,
            subpixel_offset: effective_subpixel,
        };

        // Tier 1: ASCII permanent cache (never evicted)
        if (ch as u32) < 128 {
            if let Some(&region) = self.ascii_cache.get(&key) {
                return region;
            }
        } else {
            // Tier 2: Unicode LRU cache
            if let Some(&region) = self.unicode_cache.get(&key) {
                return region;
            }
        }

        // Try primary font first (bold or regular)
        let font = if bold {
            self.font_bold.as_ref().unwrap_or(&self.font_regular)
        } else {
            &self.font_regular
        };

        // True 3x subpixel rasterization applies to every script; the CJK
        // special-casing only concerns the (currently single-bin) positional
        // binning above.
        let use_subpixel = self.subpixel_rendering;
        let rasterize = |f: &fontdue::Font| {
            if use_subpixel {
                f.rasterize_subpixel(ch, self.font_size_px)
            } else {
                f.rasterize(ch, self.font_size_px)
            }
        };
        // Check if glyph exists in primary font
        let glyph_index = font.lookup_glyph_index(ch);
        let has_glyph = glyph_index != 0;

        // If no glyph in primary font (or we want fallback for missing chars), try fallback fonts
        if !has_glyph && ch != ' ' && !ch.is_control() {
            for fb in &self.fallback_fonts {
                let fb_glyph_index = fb.lookup_glyph_index(ch);
                if fb_glyph_index != 0 {
                    let (fb_metrics, fb_bitmap) = rasterize(fb);
                    return self.rasterize_and_place(
                        &fb_metrics,
                        raster_input(use_subpixel, &fb_bitmap),
                        bold,
                        key,
                    );
                }
            }
            // Glyph not found in any font, use .notdef
            let (metrics, glyph_bitmap) = rasterize(font);
            return self.rasterize_and_place(
                &metrics,
                raster_input(use_subpixel, &glyph_bitmap),
                bold,
                key,
            );
        }

        // Glyph exists (or is space/control), rasterize from primary font
        let (metrics, glyph_bitmap) = rasterize(font);

        if glyph_bitmap.is_empty() || metrics.width == 0 || metrics.height == 0 {
            // Space, control chars, etc. — just return advance width with subpixel offset
            let subpixel_shift = match subpixel_offset {
                1 => 0.25,
                2 => 0.5,
                3 => 0.75,
                _ => 0.0,
            };
            let region = GlyphRegion {
                u0: 0.0,
                v0: 0.0,
                u1: 0.0,
                v1: 0.0,
                width_px: metrics.advance_width,
                height_px: 0.0,
                bearing_x: subpixel_shift,
                bearing_y: 0.0,
            };
            self.cache_insert(key, region);
            return region;
        }

        self.rasterize_and_place(
            &metrics,
            raster_input(use_subpixel, &glyph_bitmap),
            bold,
            key,
        )
    }

    fn reset(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.ascii_cache.clear();
        self.unicode_cache.clear();
        self.gid_cache.clear();
        self.shape_cache.clear();
        self.atlas_generation = self.atlas_generation.wrapping_add(1);
        // Clear before the nested ensure_uploaded below so we don't re-enter compaction.
        self.needs_compaction = false;
        self.shelf_x = 0;
        self.shelf_y = 0;
        self.shelf_height = 0;

        let w = INITIAL_ATLAS_SIZE;
        let h = INITIAL_ATLAS_SIZE;
        self.bitmap = vec![0u8; (w * h * 4) as usize];
        self.width = w;
        self.height = h;

        let (texture, view, sampler) = create_gpu_resources(device, w, h);
        self.texture = texture;
        self.view = view;
        self.sampler = sampler;

        let (asc, desc) = Self::compute_metrics(&self.font_regular, self.font_size_px);
        self.cached_ascent = asc;
        self.cached_descent = desc;
        self.cached_advance_width = self
            .font_regular
            .metrics('0', self.font_size_px)
            .advance_width;

        self.prepopulate_ascii();
        self.ensure_uploaded(device, queue);
        self.needs_rebind = true;
    }

    fn set_font_size_px(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, font_size_px: f32) {
        if (self.font_size_px - font_size_px).abs() < 0.01 {
            return;
        }
        self.font_size_px = font_size_px;
        // reset() recomputes cached metrics from font_size_px and re-rasterizes
        // the ASCII prepopulation at the new size; everything else fills lazily.
        self.reset(device, queue);
    }

    fn font_metrics(&self) -> (f32, f32, f32) {
        (
            self.cached_ascent,
            self.cached_descent,
            self.cached_advance_width,
        )
    }

    fn ensure_uploaded(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        // Atlas filled up at max size: rebuild from scratch to reclaim the shelf
        // space held by glyphs that were evicted from the logical caches. reset()
        // clears needs_compaction first, so this does not recurse.
        if self.needs_compaction {
            self.reset(device, queue);
            return;
        }

        // Check if texture needs to be recreated
        let tex_size = self.texture.size();
        if tex_size.width != self.width || tex_size.height != self.height {
            let (texture, view, sampler) = create_gpu_resources(device, self.width, self.height);
            self.texture = texture;
            self.view = view;
            self.sampler = sampler;
            self.needs_full_upload = true;
            // 纹理/view/sampler 已被替换,旧绑定组仍引用已失效的资源,必须重建。
            // grow() 在 CPU 阶段就更新了 self.width,使 callback 的 old==new 尺寸比较
            // 失效,因此重建信号必须在此显式置位,不能依赖尺寸比较。
            self.needs_rebind = true;
        }

        // Full upload when atlas was resized or texture recreated
        if self.needs_full_upload {
            upload_bitmap(queue, &self.texture, &self.bitmap, self.width, self.height);
            self.needs_full_upload = false;
            self.dirty_rects.clear();
            return;
        }

        // Incremental upload: process dirty rectangles
        if self.dirty_rects.is_empty() {
            return;
        }

        for rect in &self.dirty_rects {
            let x = rect.x;
            let y = rect.y;
            let w = rect.width.min(self.width.saturating_sub(x));
            let h = rect.height.min(self.height.saturating_sub(y));

            if w == 0 || h == 0 {
                continue;
            }

            // Extract dirty rectangle data from bitmap
            let mut rect_data = Vec::with_capacity((w * h * 4) as usize);
            for row in y..(y + h) {
                let src_start = ((row * self.width + x) * 4) as usize;
                let src_end = src_start + (w * 4) as usize;
                rect_data.extend_from_slice(&self.bitmap[src_start..src_end]);
            }

            // Upload only the dirty rectangle
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &rect_data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        }

        self.dirty_rects.clear();
    }

    fn backend_name(&self) -> &'static str {
        "fontdue"
    }

    fn gpu_resources(&self) -> (&wgpu::TextureView, &wgpu::Sampler) {
        (&self.view, &self.sampler)
    }

    fn atlas_dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn content_generation(&self) -> u64 {
        self.atlas_generation
    }

    fn take_needs_rebind(&mut self) -> bool {
        let v = self.needs_rebind;
        self.needs_rebind = false;
        v
    }

    fn supports_shaping(&self) -> bool {
        self.shaping_enabled
    }

    fn shape_run(&mut self, text: &str, bold: bool, subpixel_offset: u8) -> Arc<Vec<ShapedGlyph>> {
        if !self.shaping_enabled || text.is_empty() {
            // Fallback: per-character rasterization
            let mut glyphs = Vec::with_capacity(text.len());
            for (byte_idx, ch) in text.char_indices() {
                let region = self.get_or_rasterize(ch, bold, subpixel_offset);
                glyphs.push(ShapedGlyph {
                    cluster: byte_idx as u32,
                    x_advance: region.width_px,
                    x_offset: 0.0,
                    y_offset: 0.0,
                    region,
                });
            }
            return Arc::new(glyphs);
        }

        // Fast path: identical runs recur every frame (e.g. a static prompt line).
        // Returning the cached Arc avoids rebuilding the HarfRust shaper and
        // re-running the shaper on every dirty row. The cache is cleared whenever
        // the atlas grows/resets, so cached regions are always current.
        let cache_key = ShapeCacheKey {
            text: text.to_string(),
            bold,
            subpixel_offset,
        };
        if let Some(cached) = self.shape_cache.get(&cache_key) {
            return Arc::clone(cached);
        }

        // Snapshot generation so we can detect a grow triggered while rasterizing
        // glyphs below; if that happens, earlier regions are stale and must not be cached.
        let generation_before = self.atlas_generation;

        // Use the cached HarfRust table data prepared at construction.
        let shaping_font = if bold {
            self.shape_bold.as_ref().or(self.shape_regular.as_ref())
        } else {
            self.shape_regular.as_ref()
        };

        // 收集本次整形得到的字形信息为自有数据，随后即可释放对字体（&self）的借用，
        // 以便调用需要 &mut self 的 rasterize_gid。复用 UnicodeBuffer 避免每次
        // 缓存未命中都新分配一个。
        let shaped: Option<Vec<ShapedGlyphData>> = shaping_font.map(|font| {
            let mut buffer = self.shape_buffer.take().unwrap_or_default();
            buffer.push_str(text);

            let (glyph_buffer, units_per_em) = font.shape(buffer);
            let infos = glyph_buffer.glyph_infos();
            let positions = glyph_buffer.glyph_positions();

            let scale = self.font_size_px / units_per_em;

            let collected: Vec<_> = infos
                .iter()
                .zip(positions.iter())
                .map(|(info, pos)| {
                    (
                        info.glyph_id as u16,
                        info.cluster,
                        pos.x_advance as f32 * scale,
                        pos.x_offset as f32 * scale,
                        pos.y_offset as f32 * scale,
                    )
                })
                .collect();
            // Return the buffer to the pool for the next miss.
            self.shape_buffer = Some(glyph_buffer.clear());
            collected
        });

        let shaped = match shaped {
            Some(s) => s,
            None => {
                // 字体未能解析时退回逐字符光栅化。
                let mut glyphs = Vec::with_capacity(text.len());
                for (byte_idx, ch) in text.char_indices() {
                    let region = self.get_or_rasterize(ch, bold, subpixel_offset);
                    glyphs.push(ShapedGlyph {
                        cluster: byte_idx as u32,
                        x_advance: region.width_px,
                        x_offset: 0.0,
                        y_offset: 0.0,
                        region,
                    });
                }
                return Arc::new(glyphs);
            }
        };

        let mut glyphs = Vec::with_capacity(shaped.len());
        for (gid, cluster, x_advance, x_offset, y_offset) in shaped {
            let region = self.rasterize_gid(gid, bold, subpixel_offset);

            glyphs.push(ShapedGlyph {
                cluster,
                x_advance,
                x_offset,
                y_offset,
                region,
            });
        }

        let arc = Arc::new(glyphs);
        // Only cache when the atlas did not grow while rasterizing this run; a grow
        // rescales UVs and would leave the earlier glyphs in this vec pointing at the
        // wrong region.
        if self.atlas_generation == generation_before {
            self.shape_cache.put(cache_key, Arc::clone(&arc));
        }

        arc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcd_filter_widens_by_one_pixel_and_conserves_energy() {
        // A single fully covered pixel: three subpixel samples at 255.
        let src = [255u8, 255, 255];
        let out = lcd_filter_expand(&src, 1, 1, 1.0);
        // One pixel of FIR spill on each side: 3 pixels * 3 subpixels.
        assert_eq!(out.len(), 9);
        // The FIR weights sum to 256/256, so total coverage is preserved
        // (up to per-tap rounding).
        let sum_in: i64 = src.iter().map(|&v| v as i64).sum();
        let sum_out: i64 = out.iter().map(|&v| v as i64).sum();
        assert!((sum_in - sum_out).abs() <= 5);
        // Coverage stays centered: the middle pixel holds the peak.
        assert!(out[4] > out[3] && out[4] > out[5]);
        assert!(out[3] > out[0] && out[5] > out[8]);
        // Symmetric input must filter symmetrically.
        assert_eq!(out[3], out[5]);
        assert_eq!(out[0], out[8]);
    }

    #[test]
    fn lcd_filter_weight_boost_clamps_coverage() {
        let src = [255u8; 6]; // two fully covered pixels
        let out = lcd_filter_expand(&src, 2, 1, 2.0);
        // The interior subpixels saturate exactly at full coverage under a
        // 2x boost — the clamp keeps u8 arithmetic from wrapping.
        let mid = &out[3..9];
        assert!(mid.iter().all(|&v| v == 255));
    }

    #[test]
    fn harfrust_shaper_rejects_invalid_font_bytes() {
        assert!(ShapingFont::new(Arc::new(b"not a font".to_vec())).is_none());
    }

    #[test]
    fn harfrust_shaper_guesses_segment_properties_and_reuses_its_buffer() {
        // egui's default-font feature gives the test a deterministic bundled
        // font, independent of whichever fonts happen to be installed on CI.
        let definitions = egui::FontDefinitions::default();
        let hack = definitions
            .font_data
            .get("Hack")
            .expect("bundled Hack font");
        let shaping_font =
            ShapingFont::new(Arc::new(hack.font.to_vec())).expect("parse bundled Hack font");
        assert!(shaping_font.has_gsub);

        let mut input = harfrust::UnicodeBuffer::new();
        input.push_str("a->b");
        let (glyphs, units_per_em) = shaping_font.shape(input);

        assert!(units_per_em.is_finite() && units_per_em > 0.0);
        assert!(!glyphs.is_empty());
        assert!(glyphs.glyph_infos().iter().all(|glyph| glyph.cluster < 4));
        assert!(glyphs.clear().is_empty());
    }
}
