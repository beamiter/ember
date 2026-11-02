//! Explicit SSH read-only viewer; no local conversation actions are exposed.
#[derive(Default)]
pub(crate) struct RemotePanel {
    pub(crate) open: bool,
    pub(crate) alias: String,
    pub(crate) executable: String,
    pub(crate) host_id: String,
    pub(crate) query: String,
    pub(crate) generation: u64,
    pub(crate) target: String,
    pub(crate) reader: crate::workspace_remote::RemoteWorkspace,
}
impl crate::app::state::TerminalApp {
    pub(crate) fn render_workspace_remote(&mut self, ctx: &egui::Context) {
        if !self.agent_sessions.remote.open {
            return;
        }
        let panel = &mut self.agent_sessions.remote;
        let mut open = panel.open;
        let size = ctx.viewport_rect().size();
        egui::Window::new("Remote workspace · read-only").open(&mut open)
            .id(egui::Id::new("remote-workspace-viewer"))
            .default_width((size.x - 40.0).clamp(220.0, 720.0))
            .max_size(egui::vec2((size.x - 24.0).max(200.0), (size.y - 60.0).max(180.0)))
            .vscroll(true).show(ctx, |ui| {
                ui.label("Connect to an already running workspace host. SSH identity and protocol are checked before reading its catalog.");
                ui.weak("SSH config alias"); ui.text_edit_singleline(&mut panel.alias);
                ui.weak("Absolute remote frontend executable"); ui.text_edit_singleline(&mut panel.executable);
                ui.weak("Expected host ID"); ui.text_edit_singleline(&mut panel.host_id);
                ui.small("Obtain the expected ID on that host with: frontend --jterm-workspace-host-id. Known-host verification and noninteractive SSH authentication remain enabled.");
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Connect read-only").clicked() {
                        panel.generation = panel.generation.wrapping_add(1);
                        panel.target = format!("{} · {}", panel.alias.trim(), panel.host_id.trim());
                        panel.query.clear();
                        panel.reader.connect(&panel.alias, &panel.executable, &panel.host_id);
                    }
                    if ui.button("Disconnect").clicked() {
                        panel.generation = panel.generation.wrapping_add(1);
                        panel.reader.disconnect(); panel.query.clear(); panel.target.clear();
                    }
                });
                if let Some(notice) = &panel.reader.notice { ui.label(notice); }
                if panel.reader.loading || panel.reader.search_loading || panel.reader.document_loading { ui.spinner(); ctx.request_repaint_after(std::time::Duration::from_millis(100)); }
                if !panel.reader.connected { return; }
                ui.separator();
                ui.strong(format!("Verified remote · {}", panel.target));
                ui.small("Read-only: viewing does not resume a session or launch work. Context saves, queue writes, SSH task execution and DLC execution are unavailable here.");
                if ui.add(egui::TextEdit::singleline(&mut panel.query).hint_text("Search remote saved evidence").desired_width(f32::INFINITY)).changed() {
                    panel.reader.set_search(&panel.query, panel.reader.conversation().cloned());
                    ctx.request_repaint_after(std::time::Duration::from_millis(180));
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.button("All remote sessions").clicked() { panel.query.clear(); panel.reader.set_search("", None); }
                    if ui.add_enabled(!panel.reader.loading, egui::Button::new("Refresh catalog")).clicked() { panel.reader.refresh(); }
                });
                ui.push_id(("remote-connection", panel.generation), |ui| {
                    if panel.reader.searching() {
                        for hit in panel.reader.page.hits.clone() {
                            ui.push_id((&hit.row.id.0, &hit.anchor.event_id, &hit.anchor.revision), |ui| {
                                if ui.button(format!("{} · {}\n{}", crate::review_text::visible_bounded(&hit.row.title, 100), hit.source.label(), crate::review_text::visible_bounded(&hit.snippet, 220))).clicked() { panel.reader.open_hit(hit.clone()); }
                            });
                        }
                        ui.weak(panel.reader.page.coverage.label());
                        if panel.reader.page.next_before.is_some() && ui.button("More evidence").clicked() { panel.reader.load_more_search(); }
                    } else {
                        ui.weak(format!("{} of {} remote sessions loaded", panel.reader.rows.len(), panel.reader.total));
                        for row in panel.reader.rows.clone() {
                            ui.push_id(&row.id.0, |ui| {
                                if ui.button(format!("{}\n{} · {}", crate::review_text::visible_bounded(&row.title, 100), row.provider, row.presentation().status_label)).clicked() { panel.reader.set_search(&panel.query, Some(row.id.clone())); }
                            });
                        }
                        if panel.reader.next_before.is_some() && ui.button("More sessions").clicked() { panel.reader.load_more_catalog(); }
                    }
                    if let Some(document) = &panel.reader.document {
                        ui.separator(); ui.strong("Remote saved evidence");
                        ui.weak(format!("{} · {} · revision {}", document.source.label(), document.event_id, document.revision));
                        ui.label(crate::review_text::visible_multiline_bounded(&document.text, 256 * 1024));
                        if document.truncated { ui.weak("Retained excerpt; the source contains more."); }
                        if ui.button("Close remote evidence").clicked() { panel.reader.clear_document(); }
                    }
                });
            });
        panel.open = open;
    }
}
