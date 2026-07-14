use super::font_backend::FontBackend;
use super::instance::{CellInstance, GridUniforms};
use super::pipeline::{GridAtlasBindings, GridDrawSurface, GridPipeline};
use egui_wgpu::CallbackResources;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// GpuResources 缺失只可能源于初始化 bug。每帧 panic 会让整个程序崩溃且无信息,
/// 改为首帧 log::error 一次后静默跳过该帧绘制(最多黑屏)。
static MISSING_RESOURCES_LOGGED: AtomicBool = AtomicBool::new(false);
static NEXT_SURFACE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Stable identity of one [`crate::ui::TerminalRenderer`] GPU drawing surface.
/// Background and foreground callbacks for the same renderer carry the same
/// id, while different panes must never share one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GridSurfaceId(u64);

impl GridSurfaceId {
    pub fn allocate() -> Self {
        Self(NEXT_SURFACE_ID.fetch_add(1, Ordering::Relaxed))
    }
}

fn log_missing_resources(site: &str) {
    if !MISSING_RESOURCES_LOGGED.swap(true, Ordering::Relaxed) {
        log::error!("GpuResources missing in {site}; skipping GPU draw this frame");
    }
}

/// GPU resources stored in egui_wgpu's CallbackResources (TypeMap).
pub struct GpuResources {
    pub atlas: Box<dyn FontBackend>,
    pub pipeline: GridPipeline,
    pub color_atlas_view: wgpu::TextureView,
    pub color_atlas_sampler: wgpu::Sampler,
    // Retained to keep the GPU texture alive (color_atlas_view borrows from it); never read directly.
    #[allow(dead_code)]
    color_atlas_texture: wgpu::Texture,
    atlas_gen: u64,
    font_resource_generation: u64,
    draw_surfaces: HashMap<GridSurfaceId, DrawSurfaceEntry>,
}

struct DrawSurfaceEntry {
    surface: GridDrawSurface,
    last_used_frame: u64,
}

impl GpuResources {
    // A renderer that disappeared (closed pane/reconfigured UI) should not
    // retain its GPU buffers forever. Keep enough frames to survive temporary
    // clipping/minimization without churning allocations.
    const SURFACE_RETENTION_FRAMES: u64 = 120;

    pub fn new(atlas: Box<dyn FontBackend>, pipeline: GridPipeline, device: &wgpu::Device) -> Self {
        let color_atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("color_atlas_placeholder"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let color_atlas_view =
            color_atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let color_atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("color_atlas_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        GpuResources {
            atlas,
            pipeline,
            color_atlas_view,
            color_atlas_sampler,
            color_atlas_texture,
            atlas_gen: 0,
            font_resource_generation: 1,
            draw_surfaces: HashMap::new(),
        }
    }

    /// Replace the shared font/pipeline generation after a runtime font or
    /// scale change. Existing draw surfaces were created from the old bind
    /// group layout, so drop them and let each renderer recreate its own state
    /// on the next prepare pass.
    pub fn replace_font_resources(&mut self, atlas: Box<dyn FontBackend>, pipeline: GridPipeline) {
        self.atlas = atlas;
        self.pipeline = pipeline;
        self.atlas_gen = self.atlas_gen.wrapping_add(1);
        self.font_resource_generation = self.font_resource_generation.wrapping_add(1);
        self.draw_surfaces.clear();
    }

    pub fn font_content_generation(&self) -> (u64, u64) {
        (
            self.font_resource_generation,
            self.atlas.content_generation(),
        )
    }

    pub(crate) fn prepare_atlas(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let old_tex_size = self.atlas.atlas_dimensions();
        self.atlas.ensure_uploaded(device, queue);
        let new_tex_size = self.atlas.atlas_dimensions();
        if old_tex_size != new_tex_size || self.atlas.take_needs_rebind() {
            self.atlas_gen = self.atlas_gen.wrapping_add(1);
        }
    }

    fn prune_stale_surfaces(&mut self, frame_nr: u64, current: GridSurfaceId) {
        self.draw_surfaces.retain(|id, entry| {
            *id == current
                || frame_nr.saturating_sub(entry.last_used_frame) <= Self::SURFACE_RETENTION_FRAMES
        });
    }

    /// Ensure that a surface exists and is bound to the current atlas
    /// generation. Returns true when a fresh instance buffer was allocated;
    /// callers must fully upload it rather than applying a dirty-row patch.
    fn ensure_draw_surface(
        &mut self,
        device: &wgpu::Device,
        surface_id: GridSurfaceId,
        frame_nr: u64,
    ) -> bool {
        let created = !self.draw_surfaces.contains_key(&surface_id);
        if created {
            let (atlas_view, atlas_sampler) = self.atlas.gpu_resources();
            let surface = self.pipeline.create_draw_surface(
                device,
                GridAtlasBindings {
                    atlas_view,
                    atlas_sampler,
                    color_atlas_view: &self.color_atlas_view,
                    color_atlas_sampler: &self.color_atlas_sampler,
                    generation: self.atlas_gen,
                },
            );
            self.draw_surfaces.insert(
                surface_id,
                DrawSurfaceEntry {
                    surface,
                    last_used_frame: frame_nr,
                },
            );
        }

        let needs_rebind = self
            .draw_surfaces
            .get(&surface_id)
            .is_some_and(|entry| entry.surface.atlas_generation() != self.atlas_gen);
        if needs_rebind {
            let (atlas_view, atlas_sampler) = self.atlas.gpu_resources();
            let entry = self
                .draw_surfaces
                .get_mut(&surface_id)
                .expect("surface was inserted above");
            self.pipeline.rebind_draw_surface(
                &mut entry.surface,
                device,
                GridAtlasBindings {
                    atlas_view,
                    atlas_sampler,
                    color_atlas_view: &self.color_atlas_view,
                    color_atlas_sampler: &self.color_atlas_sampler,
                    generation: self.atlas_gen,
                },
            );
        }

        if let Some(entry) = self.draw_surfaces.get_mut(&surface_id) {
            entry.last_used_frame = frame_nr;
        }
        created
    }

    #[cfg(test)]
    fn surface_is_stale(last_used_frame: u64, frame_nr: u64) -> bool {
        frame_nr.saturating_sub(last_used_frame) > Self::SURFACE_RETENTION_FRAMES
    }
}

/// Background pass callback: shares instance data, uploads instances + atlas + uniforms.
pub struct GridBackgroundCallback {
    pub surface_id: GridSurfaceId,
    pub frame_nr: u64,
    pub instances: Arc<Vec<CellInstance>>,
    pub uniforms: GridUniforms,
    pub instance_count: u32,
    pub row_offsets: Arc<Vec<usize>>,
    pub row_counts: Arc<Vec<usize>>,
    pub dirty_rows: Arc<Vec<bool>>,
    pub use_partial_upload: bool,
    /// Generation used when the CPU instances and atlas UVs were built.
    /// A later pane can grow/reset the shared atlas before callbacks paint.
    pub expected_font_generation: (u64, u64),
}

impl egui_wgpu::CallbackTrait for GridBackgroundCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let res = match callback_resources.get_mut::<GpuResources>() {
            Some(r) => r,
            None => {
                log_missing_resources("GridBackgroundCallback::prepare");
                return Vec::new();
            }
        };

        res.prepare_atlas(device, queue);
        if res.font_content_generation() != self.expected_font_generation {
            return Vec::new();
        }
        res.prune_stale_surfaces(self.frame_nr, self.surface_id);
        let surface_created = res.ensure_draw_surface(device, self.surface_id, self.frame_nr);
        let entry = match res.draw_surfaces.get_mut(&self.surface_id) {
            Some(entry) => entry,
            None => return Vec::new(),
        };

        entry.surface.update_uniforms(queue, &self.uniforms);

        if !surface_created && self.use_partial_upload && !self.dirty_rows.is_empty() {
            entry.surface.update_instances_partial(
                device,
                queue,
                &self.instances,
                &self.row_offsets[..],
                &self.row_counts[..],
                &self.dirty_rows,
            );
        } else {
            entry
                .surface
                .update_instances(device, queue, &self.instances);
        }

        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &CallbackResources,
    ) {
        if self.instance_count == 0 {
            return;
        }
        let res = match callback_resources.get::<GpuResources>() {
            Some(r) => r,
            None => {
                log_missing_resources("GridBackgroundCallback::paint");
                return;
            }
        };
        if res.font_content_generation() != self.expected_font_generation {
            return;
        }
        let Some(entry) = res.draw_surfaces.get(&self.surface_id) else {
            log::error!(
                "GPU draw surface {:?} missing during background paint",
                self.surface_id
            );
            return;
        };
        render_pass.set_pipeline(res.pipeline.pipeline());
        render_pass.set_bind_group(0, &entry.surface.background_bind_group, &[]);
        render_pass.set_vertex_buffer(0, entry.surface.instance_buffer().slice(..));
        render_pass.draw(0..6, 0..self.instance_count);
    }
}

/// Foreground pass callback: only uploads uniforms; instance buffer is already on GPU.
pub struct GridForegroundCallback {
    pub surface_id: GridSurfaceId,
    pub frame_nr: u64,
    pub uniforms: GridUniforms,
    pub instance_count: u32,
    pub expected_font_generation: (u64, u64),
}

impl egui_wgpu::CallbackTrait for GridForegroundCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let res = match callback_resources.get_mut::<GpuResources>() {
            Some(r) => r,
            None => {
                log_missing_resources("GridForegroundCallback::prepare");
                return Vec::new();
            }
        };
        // Background normally creates the surface first. Keeping foreground
        // self-contained makes callback ordering changes fail safe.
        res.prepare_atlas(device, queue);
        if res.font_content_generation() != self.expected_font_generation {
            return Vec::new();
        }
        res.ensure_draw_surface(device, self.surface_id, self.frame_nr);
        if let Some(entry) = res.draw_surfaces.get_mut(&self.surface_id) {
            entry.surface.update_uniforms(queue, &self.uniforms);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &CallbackResources,
    ) {
        if self.instance_count == 0 {
            return;
        }
        let res = match callback_resources.get::<GpuResources>() {
            Some(r) => r,
            None => {
                log_missing_resources("GridForegroundCallback::paint");
                return;
            }
        };
        if res.font_content_generation() != self.expected_font_generation {
            return;
        }
        let Some(entry) = res.draw_surfaces.get(&self.surface_id) else {
            log::error!(
                "GPU draw surface {:?} missing during foreground paint",
                self.surface_id
            );
            return;
        };
        render_pass.set_pipeline(res.pipeline.pipeline());
        render_pass.set_bind_group(0, &entry.surface.foreground_bind_group, &[]);
        render_pass.set_vertex_buffer(0, entry.surface.instance_buffer().slice(..));
        render_pass.draw(0..6, 0..self.instance_count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawing_surface_ids_are_unique() {
        assert_ne!(GridSurfaceId::allocate(), GridSurfaceId::allocate());
    }

    #[test]
    fn stale_surface_age_uses_saturating_frame_math() {
        assert!(!GpuResources::surface_is_stale(10, 10));
        assert!(!GpuResources::surface_is_stale(10, 9));
        assert!(!GpuResources::surface_is_stale(
            10,
            10 + GpuResources::SURFACE_RETENTION_FRAMES
        ));
        assert!(GpuResources::surface_is_stale(
            10,
            11 + GpuResources::SURFACE_RETENTION_FRAMES
        ));
    }
}
