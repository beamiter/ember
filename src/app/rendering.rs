// Rendering coordination module

use super::state::TerminalApp;
use crate::{
    command_palette, config, config_panel, keybindings, layout, search, search_replace_panel, theme,
};
use eframe::egui;

impl TerminalApp {
    /// 自适应帧预算：根据帧时间动态调整处理量
    pub fn adjust_frame_budget(&mut self) {
        const TARGET_FRAME_MS: f64 = 16.0; // 目标 60 FPS
        const MIN_BUDGET: usize = 8192;    // 最小 8KB
        const MAX_BUDGET: usize = 131072;  // 最大 128KB
        const ADJUST_RATE: f64 = 0.1;      // 调整速率 10%

        let avg_frame_ms = self.debug_panel.get_avg_frame_time_ms();

        // 只有在有足够帧时间历史时才调整
        if avg_frame_ms > 0.0 {
            let current = self.adaptive_frame_budget as f64;
            let new_budget = if avg_frame_ms < TARGET_FRAME_MS * 0.8 {
                // 帧时间充裕，可以增加预算
                current * (1.0 + ADJUST_RATE)
            } else if avg_frame_ms > TARGET_FRAME_MS * 1.2 {
                // 帧时间紧张，减少预算
                current * (1.0 - ADJUST_RATE)
            } else {
                // 帧时间在目标范围内，保持不变
                current
            };

            self.adaptive_frame_budget = (new_budget as usize).clamp(MIN_BUDGET, MAX_BUDGET);
        }
    }

    pub fn render_terminal_content(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
            // 终端显示区域
            self.renderer.sync_font_metrics(ctx);
            let (cols, rows) = self.renderer.grid_dimensions(ui.available_size());
            crate::debug_log!("[RESIZE] grid_dimensions => {}x{}", cols, rows);

            // 单窗格才按整窗口尺寸 resize 活跃会话;多窗格时各窗格在下方
            // 各自按自己的 rect 尺寸 resize(否则活跃会话会被错误地撑成整窗口大小)。
            let multi_pane = self.layout_manager.panes().len() > 1;
            if !multi_pane && (cols != self.cols || rows != self.rows || self.force_resize_session) {
                let session = self.session_manager.get_active_session_mut();
                let _ = session.shell.resize(cols, rows);
                let mut terminal = session.terminal.lock();
                terminal.on_resize(cols, rows);
                self.cols = cols;
                self.rows = rows;
                if self.force_resize_session {
                    // Session 切换时重置 renderer 的 IME 状态缓存
                    // 这样下一帧会重新发送 IMEAllowed(true)，确保 IME 不会丢失
                    self.renderer.reset_ime_state();
                }
                self.force_resize_session = false;
            }

            // 多窗格支持：如果有多于一个窗格，则进行分屏渲染
            if self.layout_manager.panes().len() > 1 {
                let available_rect = ui.available_rect_before_wrap();

                // 计算窗格矩形
                self.layout_manager.compute_pane_rects(available_rect);

                // 获取所有窗格信息
                let panes = self.layout_manager.panes().to_vec();
                let divider_rect = self.layout_manager.get_divider_rect();

                // 为每个窗格渲染
                for (pane_idx, pane) in panes.iter().enumerate() {
                    if pane_idx >= self.pane_renderers.len() {
                        break;
                    }

                    let session_idx = pane.session_idx;
                    // 按本窗格 rect 的尺寸 resize 该窗格会话的 shell + 终端 grid,
                    // 否则窗格内的 shell 仍以为自己拥有整窗口宽高,导致换行/清屏错乱。
                    let (pane_cols, pane_rows) =
                        self.pane_renderers[pane_idx].grid_dimensions(pane.rect.size());
                    if let Some(session) = self.session_manager.get_session_mut(session_idx) {
                        let mut terminal_guard = session.terminal.lock();
                        if pane_cols != terminal_guard.grid.row_len()
                            || pane_rows != terminal_guard.grid.rows()
                        {
                            terminal_guard.on_resize(pane_cols, pane_rows);
                            let _ = session.shell.resize(pane_cols, pane_rows);
                        }
                        let visible_cells = terminal_guard.get_visible_cells();
                        let row_wrapped = terminal_guard.get_visible_row_wrapped();
                        let links = self
                            .link_detector
                            .detect_links_in_visible_cells_with_wrapping(&visible_cells, &row_wrapped);

                        // 获取当前窗格的渲染器
                        let renderer = &mut self.pane_renderers[pane_idx];

                        // 在指定矩形内渲染（多窗格模式专用方法）
                        renderer.render_in_rect(
                            ui,
                            &mut terminal_guard,
                            self.cursor_visible,
                            &self.search_state,
                            &links,
                            &self.hovered_link,
                            pane.rect,
                        );
                    }
                }

                // 绘制分隔线
                if let Some(divider) = divider_rect {
                    let painter = ui.painter();
                    let divider_color = if self.dragging_divider {
                        crate::theme::Theme::rgb_to_color32(self.current_theme.tabbar.active_border)
                    } else {
                        crate::theme::Theme::rgb_to_color32(self.current_theme.ui.border)
                    };

                    painter.rect_filled(divider, 0.0, divider_color);

                    // 处理分隔线拖拽
                    if ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary)) {
                        if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                            if divider.contains(pos) {
                                self.dragging_divider = true;
                            }
                        }
                    }

                    if self.dragging_divider {
                        if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                            // 计算新的分割比例
                            match self.layout_manager.mode {
                                layout::SplitMode::VerticalSplit { .. } => {
                                    let delta = pos.x - divider.center().x;
                                    let total_width = available_rect.width();
                                    let ratio_delta = delta / total_width * 0.1; // 降低灵敏度
                                    self.layout_manager.adjust_split_ratio(ratio_delta);
                                }
                                layout::SplitMode::HorizontalSplit { .. } => {
                                    let delta = pos.y - divider.center().y;
                                    let total_height = available_rect.height();
                                    let ratio_delta = delta / total_height * 0.1;
                                    self.layout_manager.adjust_split_ratio(ratio_delta);
                                }
                                _ => {}
                            }
                        }

                        if ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary)) {
                            self.dragging_divider = false;
                        }
                    }
                }

                // 点击某个窗格 → 切换输入焦点到该窗格(忽略落在分隔线上的点击,
                // 那是用于拖拽调整比例的)。
                if !self.dragging_divider
                    && ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary))
                {
                    if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                        let on_divider = divider_rect.map(|d| d.contains(pos)).unwrap_or(false);
                        if !on_divider && self.layout_manager.focus_pane_at(pos).is_some() {
                            self.sync_active_session_to_focused_pane();
                        }
                    }
                }
            } else {
                // 单窗格渲染（原有逻辑）
                {
                    let session = self.session_manager.get_active_session_mut();
                    let mut terminal_guard = session.terminal.lock();

                    // 获取链接列表用于渲染（使用缓存）
                    let grid_version = terminal_guard.get_grid_version();
                    let scroll_offset = terminal_guard.scroll_offset;

                    if grid_version != self.cached_links_grid_version
                        || scroll_offset != self.cached_links_scroll_offset
                    {
                        let visible_cells = terminal_guard.get_visible_cells();
                        let row_wrapped = terminal_guard.get_visible_row_wrapped();
                        self.cached_links = self.link_detector.detect_links_in_visible_cells_with_wrapping(&visible_cells, &row_wrapped);
                        self.cached_links_grid_version = grid_version;
                        self.cached_links_scroll_offset = scroll_offset;
                    }
                    // 在渲染终端之前读取滚轮值和 Ctrl 键状态
                    let ctrl_pressed_render = ui.input(|i| i.modifiers.ctrl);

                    // 从原始 MouseWheel 事件中提取 delta（因为 smooth_scroll_delta 被 egui 消费了）
                    let mut scroll_delta_from_event = 0.0;
                    if ctrl_pressed_render {
                        let all_events = ui.input(|i| i.events.clone());
                        for evt in &all_events {
                            if let egui::Event::MouseWheel {
                                delta, modifiers, ..
                            } = evt
                            {
                                if modifiers.ctrl {
                                    scroll_delta_from_event += delta.y;
                                }
                            }
                        }
                    }

                    // Ctrl+滚轮字体缩放（积累事件而不是立即应用）
                    if scroll_delta_from_event != 0.0 && ctrl_pressed_render {
                        let font_size_delta = if scroll_delta_from_event > 0.0 {
                            1.0
                        } else {
                            -1.0
                        };
                        // 积累字体大小变化
                        self.font_size_accumulator += font_size_delta;
                        self.had_ctrl_scroll_last_frame = true;
                    }

                    self.renderer.render(
                        ui,
                        &mut terminal_guard,
                        self.cursor_visible,
                        &self.search_state,
                        &self.cached_links,
                        &self.hovered_link,
                    );
                }
            }
    }

    #[allow(deprecated)]
    pub fn render_floating_panels(&mut self, ctx: &egui::Context) {
        // 搜索面板 UI（浮动窗口，右上角）
        if self.search_state.is_open {
            egui::Window::new("Search")
                .title_bar(false)
                .resizable(false)
                .default_pos(egui::pos2(ctx.available_rect().right() - 350.0, 60.0))
                .default_size([340.0, 50.0])
                .fixed_size([340.0, 50.0])
                .frame(egui::Frame {
                    fill: crate::theme::Theme::rgb_to_color32(self.current_theme.search.bg),
                    stroke: egui::Stroke::new(
                        1.0,
                        crate::theme::Theme::rgb_to_color32(self.current_theme.search.border),
                    ),
                    corner_radius: egui::CornerRadius::same(8),
                    inner_margin: egui::Margin::same(6),
                    ..Default::default()
                })
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        // 搜索输入框
                        ui.label("Search:");
                        let search_response = ui.text_edit_singleline(&mut self.search_state.query);

                        // 自动 focus 搜索框
                        if self.search_state.search_focused {
                            ui.memory_mut(|mem| mem.request_focus(search_response.id));
                            self.search_state.search_focused = false;
                        }

                        if search_response.changed() {
                            // 重新搜索
                            let session = self.session_manager.get_active_session_mut();
                            let terminal = session.terminal.lock();
                            let (matches, error) = search::SearchEngine::search(
                                &terminal.grid,
                                &self.search_state.query,
                                self.search_state.use_regex,
                                self.search_state.case_sensitive,
                            );
                            drop(terminal);
                            self.search_state.matches = matches;
                            self.search_state.error_message = error;
                            self.search_state.current_match_index = 0;
                        }

                        // 显示匹配计数
                        if !self.search_state.matches.is_empty() {
                            ui.label(format!(
                                "{}/{}",
                                self.search_state.current_match_index + 1,
                                self.search_state.matches.len()
                            ));
                        } else if !self.search_state.query.is_empty() {
                            ui.label("No matches");
                        }

                        // 上一个/下一个 按钮
                        if ui.button("↑").clicked() {
                            self.search_state.prev_match();
                        }
                        if ui.button("↓").clicked() {
                            self.search_state.next_match();
                        }

                        // 关闭按钮
                        if ui.button("✕").clicked() {
                            self.search_state.close();
                        }
                    });

                    // 显示错误信息（如正则表达式错误）
                    if let Some(error) = &self.search_state.error_message {
                        ui.label(egui::RichText::new(error).color(egui::Color32::RED));
                    }
                });
        }

        // 命令调色板 UI（中央弹窗）
        if self.command_palette.is_open {
            let screen_rect = ctx.screen_rect();
            let palette_width = 600.0;
            let palette_height = 400.0;
            let palette_pos = egui::pos2(
                (screen_rect.width() - palette_width) / 2.0,
                (screen_rect.height() - palette_height) / 3.0,
            );

            egui::Window::new("Command Palette")
                .title_bar(false)
                .resizable(false)
                .movable(true)
                .default_pos(palette_pos)
                .default_size([palette_width, palette_height])
                .fixed_size([palette_width, palette_height])
                .frame(egui::Frame {
                    fill: crate::theme::Theme::rgb_to_color32(self.current_theme.ui.panel_bg),
                    stroke: egui::Stroke::new(
                        1.0,
                        crate::theme::Theme::rgb_to_color32(self.current_theme.ui.border),
                    ),
                    corner_radius: egui::CornerRadius::same(10),
                    inner_margin: egui::Margin::same(8),
                    ..Default::default()
                })
                .show(ctx, |ui| {
                    // 搜索输入框
                    ui.horizontal(|ui| {
                        ui.label("🔍");
                        let search_response =
                            ui.text_edit_singleline(&mut self.command_palette.search_query);
                        if search_response.changed() {
                            self.command_palette.update_search_results();
                        }
                        if self.command_palette.needs_focus {
                            search_response.request_focus();
                            self.command_palette.needs_focus = false;
                        }
                        if search_response.has_focus()
                            && self.command_palette.search_query.is_empty()
                        {
                            ui.label("Search commands...");
                        }
                    });

                    ui.separator();

                    // 命令列表
                    let results = self.command_palette.get_results();
                    let selected_index = self.command_palette.selected_index;

                    egui::ScrollArea::vertical()
                        .max_height(palette_height - 100.0)
                        .show(ui, |ui| {
                            for (idx, (cmd_info, _score)) in results.iter().enumerate() {
                                let is_selected = idx == selected_index;

                                let bg_color = if is_selected {
                                    egui::Color32::from_rgb(70, 70, 80)
                                } else {
                                    egui::Color32::TRANSPARENT
                                };

                                let item_response = ui.horizontal(|ui| {
                                    let item_rect = ui.available_rect_before_wrap();
                                    ui.painter().rect_filled(item_rect, 2.0, bg_color);

                                    // 分类标签
                                    let category_color = match cmd_info.category {
                                        command_palette::CommandCategory::Session => {
                                            egui::Color32::from_rgb(100, 150, 255)
                                        }
                                        command_palette::CommandCategory::Edit => {
                                            egui::Color32::from_rgb(100, 200, 100)
                                        }
                                        command_palette::CommandCategory::Search => {
                                            egui::Color32::from_rgb(255, 200, 100)
                                        }
                                        command_palette::CommandCategory::Terminal => {
                                            egui::Color32::from_rgb(150, 150, 255)
                                        }
                                        command_palette::CommandCategory::Window => {
                                            egui::Color32::from_rgb(200, 100, 200)
                                        }
                                        command_palette::CommandCategory::Config => {
                                            egui::Color32::from_rgb(200, 180, 100)
                                        }
                                    };

                                    ui.colored_label(
                                        category_color,
                                        format!("[{}]", cmd_info.category),
                                    );

                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new(&cmd_info.name).strong());
                                        ui.label(
                                            egui::RichText::new(&cmd_info.description)
                                                .size(10.0)
                                                .color(egui::Color32::from_rgb(150, 150, 150)),
                                        );
                                    });

                                    // 快捷键显示
                                    let keybinding_str = self
                                        .keybindings
                                        .bindings
                                        .iter()
                                        .find(|(_, cmd)| {
                                            if let Ok(parsed_cmd) =
                                                cmd.parse::<keybindings::Command>()
                                            {
                                                parsed_cmd == cmd_info.command
                                            } else {
                                                false
                                            }
                                        })
                                        .map(|(binding, _)| binding.clone())
                                        .unwrap_or_else(|| "No binding".to_string());

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                egui::RichText::new(keybinding_str)
                                                    .size(10.0)
                                                    .color(egui::Color32::from_rgb(100, 150, 200)),
                                            );
                                        },
                                    );
                                });

                                // Auto-scroll to keep selected item visible
                                if is_selected {
                                    item_response
                                        .response
                                        .scroll_to_me(Some(egui::Align::Center));
                                }

                                ui.separator();
                            }

                            // 如果没有结果
                            if results.is_empty() {
                                ui.label(
                                    egui::RichText::new("No commands found")
                                        .color(egui::Color32::from_rgb(150, 150, 150)),
                                );
                            }
                        });

                    // 底部提示
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("↑↓ Navigate  Enter Execute  Esc Cancel")
                                .size(10.0)
                                .color(egui::Color32::from_rgb(100, 100, 100)),
                        );
                    });
                });
        }

        // 帮助面板 UI（浮动窗口）
        let mut help_open = self.help_panel.is_open;
        self.help_panel.show(
            ctx,
            &mut help_open,
            &self.command_palette,
            &self.keybindings,
            &self.current_theme,
        );
        self.help_panel.is_open = help_open;

        // 配置面板 UI（浮动窗口）
        let config_actions = self.config_panel.show(ctx, &self.current_theme);
        for action in config_actions {
            match action {
                config_panel::ConfigAction::CustomThemeApplied(theme) => {
                    self.current_theme = *theme.clone();
                    self.apply_runtime_config(ctx);
                }
                config_panel::ConfigAction::SaveRequested => {
                    // Apply all buffered edit values to config
                    self.config_panel.apply_to_config(&mut self.config);
                    // Update theme
                    if let Some(t) = theme::Theme::get_theme(&self.config.theme) {
                        self.current_theme = t.clone();
                    }
                    // Apply runtime changes (fonts, GPU, renderer)
                    self.apply_runtime_config(ctx);
                    // Save to file
                    if let Err(e) = self.config.save() {
                        eprintln!("[Config] Failed to save: {}", e);
                    }
                }
                config_panel::ConfigAction::ResetToDefaults => {
                    self.config = config::Config::default();
                    self.current_theme =
                        theme::Theme::get_theme(&self.config.theme).unwrap_or_default();
                    self.apply_runtime_config(ctx);
                    self.config_panel.sync_from_config(&self.config);
                    self.config_panel.edit_debug_overlay = self.debug_panel.is_open;
                    self.schedule_config_save();
                }
                config_panel::ConfigAction::DebugPanelToggled(open) => {
                    self.debug_panel.is_open = open;
                }
            }
        }

        // Find & Replace 面板（对当前选中文本操作）
        if let Some(sr_action) = self.search_replace_panel.show(ctx, &self.current_theme) {
            // 先读取选中文本并释放终端锁，再 mutate panel/clipboard/PTY
            let selection = {
                let session = self.session_manager.get_active_session_mut();
                let terminal = session.terminal.lock();
                terminal.copy_selection()
            };
            match selection {
                Some(text) => {
                    if let Some(result) = self.search_replace_panel.apply(&text) {
                        match sr_action {
                            search_replace_panel::SearchReplaceAction::ReplaceToClipboard => {
                                if let Some(clipboard) = &self.clipboard {
                                    if let Err(e) = clipboard.copy(&result) { log::warn!("{}", e); }
                                }
                            }
                            search_replace_panel::SearchReplaceAction::TypeIntoTerminal => {
                                let session = self.session_manager.get_active_session_mut();
                                let _ = session.shell.write(result.as_bytes());
                            }
                        }
                    }
                }
                None => {
                    self.search_replace_panel.status = "No selection".to_string();
                }
            }
        }

        // Debug overlay panel — only gather stats (and lock the terminal) when open.
        if self.debug_panel.is_open {
            let session = self.session_manager.get_active_session_mut();
            let terminal = session.terminal.lock();
            let grid_cols = terminal.grid.cols();
            let grid_rows = terminal.grid.rows();
            let scrollback_used = terminal.scrollback.len();
            let kitty_images_count = terminal.kitty_graphics.image_count();
            let kitty_memory_mb = terminal.kitty_graphics.image_memory_mb();
            let scrollback_max = terminal.max_scrollback();
            drop(terminal);
            let session_count = self.session_manager.len();
            let pending_output_bytes = self.pending_output.len();
            let texture_cache_size = self.renderer.texture_cache_len();
            let frame_budget_kb = self.adaptive_frame_budget / 1024;
            self.debug_panel.show(
                ctx,
                grid_cols,
                grid_rows,
                session_count,
                scrollback_used,
                scrollback_max,
                kitty_images_count,
                kitty_memory_mb,
                pending_output_bytes,
                texture_cache_size,
                frame_budget_kb,
            );
        }
    }
}
