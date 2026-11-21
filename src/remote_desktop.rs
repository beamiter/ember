//! Ember's native controls for the JWM viewer.
use crate::jwm_remote_session::{
    ClipboardDirection, ClipboardMode, ClipboardOptions, ConnectionOptions, RemoteDesktop, Scaling,
    Viewer, CLIPBOARD_FILE_LIMIT_BYTES, TICK,
};

#[derive(Default)]
pub(crate) struct Panel {
    options: ConnectionOptions,
    clipboard: Option<(ClipboardMode, ClipboardDirection)>,
    session: RemoteDesktop,
    initial_focus: bool,
}

impl Panel {
    pub(crate) fn new() -> Self {
        Self {
            initial_focus: true,
            ..Self::default()
        }
    }

    /// Return true when the modal is dismissed. Dropping the panel closes stdin
    /// and lets the viewer release input and finish its authenticated session.
    pub(crate) fn show(&mut self, ctx: &egui::Context) -> bool {
        let was_active = self.session.active();
        self.session.refresh();
        if self.session.active() {
            ctx.request_repaint_after(TICK);
        } else if was_active {
            self.options.reconnect_automatically = false;
        }
        let editable = !self.session.active();
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("jwm_remote_desktop"))
            .frame(crate::theme::workspace_frame(12))
            .show(ctx, |ui| {
                ui.set_min_width(380.0);
                ui.heading("Remote Desktop");
                ui.add_enabled_ui(editable, |ui| {
                    let previous_viewer = self.options.backend;
                    egui::Grid::new("jwm_remote_settings").show(ui, |ui| {
                        ui.label("Host");
                        let address = ui.add(
                            egui::TextEdit::singleline(&mut self.options.address)
                                .hint_text("Host or IP address")
                                .desired_width(230.0)
                                .char_limit(255),
                        );
                        if std::mem::take(&mut self.initial_focus) {
                            address.request_focus();
                        }
                        ui.end_row();

                        ui.label("Key File");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.options.key_file)
                                .hint_text("Default JWM key")
                                .desired_width(230.0),
                        );
                        ui.end_row();

                        ui.label("Viewer");
                        egui::ComboBox::from_id_salt("jwm_viewer")
                            .selected_text(self.options.backend.to_string())
                            .show_ui(ui, |ui| {
                                for viewer in Viewer::CHOICES {
                                    ui.selectable_value(
                                        &mut self.options.backend,
                                        viewer,
                                        viewer.to_string(),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("Scaling");
                        egui::ComboBox::from_id_salt("jwm_scaling")
                            .selected_text(self.options.scaling.to_string())
                            .show_ui(ui, |ui| {
                                for scale in Scaling::CHOICES {
                                    ui.selectable_value(
                                        &mut self.options.scaling,
                                        scale,
                                        scale.to_string(),
                                    );
                                }
                            });
                        ui.end_row();
                    });
                    if self.options.backend != previous_viewer {
                        self.options.control = false;
                        self.clipboard = None;
                    }
                    ui.checkbox(&mut self.options.control, "Allow keyboard and pointer");
                    if self.options.control && self.options.backend == Viewer::Wayland {
                        ui.checkbox(
                            &mut self.options.wayland_gestures,
                            "Modifier clicks and dragging",
                        );
                    } else {
                        self.options.wayland_gestures = false;
                    }
                    if self.options.backend == Viewer::X11 {
                        let mut enabled = self.clipboard.is_some();
                        if ui.checkbox(&mut enabled, "Share clipboard").changed() {
                            self.clipboard = enabled
                                .then_some((ClipboardMode::Text, ClipboardDirection::FromHost));
                        }
                        if let Some((mode, direction)) = self.clipboard.as_mut() {
                            ui.horizontal(|ui| {
                                egui::ComboBox::from_id_salt("jwm_clipboard_mode")
                                    .selected_text(mode.to_string())
                                    .show_ui(ui, |ui| {
                                        for choice in ClipboardMode::CHOICES {
                                            ui.selectable_value(mode, choice, choice.to_string());
                                        }
                                    });
                                egui::ComboBox::from_id_salt("jwm_clipboard_direction")
                                    .selected_text(direction.to_string())
                                    .show_ui(ui, |ui| {
                                        for choice in ClipboardDirection::CHOICES {
                                            ui.selectable_value(
                                                direction,
                                                choice,
                                                choice.to_string(),
                                            );
                                        }
                                    });
                            });
                            ui.small("Both sides must grant clipboard sharing.");
                            ui.small("Sending to the host can include the current clipboard.");
                            if *mode == ClipboardMode::All {
                                ui.small("File sharing is limited to 2 GiB per copy.");
                            }
                        }
                    }
                    if self
                        .options
                        .backend
                        .allows_reconnect(self.options.control, self.clipboard.is_some())
                    {
                        ui.checkbox(
                            &mut self.options.reconnect_automatically,
                            "Reconnect automatically",
                        );
                    } else {
                        self.options.reconnect_automatically = false;
                    }
                });

                ui.separator();
                ui.label(self.session.status());
                let mut selected = None;
                if let Some(current) = self.session.current_source() {
                    ui.add_enabled_ui(!self.session.closing(), |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Source");
                            egui::ComboBox::from_id_salt("jwm_source")
                                .selected_text(&current.label)
                                .show_ui(ui, |ui| {
                                    for source in self.session.sources() {
                                        if ui
                                            .selectable_label(source == current, &source.label)
                                            .clicked()
                                        {
                                            selected = Some(source.clone());
                                        }
                                    }
                                });
                        });
                    });
                }
                if let Some(source) = selected {
                    self.session.select_source(&source);
                }
                if !self.session.metrics().is_empty() {
                    ui.label(self.session.metrics());
                }
                let clipboard = self.session.clipboard_summary();
                if !clipboard.is_empty() {
                    ui.label(clipboard);
                }
                if !self.session.feedback().is_empty() {
                    ui.label(self.session.feedback());
                }

                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(editable, egui::Button::new("Connect"))
                        .clicked()
                    {
                        let mut options = self.options.clone();
                        options.clipboard = self
                            .clipboard
                            .filter(|_| options.backend == Viewer::X11)
                            .map(|(mode, direction)| {
                                let file_limit = if mode == ClipboardMode::All {
                                    CLIPBOARD_FILE_LIMIT_BYTES
                                } else {
                                    0
                                };
                                ClipboardOptions::new(mode, direction, file_limit)
                            });
                        self.session.connect(options);
                    }
                    if let Some(selection) = self
                        .session
                        .current_source()
                        .filter(|_| self.session.can_request_input())
                        .map(|source| source.selection.clone())
                    {
                        if ui.button("Request Input").clicked() {
                            self.session.request_input(&selection);
                        }
                    } else if ui
                        .add_enabled(
                            self.session.can_release_input(),
                            egui::Button::new("Release Input"),
                        )
                        .clicked()
                    {
                        self.session.release_input();
                    }
                    if ui
                        .add_enabled(
                            !editable && !self.session.closing(),
                            egui::Button::new("Disconnect"),
                        )
                        .clicked()
                    {
                        self.session.disconnect();
                    }
                    close = ui.button("Close").clicked();
                });
            });
        if self.session.active() {
            ctx.request_repaint_after(TICK);
        }
        close || response.should_close()
    }
}
