use super::instance::{CellInstance, GridUniforms};
use wgpu::util::DeviceExt;

/// Shared render-pipeline state. Per-surface buffers live in
/// [`GridDrawSurface`].
pub struct GridPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

/// Borrowed atlas resources used to create or refresh a terminal draw surface.
#[derive(Clone, Copy)]
pub struct GridAtlasBindings<'a> {
    pub atlas_view: &'a wgpu::TextureView,
    pub atlas_sampler: &'a wgpu::Sampler,
    pub color_atlas_view: &'a wgpu::TextureView,
    pub color_atlas_sampler: &'a wgpu::Sampler,
    pub generation: u64,
}

/// GPU state owned by one terminal drawing surface.
///
/// egui-wgpu calls `prepare` for every paint callback before it calls any
/// callback's `paint`.  Instance/uniform buffers therefore cannot be shared
/// between panes: a later pane would overwrite the data that an earlier pane
/// still needs for its eventual paint call.  The render pipeline and glyph
/// atlas remain shared, while this state is keyed by a stable surface id.
pub struct GridDrawSurface {
    background_uniform_buffer: wgpu::Buffer,
    foreground_uniform_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    pub background_bind_group: wgpu::BindGroup,
    pub foreground_bind_group: wgpu::BindGroup,
    atlas_generation: u64,
}

impl GridPipeline {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader_src = include_str!("shader.wgsl");
        let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("grid_shader"),
            source: wgpu::ShaderSource::Wgsl(shader_src.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("grid_bind_group_layout"),
            entries: &[
                // Uniforms
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Atlas texture
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Atlas sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Color atlas texture
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Color atlas sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("grid_pipeline_layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let fs_entry = if target_format.is_srgb() {
            eprintln!(
                "[GPU] sRGB framebuffer {:?}, using fs_main_linear",
                target_format
            );
            "fs_main_linear"
        } else {
            "fs_main_gamma"
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("grid_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader_module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[CellInstance::vertex_buffer_layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader_module,
                entry_point: Some(fs_entry),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        GridPipeline {
            pipeline,
            bind_group_layout,
        }
    }

    fn create_bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        uniform_buffer: &wgpu::Buffer,
        atlas_view: &wgpu::TextureView,
        atlas_sampler: &wgpu::Sampler,
        color_atlas_view: &wgpu::TextureView,
        color_atlas_sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("grid_bind_group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(atlas_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(color_atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(color_atlas_sampler),
                },
            ],
        })
    }

    pub fn create_draw_surface(
        &self,
        device: &wgpu::Device,
        atlas: GridAtlasBindings<'_>,
    ) -> GridDrawSurface {
        GridDrawSurface::new(device, &self.bind_group_layout, atlas)
    }

    pub fn rebind_draw_surface(
        &self,
        surface: &mut GridDrawSurface,
        device: &wgpu::Device,
        atlas: GridAtlasBindings<'_>,
    ) {
        surface.rebuild_bind_groups(device, &self.bind_group_layout, atlas);
    }

    pub fn pipeline(&self) -> &wgpu::RenderPipeline {
        &self.pipeline
    }
}

impl GridDrawSurface {
    const INITIAL_INSTANCE_CAPACITY: usize = 16384;

    fn new(
        device: &wgpu::Device,
        bind_group_layout: &wgpu::BindGroupLayout,
        atlas: GridAtlasBindings<'_>,
    ) -> Self {
        let background_uniform_buffer =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("grid_surface_background_uniforms"),
                contents: bytemuck::bytes_of(&GridUniforms::zeroed()),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });

        let foreground_uniform_buffer =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("grid_surface_foreground_uniforms"),
                contents: bytemuck::bytes_of(&GridUniforms::zeroed()),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });

        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grid_surface_instances"),
            size: (Self::INITIAL_INSTANCE_CAPACITY * std::mem::size_of::<CellInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let background_bind_group = GridPipeline::create_bind_group(
            device,
            bind_group_layout,
            &background_uniform_buffer,
            atlas.atlas_view,
            atlas.atlas_sampler,
            atlas.color_atlas_view,
            atlas.color_atlas_sampler,
        );
        let foreground_bind_group = GridPipeline::create_bind_group(
            device,
            bind_group_layout,
            &foreground_uniform_buffer,
            atlas.atlas_view,
            atlas.atlas_sampler,
            atlas.color_atlas_view,
            atlas.color_atlas_sampler,
        );

        Self {
            background_uniform_buffer,
            foreground_uniform_buffer,
            instance_buffer,
            instance_capacity: Self::INITIAL_INSTANCE_CAPACITY,
            background_bind_group,
            foreground_bind_group,
            atlas_generation: atlas.generation,
        }
    }

    pub fn atlas_generation(&self) -> u64 {
        self.atlas_generation
    }

    /// Rebuild this surface's bind groups when the shared atlas texture is
    /// recreated. Its instance and uniform buffers remain intact.
    pub fn rebuild_bind_groups(
        &mut self,
        device: &wgpu::Device,
        bind_group_layout: &wgpu::BindGroupLayout,
        atlas: GridAtlasBindings<'_>,
    ) {
        self.background_bind_group = GridPipeline::create_bind_group(
            device,
            bind_group_layout,
            &self.background_uniform_buffer,
            atlas.atlas_view,
            atlas.atlas_sampler,
            atlas.color_atlas_view,
            atlas.color_atlas_sampler,
        );
        self.foreground_bind_group = GridPipeline::create_bind_group(
            device,
            bind_group_layout,
            &self.foreground_uniform_buffer,
            atlas.atlas_view,
            atlas.atlas_sampler,
            atlas.color_atlas_view,
            atlas.color_atlas_sampler,
        );
        self.atlas_generation = atlas.generation;
    }

    /// Upload uniform data.
    pub fn update_uniforms(&self, queue: &wgpu::Queue, uniforms: &GridUniforms) {
        let uniform_buffer = if uniforms.render_phase < 0.5 {
            &self.background_uniform_buffer
        } else {
            &self.foreground_uniform_buffer
        };

        queue.write_buffer(uniform_buffer, 0, bytemuck::bytes_of(uniforms));
    }

    /// Upload instance data, resizing the buffer if needed.
    pub fn update_instances(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        instances: &[CellInstance],
    ) {
        if instances.is_empty() {
            return;
        }

        if instances.len() > self.instance_capacity {
            let new_capacity = instances.len().next_power_of_two();
            self.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("grid_instances"),
                size: (new_capacity * std::mem::size_of::<CellInstance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.instance_capacity = new_capacity;
        }

        queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(instances));
    }

    /// Upload only dirty rows' instance data in contiguous spans.
    /// Reduces GPU upload bandwidth by only sending changed rows.
    pub fn update_instances_partial(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        instances: &[CellInstance],
        row_offsets: &[usize],
        row_counts: &[usize],
        dirty_rows: &[bool],
    ) {
        if instances.is_empty() {
            return;
        }

        // A newly allocated buffer has no preserved clean rows. If capacity
        // grows, upload the complete surface instead of only the dirty spans.
        if instances.len() > self.instance_capacity {
            self.update_instances(device, queue, instances);
            return;
        }

        // Upload dirty rows in contiguous spans to minimize write_buffer calls
        let mut upload_start: Option<usize> = None;

        for (row_idx, &is_dirty) in dirty_rows.iter().enumerate() {
            if is_dirty {
                if upload_start.is_none() {
                    upload_start = Some(row_idx);
                }
            } else if let Some(start_row) = upload_start {
                // Flush span [start_row..row_idx)
                self.upload_row_span(
                    queue,
                    instances,
                    row_offsets,
                    row_counts,
                    start_row,
                    row_idx,
                );
                upload_start = None;
            }
        }

        // Flush trailing span if any
        if let Some(start_row) = upload_start {
            self.upload_row_span(
                queue,
                instances,
                row_offsets,
                row_counts,
                start_row,
                dirty_rows.len(),
            );
        }
    }

    #[inline]
    fn upload_row_span(
        &self,
        queue: &wgpu::Queue,
        instances: &[CellInstance],
        row_offsets: &[usize],
        row_counts: &[usize],
        start_row: usize,
        end_row: usize,
    ) {
        let instance_offset = row_offsets[start_row];
        let instance_count: usize = (start_row..end_row).map(|r| row_counts[r]).sum();

        if instance_count == 0 {
            return;
        }

        let byte_offset = (instance_offset * std::mem::size_of::<CellInstance>()) as u64;
        let slice = &instances[instance_offset..instance_offset + instance_count];

        queue.write_buffer(
            &self.instance_buffer,
            byte_offset,
            bytemuck::cast_slice(slice),
        );
    }

    pub fn instance_buffer(&self) -> &wgpu::Buffer {
        &self.instance_buffer
    }
}

use bytemuck::Zeroable;
