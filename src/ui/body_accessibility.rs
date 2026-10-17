//! Conservative, read-only semantics for renderer-submitted terminal rows.
//!
//! This is semantic render evidence, not proof of compositor presentation. No
//! history scan, search-result string, cached frame text, or filesystem output.
use egui::{Id, Rect, Ui};

#[allow(clippy::too_many_arguments)] // Explicit layout and provenance inputs; no stored body buffer.
pub(super) fn row(
    ui: &Ui,
    surface: Id,
    raw_row: u64,
    revision: u64,
    rect: Rect,
    clip: Rect,
    text: &str,
) {
    if raw_row == 0
        || !rect.is_finite()
        || !rect.is_positive()
        || !clip.contains_rect(rect)
        || !ui.is_rect_visible(rect)
        || text.is_empty()
        || text.len() > 1024
        || !text.bytes().all(|b| (32..=126).contains(&b))
    {
        return;
    }
    let frame = ui.ctx().cumulative_frame_nr();
    let id = surface.with(("terminal-submitted-row", raw_row));
    // Hover-only widgets cannot take keyboard focus or consume terminal clicks.
    let response = ui.interact(rect, id, egui::Sense::hover());
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), text));
    ui.ctx().accesskit_node_builder(id, |node| {
        node.set_description(format!(
            "Terminal submitted raw row; row={raw_row}; revision={revision}; frame={frame}"
        ));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(ctx: &egui::Context, text: Option<&str>, clip: Rect) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(300.0, 100.0),
                )),
                ..Default::default()
            },
            |ui| {
                if let Some(text) = text {
                    row(
                        ui,
                        Id::new("body"),
                        17,
                        3,
                        Rect::from_min_size(egui::pos2(12.0, 12.0), egui::vec2(180.0, 20.0)),
                        clip,
                        text,
                    );
                }
            },
        );
        output.textures_delta.clear();
        output
    }
    fn labels(output: &egui::FullOutput) -> Vec<&str> {
        output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .filter_map(|(_, node)| node.value())
            .collect()
    }
    #[test]
    fn submitted_row_is_read_only_and_has_frame_identity() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let out = frame(&ctx, Some("E03CAPTURED ROW000017"), Rect::EVERYTHING);
        let node = out
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|(_, n)| n.value() == Some("E03CAPTURED ROW000017"))
            .unwrap();
        assert_eq!(node.1.role(), egui::accesskit::Role::Label);
        assert!(!node.1.supports_action(egui::accesskit::Action::Click));
        assert!(!node.1.supports_action(egui::accesskit::Action::Focus));
        assert!(node
            .1
            .description()
            .unwrap()
            .starts_with("Terminal submitted raw row; row=17; revision=3; frame="));
    }
    #[test]
    fn clipped_offscreen_and_non_ascii_text_are_not_exposed() {
        for (text, clip) in [
            (
                "ROW000017",
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(190.0, 100.0)),
            ),
            (
                "ROW000017",
                Rect::from_min_max(egui::pos2(0.0, 40.0), egui::pos2(300.0, 100.0)),
            ),
            ("ROW000017\nspoof", Rect::EVERYTHING),
            ("ROW000017\u{202e}", Rect::EVERYTHING),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            assert!(!labels(&frame(&ctx, Some(text), clip)).contains(&text));
        }
    }
    #[test]
    fn previous_frame_row_is_removed_when_body_is_withheld() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let initial = frame(&ctx, Some("ROW000017"), Rect::EVERYTHING);
        assert!(labels(&initial).contains(&"ROW000017"));
        let after = frame(&ctx, None, Rect::EVERYTHING);
        assert!(!labels(&after).contains(&"ROW000017"));
        // egui sends the rebuilt root child list; stale nodes must be unreachable.
        let id = Id::new("body")
            .with(("terminal-submitted-row", 17u64))
            .accesskit_id();
        assert!(after
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .all(|(_, n)| !n.children().contains(&id)));
    }
}
