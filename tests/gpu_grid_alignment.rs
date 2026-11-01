//! Pixel regressions against the production WGSL/pipeline. Run explicitly on a
//! machine with a wgpu adapter (software Vulkan is sufficient):
//! cargo test --test gpu_grid_alignment -- --ignored --nocapture

use bytemuck::Zeroable;
use ember::gpu::{
    font_backend::{create_gpu_resources, upload_bitmap},
    instance::{CellInstance, GridUniforms},
    pipeline::{GridAtlasBindings, GridPipeline},
};
use std::{future::Future, sync::Arc, task::Wake};

// Keep native device creation and teardown deterministic across GPU tests.
static GPU_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn block_on<T>(future: impl Future<Output = T>) -> T {
    struct ThreadWake(std::thread::Thread);
    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Arc::new(ThreadWake(std::thread::current())).into();
    let mut cx = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
        std::thread::park();
    }
}

struct Fixture {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: GridPipeline,
    atlas_texture: wgpu::Texture,
    atlas_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}

impl Fixture {
    fn new(dual_source: bool, format: wgpu::TextureFormat) -> Self {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = block_on(instance.request_adapter(&Default::default()))
            .expect("GPU alignment tests require a wgpu adapter");
        let features = if dual_source {
            wgpu::Features::DUAL_SOURCE_BLENDING
        } else {
            wgpu::Features::empty()
        };
        assert!(adapter.features().contains(features));
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: features,
            ..Default::default()
        }))
        .expect("request test device");
        eprintln!(
            "GPU alignment: {:?}, dual={dual_source}, {format:?}",
            adapter.get_info()
        );
        let pipeline = GridPipeline::new(&device, format);
        let (texture, atlas_view, sampler) = create_gpu_resources(&device, 32, 32);
        // Empty ink isolates decorations while retaining real, differently sized
        // glyph quads, including ascenders/descenders and empty ligature cells.
        upload_bitmap(&queue, &texture, &[0; 32 * 32 * 4], 32, 32);
        Self {
            device,
            queue,
            pipeline,
            atlas_texture: texture,
            atlas_view,
            sampler,
        }
    }

    fn render(
        &self,
        uniforms: GridUniforms,
        instances: &[CellInstance],
        format: wgpu::TextureFormat,
    ) -> image::RgbaImage {
        let width = 576;
        let height = 1024;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("alignment_pixels"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let mut surface = self.pipeline.create_draw_surface(
            &self.device,
            GridAtlasBindings {
                atlas_view: &self.atlas_view,
                atlas_sampler: &self.sampler,
                color_atlas_view: &self.atlas_view,
                color_atlas_sampler: &self.sampler,
                generation: 0,
            },
        );
        surface.update_instances(&self.device, &self.queue, instances);
        surface.update_uniforms(&self.queue, &uniforms);
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let view = texture.create_view(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_viewport(
                17.0,
                19.0,
                uniforms.viewport_width,
                uniforms.viewport_height,
                0.0,
                1.0,
            );
            pass.set_pipeline(self.pipeline.foreground_pipeline());
            pass.set_bind_group(0, &surface.foreground_bind_group, &[]);
            pass.set_vertex_buffer(0, surface.instance_buffer().slice(..));
            pass.draw(0..6, 0..instances.len() as u32);
        }
        let bytes_per_row = (width * 4).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("alignment_readback"),
            size: (bytes_per_row * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        rx.recv().unwrap().unwrap();
        let data = buffer.slice(..).get_mapped_range().unwrap();
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for row in data.chunks(bytes_per_row as usize) {
            pixels.extend_from_slice(&row[..(width * 4) as usize]);
        }
        image::RgbaImage::from_raw(width, height, pixels).unwrap()
    }
}

#[test]
#[ignore = "requires a wgpu adapter; run explicitly for GPU pixel validation"]
fn decorations_stay_on_the_same_row_across_glyph_bounds() {
    let _gpu = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for dual in [false, true] {
        for format in [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        ] {
            let fixture = Fixture::new(dual, format);
            for cell_height in [25.0, 26.4, 26.5, 26.25, 32.0] {
                for (scroll, fixed_start_row) in
                    [(0.0, -1.0), (0.25, -1.0), (0.5, -1.0), (0.37, 12.0)]
                {
                    for style in [
                        CellInstance::UNDERLINE_SINGLE,
                        CellInstance::UNDERLINE_DOUBLE,
                        CellInstance::UNDERLINE_CURLY,
                        CellInstance::UNDERLINE_DOTTED,
                        CellInstance::UNDERLINE_DASHED,
                        CellInstance::FLAG_STRIKETHROUGH,
                    ] {
                        let instances: Vec<_> = (0..24)
                            .flat_map(|row| {
                                // Different ink bounds must not affect the common underline.
                                [
                                    (5.0, 14.0),
                                    (0.0, 30.0),
                                    (-2.0, 32.0),
                                    (4.0, 18.0),
                                    (5.0, 16.0),
                                    (0.0, 0.0),
                                    (-2.0, 32.0),
                                ]
                                .into_iter()
                                .enumerate()
                                .map(
                                    move |(col, (top, height))| CellInstance {
                                        col: col as u32,
                                        row,
                                        glyph_u1: 8.0 / 32.0,
                                        glyph_v1: height / 32.0,
                                        fg_color: [255; 4],
                                        glyph_offset_y: top,
                                        flags: style
                                            | if height > 0.0 {
                                                CellInstance::FLAG_HAS_GLYPH
                                            } else {
                                                0
                                            }
                                            | if col == 6 { CellInstance::FLAG_WIDE } else { 0 },
                                        ..CellInstance::zeroed()
                                    },
                                )
                            })
                            .collect();
                        let uniforms = GridUniforms {
                            viewport_width: 512.0,
                            viewport_height: 960.0,
                            cell_width: 11.55,
                            cell_height,
                            atlas_width: 32.0,
                            atlas_height: 32.0,
                            render_phase: 1.0,
                            scroll_pixel_offset: scroll,
                            fixed_start_row,
                            fixed_pixel_offset: 0.0,
                            viewport_origin: [17.0, 19.0],
                        };
                        let pixels = fixture.render(uniforms, &instances, format);
                        let mut without_ink = instances.clone();
                        for cell in &mut without_ink {
                            cell.flags &= !CellInstance::FLAG_HAS_GLYPH;
                        }
                        let reference_pixels = fixture.render(uniforms, &without_ink, format);
                        assert!(pixels == reference_pixels,
                        "decoration depends on glyph bounds: dual={dual}, format={format:?}, cell_height={cell_height}, scroll={scroll}, fixed={fixed_start_row}, style={style}");
                        let ink_rows = |x| {
                            (19..(19.0 + 24.0 * cell_height - scroll) as u32)
                                .filter(|&y| pixels.get_pixel(x, y)[0] > 127)
                                .collect::<Vec<_>>()
                        };
                        assert!(
                            (18..109).any(|x| !ink_rows(x).is_empty()),
                            "positive control: underlines must be visible"
                        );
                        if style == CellInstance::UNDERLINE_SINGLE
                            || style == CellInstance::UNDERLINE_DOUBLE
                            || style == CellInstance::FLAG_STRIKETHROUGH
                        {
                            let reference = ink_rows(19);
                            for x in 18..(17.0 + 8.0 * 11.55 - 1.0) as u32 {
                                assert_eq!(ink_rows(x), reference,
                            "underline moved/gapped: dual={dual}, format={format:?}, cell_height={cell_height}, scroll={scroll}, x={x}");
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "requires a wgpu adapter; run explicitly for GPU pixel validation"]
fn glyphs_with_different_bearings_share_a_baseline_at_half_pixel_rows() {
    let _gpu = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for dual in [false, true] {
        for format in [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        ] {
            let fixture = Fixture::new(dual, format);
            let heights = [10, 11, 18, 19, 22, 23];
            let mut bitmap = [0; 32 * 32 * 4];
            for (col, height) in heights.into_iter().enumerate() {
                // Each synthetic glyph has one identical horizontal baseline
                // stroke, but a different bounding box and vertical bearing.
                for x in col * 5..col * 5 + 4 {
                    let i = ((height - 1) * 32 + x) * 4;
                    bitmap[i..i + 4].fill(255);
                }
            }
            upload_bitmap(&fixture.queue, &fixture.atlas_texture, &bitmap, 32, 32);
            let instances: Vec<_> = (0..24)
                .flat_map(|row| {
                    heights
                        .into_iter()
                        .enumerate()
                        .map(move |(col, height)| CellInstance {
                            row,
                            col: col as u32,
                            glyph_u0: (col * 5) as f32 / 32.0,
                            glyph_u1: (col * 5 + 4) as f32 / 32.0,
                            glyph_v1: height as f32 / 32.0,
                            glyph_offset_y: 20.0 - height as f32,
                            fg_color: [255; 4],
                            flags: CellInstance::FLAG_HAS_GLYPH,
                            ..CellInstance::zeroed()
                        })
                })
                .collect();
            for cell_height in [25.5, 26.4, 26.5] {
                for scroll in [0.0, 0.25] {
                    let pixels = fixture.render(
                        GridUniforms {
                            viewport_width: 512.0,
                            viewport_height: 960.0,
                            cell_width: 11.55,
                            cell_height,
                            atlas_width: 32.0,
                            atlas_height: 32.0,
                            render_phase: 1.0,
                            scroll_pixel_offset: scroll,
                            fixed_start_row: -1.0,
                            viewport_origin: [17.0, 19.0],
                            ..GridUniforms::zeroed()
                        },
                        &instances,
                        format,
                    );
                    let ink_rows = |x| {
                        (19..900)
                            .filter(|&y| pixels.get_pixel(x, y)[0] > 127)
                            .collect::<Vec<_>>()
                    };
                    let reference = ink_rows(19);
                    assert_eq!(
                        reference.len(),
                        24,
                        "positive control: every baseline stroke is visible"
                    );
                    for col in 1..6 {
                        let x = 17 + (col as f32 * 11.55).round() as u32 + 2;
                        assert_eq!(ink_rows(x), reference,
                            "glyph baselines diverged: dual={dual}, format={format:?}, cell_height={cell_height}, scroll={scroll}, col={col}");
                    }
                }
            }
        }
    }
}
