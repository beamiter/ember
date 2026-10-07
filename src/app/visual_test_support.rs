//! Opt-in CPU capture of real egui meshes for headless layout review.
//! This is not a window-system/GPU screenshot or a replacement for native QA.

use std::collections::HashMap;

#[derive(Default)]
pub(super) struct OffscreenCapture {
    textures: HashMap<egui::TextureId, egui::ColorImage>,
}

impl OffscreenCapture {
    pub(super) fn save(
        &mut self,
        ctx: &egui::Context,
        output: &mut egui::FullOutput,
        size: [u32; 2],
        path: &std::path::Path,
    ) {
        for (id, deltas) in output.textures_delta.set.drain() {
            for delta in deltas {
                let egui::ImageData::Color(image) = delta.image;
                if let Some([x, y]) = delta.pos {
                    let texture = self
                        .textures
                        .get_mut(&id)
                        .expect("texture patch has a base");
                    for row in 0..image.size[1] {
                        let start = (y + row) * texture.size[0] + x;
                        texture.pixels[start..start + image.size[0]].copy_from_slice(
                            &image.pixels[row * image.size[0]..(row + 1) * image.size[0]],
                        );
                    }
                } else {
                    self.textures.insert(id, (*image).clone());
                }
            }
        }
        let primitives =
            ctx.tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
        let mut image =
            image::RgbaImage::from_pixel(size[0], size[1], image::Rgba([20, 22, 28, 255]));
        for primitive in primitives {
            let egui::epaint::Primitive::Mesh(mesh) = primitive.primitive else {
                panic!("headless layout fixture must use CPU terminal paint, not GPU callbacks");
            };
            let texture = self
                .textures
                .get(&mesh.texture_id)
                .expect("mesh texture exists");
            let clip = primitive.clip_rect * output.pixels_per_point;
            self.paint_mesh(&mut image, &mesh, texture, clip, output.pixels_per_point);
        }
        for id in output.textures_delta.free.drain() {
            self.textures.remove(&id);
        }
        image.save(path).expect("save headless layout capture");
    }

    fn paint_mesh(
        &self,
        image: &mut image::RgbaImage,
        mesh: &egui::epaint::Mesh,
        texture: &egui::ColorImage,
        clip: egui::Rect,
        scale: f32,
    ) {
        for triangle in mesh.indices.as_chunks::<3>().0 {
            let vertices = [
                mesh.vertices[triangle[0] as usize],
                mesh.vertices[triangle[1] as usize],
                mesh.vertices[triangle[2] as usize],
            ];
            let points = vertices.map(|vertex| vertex.pos * scale);
            let edge = |a: egui::Pos2, b: egui::Pos2, p: egui::Pos2| {
                (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
            };
            let area = edge(points[0], points[1], points[2]);
            if area.abs() < 0.0001 {
                continue;
            }
            let bounds = egui::Rect::from_points(&points).intersect(clip);
            let x0 = bounds.left().floor().max(0.0) as u32;
            let y0 = bounds.top().floor().max(0.0) as u32;
            let x1 = bounds.right().ceil().max(0.0).min(image.width() as f32) as u32;
            let y1 = bounds.bottom().ceil().max(0.0).min(image.height() as f32) as u32;
            for y in y0..y1 {
                for x in x0..x1 {
                    let point = egui::pos2(x as f32 + 0.5, y as f32 + 0.5);
                    if !clip.contains(point) {
                        continue;
                    }
                    let weights = [
                        edge(points[1], points[2], point) / area,
                        edge(points[2], points[0], point) / area,
                        edge(points[0], points[1], point) / area,
                    ];
                    if weights.iter().any(|weight| *weight < 0.0) {
                        continue;
                    }
                    let uv = vertices[0].uv.to_vec2() * weights[0]
                        + vertices[1].uv.to_vec2() * weights[1]
                        + vertices[2].uv.to_vec2() * weights[2];
                    let tx = (uv.x * texture.size[0] as f32).floor().max(0.0) as usize;
                    let ty = (uv.y * texture.size[1] as f32).floor().max(0.0) as usize;
                    let texel = texture.pixels[ty.min(texture.size[1] - 1) * texture.size[0]
                        + tx.min(texture.size[0] - 1)]
                    .to_array();
                    let colors = vertices.map(|vertex| vertex.color.to_array());
                    let mut source = [0.0; 4];
                    for (channel, source_channel) in source.iter_mut().enumerate() {
                        *source_channel = (0..3)
                            .map(|index| colors[index][channel] as f32 * weights[index])
                            .sum::<f32>()
                            * texel[channel] as f32
                            / 255.0;
                    }
                    let destination = image.get_pixel_mut(x, y);
                    for (channel, source_channel) in source.iter().take(3).enumerate() {
                        destination.0[channel] = (*source_channel
                            + destination.0[channel] as f32 * (1.0 - source[3] / 255.0))
                            .round()
                            .clamp(0.0, 255.0)
                            as u8;
                    }
                }
            }
        }
    }
}
