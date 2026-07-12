//! 快捷键帮助面板模块

use egui::{Color32, RichText, Window};

use crate::command_palette::{CommandCategory, CommandPalette};
use crate::keybindings::KeyBindings;
use crate::theme::Theme;

/// 帮助面板状态
#[derive(Debug, Clone)]
pub struct HelpPanel {
    pub is_open: bool,
}

impl HelpPanel {
    pub fn new() -> Self {
        HelpPanel { is_open: false }
    }

    pub fn toggle(&mut self) {
        self.is_open = !self.is_open;
    }

    /// 渲染帮助面板：实时从命令列表 + 当前快捷键绑定生成，跟随主题配色
    pub fn show(
        &self,
        ctx: &egui::Context,
        open: &mut bool,
        palette: &CommandPalette,
        keybindings: &KeyBindings,
        theme: &Theme,
    ) {
        if !*open {
            return;
        }

        let text_color = Theme::rgb_to_color32(theme.ui.text);
        let dim_color = Theme::rgb_to_color32(theme.ui.text_disabled);
        let panel_bg = Theme::rgb_to_color32(theme.ui.panel_bg);
        let border = Theme::rgb_to_color32(theme.ui.border);

        let categories = [
            CommandCategory::Session,
            CommandCategory::Edit,
            CommandCategory::Search,
            CommandCategory::Terminal,
            CommandCategory::Window,
            CommandCategory::Config,
        ];

        Window::new("⌨  Keybindings")
            .open(open)
            .default_size([560.0, 520.0])
            .resizable(true)
            .vscroll(true)
            .frame(egui::Frame {
                fill: panel_bg,
                stroke: egui::Stroke::new(1.0, border),
                corner_radius: egui::CornerRadius::same(10),
                inner_margin: egui::Margin::same(12),
                ..Default::default()
            })
            .show(ctx, |ui| {
                ui.heading(RichText::new("JTerm2 快捷键").size(18.0).color(text_color));
                ui.label(
                    RichText::new("从这里开始：用命令面板发现功能，用本页查找快捷键。")
                        .size(12.0)
                        .color(dim_color),
                );
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    ui.add_sized(
                        [150.0, 18.0],
                        egui::Label::new(
                            RichText::new("Ctrl+Shift+P").monospace().color(text_color),
                        ),
                    );
                    ui.label(RichText::new("打开命令面板，搜索全部功能").color(text_color));
                });
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [150.0, 18.0],
                        egui::Label::new(RichText::new("Ctrl+?").monospace().color(text_color)),
                    );
                    ui.label(RichText::new("打开或关闭本快捷键帮助").color(text_color));
                });
                ui.separator();

                for category in categories {
                    let cat_color = Self::category_color(category, theme);

                    // 该类别下所有命令:有绑定的排前,未绑定的标灰显示在后面,
                    // 让用户也能发现"可通过命令面板执行的能力",而不是被快捷键
                    // 配置筛掉。
                    let mut bound: Vec<(String, String)> = Vec::new();
                    let mut unbound: Vec<String> = Vec::new();
                    for c in palette
                        .all_commands()
                        .iter()
                        .filter(|c| c.category == category)
                    {
                        let binds = keybindings.pretty_bindings_for(&c.command.to_string());
                        if binds.is_empty() {
                            unbound.push(c.name.clone());
                        } else {
                            bound.push((c.name.clone(), binds.join(" / ")));
                        }
                    }
                    bound.sort();
                    unbound.sort();

                    if bound.is_empty() && unbound.is_empty() {
                        continue;
                    }

                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(category.to_string())
                            .size(13.0)
                            .strong()
                            .color(cat_color),
                    );
                    ui.separator();

                    for (name, keys) in bound {
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [150.0, 18.0],
                                egui::Label::new(RichText::new(keys).monospace().color(cat_color)),
                            );
                            ui.label(RichText::new(name).color(text_color));
                        });
                    }
                    for name in unbound {
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [150.0, 18.0],
                                egui::Label::new(
                                    RichText::new("(未绑定)").monospace().color(dim_color),
                                ),
                            );
                            ui.label(RichText::new(name).color(dim_color));
                        });
                    }
                }

                ui.add_space(10.0);
                ui.label(
                    RichText::new("💡 更多命令见命令调色板 (Ctrl+Shift+P)")
                        .size(11.0)
                        .color(dim_color),
                );
            });
    }

    fn category_color(category: CommandCategory, theme: &Theme) -> Color32 {
        let p = &theme.palette;
        let rgb = match category {
            CommandCategory::Session => p.session_color,
            CommandCategory::Edit => p.edit_color,
            CommandCategory::Search => p.search_color,
            CommandCategory::Terminal => p.terminal_color,
            CommandCategory::Window => p.window_color,
            CommandCategory::Config => p.window_color,
        };
        Theme::rgb_to_color32(rgb)
    }
}

impl Default for HelpPanel {
    fn default() -> Self {
        Self::new()
    }
}
