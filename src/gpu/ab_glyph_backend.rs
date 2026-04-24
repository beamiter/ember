use ab_glyph::{point, Font, FontVec, GlyphId, PxScale, ScaleFont};
use std::collections::HashMap;
use super::font_backend::{FontBackend, GlyphRegion, AtlasGlyphKey, DirtyRect, GLYPH_PADDING, INITIAL_ATLAS_SIZE, MAX_ATLAS_SIZE, create_gpu_resources, upload_bitmap, empty_glyph_region, alpha_from_coverage};

fn is_cjk_or_wide(ch: char) -> bool {
    matches!(ch as u32,
        0x2E80..=0x2EFF |
        0x3000..=0x303F |
        0x3040..=0x309F |
        0x30A0..=0x30FF |
        0x3100..=0x312F |
        0x3130..=0x318F |
        0x3190..=0x319F |
        0x31A0..=0x31BF |
        0x31C0..=0x31EF |
        0x31F0..=0x31FF |
        0x3200..=0x32FF |
        0x3300..=0x33FF |
        0x4E00..=0x9FFF |
        0xF900..=0xFAFF |
        0x20000..=0x2A6DF
    )
}

pub struct AbGlyphAtlas {
    font_regular: FontVec,
    font_bold: Option<FontVec>,
    fallback_fonts: Vec<FontVec>,
    font_size_px: f32,
    font_weight: f32,
    bitmap: Vec<u8>,
    width: u32,
    height: u32,
    shelf_x: u32,
    shelf_y: u32,
    shelf_height: u32,
    cache: HashMap<AtlasGlyphKey, GlyphRegion>,
    dirty_rects: Vec<DirtyRect>,
    needs_full_upload: bool,
    needs_rebind: bool,
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
}

impl AbGlyphAtlas {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        font_data_regular: &[u8],
        font_data_bold: Option<&[u8]>,
        fallback_font_data: &[Vec<u8>],
        font_size_px: f32,
        font_weight: f32,
    ) -> Self {
        let font_regular =
            FontVec::try_from_vec(font_data_regular.to_vec()).expect("failed to load regular font");
        let font_bold = font_data_bold
            .map(|data| FontVec::try_from_vec(data.to_vec()).expect("failed to load bold font"));
        let fallback_fonts: Vec<FontVec> = fallback_font_data
            .iter()
            .filter_map(|data| FontVec::try_from_vec(data.clone()).ok())
            .collect();

        let width = INITIAL_ATLAS_SIZE;
        let height = INITIAL_ATLAS_SIZE;
        let bitmap = vec![0u8; (width * height * 4) as usize];

        let (texture, view, sampler) = create_gpu_resources(device, width, height);
        upload_bitmap(queue, &texture, &bitmap, width, height);

        let mut atlas = AbGlyphAtlas {
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
            cache: HashMap::with_capacity(256),
            dirty_rects: Vec::new(),
            needs_full_upload: false,
            needs_rebind: false,
            texture,
            view,
            sampler,
        };

        atlas.prepopulate_ascii();
        atlas
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
        for region in self.cache.values_mut() {
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
}

impl FontBackend for AbGlyphAtlas {
    fn get_or_rasterize(&mut self, ch: char, bold: bool, subpixel_offset: u8) -> GlyphRegion {
        let effective_subpixel = if is_cjk_or_wide(ch) { 0 } else { subpixel_offset };
        let key = AtlasGlyphKey { ch, bold, subpixel_offset: effective_subpixel };
        if let Some(&region) = self.cache.get(&key) {
            return region;
        }

        let font = if bold {
            self.font_bold.as_ref().unwrap_or(&self.font_regular)
        } else {
            &self.font_regular
        };

        let scale = PxScale::from(self.font_size_px);
        let scaled_font = font.as_scaled(scale);

        let glyph_id = font.glyph_id(ch);
        let (glyph_id, used_font): (GlyphId, &FontVec) = if glyph_id.0 == 0 && bold {
            let fallback_id = self.font_regular.glyph_id(ch);
            if fallback_id.0 != 0 {
                (fallback_id, &self.font_regular)
            } else {
                let mut found = None;
                for fb in &self.fallback_fonts {
                    let fb_id = fb.glyph_id(ch);
                    if fb_id.0 != 0 {
                        found = Some((fb_id, fb as &FontVec));
                        break;
                    }
                }
                found.unwrap_or((fallback_id, &self.font_regular))
            }
        } else if glyph_id.0 == 0 {
            let mut found = None;
            for fb in &self.fallback_fonts {
                let fb_id = fb.glyph_id(ch);
                if fb_id.0 != 0 {
                    found = Some((fb_id, fb as &FontVec));
                    break;
                }
            }
            found.unwrap_or((glyph_id, font))
        } else {
            (glyph_id, font)
        };

        let primary_ascent = self.font_regular.as_scaled(scale).ascent();
        let glyph = glyph_id.with_scale_and_position(scale, point(0.0, primary_ascent));

        if let Some(outlined) = used_font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            let glyph_w = (bounds.max.x - bounds.min.x).ceil() as u32;
            let glyph_h = (bounds.max.y - bounds.min.y).ceil() as u32;

            if glyph_w == 0 || glyph_h == 0 {
                let region = empty_glyph_region();
                self.cache.insert(key, region);
                return region;
            }

            let padded_w = glyph_w + GLYPH_PADDING * 2;
            let padded_h = glyph_h + GLYPH_PADDING * 2;

            if !self.allocate_shelf(padded_w, padded_h) {
                if !self.grow() {
                    let region = empty_glyph_region();
                    self.cache.insert(key, region);
                    return region;
                }
                if !self.allocate_shelf(padded_w, padded_h) {
                    let region = empty_glyph_region();
                    self.cache.insert(key, region);
                    return region;
                }
            }

            let atlas_x = self.shelf_x - padded_w;
            let atlas_y = self.shelf_y;

            let bx = atlas_x + GLYPH_PADDING;
            let by = atlas_y + GLYPH_PADDING;
            let weight_boost = if bold { 1.0 } else { self.font_weight };
            outlined.draw(|x, y, alpha| {
                let px = bx + x;
                let py = by + y;
                if px < self.width && py < self.height {
                    let boosted_alpha = (alpha * weight_boost).min(1.0);
                    let coverage_alpha = alpha_from_coverage(boosted_alpha);
                    let pixel = [255, 255, 255, (coverage_alpha * 255.0 + 0.5) as u8];
                    let dst_idx = ((py * self.width + px) * 4) as usize;
                    self.bitmap[dst_idx..dst_idx + 4].copy_from_slice(&pixel);
                }
            });

            // Record dirty rectangle (with padding)
            self.dirty_rects.push(DirtyRect::new(atlas_x, atlas_y, padded_w, padded_h));

            // Apply subpixel offset to bearing_x: 0 → 0.0px, 1 → 0.25px, 2 → 0.5px, 3 → 0.75px
            let subpixel_shift = match subpixel_offset {
                1 => 0.25,
                2 => 0.5,
                3 => 0.75,
                _ => 0.0,
            };

            let region = GlyphRegion {
                u0: bx as f32 / self.width as f32,
                v0: by as f32 / self.height as f32,
                u1: (bx + glyph_w) as f32 / self.width as f32,
                v1: (by + glyph_h) as f32 / self.height as f32,
                width_px: glyph_w as f32,
                height_px: glyph_h as f32,
                bearing_x: bounds.min.x + subpixel_shift,
                bearing_y: bounds.min.y,
            };
            self.cache.insert(key, region);
            region
        } else {
            let h_advance = scaled_font.h_advance(glyph_id);
            let subpixel_shift = match effective_subpixel {
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
                width_px: h_advance,
                height_px: 0.0,
                bearing_x: subpixel_shift,
                bearing_y: 0.0,
            };
            self.cache.insert(key, region);
            region
        }
    }

    fn reset(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.cache.clear();
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

        self.prepopulate_ascii();
        self.ensure_uploaded(device, queue);
        self.needs_rebind = true;
    }

    fn font_metrics(&self) -> (f32, f32, f32) {
        let scale = PxScale::from(self.font_size_px);
        let scaled = self.font_regular.as_scaled(scale);
        let ascent = scaled.ascent();
        let descent = scaled.descent();
        let advance = scaled.h_advance(self.font_regular.glyph_id('0'));
        (ascent, descent, advance)
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
        "ab_glyph"
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
}
