//! Egui adapter for the shared, persistent window title row.
use super::state::TerminalApp;
use crate::theme::{Theme, ThemeExt as _};
use egui::{Align2, Color32, FontId, Rect, Sense, Vec2};

impl TerminalApp {
    pub(crate) fn title_bar_content(&mut self) -> jterm_core::title_bar::Content {
        let tab = self.tabs.active_index();
        let private = self.tabs.flags(tab).private_title;
        let status = self.pane_status(
            self.session_manager.active_index(),
            std::time::Instant::now(),
        );
        let title = self
            .session_manager
            .sessions()
            .get(self.session_manager.active_index())
            .map(|session| {
                if session.metadata.custom_name.is_some() {
                    return status.title.clone();
                }
                let terminal = session.terminal.lock();
                super::window::safe_window_title(&terminal.window_title, &status.title)
            })
            .unwrap_or_default();
        let mode = match self.config.effective_terminal_mode() {
            crate::config::TerminalMode::Normal => "normal",
            crate::config::TerminalMode::Block => "block",
            crate::config::TerminalMode::Chat => "chat",
        };
        jterm_core::title_bar::compose(&jterm_core::title_bar::Snapshot {
            app: "Ember",
            title: &title,
            cwd: status.cwd.as_deref().map(std::path::Path::new),
            mode,
            tab_index: tab,
            tab_count: self.tabs.len(),
            pane_count: self.layout().panes().len(),
            private,
            ..Default::default()
        })
    }

    pub(crate) fn render_window_title_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let content = self.title_bar_content();
        let font_size = self.config.effective_ui_font_size();
        let height = jterm_core::title_bar::height(font_size);
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
        let bg = Theme::rgb_to_color32(self.renderer.theme.tabbar.bg);
        let fg = Theme::rgb_to_color32(self.renderer.theme.tabbar.active_text);
        let muted = Theme::rgb_to_color32(self.renderer.theme.tabbar.inactive_text);
        ui.painter().rect_filled(
            rect,
            0.0,
            Color32::from_rgba_unmultiplied(
                bg.r(),
                bg.g(),
                bg.b(),
                (self.renderer.opacity * 255.0) as u8,
            ),
        );
        let button_width = (font_size + 20.0).max(32.0);
        let caption = Rect::from_min_max(
            rect.min,
            egui::pos2(
                (rect.right() - button_width * 3.0).max(rect.left()),
                rect.bottom(),
            ),
        );
        let workspace_drag = self.pane_drag.is_some() || self.dragging_tab_session_id.is_some();
        let response = ui
            .interact(
                caption,
                ui.id().with("window-caption"),
                Sense::click_and_drag(),
            )
            .on_hover_text(&content.tooltip);
        let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
        if !workspace_drag {
            if response.double_clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
            } else if response.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
        }
        let details = content.details_for_width(rect.width());
        let font = FontId::proportional(font_size);
        let details_galley = ui
            .painter()
            .layout_no_wrap(details.to_string(), font.clone(), muted);
        let details_width = details_galley.size().x.min(caption.width() * 0.58);
        let title_rect = Rect::from_min_max(
            rect.min + egui::vec2(10.0, 0.0),
            egui::pos2(
                (caption.right() - details_width - 16.0).max(rect.left() + 10.0),
                rect.bottom(),
            ),
        );
        paint_label(ui, title_rect, &content.heading, font.clone(), fg);
        if !details.is_empty() {
            let detail_rect = Rect::from_min_max(
                egui::pos2(caption.right() - details_width - 6.0, rect.top()),
                egui::pos2(caption.right() - 6.0, rect.bottom()),
            );
            paint_label(ui, detail_rect, details, font, muted);
        }
        for (index, glyph, help) in [
            (0, "−", "Minimize window"),
            (
                1,
                if maximized { "❐" } else { "□" },
                if maximized {
                    "Restore window"
                } else {
                    "Maximize window"
                },
            ),
            (2, "×", "Close window"),
        ] {
            let button_rect = Rect::from_min_size(
                egui::pos2(caption.right() + index as f32 * button_width, rect.top()),
                egui::vec2(button_width, height),
            );
            let response = ui
                .put(
                    button_rect,
                    egui::Button::new(egui::RichText::new(glyph).size(font_size + 1.0))
                        .frame(false),
                )
                .on_hover_text(help);
            if index == 2 && response.hovered() {
                ui.painter()
                    .rect_filled(button_rect, 3.0, Color32::from_rgb(190, 55, 55));
                ui.painter().text(
                    button_rect.center(),
                    Align2::CENTER_CENTER,
                    "×",
                    FontId::proportional(font_size + 1.0),
                    Color32::WHITE,
                );
            }
            if response.clicked() && !workspace_drag {
                match index {
                    0 => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                    1 => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized)),
                    _ => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                }
            }
        }
    }
}

fn paint_label(ui: &egui::Ui, rect: Rect, text: &str, font: FontId, color: Color32) {
    if rect.width() <= 0.0 {
        return;
    }
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap.max_width = rect.width();
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = ui.painter().layout_job(job);
    ui.painter()
        .with_clip_rect(rect.intersect(ui.clip_rect()))
        .galley(
            egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
}
