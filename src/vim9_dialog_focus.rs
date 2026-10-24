//! A pending query-focus request survives the modal's hidden sizing pass.
//!
//! Request before TextEdit processes input, but only acknowledge the request
//! when the field can interact. Hidden/disabled TextEdit surrenders focus.

pub(super) fn request(ui: &egui::Ui, id: egui::Id, pending: &mut bool) {
    if *pending && ui.is_visible() && ui.is_enabled() {
        ui.memory_mut(|memory| memory.request_focus(id));
        *pending = false;
    }
}

/// A provider chip replaces the query, so its caret must follow that prefix.
/// Character indices (not UTF-8 bytes) also cover translated/custom prefixes.
pub(super) fn move_caret_to_end(ctx: &egui::Context, id: egui::Id, query: &str) {
    if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(
            egui::text::CCursor::new(query.chars().count()),
        )));
        state.store(ctx, id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        ctx: egui::Context,
        pending: bool,
        query: String,
        query_id: Option<egui::Id>,
        cancel_id: Option<egui::Id>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                ctx: egui::Context::default(),
                pending: true,
                query: String::new(),
                query_id: None,
                cancel_id: None,
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>, enabled: bool, open: bool) {
            let ctx = self.ctx.clone();
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 800.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    ui.label("Vim buffer");
                    if !open {
                        return;
                    }
                    egui::Modal::new(egui::Id::new("native-vim-query"))
                        .show(&ctx, |ui| {
                            ui.set_width(600.0);
                            ui.heading("Quick Open / Go to line");
                            ui.add_enabled_ui(enabled, |ui| {
                                let id = ui.make_persistent_id("query");
                                self.query_id = Some(id);
                                request(ui, id, &mut self.pending);
                                ui.add(egui::TextEdit::singleline(&mut self.query).id(id));
                                self.cancel_id = Some(ui.button("Cancel").id);
                            });
                        });
                },
            )
            .drop_without_applying_deltas();
        }

        fn owns_query(&self) -> bool {
            self.query_id
                .is_some_and(|id| self.ctx.memory(|memory| memory.has_focus(id)))
        }
    }

    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn cold_modal_retains_focus_request_until_first_interactive_frame() {
        let mut f = Fixture::new();
        f.frame(vec![], true, true);
        assert!(f.pending, "a hidden sizing pass cannot consume the request");
        assert!(!f.owns_query());
        f.frame(vec![egui::Event::Text(">vertical split".into())], true, true);
        assert_eq!(f.query, ">vertical split");
        assert!(!f.pending);
        assert!(f.owns_query());
    }

    #[test]
    fn disabled_modal_defers_focus_without_claiming_text() {
        let mut f = Fixture::new();
        f.frame(vec![], false, true);
        f.frame(vec![egui::Event::Text("ignored".into())], false, true);
        assert!(f.pending);
        assert!(f.query.is_empty());
        assert!(!f.owns_query());
        f.frame(vec![egui::Event::Text("42:8".into())], true, true);
        assert_eq!(f.query, "42:8");
        assert!(f.owns_query());
    }

    #[test]
    fn replacement_query_keeps_unicode_and_same_frame_edit_order() {
        let mut f = Fixture::new();
        f.frame(vec![], true, true);
        f.frame(vec![egui::Event::Text(">format".into())], true, true);
        f.pending = true;
        let modifiers = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
        f.frame(vec![key(egui::Key::A, modifiers), egui::Event::Text("目录/中e\u{301}🙂.rs:42".into())], true, true);
        assert_eq!(f.query, "目录/中e\u{301}🙂.rs:42");
        assert!(f.owns_query());
    }

    #[test]
    fn acknowledged_request_does_not_steal_button_focus() {
        let mut f = Fixture::new();
        f.frame(vec![], true, true);
        f.frame(vec![], true, true);
        let cancel = f.cancel_id.unwrap();
        f.ctx.memory_mut(|memory| memory.request_focus(cancel));
        f.frame(vec![], true, true);
        assert!(f.ctx.memory(|memory| memory.has_focus(cancel)));
        assert!(!f.owns_query());
        assert!(!f.pending);
    }

    #[test]
    fn provider_switch_focus_precedes_text_processing() {
        let mut f = Fixture::new();
        f.frame(vec![], true, true);
        f.frame(vec![], true, true);
        f.ctx.memory_mut(|memory| memory.request_focus(f.cancel_id.unwrap()));
        f.query = ">simplegit ".into();
        f.pending = true;
        move_caret_to_end(&f.ctx, f.query_id.unwrap(), &f.query);
        f.frame(vec![egui::Event::Text("hunk".into())], true, true);
        assert_eq!(f.query, ">simplegit hunk");
        assert!(f.owns_query());
    }

    #[test]
    fn reopening_reacquires_query_without_a_pointer_click() {
        let mut f = Fixture::new();
        f.frame(vec![], true, true);
        f.frame(vec![egui::Event::Text("old".into())], true, true);
        f.ctx.memory_mut(|memory| memory.surrender_focus(f.query_id.unwrap()));
        f.frame(vec![], true, false);
        f.query.clear();
        f.pending = true;
        f.frame(vec![], true, true);
        f.frame(vec![egui::Event::Text("new".into())], true, true);
        assert_eq!(f.query, "new");
        assert!(f.owns_query());
    }
}
