// Input handling module

use super::events::build_keybinding_string;
use super::state::TerminalApp;
use crate::{config, keybindings, layout, search};
use eframe::egui;

/// Central input-routing decision for UI surfaces that own keyboard input.
/// Keep this pure so regressions (especially Enter/Escape leaking into the PTY)
/// can be covered without constructing a PTY-backed [`TerminalApp`].
pub(crate) fn should_block_terminal_input(
    search_open: bool,
    config_open: bool,
    replace_open: bool,
    paste_confirmation_open: bool,
    command_palette_open: bool,
    text_edit_focused: bool,
) -> bool {
    search_open
        || config_open
        || replace_open
        || paste_confirmation_open
        || command_palette_open
        || text_edit_focused
}

pub(crate) fn routed_terminal_events(
    events: &[egui::Event],
    terminal_input_blocked: bool,
) -> Vec<egui::Event> {
    if terminal_input_blocked {
        Vec::new()
    } else {
        events.to_vec()
    }
}

/// Legacy viewport scrolling is intentionally limited to PageUp/PageDown.
/// Ctrl+Up/Ctrl+Down are configurable commands and must not also be handled by
/// a second hard-coded path.
pub(crate) fn viewport_scroll_delta(
    key: egui::Key,
    modifiers: egui::Modifiers,
    rows: usize,
) -> Option<isize> {
    match key {
        egui::Key::PageUp if !modifiers.ctrl => Some(rows as isize),
        egui::Key::PageDown if !modifiers.ctrl => Some(-(rows as isize)),
        _ => None,
    }
}

pub(crate) fn ctrl_wheel_zoom_delta(events: &[egui::Event]) -> f32 {
    let total: f32 = events
        .iter()
        .filter_map(|event| match event {
            egui::Event::MouseWheel {
                delta, modifiers, ..
            } if modifiers.ctrl && !modifiers.alt => Some(delta.y),
            _ => None,
        })
        .sum();

    // `Iterator::sum::<f32>()` uses -0.0 as its empty-sum identity, and
    // `(-0.0_f32).signum()` is -1.0. Calling `signum` directly would therefore
    // zoom out once on every frame that contains no Ctrl+wheel input.
    if total > 0.0 {
        1.0
    } else if total < 0.0 {
        -1.0
    } else {
        0.0
    }
}

impl TerminalApp {
    pub(crate) fn terminal_input_blocked(&self, ctx: &egui::Context) -> bool {
        should_block_terminal_input(
            self.search_state.is_open,
            self.config_panel.is_open,
            self.search_replace_panel.is_open,
            self.pending_paste_confirm.is_some(),
            self.command_palette.is_open,
            ctx.text_edit_focused(),
        )
    }

    fn copy_active_selection(&mut self) {
        let selected = {
            let session = self.session_manager.get_active_session_mut();
            session.terminal.lock().copy_selection()
        };

        let Some(text) = selected else {
            self.set_status("Nothing selected");
            return;
        };
        let char_count = text.chars().count();
        match self
            .clipboard
            .as_ref()
            .map(|clipboard| clipboard.copy(&text))
        {
            Some(Ok(())) => self.set_status(format!("Copied {} characters", char_count)),
            Some(Err(error)) => self.set_status_for(
                format!("Copy failed: {}", error),
                std::time::Duration::from_secs(4),
            ),
            None => self.set_status("Clipboard is unavailable"),
        }
    }

    fn paste_active_clipboard(&mut self) {
        let Some(clipboard) = &self.clipboard else {
            self.set_status("Clipboard is unavailable");
            return;
        };
        let content = match clipboard.paste_contents() {
            Ok(content) => content,
            Err(error) => {
                self.set_status_for(
                    format!("Paste failed: {}", error),
                    std::time::Duration::from_secs(4),
                );
                return;
            }
        };

        match content {
            crate::clipboard::ClipboardContent::Text(text) => {
                let session = self.session_manager.get_active_session_mut();
                match crate::paste_text_into_session(
                    session,
                    text,
                    self.config.paste_confirm,
                    &mut self.pending_paste_confirm,
                ) {
                    Ok(true) => {}
                    Ok(false) => self.set_status("Clipboard contains no text"),
                    Err(error) => self.set_status_for(
                        format!("Paste failed: {error}"),
                        std::time::Duration::from_secs(4),
                    ),
                }
            }
            crate::clipboard::ClipboardContent::Binary(_) => self.set_status_for(
                "Image paste requires an OSC 5522-aware application",
                std::time::Duration::from_secs(4),
            ),
        }
    }

    /// The one execution path for commands, independent of whether they came
    /// from a configurable keybinding or the command palette. `true` means the
    /// application requested that the viewport close.
    pub(crate) fn dispatch_command(
        &mut self,
        ctx: &egui::Context,
        command: keybindings::Command,
    ) -> bool {
        match command {
            keybindings::Command::SessionNew => {
                let new_idx = self.create_session_with_current_config(None, None);
                self.activate_session(new_idx);
                self.schedule_session_save();
            }
            keybindings::Command::SessionClose => {
                if self.session_manager.len() > 1 {
                    let active_idx = self.session_manager.active_index();
                    self.close_session_synced(active_idx);
                    self.schedule_session_save();
                } else {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return true;
                }
            }
            keybindings::Command::SessionNext => self.activate_next_session(),
            keybindings::Command::SessionPrev => self.activate_prev_session(),
            keybindings::Command::SessionJump(index) => {
                if !self.activate_session(index) {
                    self.set_status(format!("Session {} is not available", index + 1));
                }
            }
            keybindings::Command::SessionLast => {
                if let Some(last_index) = self.session_manager.len().checked_sub(1) {
                    self.activate_session(last_index);
                }
            }
            keybindings::Command::SessionPrevActive => {
                if !self.activate_previous_session() {
                    self.set_status("No previous session to switch to");
                }
            }
            keybindings::Command::EditCopy => self.copy_active_selection(),
            keybindings::Command::EditPaste => self.paste_active_clipboard(),
            keybindings::Command::SearchOpen => {
                self.search_state.open();
                self.refresh_search_matches();
            }
            keybindings::Command::SearchClose => {
                self.search_state.close();
                self.save_ui_history();
            }
            keybindings::Command::SearchNext => self.search_state.next_match(),
            keybindings::Command::SearchPrev => self.search_state.prev_match(),
            keybindings::Command::SearchHistoryPrev => {
                self.search_state.history_prev();
                self.refresh_search_matches();
            }
            keybindings::Command::SearchHistoryNext => {
                self.search_state.history_next();
                self.refresh_search_matches();
            }
            keybindings::Command::SearchReplaceToggle => self.search_replace_panel.toggle(),
            keybindings::Command::TerminalSendSigint => {
                if !self
                    .session_manager
                    .get_active_session_mut()
                    .queue_input(&[0x03])
                {
                    self.set_status("Terminal input retry buffer is full");
                }
            }
            keybindings::Command::TerminalSendEof => {
                if !self
                    .session_manager
                    .get_active_session_mut()
                    .queue_input(&[0x04])
                {
                    self.set_status("Terminal input retry buffer is full");
                }
            }
            keybindings::Command::TerminalClear => {
                if !self
                    .session_manager
                    .get_active_session_mut()
                    .queue_input(&[0x0c])
                {
                    self.set_status("Terminal input retry buffer is full");
                }
            }
            keybindings::Command::TerminalScrollUp => {
                let mut terminal = self
                    .session_manager
                    .get_active_session_mut()
                    .terminal
                    .lock();
                if !terminal.is_alt_buffer_active() {
                    terminal.scroll(3);
                }
            }
            keybindings::Command::TerminalScrollDown => {
                let mut terminal = self
                    .session_manager
                    .get_active_session_mut()
                    .terminal
                    .lock();
                if !terminal.is_alt_buffer_active() {
                    terminal.scroll(-3);
                }
            }
            keybindings::Command::TerminalJumpPrevMark => {
                let jumped = self
                    .session_manager
                    .get_active_session_mut()
                    .terminal
                    .lock()
                    .jump_to_prev_command();
                if !jumped {
                    self.set_status("No previous command mark");
                }
            }
            keybindings::Command::TerminalJumpNextMark => {
                let jumped = self
                    .session_manager
                    .get_active_session_mut()
                    .terminal
                    .lock()
                    .jump_to_next_command();
                if !jumped {
                    self.set_status("No next command mark");
                }
            }
            keybindings::Command::TerminalSplitVertical => self.split_terminal(false),
            keybindings::Command::TerminalSplitHorizontal => self.split_terminal(true),
            keybindings::Command::TerminalClosePane => self.close_focused_pane_or_session(),
            keybindings::Command::PaneFocusNext => {
                if !self.layout_manager.focus_pane(layout::PaneDirection::Next) {
                    self.set_status("Only one pane is open");
                }
                self.sync_active_session_to_focused_pane();
            }
            keybindings::Command::PaneFocusPrev => {
                if !self.layout_manager.focus_pane(layout::PaneDirection::Prev) {
                    self.set_status("Only one pane is open");
                }
                self.sync_active_session_to_focused_pane();
            }
            keybindings::Command::PaneFocusLeft => {
                self.focus_physical_pane(layout::PaneDirection::Left, "left")
            }
            keybindings::Command::PaneFocusRight => {
                self.focus_physical_pane(layout::PaneDirection::Right, "right")
            }
            keybindings::Command::PaneFocusUp => {
                self.focus_physical_pane(layout::PaneDirection::Up, "above")
            }
            keybindings::Command::PaneFocusDown => {
                self.focus_physical_pane(layout::PaneDirection::Down, "below")
            }
            keybindings::Command::PaneResizeLeft => {
                self.resize_pane(layout::PaneDirection::Left, "left")
            }
            keybindings::Command::PaneResizeRight => {
                self.resize_pane(layout::PaneDirection::Right, "right")
            }
            keybindings::Command::PaneResizeUp => self.resize_pane(layout::PaneDirection::Up, "up"),
            keybindings::Command::PaneResizeDown => {
                self.resize_pane(layout::PaneDirection::Down, "down")
            }
            keybindings::Command::WindowClose => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return true;
            }
            keybindings::Command::CommandPaletteToggle => {
                if self.command_palette.is_open {
                    self.command_palette.close();
                    self.set_status("命令面板已关闭");
                } else {
                    self.command_palette.open();
                    self.set_status("命令面板已打开，直接输入即可搜索命令");
                }
            }
            keybindings::Command::HelpToggle => {
                self.help_panel.toggle();
                self.set_status(if self.help_panel.is_open {
                    "快捷键帮助已打开，按 Ctrl+Shift+/ 可关闭"
                } else {
                    "快捷键帮助已关闭"
                });
            }
            keybindings::Command::ConfigOpen => {
                self.config_panel.open(&self.config);
                self.config_panel.edit_debug_overlay = self.debug_panel.is_open;
            }
            keybindings::Command::ConfigClose => self.config_panel.close(),
            keybindings::Command::ConfigToggle => self.config_panel.toggle(&self.config),
            keybindings::Command::DebugToggle => {
                self.debug_panel.toggle();
                self.set_status("Debug overlay toggled");
            }
            keybindings::Command::SidebarToggle => {
                self.sidebar.visible = !self.sidebar.visible;
                if self.sidebar.visible {
                    self.sidebar.refresh();
                }
            }
        }
        false
    }

    pub(crate) fn dispatch_palette_command(
        &mut self,
        ctx: &egui::Context,
        command: keybindings::Command,
    ) -> bool {
        self.command_palette.execute_command(command.clone());
        if command == keybindings::Command::CommandPaletteToggle {
            let close_requested = self.dispatch_command(ctx, command);
            self.save_ui_history();
            return close_requested;
        }
        self.command_palette.close();
        self.save_ui_history();
        self.dispatch_command(ctx, command)
    }

    /// 切换活跃会话并同步分屏布局。若目标已在某个窗格中则聚焦它，否则
    /// 将目标显示在当前焦点窗格，避免 tab 高亮、键盘输入和可见内容分离。
    pub fn activate_session(&mut self, index: usize) -> bool {
        let target_session_id = self
            .session_manager
            .sessions()
            .get(index)
            .map(|session| session.metadata.session_id.clone());
        if !self.session_manager.switch_session(index) {
            return false;
        }
        self.layout_manager.show_session(index);
        self.force_resize_session = true;
        self.smooth_scroll_velocity = 0.0;
        self.smooth_scroll_pixel_offset = 0.0;
        // Application mouse reporting remains routed to the press-time PTY.
        // A local text selection cannot safely continue after its pane/tab is
        // replaced, so cancel it while retaining capture until button-up; this
        // also prevents PRIMARY from being overwritten by the new session.
        let cancelled_local_terminal = self
            .terminal_mouse_capture
            .as_mut()
            .filter(|capture| {
                !capture.reported_to_app && target_session_id.as_ref() != Some(&capture.session_id)
            })
            .map(|capture| {
                capture.local_selection_cancelled = true;
                std::sync::Arc::clone(&capture.terminal)
            });
        if let Some(terminal) = cancelled_local_terminal {
            terminal.lock().selection = None;
            self.renderer.cancel_local_selection_capture();
            for renderer in &mut self.pane_renderers {
                renderer.cancel_local_selection_capture();
            }
        } else if self.terminal_mouse_capture.is_none() {
            self.last_terminal_mouse_motion = None;
        }
        self.renderer.scroll_pixel_offset = 0.0;
        self.renderer.cursor_move_input.clear();
        self.renderer.cursor_move_terminal_ptr = None;
        for renderer in &mut self.pane_renderers {
            renderer.scroll_pixel_offset = 0.0;
            renderer.cursor_move_input.clear();
            renderer.cursor_move_terminal_ptr = None;
        }
        if self.search_state.is_open {
            self.refresh_search_matches();
        }
        true
    }

    /// 针对当前活跃会话重算搜索结果，并记录结果所属的 grid/session 版本。
    pub(super) fn refresh_search_matches(&mut self) {
        let session_idx = self.session_manager.active_index();
        let (matches, error, grid_version) = {
            let session = self.session_manager.get_active_session_mut();
            let terminal = session.terminal.lock();
            let (matches, error) = search::SearchEngine::search(
                &terminal.grid,
                &self.search_state.query,
                self.search_state.use_regex,
                self.search_state.case_sensitive,
            );
            (matches, error, terminal.get_grid_version())
        };
        self.search_state.matches = matches;
        self.search_state.error_message = error;
        self.search_state.current_match_index = 0;
        self.search_state.results_grid_version = Some(grid_version);
        self.search_state.results_session_idx = Some(session_idx);
    }

    fn activate_next_session(&mut self) {
        let index = self.session_manager.switch_to_next_session();
        self.activate_session(index);
    }

    fn activate_prev_session(&mut self) {
        let index = self.session_manager.switch_to_prev_session();
        self.activate_session(index);
    }

    fn activate_previous_session(&mut self) -> bool {
        if !self.session_manager.switch_to_previous_active() {
            return false;
        }
        let index = self.session_manager.active_index();
        self.activate_session(index)
    }

    fn focus_physical_pane(&mut self, direction: layout::PaneDirection, label: &str) {
        if self.layout_manager.focus_pane(direction) {
            self.sync_active_session_to_focused_pane();
        } else {
            self.set_status(format!("No pane {label}"));
        }
    }

    fn resize_pane(&mut self, direction: layout::PaneDirection, label: &str) {
        const RESIZE_STEP: f32 = 0.05;
        if !self.layout_manager.resize_split(direction, RESIZE_STEP) {
            self.set_status(format!("Cannot resize pane {label}"));
        }
    }

    /// 关闭 pane 时同时关闭它拥有的 shell session。旧行为只从布局中摘掉
    /// pane，却把 PTY 留成隐藏 tab，既泄漏后台进程，也让 split 看起来像
    /// 在拼接已有 session。
    fn close_focused_pane_or_session(&mut self) {
        if self.layout_manager.panes().len() > 1 {
            let Some(closing_session_idx) = self.layout_manager.focused_session_idx() else {
                self.set_status("No focused pane to close");
                return;
            };
            if let Err(error) = self.layout_manager.close_focused_pane() {
                self.set_status(error);
                return;
            }

            // 先激活折叠后留下的 pane，再删除原 session。这样 SessionManager
            // 删除索引时会继续跟踪同一个 PTY，close_session_synced 也只需做
            // 常规的索引平移，不会用隐藏 tab 替换当前可见 pane。
            self.sync_active_session_to_focused_pane();
            if self.close_session_synced(closing_session_idx) {
                self.set_status("Closed pane and session");
                self.schedule_session_save();
            }
            return;
        }

        if self.session_manager.len() > 1 {
            let active_idx = self.session_manager.active_index();
            if self.close_session_synced(active_idx) {
                self.schedule_session_save();
            }
        } else {
            self.set_status("Cannot close the last pane");
        }
    }

    /// 创建一个全新的 shell session，并从当前焦点 pane 原地分出新 pane。
    /// session 创建失败时不改变布局，布局更新失败时回滚刚创建的 session。
    fn split_terminal(&mut self, horizontal: bool) {
        if !self.layout_manager.can_split() {
            self.set_status("No focused pane to split");
            return;
        }

        let active_idx = self.session_manager.active_index();
        self.layout_manager.show_session(active_idx);
        let old_len = self.session_manager.len();
        let new_session_idx = self.create_session_with_current_config(None, None);
        if self.session_manager.len() == old_len {
            self.set_status("Failed to create session for split");
            return;
        }

        match self.layout_manager.split(new_session_idx, horizontal) {
            Ok(()) => {
                self.sync_active_session_to_focused_pane();
                self.set_status(if horizontal {
                    "Created new session in horizontal split"
                } else {
                    "Created new session in vertical split"
                });
                self.schedule_session_save();
            }
            Err(error) => {
                // 若布局状态意外变化，回滚刚创建的 session。
                self.close_session_synced(new_session_idx);
                self.set_status(error);
            }
        }
    }

    /// 把全局活跃会话切换到当前焦点窗格对应的会话,使键盘输入/复制等
    /// 路由到正确的分屏窗格。focus 变化(分屏、Next/Prev、关闭、点击)后调用。
    pub fn sync_active_session_to_focused_pane(&mut self) {
        if let Some(idx) = self.layout_manager.focused_session_idx() {
            if idx != self.session_manager.active_index() {
                self.activate_session(idx);
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

    /// Handle palette-owned keys without ending the frame. PTY parsing,
    /// protocol replies and persistence must continue while the overlay stays
    /// open, otherwise a full event channel back-pressures the foreground job.
    /// Returns `(close_viewport, palette_owned_this_frame)`.
    pub fn handle_command_palette_input(&mut self, ctx: &egui::Context) -> (bool, bool) {
        if !self.command_palette.is_open {
            return (false, false);
        }

        let events_copy = self.frame_events.clone();
        let mut selected_command = None;
        for evt in &events_copy {
            let egui::Event::Key {
                key, pressed: true, ..
            } = evt
            else {
                continue;
            };
            match key {
                egui::Key::Escape => self.command_palette.close(),
                egui::Key::ArrowUp => self.command_palette.select_prev(),
                egui::Key::ArrowDown => self.command_palette.select_next(),
                egui::Key::Enter => {
                    selected_command = self.command_palette.get_selected_command();
                    break;
                }
                _ => {}
            }
        }

        let close_requested = selected_command
            .map(|command| self.dispatch_palette_command(ctx, command))
            .unwrap_or(false);
        (close_requested, true)
    }

    /// 处理可配置快捷键派发。返回 true 仅表示 update 应提前返回
    /// （例如请求关闭窗口）。普通快捷键会从本帧事件里移除，避免继续透传给 PTY，
    /// 但仍允许本帧继续渲染，防止透明窗口被 clear 后空一帧。
    pub fn handle_keybindings(
        &mut self,
        ctx: &egui::Context,
        terminal_input_blocked: bool,
    ) -> bool {
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
                    if terminal_input_blocked {
                        let modal_command = match command {
                            keybindings::Command::SearchClose
                            | keybindings::Command::SearchNext
                            | keybindings::Command::SearchPrev
                            | keybindings::Command::SearchHistoryPrev
                            | keybindings::Command::SearchHistoryNext => self.search_state.is_open,
                            keybindings::Command::ConfigClose
                            | keybindings::Command::ConfigToggle => self.config_panel.is_open,
                            keybindings::Command::CommandPaletteToggle => {
                                self.command_palette.is_open
                            }
                            keybindings::Command::SearchReplaceToggle => {
                                self.search_replace_panel.is_open
                            }
                            _ => false,
                        };
                        if !modal_command {
                            continue;
                        }
                    }
                    self.frame_events.retain(|evt| {
                        !matches!(
                            evt,
                            egui::Event::Key {
                                key: event_key,
                                modifiers: event_modifiers,
                                pressed: true,
                                ..
                            } if *event_key == key && *event_modifiers == modifiers
                        )
                    });
                    return self.dispatch_command(ctx, command);
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
                        drop(terminal);
                        if !text.is_empty() && !session.queue_input(text.as_bytes()) {
                            log::warn!("terminal input retry buffer full; IME commit retained by neither PTY nor UI");
                            self.status_message =
                                "终端输入重试缓冲区已满，IME 文本未发送".to_string();
                            self.status_expires_at =
                                Some(std::time::Instant::now() + std::time::Duration::from_secs(4));
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

        // Route Ctrl+wheel here for both single- and multi-pane layouts. The
        // terminal scroll paths explicitly ignore the same events below.
        self.font_size_accumulator += ctrl_wheel_zoom_delta(&self.frame_events);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_ui_surfaces_block_terminal_input() {
        assert!(!should_block_terminal_input(
            false, false, false, false, false, false
        ));
        assert!(should_block_terminal_input(
            false, false, true, false, false, false
        ));
        assert!(should_block_terminal_input(
            false, false, false, true, false, false
        ));
        assert!(should_block_terminal_input(
            false, false, false, false, true, false
        ));
        assert!(should_block_terminal_input(
            false, false, false, false, false, true
        ));
        assert!(should_block_terminal_input(
            true, false, false, false, false, false
        ));
        assert!(should_block_terminal_input(
            false, true, false, false, false, false
        ));
    }

    #[test]
    fn ctrl_wheel_is_classified_as_zoom_not_terminal_scroll() {
        let zoom = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 12.0),
            modifiers: egui::Modifiers::CTRL,
            phase: egui::TouchPhase::Move,
        };
        let plain = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -20.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        };
        assert_eq!(ctrl_wheel_zoom_delta(&[zoom, plain]), 1.0);
    }

    #[test]
    fn absent_or_balanced_ctrl_wheel_does_not_zoom() {
        assert_eq!(ctrl_wheel_zoom_delta(&[]), 0.0);

        let plain = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, -1.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        };
        assert_eq!(ctrl_wheel_zoom_delta(&[plain]), 0.0);

        let ctrl_wheel = |delta_y| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, delta_y),
            modifiers: egui::Modifiers::CTRL,
            phase: egui::TouchPhase::Move,
        };
        assert_eq!(
            ctrl_wheel_zoom_delta(&[ctrl_wheel(1.0), ctrl_wheel(-1.0)]),
            0.0
        );
    }

    #[test]
    fn configurable_ctrl_arrows_have_no_legacy_second_scroll() {
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        assert_eq!(viewport_scroll_delta(egui::Key::ArrowUp, ctrl, 24), None);
        assert_eq!(viewport_scroll_delta(egui::Key::ArrowDown, ctrl, 24), None);

        assert_eq!(
            viewport_scroll_delta(egui::Key::PageUp, egui::Modifiers::NONE, 24),
            Some(24)
        );
        assert_eq!(
            viewport_scroll_delta(egui::Key::PageDown, egui::Modifiers::NONE, 24),
            Some(-24)
        );
    }

    #[test]
    fn modal_enter_and_escape_are_not_routed_to_the_terminal() {
        let events = vec![
            egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: Some(egui::Key::Enter),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: Some(egui::Key::Escape),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ];

        assert!(routed_terminal_events(&events, true).is_empty());
        assert_eq!(routed_terminal_events(&events, false).len(), 2);
    }
}
