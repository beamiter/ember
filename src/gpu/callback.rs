use super::font_backend::FontBackend;
use super::instance::{CellInstance, GridUniforms};
use super::pipeline::GridPipeline;
use egui_wgpu::CallbackResources;
use std::sync::Arc;

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
}

impl GpuResources {
    pub fn new(atlas: Box<dyn FontBackend>, pipeline: GridPipeline, device: &wgpu::Device) -> Self {
        let color_atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("color_atlas_placeholder"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let color_atlas_view = color_atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
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
        }
    }
}

/// Background pass callback: shares instance data, uploads instances + atlas + uniforms.
pub struct GridBackgroundCallback {
    pub instances: Arc<Vec<CellInstance>>,
    pub uniforms: GridUniforms,
    pub instance_count: u32,
    pub row_offsets: Arc<Vec<usize>>,
    pub row_counts: Arc<Vec<usize>>,
    pub dirty_rows: Vec<bool>,
    pub use_partial_upload: bool,
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
        let res = callback_resources.get_mut::<GpuResources>().unwrap();

        let old_tex_size = res.atlas.atlas_dimensions();
        res.atlas.ensure_uploaded(device, queue);
        let new_tex_size = res.atlas.atlas_dimensions();

        if old_tex_size != new_tex_size || res.atlas.take_needs_rebind() {
            res.atlas_gen += 1;
            let (view, sampler) = res.atlas.gpu_resources();
            res.pipeline.rebuild_bind_group(
                device, view, sampler,
                &res.color_atlas_view, &res.color_atlas_sampler,
            );
        }

        res.pipeline.update_uniforms(queue, &self.uniforms);

        if self.use_partial_upload && !self.dirty_rows.is_empty() {
            res.pipeline.update_instances_partial(
                device,
                queue,
                &self.instances,
                &self.row_offsets[..],
                &self.row_counts[..],
                &self.dirty_rows,
            );
        } else {
            res.pipeline
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
        let res = callback_resources.get::<GpuResources>().unwrap();
        render_pass.set_pipeline(res.pipeline.pipeline());
        render_pass.set_bind_group(0, &res.pipeline.background_bind_group, &[]);
        render_pass.set_vertex_buffer(0, res.pipeline.instance_buffer().slice(..));
        render_pass.draw(0..6, 0..self.instance_count);
    }
}

/// Foreground pass callback: only uploads uniforms; instance buffer is already on GPU.
pub struct GridForegroundCallback {
    pub uniforms: GridUniforms,
    pub instance_count: u32,
}

impl egui_wgpu::CallbackTrait for GridForegroundCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let res = callback_resources.get_mut::<GpuResources>().unwrap();
        res.pipeline.update_uniforms(queue, &self.uniforms);
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
        let res = callback_resources.get::<GpuResources>().unwrap();
        render_pass.set_pipeline(res.pipeline.pipeline());
        render_pass.set_bind_group(0, &res.pipeline.foreground_bind_group, &[]);
        render_pass.set_vertex_buffer(0, res.pipeline.instance_buffer().slice(..));
        render_pass.draw(0..6, 0..self.instance_count);
    }
}
