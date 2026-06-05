use std::collections::HashMap;
use std::sync::Arc;
use lru::LruCache;
use std::num::NonZeroUsize;
use super::font_backend::{FontBackend, GlyphRegion, ShapedGlyph, AtlasGlyphKey, DirtyRect, GLYPH_PADDING, INITIAL_ATLAS_SIZE, MAX_ATLAS_SIZE, create_gpu_resources, upload_bitmap, empty_glyph_region, alpha_from_coverage};

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
    gid_cache: HashMap<GidGlyphKey, GlyphRegion>,
    dirty_rects: Vec<DirtyRect>,
    needs_full_upload: bool,
    needs_rebind: bool,
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
    cached_ascent: f32,
    cached_descent: f32,
    cached_advance_width: f32,
    font_data_regular: Arc<Vec<u8>>,
    font_data_bold: Option<Arc<Vec<u8>>>,
    shaping_enabled: bool,
    // Subpixel rendering
    subpixel_rendering: bool,
}

impl FontdueAtlas {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        font_data_regular: &[u8],
        font_data_bold: Option<&[u8]>,
        fallback_font_data: &[Vec<u8>],
        font_size_px: f32,
        font_weight: f32,
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

        // Check if font supports shaping (has GSUB table for ligatures)
        let font_data_arc = Arc::new(font_data_regular.to_vec());
        let font_data_bold_arc = font_data_bold.map(|d| Arc::new(d.to_vec()));
        let shaping_enabled = ttf_parser::Face::parse(&font_data_arc, 0)
            .map(|face| face.tables().gsub.is_some())
            .unwrap_or(false);

        let mut atlas = FontdueAtlas {
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
            gid_cache: HashMap::with_capacity(512),
            dirty_rects: Vec::new(),
            needs_full_upload: false,
            needs_rebind: false,
            texture,
            view,
            sampler,
            cached_ascent,
            cached_descent,
            cached_advance_width,
            font_data_regular: font_data_arc,
            font_data_bold: font_data_bold_arc,
            shaping_enabled,
            subpixel_rendering: true,
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
        for ch in ' '..='~' {
            for subpixel in 0..=3 {
                self.get_or_rasterize(ch, false, subpixel);
            }
        }
        for ch in ' '..='~' {
            for subpixel in 0..=3 {
                self.get_or_rasterize(ch, true, subpixel);
            }
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
        glyph_bitmap: &[u8],
        bold: bool,
        key: AtlasGlyphKey,
    ) -> GlyphRegion {
        let glyph_w = metrics.width as u32;
        let glyph_h = metrics.height as u32;

        if glyph_w == 0 || glyph_h == 0 {
            let region = empty_glyph_region();
            self.cache_insert(key, region);
            return region;
        }

        let padded_w = glyph_w + GLYPH_PADDING * 2;
        let padded_h = glyph_h + GLYPH_PADDING * 2;

        if !self.allocate_shelf(padded_w, padded_h) {
            if !self.grow() {
                let region = empty_glyph_region();
                self.cache_insert(key, region);
                return region;
            }
            if !self.allocate_shelf(padded_w, padded_h) {
                let region = empty_glyph_region();
                self.cache_insert(key, region);
                return region;
            }
        }

        let atlas_x = self.shelf_x - padded_w;
        let atlas_y = self.shelf_y;
        let bx = atlas_x + GLYPH_PADDING;
        let by = atlas_y + GLYPH_PADDING;

        let weight_boost = if bold { 1.0 } else { self.font_weight };
        let use_subpixel = self.subpixel_rendering && !is_cjk_or_wide(key.ch);

        if use_subpixel {
            self.rasterize_subpixel(
                metrics, glyph_bitmap, glyph_w, glyph_h, bx, by, weight_boost,
            );
        } else {
            for gy in 0..glyph_h {
                for gx in 0..glyph_w {
                    let src_idx = (gy * glyph_w + gx) as usize;
                    let dst_x = bx + gx;
                    let dst_y = by + gy;
                    if dst_x < self.width && dst_y < self.height {
                        let coverage = glyph_bitmap[src_idx] as f32 / 255.0;
                        let boosted = (coverage * weight_boost).min(1.0);
                        let alpha = alpha_from_coverage(boosted);
                        let a8 = (alpha * 255.0 + 0.5) as u8;
                        let pixel = [a8, a8, a8, a8];
                        let dst_idx = ((dst_y * self.width + dst_x) * 4) as usize;
                        self.bitmap[dst_idx..dst_idx + 4].copy_from_slice(&pixel);
                    }
                }
            }
        }

        // Record dirty rectangle (with padding)
        self.dirty_rects.push(DirtyRect::new(atlas_x, atlas_y, padded_w, padded_h));

        let subpixel_shift = match key.subpixel_offset {
            1 => 0.25,
            2 => 0.5,
            3 => 0.75,
            _ => 0.0,
        };
        let bearing_x = metrics.xmin as f32 + subpixel_shift;
        let bearing_y = self.cached_ascent - (metrics.ymin as f32 + metrics.height as f32);

        let region = GlyphRegion {
            u0: bx as f32 / self.width as f32,
            v0: by as f32 / self.height as f32,
            u1: (bx + glyph_w) as f32 / self.width as f32,
            v1: (by + glyph_h) as f32 / self.height as f32,
            width_px: glyph_w as f32,
            height_px: glyph_h as f32,
            bearing_x,
            bearing_y,
        };
        self.cache_insert(key, region);
        region
    }

    fn rasterize_subpixel(
        &mut self,
        _metrics: &fontdue::Metrics,
        glyph_bitmap: &[u8],
        glyph_w: u32,
        glyph_h: u32,
        bx: u32,
        by: u32,
        weight_boost: f32,
    ) {
        // For subpixel rendering, the input bitmap is 1x resolution grayscale.
        // We treat each pixel as 3 subpixels (RGB) and use a simple box filter
        // weighted by the neighboring pixels to produce per-channel coverage.
        // FIR filter weights (simple 1/3-weight kernel centered on each subpixel)
        const W: [f32; 5] = [1.0 / 9.0, 2.0 / 9.0, 3.0 / 9.0, 2.0 / 9.0, 1.0 / 9.0];

        for gy in 0..glyph_h {
            for gx in 0..glyph_w {
                let dst_x = bx + gx;
                let dst_y = by + gy;
                if dst_x >= self.width || dst_y >= self.height {
                    continue;
                }

                // Sample 5 horizontal neighbors (clamped)
                let mut samples = [0.0f32; 5];
                for i in 0..5i32 {
                    let sx = (gx as i32 + i - 2).clamp(0, glyph_w as i32 - 1) as u32;
                    let src_idx = (gy * glyph_w + sx) as usize;
                    let cov = glyph_bitmap[src_idx] as f32 / 255.0;
                    samples[i as usize] = (cov * weight_boost).min(1.0);
                }

                // R subpixel: centered at -1/3 pixel offset
                let r_cov = samples[0] * W[0] + samples[1] * W[1] + samples[2] * W[2]
                    + samples[3] * W[3] + samples[4] * W[4];
                // G subpixel: centered at 0
                let g_cov = {
                    let src_idx = (gy * glyph_w + gx) as usize;
                    let cov = glyph_bitmap[src_idx] as f32 / 255.0;
                    (cov * weight_boost).min(1.0)
                };
                // B subpixel: centered at +1/3 pixel offset
                // Use shifted samples
                let mut b_samples = [0.0f32; 5];
                for i in 0..5i32 {
                    let sx = (gx as i32 + i - 1).clamp(0, glyph_w as i32 - 1) as u32;
                    let src_idx = (gy * glyph_w + sx) as usize;
                    let cov = glyph_bitmap[src_idx] as f32 / 255.0;
                    b_samples[i as usize] = (cov * weight_boost).min(1.0);
                }
                let b_cov = b_samples[0] * W[0] + b_samples[1] * W[1] + b_samples[2] * W[2]
                    + b_samples[3] * W[3] + b_samples[4] * W[4];

                let r = alpha_from_coverage(r_cov);
                let g = alpha_from_coverage(g_cov);
                let b = alpha_from_coverage(b_cov);
                let a = r.max(g).max(b);

                let pixel = [
                    (r * 255.0 + 0.5) as u8,
                    (g * 255.0 + 0.5) as u8,
                    (b * 255.0 + 0.5) as u8,
                    (a * 255.0 + 0.5) as u8,
                ];
                let dst_idx = ((dst_y * self.width + dst_x) * 4) as usize;
                self.bitmap[dst_idx..dst_idx + 4].copy_from_slice(&pixel);
            }
        }
    }

    fn rasterize_gid(&mut self, gid: u16, bold: bool, subpixel_offset: u8) -> GlyphRegion {
        let key = GidGlyphKey { gid, bold, subpixel_offset };
        if let Some(&region) = self.gid_cache.get(&key) {
            return region;
        }

        let font = if bold {
            self.font_bold.as_ref().unwrap_or(&self.font_regular)
        } else {
            &self.font_regular
        };

        let (metrics, glyph_bitmap) = font.rasterize_indexed(gid as u16, self.font_size_px);

        if glyph_bitmap.is_empty() || metrics.width == 0 || metrics.height == 0 {
            let region = empty_glyph_region();
            self.gid_cache.insert(key, region);
            return region;
        }

        let glyph_w = metrics.width as u32;
        let glyph_h = metrics.height as u32;
        let padded_w = glyph_w + GLYPH_PADDING * 2;
        let padded_h = glyph_h + GLYPH_PADDING * 2;

        if !self.allocate_shelf(padded_w, padded_h) {
            if !self.grow() {
                let region = empty_glyph_region();
                self.gid_cache.insert(key, region);
                return region;
            }
            if !self.allocate_shelf(padded_w, padded_h) {
                let region = empty_glyph_region();
                self.gid_cache.insert(key, region);
                return region;
            }
        }

        let atlas_x = self.shelf_x - padded_w;
        let atlas_y = self.shelf_y;
        let bx = atlas_x + GLYPH_PADDING;
        let by = atlas_y + GLYPH_PADDING;

        let weight_boost = if bold { 1.0 } else { self.font_weight };

        for gy in 0..glyph_h {
            for gx in 0..glyph_w {
                let src_idx = (gy * glyph_w + gx) as usize;
                let dst_x = bx + gx;
                let dst_y = by + gy;
                if dst_x < self.width && dst_y < self.height {
                    let coverage = glyph_bitmap[src_idx] as f32 / 255.0;
                    let boosted = (coverage * weight_boost).min(1.0);
                    let alpha = alpha_from_coverage(boosted);
                    let pixel = [255, 255, 255, (alpha * 255.0 + 0.5) as u8];
                    let dst_idx = ((dst_y * self.width + dst_x) * 4) as usize;
                    self.bitmap[dst_idx..dst_idx + 4].copy_from_slice(&pixel);
                }
            }
        }

        self.dirty_rects.push(DirtyRect::new(atlas_x, atlas_y, padded_w, padded_h));

        let subpixel_shift = match subpixel_offset {
            1 => 0.25,
            2 => 0.5,
            3 => 0.75,
            _ => 0.0,
        };
        let bearing_x = metrics.xmin as f32 + subpixel_shift;
        let bearing_y = self.cached_ascent - (metrics.ymin as f32 + metrics.height as f32);

        let region = GlyphRegion {
            u0: bx as f32 / self.width as f32,
            v0: by as f32 / self.height as f32,
            u1: (bx + glyph_w) as f32 / self.width as f32,
            v1: (by + glyph_h) as f32 / self.height as f32,
            width_px: glyph_w as f32,
            height_px: glyph_h as f32,
            bearing_x,
            bearing_y,
        };
        self.gid_cache.insert(key, region);
        region
    }
}

impl FontBackend for FontdueAtlas {
    fn get_or_rasterize(&mut self, ch: char, bold: bool, subpixel_offset: u8) -> GlyphRegion {
        // Force CJK characters to always use subpixel bin 0 (no subpixel variation needed)
        let effective_subpixel = if is_cjk_or_wide(ch) { 0 } else { subpixel_offset };
        let key = AtlasGlyphKey { ch, bold, subpixel_offset: effective_subpixel };

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

        // Check if glyph exists in primary font
        let glyph_index = font.lookup_glyph_index(ch);
        let has_glyph = glyph_index != 0;

        // Check if this is a Nerd Font icon (Private Use Area)
        let is_nerd_font_char = matches!(ch as u32,
            0xE000..=0xF8FF |     // Private Use Area (common Nerd Font range)
            0xF0000..=0xFFFFD |   // Supplementary Private Use Area-A
            0x100000..=0x10FFFD   // Supplementary Private Use Area-B
        );

        // If no glyph in primary font (or we want fallback for missing chars), try fallback fonts
        if !has_glyph && ch != ' ' && !ch.is_control() {
            // For Nerd Font characters, ONLY use primary font even if glyph_index is 0
            // Don't fall back to CJK fonts which won't have icon glyphs
            if is_nerd_font_char {
                let (metrics, glyph_bitmap) = font.rasterize(ch, self.font_size_px);
                return self.rasterize_and_place(&metrics, &glyph_bitmap, bold, key);
            }

            // For non-Nerd-Font chars, try fallback fonts (CJK, etc.)
            for fb in &self.fallback_fonts {
                let fb_glyph_index = fb.lookup_glyph_index(ch);
                if fb_glyph_index != 0 {
                    let (fb_metrics, fb_bitmap) = fb.rasterize(ch, self.font_size_px);
                    return self.rasterize_and_place(&fb_metrics, &fb_bitmap, bold, key);
                }
            }
            // Glyph not found in any font, use .notdef
            let (metrics, glyph_bitmap) = font.rasterize(ch, self.font_size_px);
            return self.rasterize_and_place(&metrics, &glyph_bitmap, bold, key);
        }

        // Glyph exists (or is space/control), rasterize from primary font
        let (metrics, glyph_bitmap) = font.rasterize(ch, self.font_size_px);

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

        self.rasterize_and_place(&metrics, &glyph_bitmap, bold, key)
    }

    fn reset(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.ascii_cache.clear();
        self.unicode_cache.clear();
        self.gid_cache.clear();
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
        self.cached_advance_width = self.font_regular.metrics('0', self.font_size_px).advance_width;

        self.prepopulate_ascii();
        self.ensure_uploaded(device, queue);
        self.needs_rebind = true;
    }

    fn font_metrics(&self) -> (f32, f32, f32) {
        (self.cached_ascent, self.cached_descent, self.cached_advance_width)
    }

    fn ensure_uploaded(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        // Check if texture needs to be recreated
        let tex_size = self.texture.size();
        if tex_size.width != self.width || tex_size.height != self.height {
            let (texture, view, sampler) = create_gpu_resources(device, self.width, self.height);
            self.texture = texture;
            self.view = view;
            self.sampler = sampler;
            self.needs_full_upload = true;
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

    fn take_needs_rebind(&mut self) -> bool {
        let v = self.needs_rebind;
        self.needs_rebind = false;
        v
    }

    fn supports_shaping(&self) -> bool {
        self.shaping_enabled
    }

    fn shape_run(&mut self, text: &str, bold: bool, subpixel_offset: u8) -> Vec<ShapedGlyph> {
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
            return glyphs;
        }

        let font_data = if bold {
            self.font_data_bold.as_ref().unwrap_or(&self.font_data_regular)
        } else {
            &self.font_data_regular
        };

        let face = match rustybuzz::Face::from_slice(font_data, 0) {
            Some(f) => f,
            None => {
                // Fallback if face parsing fails
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
                return glyphs;
            }
        };

        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(text);

        let glyph_buffer = rustybuzz::shape(&face, &[], buffer);
        let infos = glyph_buffer.glyph_infos();
        let positions = glyph_buffer.glyph_positions();

        let upem = face.units_per_em() as f32;
        let scale = self.font_size_px / upem;

        let mut glyphs = Vec::with_capacity(infos.len());
        for (info, pos) in infos.iter().zip(positions.iter()) {
            let gid = info.glyph_id as u16;
            let region = self.rasterize_gid(gid, bold, subpixel_offset);

            glyphs.push(ShapedGlyph {
                cluster: info.cluster,
                x_advance: pos.x_advance as f32 * scale,
                x_offset: pos.x_offset as f32 * scale,
                y_offset: pos.y_offset as f32 * scale,
                region,
            });
        }

        glyphs
    }
}
