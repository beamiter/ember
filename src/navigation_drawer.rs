//! Presentation geometry and stable egui identity for the single navigation body.
pub(crate) fn geometry(
    screen: egui::Rect,
    left: f32,
    top: f32,
    preferred: f32,
) -> Option<egui::Rect> {
    let width = preferred.min((screen.right() - left - 24.0).max(0.0));
    let rect = egui::Rect::from_min_max(
        egui::pos2(left, top),
        egui::pos2(left + width, screen.bottom() - 10.0),
    );
    (width >= 80.0 && rect.height() >= 80.0).then_some(rect)
}

pub(crate) fn body<R>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .id(egui::Id::new("reference-navigation-body")),
        contents,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drawer_bounds_preserve_the_preference_and_leave_the_rail_accessible() {
        let preferred = 312.0;
        for width in [350.0, 550.0, 700.0] {
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 700.0));
            let rect = geometry(screen, 48.0, 42.0, preferred).unwrap();
            assert_eq!(rect.left(), 48.0);
            assert!(screen.contains_rect(rect));
            assert!(rect.width() <= preferred);
            assert!(rect.right() <= width - 24.0);
        }
        assert_eq!(preferred, 312.0);
        assert!(geometry(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 700.0)),
            48.0,
            42.0,
            preferred
        )
        .is_none());
    }
    #[test]
    fn rehosting_a_live_search_preserves_widget_identity_and_text_selection() {
        let ctx = egui::Context::default();
        let mut draft = "same query 中文".to_owned();
        let mut old = None;
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(700.0, 700.0),
            )),
            ..Default::default()
        };
        let mut first_frame = ctx.run_ui(input(), |root| {
            body(root, root.max_rect(), |ui| {
                let output = egui::TextEdit::singleline(&mut draft).show(ui);
                old = Some(output.response.id);
                output.response.request_focus();
                let mut state = output.state;
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(2),
                        egui::text::CCursor::new(6),
                    )));
                state.store(&ctx, output.response.id);
            });
        });
        first_frame.textures_delta.clear();
        let mut second_frame = ctx.run_ui(input(), |root| {
            egui::Area::new(egui::Id::new("a-different-overlay-parent")).show(root.ctx(), |ui| {
                body(
                    ui,
                    egui::Rect::from_min_size(egui::pos2(48.0, 42.0), egui::vec2(312.0, 600.0)),
                    |ui| {
                        let output = egui::TextEdit::singleline(&mut draft).show(ui);
                        assert_eq!(Some(output.response.id), old);
                        let range = output
                            .state
                            .cursor
                            .char_range()
                            .unwrap()
                            .as_sorted_char_range();
                        assert_eq!((range.start.0, range.end.0), (2, 6));
                    },
                );
            });
        });
        second_frame.textures_delta.clear();
        assert_eq!(draft, "same query 中文");
    }
}
