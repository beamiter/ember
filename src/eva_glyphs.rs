//! Native glyph shaping and overlay painting, independent of transport and layout.
use egui::{Color32, FontId, Galley, Rect, Ui};
use jterm_core::companion_host::atlas;
use std::sync::Arc;

pub(crate) fn protected_region(
    region: Option<Rect>,
    input_or_ime_or_approval: bool,
    selecting: bool,
) -> Option<Rect> {
    if input_or_ime_or_approval || selecting {
        None
    } else {
        region.filter(|r| r.is_finite() && r.is_positive())
    }
}
pub(crate) fn shape(
    ui: &mut Ui,
    frame: &atlas::Frame,
    skin: atlas::Skin,
    high_contrast: bool,
) -> Arc<Galley> {
    let palette = skin.palette(false);
    let mut job = egui::text::LayoutJob::default();
    for y in 0..frame.height {
        for x in 0..frame.width {
            let cell = &frame.cells[y * frame.width + x];
            let c = palette.color(cell.ink);
            let color = if high_contrast {
                ui.visuals().text_color()
            } else if ui.visuals().dark_mode {
                Color32::from_rgb(c[0], c[1], c[2])
            } else {
                Color32::from_rgb(c[0] / 2, c[1] / 2, c[2] / 2)
            };
            job.append(
                &cell.glyph.to_string(),
                0.0,
                egui::TextFormat {
                    font_id: FontId::monospace(11.0),
                    color,
                    line_height: Some(11.0),
                    ..Default::default()
                },
            );
        }
        if y + 1 < frame.height {
            job.append(
                "\n",
                0.0,
                egui::TextFormat {
                    font_id: FontId::monospace(11.0),
                    line_height: Some(11.0),
                    ..Default::default()
                },
            );
        }
    }
    ui.fonts_mut(|f| f.layout_job(job))
}
/// Paint-only: does not allocate document space or request animation frames.
pub(crate) fn paint(ui: &Ui, safe: Rect, galley: Arc<Galley>) -> Option<Rect> {
    let rect = Rect::from_min_size(safe.right_bottom() - galley.size(), galley.size());
    if !safe.is_finite() || !safe.contains_rect(rect) {
        return None;
    }
    ui.ctx()
        .layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            "eva-companion-overlay".into(),
        ))
        .with_clip_rect(safe)
        .galley(rect.min, galley, Color32::WHITE);
    Some(rect)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_egui_glyphs_all_skins_and_poses_keep_bounds_colors_and_document_height() {
        for skin in atlas::Skin::ALL {
            for pose in atlas::Pose::ALL {
                let ctx = egui::Context::default();
                ctx.set_visuals(egui::Visuals::dark());
                let mut request = atlas::Request::new(skin, pose);
                request.motion = atlas::Motion::Static;
                let frame = atlas::sample(request).unwrap();
                assert!(frame.next_frame_in_ms.is_none());
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(600.0, 500.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        let before = ui.available_rect_before_wrap();
                        let galley = shape(ui, &frame, skin, false);
                        let safe =
                            Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(400.0, 350.0));
                        let composer =
                            Rect::from_min_size(egui::pos2(0.0, 400.0), egui::vec2(600.0, 100.0));
                        let painted = paint(ui, safe, galley).expect("native monospace frame fits");
                        assert!(!painted.intersects(composer));
                        assert_eq!(before, ui.available_rect_before_wrap());
                    },
                );
                // CPU shaping test deliberately does not upload textures to a GPU.
                output.textures_delta.clear();
                let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
                let mut colors = std::collections::HashSet::new();
                let mut vertices = 0;
                for primitive in primitives {
                    if let egui::epaint::Primitive::Mesh(mesh) = primitive.primitive {
                        vertices += mesh.vertices.len();
                        for v in mesh.vertices {
                            colors.insert(v.color.to_array());
                        }
                    }
                }
                assert!(vertices > 20, "{skin:?}/{pose:?}");
                assert!(colors.len() > 2, "native atlas colors retained");
            }
        }
    }
    #[test]
    fn input_ime_approval_and_selection_remove_region() {
        let r = Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(300.0, 300.0),
        ));
        assert!(protected_region(r, true, false).is_none());
        assert!(protected_region(r, false, true).is_none());
        assert_eq!(protected_region(r, false, false), r);
    }
    #[test]
    fn hidden_static_and_reduced_motion_have_no_animation_wake() {
        for skin in atlas::Skin::ALL {
            let mut request = atlas::Request::new(skin, atlas::Pose::Idle);
            request.visible = false;
            assert!(atlas::sample(request).is_none());
            request.visible = true;
            request.motion = atlas::Motion::Static;
            assert!(atlas::sample(request).unwrap().next_frame_in_ms.is_none());
            request.motion = atlas::Motion::Full;
            request.reduced_motion = true;
            assert!(atlas::sample(request).unwrap().next_frame_in_ms.is_none());
        }
    }
    #[test]
    fn too_small_native_surface_does_not_paint() {
        let ctx = egui::Context::default();
        let frame =
            atlas::sample(atlas::Request::new(atlas::Skin::Cat, atlas::Pose::Idle)).unwrap();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let galley = shape(ui, &frame, atlas::Skin::Cat, false);
            assert!(paint(
                ui,
                Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1.0, 1.0)),
                galley
            )
            .is_none());
        });
        output.textures_delta.clear();
    }
}
