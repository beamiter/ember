// Input handling module

use super::events::build_keybinding_string;
use super::state::TerminalApp;
use crate::{config, keybindings, layout};
use eframe::egui;

impl TerminalApp {
    /// 把全局活跃会话切换到当前焦点窗格对应的会话,使键盘输入/复制等
    /// 路由到正确的分屏窗格。focus 变化(分屏、Next/Prev、关闭、点击)后调用。
    pub fn sync_active_session_to_focused_pane(&mut self) {
        if let Some(idx) = self.layout_manager.focused_session_idx() {
            if idx != self.session_manager.active_index() {
                self.session_manager.switch_session(idx);
                // 触发下一帧按目标窗格尺寸重算 grid
                self.force_resize_session = true;
            }
        }
    }

    /// 处理搜索面板打开时的键盘事件（Esc 关闭、Enter 跳转、上下键浏览历史）。
    pub fn handle_search_panel_input(&mut self) {
        if self.search_state.is_open {
            let events_copy = self.frame_events.clone();
            for evt in &events_copy {
                match evt {
                    egui::Event::Key {
                        key,
                        modifiers,
                        pressed,
                        ..
                    } if *pressed => match key {
                        egui::Key::Escape => {
                            self.search_state.close();
                            self.save_ui_history();
                        }
                        egui::Key::Enter => {
                            if !modifiers.shift {
                                self.search_state.next_match();
                            } else {
                                self.search_state.prev_match();
                            }
                        }
                        egui::Key::ArrowUp => {
                            self.search_state.history_prev();
                        }
                        egui::Key::ArrowDown => {
                            self.search_state.history_next();
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
    }

    /// 处理命令调色板打开时的输入。返回 true 表示 update 应提前结束本帧。
    pub fn handle_command_palette_input(&mut self, root_ui: &mut egui::Ui) -> bool {
        // 命令调色板既要消费 egui::Context 上的输入/视口操作,又要触发立即重绘(render_ui)
        // ——后者在 egui 0.35 起需要 &mut Ui。这里克隆 Context(Arc 引用计数,几乎零成本)
        // 同时保留对 root_ui 的可变借用,二者无冲突。
        let ctx_owned = root_ui.ctx().clone();
        let ctx = &ctx_owned;
        if self.command_palette.is_open {
            let events_copy = self.frame_events.clone();
            for evt in &events_copy {
                match evt {
                    egui::Event::Key {
                        key,
                        modifiers: _,
                        pressed,
                        ..
                    } if *pressed => {
                        match key {
                            egui::Key::Escape => {
                                self.command_palette.close();
                            }
                            egui::Key::ArrowUp => {
                                self.command_palette.select_prev();
                            }
                            egui::Key::ArrowDown => {
                                self.command_palette.select_next();
                            }
                            egui::Key::Enter => {
                                if let Some(command) = self.command_palette.get_selected_command() {
                                    self.command_palette.execute_command(command.clone());
                                    // 持久化最近命令,避免 crash/Force-quit 丢失 MRU。
                                    self.save_ui_history();
                                    self.command_palette.close();
                                    // 执行命令
                                    match command {
                                        keybindings::Command::SearchOpen => {
                                            self.search_state.toggle();
                                        }
                                        keybindings::Command::SearchClose => {
                                            self.search_state.close();
                                            self.save_ui_history();
                                        }
                                        keybindings::Command::SessionNew => {
                                            let new_idx =
                                                self.create_session_with_current_config(None, None);
                                            self.session_manager.switch_session(new_idx);
                                            self.force_resize_session = true;
                                            self.schedule_session_save();
                                        }
                                        keybindings::Command::SessionClose => {
                                            if self.session_manager.len() > 1 {
                                                let active_idx =
                                                    self.session_manager.active_index();
                                                self.close_session_synced(active_idx);
                                                self.schedule_session_save();
                                            } else {
                                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                                return true;
                                            }
                                        }
                                        keybindings::Command::TerminalSendEof => {
                                            let session =
                                                self.session_manager.get_active_session_mut();
                                            let _ = session.shell.write(&[0x04]);
                                            // EOF (Ctrl+D)
                                        }
                                        keybindings::Command::SessionNext => {
                                            self.session_manager.switch_to_next_session();
                                            self.force_resize_session = true;
                                        }
                                        keybindings::Command::SessionPrev => {
                                            self.session_manager.switch_to_prev_session();
                                            self.force_resize_session = true;
                                        }
                                        keybindings::Command::SessionJump(n) => {
                                            if n < 9 {
                                                self.session_manager.switch_session(n);
                                                self.force_resize_session = true;
                                            }
                                        }
                                        keybindings::Command::SessionPrevActive => {
                                            if self.session_manager.switch_to_previous_active() {
                                                self.force_resize_session = true;
                                            } else {
                                                self.set_status("No previous session to switch to");
                                            }
                                        }
                                        keybindings::Command::TerminalScrollUp => {
                                            let session =
                                                self.session_manager.get_active_session_mut();
                                            let mut terminal = session.terminal.lock();
                                            if !terminal.is_alt_buffer_active() {
                                                terminal.scroll(3);
                                            }
                                        }
                                        keybindings::Command::TerminalScrollDown => {
                                            let session =
                                                self.session_manager.get_active_session_mut();
                                            let mut terminal = session.terminal.lock();
                                            if !terminal.is_alt_buffer_active() {
                                                terminal.scroll(-3);
                                            }
                                        }
                                        keybindings::Command::TerminalJumpPrevCommand => {
                                            let jumped = {
                                                let session =
                                                    self.session_manager.get_active_session_mut();
                                                let mut terminal = session.terminal.lock();
                                                terminal.jump_to_prev_command()
                                            };
                                            if !jumped {
                                                self.set_status("No previous command mark");
                                            }
                                        }
                                        keybindings::Command::TerminalJumpNextCommand => {
                                            let jumped = {
                                                let session =
                                                    self.session_manager.get_active_session_mut();
                                                let mut terminal = session.terminal.lock();
                                                terminal.jump_to_next_command()
                                            };
                                            if !jumped {
                                                self.set_status("No next command mark");
                                            }
                                        }
                                        // 分屏命令处理
                                        keybindings::Command::TerminalSplitVertical => {
                                            // 垂直分割（左右）
                                            let new_session_idx =
                                                self.create_session_with_current_config(None, None);
                                            let _ =
                                                self.layout_manager.split(new_session_idx, false);
                                            self.sync_active_session_to_focused_pane();
                                            self.set_status("Split vertically");
                                            self.schedule_session_save();
                                        }
                                        keybindings::Command::TerminalSplitHorizontal => {
                                            // 水平分割（上下）
                                            let new_session_idx =
                                                self.create_session_with_current_config(None, None);
                                            let _ =
                                                self.layout_manager.split(new_session_idx, true);
                                            self.sync_active_session_to_focused_pane();
                                            self.set_status("Split horizontally");
                                            self.schedule_session_save();
                                        }
                                        keybindings::Command::TerminalClosePane => {
                                            // 关闭当前窗格
                                            if let Err(e) = self.layout_manager.close_focused_pane()
                                            {
                                                self.set_status(e);
                                            } else {
                                                self.sync_active_session_to_focused_pane();
                                            }
                                        }
                                        keybindings::Command::PaneFocusNext => {
                                            // 切换到下一个窗格
                                            self.layout_manager
                                                .focus_pane(layout::PaneDirection::Next);
                                            self.sync_active_session_to_focused_pane();
                                        }
                                        keybindings::Command::PaneFocusPrev => {
                                            // 切换到前一个窗格
                                            self.layout_manager
                                                .focus_pane(layout::PaneDirection::Prev);
                                            self.sync_active_session_to_focused_pane();
                                        }
                                        keybindings::Command::WindowClose => {
                                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                            return true;
                                        }
                                        keybindings::Command::ConfigOpen => {
                                            self.config_panel.open(&self.config);
                                            self.config_panel.edit_debug_overlay =
                                                self.debug_panel.is_open;
                                        }
                                        keybindings::Command::ConfigClose => {
                                            self.config_panel.close();
                                        }
                                        keybindings::Command::ConfigToggle => {
                                            self.config_panel.toggle(&self.config);
                                        }
                                        keybindings::Command::SidebarToggle => {
                                            self.sidebar.visible = !self.sidebar.visible;
                                            if self.sidebar.visible {
                                                self.sidebar.refresh();
                                            }
                                        }
                                        keybindings::Command::SearchReplaceToggle => {
                                            self.search_replace_panel.toggle();
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }

            // 如果调色板打开，不处理其他快捷键
            if self.command_palette.is_open {
                // 获取命令调色板选中的命令，但不执行（仅在按 Enter 时执行）
                // render_ui 中会显示调色板
                self.render_ui(root_ui);
                return true;
            }
        }
        false
    }

    /// 处理可配置快捷键派发。返回 true 表示请求关闭窗口，update 应提前返回。
    pub fn handle_keybindings(&mut self, ctx: &egui::Context, active_session_idx: usize) -> bool {
        // 收集所有按下的快捷键
        let pressed_keys: Vec<(egui::Key, egui::Modifiers)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|evt| {
                    if let egui::Event::Key {
                        key,
                        modifiers,
                        pressed: true,
                        ..
                    } = evt
                    {
                        Some((*key, *modifiers))
                    } else {
                        None
                    }
                })
                .collect()
        });

        // 处理每个按下的快捷键
        for (key, modifiers) in pressed_keys {
            if let Some(keybinding_str) = build_keybinding_string(key, modifiers) {
                let command = self.keybindings.get_command(&keybinding_str);
                crate::debug_log!(
                    "[KEYBINDING] Looking up: '{}' => {:?}",
                    keybinding_str,
                    command
                );
                if let Some(command) = command {
                    match command {
                        keybindings::Command::SearchOpen => {
                            self.search_state.toggle();
                        }
                        keybindings::Command::SearchClose => {
                            self.search_state.close();
                            self.save_ui_history();
                        }
                        keybindings::Command::SessionNew => {
                            let new_idx = self.create_session_with_current_config(None, None);
                            self.session_manager.switch_session(new_idx);
                            self.force_resize_session = true;
                            self.schedule_session_save();
                        }
                        keybindings::Command::SessionClose => {
                            if self.session_manager.len() > 1 {
                                self.close_session_synced(active_session_idx);
                                self.schedule_session_save();
                            } else {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                return true;
                            }
                        }
                        keybindings::Command::TerminalSendEof => {
                            let session = self.session_manager.get_active_session_mut();
                            let _ = session.shell.write(&[0x04]); // EOF (Ctrl+D)
                        }
                        keybindings::Command::SessionNext => {
                            self.session_manager.switch_to_next_session();
                            self.force_resize_session = true;
                        }
                        keybindings::Command::SessionPrev => {
                            self.session_manager.switch_to_prev_session();
                            self.force_resize_session = true;
                        }
                        keybindings::Command::SessionJump(n) => {
                            if n < 9 {
                                self.session_manager.switch_session(n);
                                self.force_resize_session = true;
                            }
                        }
                        keybindings::Command::SessionPrevActive => {
                            if self.session_manager.switch_to_previous_active() {
                                self.force_resize_session = true;
                            } else {
                                self.set_status("No previous session to switch to");
                            }
                        }
                        keybindings::Command::TerminalScrollUp => {
                            let session = self.session_manager.get_active_session_mut();
                            let mut terminal = session.terminal.lock();
                            if !terminal.is_alt_buffer_active() {
                                terminal.scroll(3);
                            }
                        }
                        keybindings::Command::TerminalScrollDown => {
                            let session = self.session_manager.get_active_session_mut();
                            let mut terminal = session.terminal.lock();
                            if !terminal.is_alt_buffer_active() {
                                terminal.scroll(-3);
                            }
                        }
                        keybindings::Command::TerminalJumpPrevCommand => {
                            let session = self.session_manager.get_active_session_mut();
                            let mut terminal = session.terminal.lock();
                            if !terminal.jump_to_prev_command() {
                                self.status_message = "No previous command mark".to_string();
                            }
                        }
                        keybindings::Command::TerminalJumpNextCommand => {
                            let session = self.session_manager.get_active_session_mut();
                            let mut terminal = session.terminal.lock();
                            if !terminal.jump_to_next_command() {
                                self.status_message = "No next command mark".to_string();
                            }
                        }
                        keybindings::Command::TerminalSplitVertical => {
                            let new_session_idx =
                                self.create_session_with_current_config(None, None);
                            let _ = self.layout_manager.split(new_session_idx, false);
                            self.sync_active_session_to_focused_pane();
                            self.set_status("Split vertically");
                            self.schedule_session_save();
                        }
                        keybindings::Command::TerminalSplitHorizontal => {
                            let new_session_idx =
                                self.create_session_with_current_config(None, None);
                            let _ = self.layout_manager.split(new_session_idx, true);
                            self.sync_active_session_to_focused_pane();
                            self.set_status("Split horizontally");
                            self.schedule_session_save();
                        }
                        keybindings::Command::TerminalClosePane => {
                            if let Err(e) = self.layout_manager.close_focused_pane() {
                                if self.session_manager.len() > 1 {
                                    self.close_session_synced(active_session_idx);
                                    self.schedule_session_save();
                                } else {
                                    self.set_status(e);
                                }
                            } else {
                                self.sync_active_session_to_focused_pane();
                            }
                        }
                        keybindings::Command::PaneFocusNext => {
                            self.layout_manager.focus_pane(layout::PaneDirection::Next);
                            self.sync_active_session_to_focused_pane();
                        }
                        keybindings::Command::PaneFocusPrev => {
                            self.layout_manager.focus_pane(layout::PaneDirection::Prev);
                            self.sync_active_session_to_focused_pane();
                        }
                        keybindings::Command::WindowClose => {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            return true;
                        }
                        keybindings::Command::ConfigOpen => {
                            self.config_panel.open(&self.config);
                            self.config_panel.edit_debug_overlay = self.debug_panel.is_open;
                        }
                        keybindings::Command::ConfigClose => {
                            self.config_panel.close();
                        }
                        keybindings::Command::ConfigToggle => {
                            self.config_panel.toggle(&self.config);
                        }
                        keybindings::Command::SidebarToggle => {
                            self.sidebar.visible = !self.sidebar.visible;
                            if self.sidebar.visible {
                                self.sidebar.refresh();
                            }
                        }
                        keybindings::Command::SearchReplaceToggle => {
                            self.search_replace_panel.toggle();
                        }
                        // 复制/粘贴等命令由 main.rs 后续专用路径处理，避免在这里提前吞掉。
                        _ => continue,
                    }
                    return true;
                }
            }
        }
        false
    }

    /// 处理 IME 事件、窗口标题更新；返回当前是否存在预编辑文本。
    pub fn handle_ime_events(&mut self, ctx: &egui::Context) -> bool {
        let session = self.session_manager.get_active_session_mut();

        // Step 1: 处理 IME 事件
        for evt in &self.frame_events {
            if let egui::Event::Ime(ime_event) = evt {
                let mut terminal = session.terminal.lock();
                #[allow(deprecated)]
                match ime_event {
                    egui::ImeEvent::Enabled => {
                        crate::debug_log!("[IME] Enabled");
                        terminal.ime_enabled = true;
                    }
                    egui::ImeEvent::Preedit { text, .. } => {
                        crate::debug_log!("[IME] Preedit: {:?}", text);
                        // egui 0.35 起 Enabled/Disabled 不再触发,改由 Preedit 的 text 是否为空来表达
                        // IME 活跃状态:非空 = 输入中,空 = 已退出。
                        if text.is_empty() {
                            terminal.ime_enabled = false;
                            terminal.clear_preedit();
                        } else {
                            terminal.ime_enabled = true;
                            // 光标位置用字符数而非字节数:CJK 预编辑文本每字符多字节,
                            // 用 byte len 会让光标落到错误(过大)的位置。
                            let cursor = text.chars().count();
                            terminal.set_preedit(text.clone(), cursor);
                        }
                    }
                    egui::ImeEvent::Commit(text) => {
                        crate::debug_log!("[IME] Commit: {:?}", text);
                        terminal.clear_preedit();
                        if !text.is_empty() {
                            let _ = session.shell.write(text.as_bytes());
                        }
                        // 不要在 commit 时置 ime_enabled = false
                        // commit 只是确认一个字/词，不代表用户要退出中文输入模式
                        // 只有 ImeEvent::Disabled 才是真正的 IME 关闭信号
                    }
                    egui::ImeEvent::Disabled => {
                        crate::debug_log!("[IME] Disabled");
                        terminal.ime_enabled = false;
                        terminal.clear_preedit();
                    }
                }
            }
        }
        // 使用 terminal 持久状态判断是否有预编辑，而不是帧局部变量
        // 这样即使跨帧也能正确抑制 Text 事件
        let has_preedit = {
            let terminal = session.terminal.lock();
            !terminal.preedit_text.is_empty()
        };

        let window_title = {
            let terminal = session.terminal.lock();
            terminal.window_title.clone()
        };
        if !window_title.is_empty() && window_title != self.last_window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(window_title.clone()));
            self.last_window_title = window_title;
        }
        has_preedit
    }

    /// 处理累积的 Ctrl+滚轮字体缩放。
    pub fn handle_font_zoom(&mut self, ctx: &egui::Context) {
        let mut keyboard_delta = 0.0;
        let mut reset_font_size = false;
        ctx.input(|i| {
            for evt in &i.events {
                if let egui::Event::Key {
                    key,
                    modifiers,
                    pressed: true,
                    ..
                } = evt
                {
                    if modifiers.ctrl && !modifiers.alt {
                        match key {
                            egui::Key::Plus => keyboard_delta += 1.0,
                            egui::Key::Equals if !modifiers.shift => keyboard_delta += 1.0,
                            egui::Key::Minus if !modifiers.shift => keyboard_delta -= 1.0,
                            egui::Key::Num0 if !modifiers.shift => reset_font_size = true,
                            _ => {}
                        }
                    }
                }
            }
        });

        if reset_font_size || keyboard_delta != 0.0 {
            let target_size = if reset_font_size {
                config::Config::default().font_size
            } else {
                self.config.font_size + keyboard_delta
            };
            let new_font_size = config::Config::clamp_font_size(target_size);
            if (new_font_size - self.config.font_size).abs() > 0.01 {
                self.config.font_size = new_font_size;
                self.apply_runtime_config(ctx);
                self.schedule_config_save();
            }
            self.font_size_accumulator = 0.0;
        }

        // Step 1.5: 处理累积的Ctrl+滚轮字体缩放
        // 检查是否有ctrl+scroll事件
        let has_ctrl_scroll_this_frame = {
            let ctrl_pressed = ctx.input(|i| i.modifiers.ctrl);
            ctrl_pressed && self.frame_events.iter().any(|evt| {
                matches!(evt, egui::Event::MouseWheel { modifiers, .. } if modifiers.ctrl)
            })
        };

        // 如果有累积值，并且（滚轮事件停止 或 累积超过1.0），则应用变化
        if self.font_size_accumulator.abs() > 0.0 {
            let should_apply = !has_ctrl_scroll_this_frame // 滚轮停止
                || self.font_size_accumulator.abs() >= 1.0; // 或累积超过1.0

            if should_apply {
                let steps = self.font_size_accumulator.floor() as i32;
                if steps != 0 {
                    let new_font_size =
                        config::Config::clamp_font_size(self.config.font_size + steps as f32);

                    if (new_font_size - self.config.font_size).abs() > 0.01 {
                        self.config.font_size = new_font_size;
                        self.apply_runtime_config(ctx);
                        self.schedule_config_save();
                    }

                    // 保留小数部分
                    self.font_size_accumulator -= steps as f32;
                }

                // 如果滚轮停止，清空累积器
                if !has_ctrl_scroll_this_frame {
                    self.font_size_accumulator = 0.0;
                }
            }
        }

        self.had_ctrl_scroll_last_frame = has_ctrl_scroll_this_frame;
    }
}
