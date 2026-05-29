/// UV region of a rasterized glyph inside the atlas texture.
#[derive(Clone, Copy, Debug)]
pub struct GlyphRegion {
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
    pub width_px: f32,
    pub height_px: f32,
    pub bearing_x: f32,
    pub bearing_y: f32,
}

/// Dirty rectangle region (pixel coordinates) for incremental texture upload.
#[derive(Clone, Copy, Debug)]
pub struct DirtyRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl DirtyRect {
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }
}

/// Key for the glyph cache: character + style + subpixel position.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct AtlasGlyphKey {
    pub ch: char,
    pub bold: bool,
    /// Subpixel horizontal offset: 0 = 0.0px, 1 = 0.25px, 2 = 0.5px, 3 = 0.75px
    pub subpixel_offset: u8,
}

/// A shaped glyph from rustybuzz, ready for rendering.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct ShapedGlyph {
    pub cluster: u32,
    pub x_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
    pub region: GlyphRegion,
}

pub trait FontBackend: Send + Sync {
    fn get_or_rasterize(&mut self, ch: char, bold: bool, subpixel_offset: u8) -> GlyphRegion;
    #[allow(dead_code)]
    fn reset(&mut self, device: &wgpu::Device, queue: &wgpu::Queue);
    fn font_metrics(&self) -> (f32, f32, f32);
    fn ensure_uploaded(&mut self, device: &wgpu::Device, queue: &wgpu::Queue);
    #[allow(dead_code)]
    fn backend_name(&self) -> &'static str;
    fn gpu_resources(&self) -> (&wgpu::TextureView, &wgpu::Sampler);
    fn atlas_dimensions(&self) -> (u32, u32);
    fn take_needs_rebind(&mut self) -> bool;

    #[allow(dead_code)]
    fn shape_run(&mut self, text: &str, bold: bool, subpixel_offset: u8) -> Vec<ShapedGlyph> {
        let mut glyphs = Vec::new();
        for ch in text.chars() {
            let region = self.get_or_rasterize(ch, bold, subpixel_offset);
            glyphs.push(ShapedGlyph {
                cluster: 0,
                x_advance: region.width_px,
                x_offset: 0.0,
                y_offset: 0.0,
                region,
            });
        }
        glyphs
    }

    #[allow(dead_code)]
    fn supports_shaping(&self) -> bool {
        false
    }
}

pub const GLYPH_PADDING: u32 = 2;
pub const INITIAL_ATLAS_SIZE: u32 = 1024;
pub const MAX_ATLAS_SIZE: u32 = 4096;

/// Convert coverage alpha to baked color using TwoCoverageMinusCoverageSq function.
/// This matches egui's approach: alpha = 2c - c² where c is coverage in [0,1].
/// Produces perceptually correct blending for both light and dark backgrounds.
pub fn alpha_from_coverage(coverage: f32) -> f32 {
    let c = coverage.clamp(0.0, 1.0);
    2.0 * c - c * c
}

pub fn create_gpu_resources(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView, wgpu::Sampler) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("glyph_atlas"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("glyph_atlas_sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    (texture, view, sampler)
}

pub fn upload_bitmap(queue: &wgpu::Queue, texture: &wgpu::Texture, bitmap: &[u8], width: u32, height: u32) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bitmap,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

pub fn empty_glyph_region() -> GlyphRegion {
    GlyphRegion {
        u0: 0.0,
        v0: 0.0,
        u1: 0.0,
        v1: 0.0,
        width_px: 0.0,
        height_px: 0.0,
        bearing_x: 0.0,
        bearing_y: 0.0,
    }
}
