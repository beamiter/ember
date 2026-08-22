//! The remote host picker: a small floating window over the terminal that
//! turns a `[[remote_hosts]]` entry into a new session.
//!
//! The entries themselves are the family-shared
//! [`jterm_core::jsh_remote::RemoteHostConfig`] — grammar, validation and the
//! argv a tab runs all live in the shared crate, so this file is only the
//! choosing. A host that fails the application gate is shown greyed out with its
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

    /// Draw the picker and return the index of the host the user chose, if any.
    /// Returning the stable config index lets the connection path re-run the
    /// same application gate immediately before it builds argv.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        hosts: &[RemoteHostConfig],
        theme: &Theme,
    ) -> Option<usize> {
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
                    for (index, host) in hosts
                        .iter()
                        .take(crate::config::MAX_REMOTE_HOST_UI_ROWS)
                        .enumerate()
                    {
                        // Run the complete app gate before deriving any UI text.
                        // In particular, do not clone a hostile oversized deploy
                        // draft once per egui frame before discovering it is
                        // invalid.
                        let validation = crate::config::validate_remote_host_at(hosts, index);
                        let display_name = crate::config::remote_host_display_name(host, index);
                        match validation {
                            Ok(_) => {
                                let transport = if host.docker { "docker" } else { "ssh" };
                                let deploy = jterm_core::review_input::safe_inline_display(
                                    if host.deploy.is_empty() {
                                        "off"
                                    } else {
                                        host.deploy.as_str()
                                    },
                                    64,
                                );
                                let label = RichText::new(format!(
                                    "{}  —  {} · deploy {}",
                                    display_name, transport, deploy
                                ))
                                .color(text_color);
                                if ui.button(label).clicked() {
                                    chosen = Some(index);
                                }
                            }
                            Err(problem) => {
                                // Readable, not clickable: the picker is where
                                // a config typo gets discovered.
                                ui.label(
                                    RichText::new(format!("{display_name}  —  {problem}"))
                                        .color(dim_color)
                                        .strikethrough(),
                                );
                            }
                        }
                    }
                    if hosts.len() > crate::config::MAX_REMOTE_HOST_UI_ROWS {
                        ui.label(
                            RichText::new(format!(
                                "{} additional drafts are retained in config.toml but omitted from this bounded view.",
                                hosts.len() - crate::config::MAX_REMOTE_HOST_UI_ROWS
                            ))
                            .color(dim_color)
                            .small(),
                        );
                    }
                    if hosts.is_empty() {
                        ui.label(
                            RichText::new(
                                "No [[remote_hosts]] configured. Add one in \
                                 Settings → Remote, or in config.toml:\n\n\
                                 [[remote_hosts]]\n\
                                 host = \"dev.example.com\"\n\
                                 user = \"yj\"\n\
                                 deploy = \"persist\"\n\
                                 ssh_args = [\"-p\", \"22\"]\n\n\
                                 [[remote_hosts]]\n\
                                 host = \"myubuntu\"  # running container\n\
                                 docker = true\n\
                                 deploy = \"persist\"",
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
