// Input handling module

use super::events::build_keybinding_string;
use super::state::TerminalApp;
use crate::{keybindings, layout};
use eframe::egui;

impl TerminalApp {
    /// 处理命令调色板打开时的输入。返回 true 表示 update 应提前结束本帧。
    pub fn handle_command_palette_input(&mut self, ctx: &egui::Context) -> bool {
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
                                    self.command_palette.close();
                                    // 执行命令
                                    match command {
                                        keybindings::Command::SearchOpen => {
                                            self.search_state.toggle();
                                        }
                                        keybindings::Command::SearchClose => {
                                            self.search_state.close();
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
                                                self.session_manager.close_session(active_idx);
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
                                        // 分屏命令处理
                                        keybindings::Command::TerminalSplitVertical => {
                                            // 垂直分割（左右）
                                            let new_session_idx =
                                                self.create_session_with_current_config(None, None);
                                            let _ =
                                                self.layout_manager.split(new_session_idx, false);
                                            self.status_message = "Split vertically".to_string();
                                            self.schedule_session_save();
                                        }
                                        keybindings::Command::TerminalSplitHorizontal => {
                                            // 水平分割（上下）
                                            let new_session_idx =
                                                self.create_session_with_current_config(None, None);
                                            let _ =
                                                self.layout_manager.split(new_session_idx, true);
                                            self.status_message = "Split horizontally".to_string();
                                            self.schedule_session_save();
                                        }
                                        keybindings::Command::TerminalClosePane => {
                                            // 关闭当前窗格
                                            if let Err(e) = self.layout_manager.close_focused_pane()
                                            {
                                                self.status_message = e;
                                            }
                                        }
                                        keybindings::Command::PaneFocusNext => {
                                            // 切换到下一个窗格
                                            self.layout_manager
                                                .focus_pane(layout::PaneDirection::Next);
                                        }
                                        keybindings::Command::PaneFocusPrev => {
                                            // 切换到前一个窗格
                                            self.layout_manager
                                                .focus_pane(layout::PaneDirection::Prev);
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
                self.render_ui(ctx);
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
                        }
                        keybindings::Command::SessionNew => {
                            let new_idx = self.create_session_with_current_config(None, None);
                            self.session_manager.switch_session(new_idx);
                            self.force_resize_session = true;
                            self.schedule_session_save();
                        }
                        keybindings::Command::SessionClose => {
                            if self.session_manager.len() > 1 {
                                self.session_manager.close_session(active_session_idx);
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
                        // 其他命令在下面处理
                        _ => {}
                    }
                }
            }
        }
        false
    }
}
