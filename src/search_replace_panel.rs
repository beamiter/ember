// 查找替换面板 UI（引擎逻辑见 search_replace.rs）
//
// 语义：终端 scrollback 是只读程序输出，原地替换无意义。
// 本面板对「当前选中文本」做查找替换，默认把结果复制到剪贴板；
// 也可显式发送到 PTY（不带回车，避免误执行）。

use crate::search_replace::{ReplaceOptions, SearchAndReplaceEngine, SearchConfig};
use crate::theme::Theme;
use eframe::egui;

/// 调用方需要执行的动作（面板本身不持有终端/剪贴板）
pub enum SearchReplaceAction {
    /// 对选中文本替换后复制到剪贴板
    ReplaceToClipboard,
    /// 对选中文本替换后发送到 PTY（不带换行）
    TypeIntoTerminal,
}

pub struct SearchReplacePanel {
    pub is_open: bool,
    pub search_input: String,
    pub replace_input: String,
    pub config: SearchConfig,
    pub options: ReplaceOptions,
    pub status: String,
    pub needs_focus: bool,
}

impl Default for SearchReplacePanel {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchReplacePanel {
    pub fn new() -> Self {
        Self {
            is_open: false,
            search_input: String::new(),
            replace_input: String::new(),
            config: SearchConfig::default(),
            options: ReplaceOptions::default(),
            status: String::new(),
            needs_focus: false,
        }
    }

    pub fn toggle(&mut self) {
        self.is_open = !self.is_open;
        if self.is_open {
            self.needs_focus = true;
        }
    }

    /// 渲染面板。返回调用方需要执行的动作（若用户点击了某个按钮）。
    #[allow(deprecated)]
    pub fn show(&mut self, ctx: &egui::Context, theme: &Theme) -> Option<SearchReplaceAction> {
        if !self.is_open {
            return None;
        }

        let mut action = None;

        egui::Window::new("Find & Replace")
            .title_bar(false)
            .resizable(false)
            .default_pos(egui::pos2(ctx.available_rect().right() - 360.0, 120.0))
            .default_size([350.0, 120.0])
            .frame(egui::Frame {
                fill: Theme::rgb_to_color32(theme.search.bg),
                stroke: egui::Stroke::new(1.0, Theme::rgb_to_color32(theme.search.border)),
                corner_radius: egui::CornerRadius::same(8),
                inner_margin: egui::Margin::same(8),
                ..Default::default()
            })
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Find & Replace").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("✕").clicked() {
                            self.is_open = false;
                        }
                    });
                });

                egui::Grid::new("find_replace_grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Find:");
                        let find_resp = ui.text_edit_singleline(&mut self.search_input);
                        if self.needs_focus {
                            ui.memory_mut(|m| m.request_focus(find_resp.id));
                            self.needs_focus = false;
                        }
                        ui.end_row();

                        ui.label("Replace:");
                        ui.text_edit_singleline(&mut self.replace_input);
                        ui.end_row();
                    });

                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.config.use_regex, "Regex");
                    ui.checkbox(&mut self.config.case_sensitive, "Case");
                    ui.checkbox(&mut self.options.replace_all, "All");
                });

                ui.separator();

                ui.horizontal(|ui| {
                    if ui
                        .button("Replace → Clipboard")
                        .on_hover_text("对选中文本替换后复制到剪贴板")
                        .clicked()
                    {
                        action = Some(SearchReplaceAction::ReplaceToClipboard);
                    }
                    if ui
                        .button("Type into terminal")
                        .on_hover_text("替换后发送到终端（不带回车）")
                        .clicked()
                    {
                        action = Some(SearchReplaceAction::TypeIntoTerminal);
                    }
                });

                if !self.status.is_empty() {
                    ui.label(
                        egui::RichText::new(&self.status)
                            .color(Theme::rgb_to_color32(theme.search.text)),
                    );
                }
            });

        action
    }

    /// 对给定文本执行替换，更新状态，返回替换后的文本（失败返回 None）。
    pub fn apply(&mut self, text: &str) -> Option<String> {
        match SearchAndReplaceEngine::search_and_replace(
            text,
            &self.search_input,
            &self.replace_input,
            &self.config,
            &self.options,
        ) {
            Ok((result, count)) => {
                self.status = format!("{} replacement(s)", count);
                Some(result)
            }
            Err(e) => {
                self.status = e;
                None
            }
        }
    }
}
