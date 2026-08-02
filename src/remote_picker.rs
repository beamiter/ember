//! The remote host picker: a small floating window over the terminal that
//! turns a `[[remote_hosts]]` entry into a new session.
//!
//! The entries themselves are the family-shared
//! [`jterm_core::jsh_remote::RemoteHostConfig`] — grammar, validation and the
//! argv a tab runs all live in the shared crate, so this file is only the
//! choosing. A host that fails validation is shown greyed out with its
//! reason rather than hidden: a typo in the config should be readable in the
//! picker, not silently absent from it.

use egui::{RichText, Window};
use jterm_core::jsh_remote::RemoteHostConfig;

use crate::theme::Theme;
use crate::theme::ThemeExt as _;

#[derive(Debug, Clone, Default)]
pub struct RemotePicker {
    pub is_open: bool,
}

impl RemotePicker {
    pub fn toggle(&mut self) {
        self.is_open = !self.is_open;
    }

    /// Draw the picker and return the host the user chose, if any.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        hosts: &[RemoteHostConfig],
        theme: &Theme,
    ) -> Option<RemoteHostConfig> {
        if !self.is_open {
            return None;
        }
        let mut chosen = None;
        let mut open = true;

        let text_color = Theme::rgb_to_color32(theme.ui.text);
        let dim_color = Theme::rgb_to_color32(theme.ui.text_disabled);
        let panel_bg = Theme::rgb_to_color32(theme.ui.panel_bg);
        let border = Theme::rgb_to_color32(theme.ui.border);

        Window::new("🖧  Remote hosts")
            .open(&mut open)
            .default_size([460.0, 320.0])
            .resizable(true)
            .collapsible(false)
            .frame(egui::Frame {
                fill: panel_bg,
                stroke: egui::Stroke::new(1.0, border),
                corner_radius: egui::CornerRadius::same(10),
                inner_margin: egui::Margin::same(12),
                ..Default::default()
            })
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for host in hosts {
                        let transport = if host.docker { "docker" } else { "ssh" };
                        let deploy = if host.deploy.is_empty() {
                            "off".to_string()
                        } else {
                            host.deploy.clone()
                        };
                        match host.validate() {
                            Ok(()) => {
                                let label = RichText::new(format!(
                                    "{}  —  {} · deploy {}",
                                    host.display_name(),
                                    transport,
                                    deploy
                                ))
                                .color(text_color);
                                if ui.button(label).clicked() {
                                    chosen = Some(host.clone());
                                }
                            }
                            Err(problem) => {
                                // Readable, not clickable: the picker is where
                                // a config typo gets discovered.
                                ui.label(
                                    RichText::new(format!("{}  —  {problem}", host.display_name()))
                                        .color(dim_color)
                                        .strikethrough(),
                                );
                            }
                        }
                    }
                    if hosts.is_empty() {
                        ui.label(
                            RichText::new(
                                "No [[remote_hosts]] configured. Add one to config.toml:\n\n\
                                 [[remote_hosts]]\n\
                                 host = \"myubuntu\"\n\
                                 docker = true\n\
                                 deploy = \"incognito\"",
                            )
                            .color(dim_color)
                            .monospace(),
                        );
                    }
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "deploy off connects plainly; persist/incognito bring jsh along \
                             (the local one when it is static).",
                        )
                        .color(dim_color)
                        .small(),
                    );
                });
            });

        if chosen.is_some() || !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.is_open = false;
        }
        chosen
    }
}
