// Rendering coordination module

use super::state::TerminalApp;
use crate::{command_palette, config, config_panel, layout, search_replace_panel, theme};
use eframe::egui;

impl TerminalApp {
    /// 自适应帧预算：根据帧时间动态调整处理量
    pub fn adjust_frame_budget(&mut self) {
        const TARGET_FRAME_MS: f64 = 16.0; // 目标 60 FPS
        const MIN_BUDGET: usize = 8192; // 最小 8KB
        const MAX_BUDGET: usize = 131072; // 最大 128KB
        const ADJUST_RATE: f64 = 0.1; // 调整速率 10%

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
                    // per-pane 链接缓存:仅当 grid 或滚动变化时重建,避免每帧重做
                    // 链接检测(含逐行 String 分配)。失效条件与单窗格路径一致。
                    let grid_version = terminal_guard.get_grid_version();
                    let scroll_offset = terminal_guard.scroll_offset;
                    let renderer = &mut self.pane_renderers[pane_idx];
                    if grid_version != renderer.cached_links_grid_version
                        || scroll_offset != renderer.cached_links_scroll_offset
                    {
                        let visible_cells = terminal_guard.get_visible_cells();
                        let row_wrapped = terminal_guard.get_visible_row_wrapped();
                        renderer.cached_links = std::sync::Arc::new(
                            self.link_detector
                                .detect_links_in_visible_cells_with_wrapping(
                                    &visible_cells,
                                    &row_wrapped,
                                ),
                        );
                        renderer.cached_links_grid_version = grid_version;
                        renderer.cached_links_scroll_offset = scroll_offset;
                    }
                    // O(1) clone Arc,规避 &mut renderer 与 &renderer.cached_links 借用冲突。
                    let links = renderer.cached_links.clone();

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
                    self.cached_links = self
                        .link_detector
                        .detect_links_in_visible_cells_with_wrapping(&visible_cells, &row_wrapped);
                    self.cached_links_grid_version = grid_version;
                    self.cached_links_scroll_offset = scroll_offset;
                }
                // 在渲染终端之前读取滚轮值和 Ctrl 键状态
                let ctrl_pressed_render = ui.input(|i| i.modifiers.ctrl);

                // 从原始 MouseWheel 事件中提取 delta（因为 smooth_scroll_delta 被 egui 消费了）
                let mut scroll_delta_from_event = 0.0;
                if ctrl_pressed_render {
                    scroll_delta_from_event = ui.input(|i| {
                        i.events
                            .iter()
                            .filter_map(|evt| match evt {
                                egui::Event::MouseWheel {
                                    delta, modifiers, ..
                                } if modifiers.ctrl => Some(delta.y),
                                _ => None,
                            })
                            .sum()
                    });
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
        let search_needs_refresh = if self.search_state.is_open {
            let session_idx = self.session_manager.active_index();
            let grid_version = {
                let session = self.session_manager.get_active_session_mut();
                session.terminal.lock().get_grid_version()
            };
            self.search_state.results_session_idx != Some(session_idx)
                || self.search_state.results_grid_version != Some(grid_version)
        } else {
            false
        };
        if search_needs_refresh {
            self.refresh_search_matches();
        }

        // 搜索面板 UI（浮动窗口，右上角）
        if self.search_state.is_open {
            let screen_rect = ctx.viewport_rect();
            let search_width = (screen_rect.width() - 24.0).clamp(300.0, 520.0);
            let search_height = if self.search_state.error_message.is_some() {
                82.0
            } else {
                52.0
            };
            egui::Window::new("Search")
                .title_bar(false)
                .resizable(false)
                .default_pos(egui::pos2(
                    (screen_rect.right() - search_width - 12.0).max(screen_rect.left() + 12.0),
                    screen_rect.top() + 48.0,
                ))
                .default_size([search_width, search_height])
                .fixed_size([search_width, search_height])
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

                        // Aa / .* 切换按钮:用 selectable_label 表达 on/off 状态。
                        // 切换后需要立刻按新选项重新搜索,否则用户看不到效果。
                        let case_btn = ui
                            .selectable_label(self.search_state.case_sensitive, "Aa")
                            .on_hover_text("区分大小写 (Match Case)");
                        if case_btn.clicked() {
                            self.search_state.case_sensitive = !self.search_state.case_sensitive;
                        }
                        let regex_btn = ui
                            .selectable_label(self.search_state.use_regex, ".*")
                            .on_hover_text("正则表达式 (Regex)");
                        if regex_btn.clicked() {
                            self.search_state.use_regex = !self.search_state.use_regex;
                        }

                        if search_response.changed() || case_btn.clicked() || regex_btn.clicked() {
                            self.refresh_search_matches();
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
                        if ui
                            .button("↑")
                            .on_hover_text("Previous match (Shift+Enter)")
                            .clicked()
                        {
                            self.search_state.prev_match();
                        }
                        if ui.button("↓").on_hover_text("Next match (Enter)").clicked() {
                            self.search_state.next_match();
                        }

                        // 关闭按钮
                        if ui.button("✕").on_hover_text("Close search (Esc)").clicked() {
                            self.search_state.close();
                            self.save_ui_history();
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
            let screen_rect = ctx.viewport_rect();
            let palette_width = (screen_rect.width() - 32.0).clamp(360.0, 720.0);
            let palette_height = (screen_rect.height() - 96.0).clamp(300.0, 520.0);
            let palette_pos = egui::pos2(
                screen_rect.center().x - palette_width / 2.0,
                screen_rect.top() + (screen_rect.height() * 0.12).max(24.0),
            );

            egui::Window::new("Command Palette")
                .title_bar(false)
                .resizable(false)
                .movable(false)
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
                                    crate::theme::Theme::rgb_to_color32(
                                        self.current_theme.tabbar.active_border,
                                    )
                                    .gamma_multiply(0.18)
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
                                                .color(ui.visuals().weak_text_color()),
                                        );
                                    });

                                    // 快捷键显示 — 走 pretty_bindings_for 统一美化:
                                    // 之前直接展示原始小写 "ctrl+shift+f",这里改为 "Ctrl+Shift+F",
                                    // 与帮助面板保持一致。
                                    let pretty = self
                                        .keybindings
                                        .pretty_bindings_for(&cmd_info.command.to_string());
                                    let keybinding_str = if pretty.is_empty() {
                                        "No binding".to_string()
                                    } else {
                                        pretty.join(" / ")
                                    };

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
                                        .color(ui.visuals().weak_text_color()),
                                );
                            }
                        });

                    // 底部提示
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("↑↓ Navigate  Enter Execute  Esc Cancel")
                                .size(10.0)
                                .color(ui.visuals().weak_text_color()),
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

        // 危险粘贴确认弹窗:含换行或超过阈值时,先让用户预览并按"粘贴 / 取消"。
        self.show_paste_confirm_dialog(ctx);

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
                                    if let Err(e) = clipboard.copy(&result) {
                                        log::warn!("{}", e);
                                    }
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

        // 状态 toast(右下角)——把分散在 input/main/window 里写入 status_message
        // 的反馈集中显示;过期由 current_status_for_display 内部判定后清理。
        self.render_status_toast(ctx);

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

    /// 状态消息 toast。固定锚在屏幕右下角,过期后下一帧自动消失。
    /// 之前 status_message 被多处写入却没有渲染端,所有反馈都被悄悄丢弃。
    fn render_status_toast(&mut self, ctx: &egui::Context) {
        let Some(message) = self.current_status_for_display().map(|s| s.to_string()) else {
            return;
        };
        // 临近过期时淡出,避免突兀消失。
        let fade_alpha: f32 = if let Some(deadline) = self.status_expires_at {
            let remaining = deadline
                .saturating_duration_since(std::time::Instant::now())
                .as_secs_f32();
            // 最后 350ms 做线性淡出
            (remaining / 0.35).clamp(0.0, 1.0)
        } else {
            1.0
        };
        if fade_alpha <= 0.0 {
            return;
        }

        let panel_bg = crate::theme::Theme::rgb_to_color32(self.current_theme.ui.panel_bg);
        let border = crate::theme::Theme::rgb_to_color32(self.current_theme.ui.border);
        let text_color = crate::theme::Theme::rgb_to_color32(self.current_theme.ui.text);
        let alpha = (fade_alpha * 230.0) as u8;
        let bg =
            egui::Color32::from_rgba_unmultiplied(panel_bg.r(), panel_bg.g(), panel_bg.b(), alpha);

        egui::Area::new(egui::Id::new("status_toast"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .order(egui::Order::Tooltip)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame {
                    fill: bg,
                    stroke: egui::Stroke::new(1.0, border),
                    corner_radius: egui::CornerRadius::same(8),
                    inner_margin: egui::Margin::symmetric(12, 8),
                    ..Default::default()
                }
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(message)
                            .color(text_color.gamma_multiply(fade_alpha))
                            .size(12.0),
                    );
                });
            });

        // 还在显示期间持续重绘,保证淡出/到期清理及时生效。
        ctx.request_repaint();
    }

    fn show_paste_confirm_dialog(&mut self, ctx: &egui::Context) {
        // Snapshot what we need from the pending paste and decide outside of
        // the dialog closure so we can mutably touch session_manager / shell
        // without holding any borrow on self.pending_paste_confirm.
        let Some(pending) = self.pending_paste_confirm.as_ref() else {
            return;
        };

        let panel_bg = crate::theme::Theme::rgb_to_color32(self.current_theme.ui.panel_bg);
        let text_color = crate::theme::Theme::rgb_to_color32(self.current_theme.ui.text);
        let border = crate::theme::Theme::rgb_to_color32(self.current_theme.ui.border);

        let line_count = pending.text.lines().count();
        let byte_len = pending.text.len();
        // First few lines as a preview; truncate long single lines too.
        let preview: String = pending
            .text
            .lines()
            .take(8)
            .map(|l| {
                if l.len() > 200 {
                    let clipped: String = l.chars().take(200).collect();
                    format!("{}…", clipped)
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let truncated_preview = line_count > 8 || pending.text.len() != preview.len();

        let mut decision: Option<bool> = None;
        // 通过引用让 checkbox 在 self 上持久(对话框可能跨多帧)。
        let mut dont_ask_again = self.paste_dont_ask_again;
        // Some(true) = paste, Some(false) = cancel.
        egui::Window::new("⚠ 确认粘贴")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .frame(egui::Frame {
                fill: panel_bg,
                stroke: egui::Stroke::new(1.0, border),
                corner_radius: egui::CornerRadius::same(10),
                inner_margin: egui::Margin::same(14),
                ..Default::default()
            })
            .show(ctx, |ui| {
                ui.set_max_width(640.0);
                ui.label(
                    egui::RichText::new(format!(
                        "粘贴包含 {} 行 / {} 字节,执行前请确认内容:",
                        line_count, byte_len
                    ))
                    .color(text_color),
                );
                ui.add_space(6.0);
                egui::Frame::group(ui.style())
                    .stroke(egui::Stroke::new(1.0, border))
                    .show(ui, |ui| {
                        ui.set_min_width(600.0);
                        egui::ScrollArea::vertical()
                            .max_height(220.0)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&preview).monospace().color(text_color),
                                    )
                                    .wrap(),
                                );
                                if truncated_preview {
                                    ui.label(
                                        egui::RichText::new("…(预览已截断)").color(text_color),
                                    );
                                }
                            });
                    });
                ui.add_space(8.0);
                ui.checkbox(&mut dont_ask_again, "不再询问(可在配置里重新开启)");
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("取消").clicked() {
                        decision = Some(false);
                    }
                    if ui.button("粘贴").clicked() {
                        decision = Some(true);
                    }
                });
                // Esc / Enter shortcuts.
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    decision = Some(false);
                }
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    decision = Some(true);
                }
            });

        self.paste_dont_ask_again = dont_ask_again;

        let Some(confirmed) = decision else {
            return;
        };
        // 用户做出选择后:若勾选"不再询问"则关掉确认对话框并落盘。
        // 取消粘贴时也尊重选择,符合"我不想再被打扰"的语义。
        if dont_ask_again && self.config.paste_confirm {
            self.config.paste_confirm = false;
            if let Err(e) = self.config.save() {
                eprintln!("[Config] failed to save paste_confirm preference: {}", e);
            }
        }
        self.paste_dont_ask_again = false;
        let pending = self.pending_paste_confirm.take().expect("pending was Some");
        if !confirmed {
            return;
        }
        // 只在仍是同一个 tab 时投递,避免误粘到刚切换过去的会话。
        if self.session_manager.active_index() != pending.session_idx {
            return;
        }
        let bytes = pending.text.into_bytes();
        let paste_bytes = if pending.bracketed {
            crate::wrap_bracketed_paste(bytes)
        } else {
            bytes
        };
        let session = self.session_manager.get_active_session_mut();
        let _ = session.shell.write(&paste_bytes);
    }
}
