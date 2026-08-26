// Input handling module

use super::events::build_keybinding_string;
use super::state::TerminalApp;
use crate::{config, keybindings, layout, search};
use eframe::egui;

fn transformed_next_command_returns_to_bottom(
    next: bool,
    target_found: bool,
    offset_from_bottom: usize,
) -> bool {
    next && !target_found && offset_from_bottom > 0
}

/// Central input-routing decision for UI surfaces that own keyboard input.
/// Keep this pure so regressions (especially Enter/Escape leaking into the PTY)
/// can be covered without constructing a PTY-backed [`TerminalApp`].
pub(crate) fn should_block_terminal_input(
    search_open: bool,
    config_open: bool,
    replace_open: bool,
    paste_confirmation_open: bool,
    command_palette_open: bool,
    block_search_open: bool,
    text_edit_focused: bool,
) -> bool {
    search_open
        || config_open
        || replace_open
        || paste_confirmation_open
        || command_palette_open
        || block_search_open
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

/// A semantic Paste that opens confirmation or starts an asynchronous OSC 5522
/// notification claims every later terminal event in the same OS batch. Keep
/// only the prefix before the first Paste; fail closed if the claimed boundary
/// cannot be found.
pub(crate) fn terminal_events_before_semantic_paste_claim(
    events: &[egui::Event],
    claim_rest: bool,
) -> Vec<egui::Event> {
    if !claim_rest {
        return events.to_vec();
    }
    events
        .iter()
        .position(|event| matches!(event, egui::Event::Paste(_)))
        .map_or_else(Vec::new, |boundary| events[..boundary].to_vec())
}

/// Whether the first semantic Paste in this batch has older terminal-bound
/// input that has not yet reached the session writer. Bound shortcuts and IME
/// commits already admitted by the ordered dispatcher have been removed from
/// `events`; the remaining variants are conservatively treated as PTY input.
pub(crate) fn semantic_paste_has_terminal_prefix(events: &[egui::Event]) -> bool {
    let Some(boundary) = events
        .iter()
        .position(|event| matches!(event, egui::Event::Paste(_)))
    else {
        return false;
    };
    events[..boundary].iter().any(|event| match event {
        egui::Event::Key { pressed: true, .. } => true,
        egui::Event::Text(text) => !text.is_empty(),
        egui::Event::Ime(egui::ImeEvent::Commit(text)) => !text.is_empty(),
        egui::Event::MouseWheel { .. }
        | egui::Event::PointerButton { .. }
        | egui::Event::PointerMoved(_)
        | egui::Event::MouseMoved(_) => true,
        _ => false,
    })
}

pub(crate) fn semantic_paste_has_mouse_prefix(events: &[egui::Event]) -> bool {
    let Some(boundary) = events
        .iter()
        .position(|event| matches!(event, egui::Event::Paste(_)))
    else {
        return false;
    };
    events[..boundary].iter().any(|event| {
        matches!(
            event,
            egui::Event::MouseWheel { .. }
                | egui::Event::PointerButton { .. }
                | egui::Event::PointerMoved(_)
                | egui::Event::MouseMoved(_)
        )
    })
}

/// Whether the first semantic Paste precedes every pointer event in the batch.
/// The pre-paste pane-focus fast path must not observe such a suffix before the
/// Paste router decides whether that input is accepted or claimed.
pub(crate) fn semantic_paste_precedes_mouse_input(events: &[egui::Event]) -> bool {
    let Some(boundary) = events
        .iter()
        .position(|event| matches!(event, egui::Event::Paste(_)))
    else {
        return false;
    };
    let is_mouse = |event: &egui::Event| {
        matches!(
            event,
            egui::Event::MouseWheel { .. }
                | egui::Event::PointerButton { .. }
                | egui::Event::PointerMoved(_)
                | egui::Event::MouseMoved(_)
        )
    };
    !events[..boundary].iter().any(&is_mouse) && events[boundary + 1..].iter().any(is_mouse)
}

pub(crate) fn semantic_paste_pointer_input_blocked(
    terminal_input_blocked: bool,
    rejected_mouse_prefix_allows_pointer: bool,
    accepted_or_confirmed_paste: bool,
) -> bool {
    (terminal_input_blocked && !rejected_mouse_prefix_allows_pointer) || accepted_or_confirmed_paste
}

/// A retained transcript has no live PTY input target, so local selection and
/// scrolling may bypass that read-only boundary. It must never bypass a modal
/// UI owner or the ordering claim made by semantic paste handling.
pub(crate) fn retained_terminal_pointer_input_blocked(
    base_blocked: bool,
    retained_terminal: bool,
    ui_input_blocked: bool,
    semantic_paste_blocks_pointer: bool,
) -> bool {
    if retained_terminal && !ui_input_blocked && !semantic_paste_blocks_pointer {
        false
    } else {
        base_blocked
    }
}

pub(crate) fn osc_paste_route_is_clean(
    pending_input: bool,
    session_route_blocked: bool,
    events: &[egui::Event],
) -> bool {
    !pending_input && !semantic_paste_direct_input_blocked(session_route_blocked, events)
}

pub(crate) fn semantic_paste_direct_input_blocked(
    session_route_blocked: bool,
    events: &[egui::Event],
) -> bool {
    session_route_blocked || semantic_paste_has_terminal_prefix(events)
}

/// Once a modal/UI surface owns a frame's keyboard input, it owns the entire
/// batch even if a shortcut closes that surface midway through processing.
/// Otherwise later events from the same OS batch could escape into the PTY.
pub(crate) fn terminal_input_blocked_after_commands(
    blocked_at_frame_start: bool,
    palette_owned_input: bool,
    currently_blocked: bool,
) -> bool {
    blocked_at_frame_start || palette_owned_input || currently_blocked
}

pub(crate) fn accepted_terminal_input_clears_block_selection(
    accepted_terminal_input: bool,
    selection_postdates_terminal_input: bool,
) -> bool {
    accepted_terminal_input && !selection_postdates_terminal_input
}

/// IME lifecycle events are sampled before shortcut dispatch so the current
/// preedit can render without lag. If a shortcut opens a modal in the same
/// batch, roll that speculative terminal preedit back: the new UI owner, not
/// the terminal, owns every event after the modal boundary.
pub(crate) fn clear_terminal_preedit_for_ui_owner(
    terminal: &mut crate::terminal::TerminalState,
    ui_owns_input: bool,
) {
    if ui_owns_input {
        terminal.ime_enabled = false;
        terminal.clear_preedit();
    }
}

#[derive(Clone, Debug)]
struct OrderedKeyPress {
    event: egui::Event,
    key: egui::Key,
    modifiers: egui::Modifiers,
    command: Option<keybindings::Command>,
    /// A terminal-bound text/paste event appeared after the preceding key press
    /// and before this one. Terminal encoding happens later in `update`, so a
    /// subsequent command must yield instead of overtaking it.
    terminal_input_before: bool,
    ime_commits_before: Vec<OrderedImeCommit>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OrderedImeCommit {
    event_index: usize,
    text: String,
    /// Text/paste input still awaiting the deferred encoder appeared before
    /// this commit. In that rare mixed batch the IME must remain deferred too,
    /// rather than being queued ahead of older bytes.
    deferred_input_before: bool,
}

#[derive(Clone, Debug, Default)]
struct OrderedKeyBatch {
    presses: Vec<OrderedKeyPress>,
    /// Terminal input after the final key press still clears an older block
    /// selection even though there is no following press to carry the marker.
    trailing_terminal_input: bool,
    trailing_ime_commits: Vec<OrderedImeCommit>,
}

/// Keep key presses in the exact order delivered by egui while resolving each
/// configurable binding up front. Commands and unbound block-context keys must
/// be interleaved: batching commands first reverses `[Enter, Ctrl+Up]` into
/// "select, then recall" and `[Up, Ctrl+Up]` into "select, then move".
fn ordered_key_presses(
    events: &[egui::Event],
    bindings: &keybindings::KeyBindings,
) -> OrderedKeyBatch {
    let mut batch = OrderedKeyBatch::default();
    let mut terminal_input_before = false;
    let mut printable_press_awaiting_text = false;
    let mut ime_commits_before = Vec::new();

    for (event_index, event) in events.iter().enumerate() {
        match event {
            egui::Event::Key {
                key,
                modifiers,
                pressed: true,
                ..
            } => {
                // If the previous printable press emitted no Text event, a new
                // press ends that possible pairing.
                printable_press_awaiting_text = is_printable_key(*key);
                let command = build_keybinding_string(*key, *modifiers)
                    .and_then(|chord| bindings.get_command(&chord));
                batch.presses.push(OrderedKeyPress {
                    event: event.clone(),
                    key: *key,
                    modifiers: *modifiers,
                    command,
                    terminal_input_before,
                    ime_commits_before: std::mem::take(&mut ime_commits_before),
                });
                terminal_input_before = false;
            }
            // egui normally emits Key then Text for one printable press. The
            // Key already determines whether that input was consumed or must
            // reach the PTY; do not treat its paired Text as a later action.
            egui::Event::Text(text) if printable_press_awaiting_text => {
                printable_press_awaiting_text = false;
                if text.is_empty() {
                    continue;
                }
            }
            egui::Event::Text(text) if !text.is_empty() => terminal_input_before = true,
            // Paste is delivered later in update, possibly through the OSC
            // paste protocol, so later commands cannot overtake it.
            egui::Event::Paste(_) => terminal_input_before = true,
            // Non-empty IME commits are queued at their ordered position by
            // handle_keybindings. Empty commits carry no terminal input and
            // must not defer a following block command.
            egui::Event::Ime(egui::ImeEvent::Commit(text)) if !text.is_empty() => {
                ime_commits_before.push(OrderedImeCommit {
                    event_index,
                    text: text.clone(),
                    deferred_input_before: terminal_input_before,
                });
            }
            _ => {}
        }
    }

    batch.trailing_terminal_input = terminal_input_before;
    batch.trailing_ime_commits = ime_commits_before;
    batch
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OrderedPressOutcome {
    Ignored,
    Consumed,
    /// Consume this press and let the newly opened/already-owning modal claim
    /// every later event in the OS batch.
    ClaimRest,
    CloseViewport,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct OrderedDispatchResult {
    consumed: Vec<(egui::Key, egui::Modifiers)>,
    close_requested: bool,
    claimed_rest: bool,
}

fn dispatch_ordered_key_presses(
    presses: &[OrderedKeyPress],
    mut dispatch: impl FnMut(&OrderedKeyPress) -> OrderedPressOutcome,
) -> OrderedDispatchResult {
    let mut result = OrderedDispatchResult::default();
    for press in presses {
        match dispatch(press) {
            OrderedPressOutcome::Ignored => {}
            OrderedPressOutcome::Consumed => {
                result.consumed.push((press.key, press.modifiers));
            }
            OrderedPressOutcome::ClaimRest => {
                result.consumed.push((press.key, press.modifiers));
                result.claimed_rest = true;
                break;
            }
            OrderedPressOutcome::CloseViewport => {
                result.consumed.push((press.key, press.modifiers));
                result.close_requested = true;
                break;
            }
        }
    }
    result
}

/// Test-only view of [`is_printable_key`], so the events-layer chord test can
/// assert that a new bindable key also consumes its paired text event.
#[cfg(test)]
pub(crate) fn is_printable_key_for_test(key: egui::Key) -> bool {
    is_printable_key(key)
}

fn is_printable_key(key: egui::Key) -> bool {
    matches!(
        key,
        egui::Key::A
            | egui::Key::B
            | egui::Key::C
            | egui::Key::D
            | egui::Key::E
            | egui::Key::F
            | egui::Key::G
            | egui::Key::H
            | egui::Key::I
            | egui::Key::J
            | egui::Key::K
            | egui::Key::L
            | egui::Key::M
            | egui::Key::N
            | egui::Key::O
            | egui::Key::P
            | egui::Key::Q
            | egui::Key::R
            | egui::Key::S
            | egui::Key::T
            | egui::Key::U
            | egui::Key::V
            | egui::Key::W
            | egui::Key::X
            | egui::Key::Y
            | egui::Key::Z
            | egui::Key::Num0
            | egui::Key::Num1
            | egui::Key::Num2
            | egui::Key::Num3
            | egui::Key::Num4
            | egui::Key::Num5
            | egui::Key::Num6
            | egui::Key::Num7
            | egui::Key::Num8
            | egui::Key::Num9
            | egui::Key::Comma
            | egui::Key::Period
            | egui::Key::Plus
            | egui::Key::Minus
            | egui::Key::Slash
            | egui::Key::Backslash
            | egui::Key::Semicolon
            | egui::Key::Quote
            | egui::Key::OpenBracket
            | egui::Key::CloseBracket
            | egui::Key::OpenCurlyBracket
            | egui::Key::CloseCurlyBracket
            | egui::Key::Equals
            | egui::Key::Backtick
    )
}

/// Remove one handled key press and the text event generated by that same
/// printable key. Without consuming both halves, a user binding such as
/// `alt+x = "config:toggle"` could open Settings and still type text. Do not
/// infer text emission from modifiers: Alt/Option and AltGr combinations can
/// still produce `Event::Text` on supported window systems.
fn consume_bound_key_event(
    events: &mut Vec<egui::Event>,
    key: egui::Key,
    modifiers: egui::Modifiers,
) {
    let Some(key_index) = events.iter().position(|event| {
        matches!(
            event,
            egui::Event::Key {
                key: event_key,
                modifiers: event_modifiers,
                pressed: true,
                ..
            } if *event_key == key && *event_modifiers == modifiers
        )
    }) else {
        return;
    };
    events.remove(key_index);

    if !is_printable_key(key) {
        return;
    }
    let mut index = key_index;
    while index < events.len() {
        match &events[index] {
            egui::Event::Text(_) => {
                events.remove(index);
                break;
            }
            egui::Event::Key { pressed: true, .. } => break,
            _ => index += 1,
        }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockSelectionKeyAction {
    Move(crate::block_mode::SelectStep),
    Extend(crate::block_mode::SelectStep),
    Reinput,
    Clear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CopySelectionRoute {
    TerminalText,
    CommandBlocks,
    None,
}

fn copy_selection_route(has_terminal_text: bool, has_command_blocks: bool) -> CopySelectionRoute {
    if has_terminal_text {
        CopySelectionRoute::TerminalText
    } else if has_command_blocks {
        CopySelectionRoute::CommandBlocks
    } else {
        CopySelectionRoute::None
    }
}

fn contextual_block_shortcut_available(
    command: &keybindings::Command,
    has_completed_selection: bool,
) -> bool {
    !matches!(
        command,
        keybindings::Command::BlockReinputSelectedCommands
            | keybindings::Command::BlockToggleBookmark
    ) || has_completed_selection
}

fn ime_commit_can_queue_now(terminal_input_seen: bool, commit: &OrderedImeCommit) -> bool {
    !terminal_input_seen && !commit.deferred_input_before
}

/// Context-only block keys: they are deliberately unbound while no block is
/// selected, so normal shell history, `read`, and full-screen applications keep
/// receiving arrows/Enter/Escape. Ctrl+Up/Down remain configurable commands and
/// enter this model through `TerminalScroll*` dispatch instead.
fn block_selection_key_action(
    event: &egui::Event,
    has_selection: bool,
) -> Option<BlockSelectionKeyAction> {
    if !has_selection {
        return None;
    }
    let egui::Event::Key {
        key,
        modifiers,
        pressed: true,
        ..
    } = event
    else {
        return None;
    };
    let no_command_modifier = !modifiers.ctrl && !modifiers.alt && !modifiers.command;
    match key {
        egui::Key::ArrowUp if no_command_modifier && modifiers.shift => Some(
            BlockSelectionKeyAction::Extend(crate::block_mode::SelectStep::Older),
        ),
        egui::Key::ArrowDown if no_command_modifier && modifiers.shift => Some(
            BlockSelectionKeyAction::Extend(crate::block_mode::SelectStep::Newer),
        ),
        egui::Key::ArrowUp if no_command_modifier && !modifiers.shift => Some(
            BlockSelectionKeyAction::Move(crate::block_mode::SelectStep::Older),
        ),
        egui::Key::ArrowDown if no_command_modifier && !modifiers.shift => Some(
            BlockSelectionKeyAction::Move(crate::block_mode::SelectStep::Newer),
        ),
        egui::Key::Enter if no_command_modifier && !modifiers.shift => {
            Some(BlockSelectionKeyAction::Reinput)
        }
        egui::Key::Escape if no_command_modifier && !modifiers.shift => {
            Some(BlockSelectionKeyAction::Clear)
        }
        _ => None,
    }
}

fn block_selection_context_available(
    block_mode: bool,
    block_canvas_visible: bool,
    selection_targets_active_session: bool,
) -> bool {
    block_mode && block_canvas_visible && selection_targets_active_session
}

impl TerminalApp {
    pub(crate) fn terminal_input_blocked(&self, ctx: &egui::Context) -> bool {
        should_block_terminal_input(
            self.search_state.is_open,
            self.config_panel.is_open,
            self.search_replace_panel.is_open,
            self.pending_paste_confirm.is_some(),
            self.command_palette.is_open,
            self.block_search.is_open,
            ctx.text_edit_focused(),
        )
    }

    pub(crate) fn active_terminal_is_read_only(&self) -> bool {
        self.session_manager
            .sessions()
            .get(self.session_manager.active_index())
            .is_some_and(|session| {
                session.purpose == crate::session::SessionPurpose::RetainedCommand
            })
    }

    fn copy_active_selection(&mut self) {
        let selected = {
            let session = self.session_manager.get_active_session_mut();
            session.terminal.lock().copy_selection()
        };

        let active_session_id = self
            .session_manager
            .get_active_session_mut()
            .metadata
            .session_id
            .clone();
        let has_blocks = self
            .block_selection
            .as_ref()
            .is_some_and(|selection| selection.session_id == active_session_id);
        match copy_selection_route(selected.is_some(), has_blocks) {
            CopySelectionRoute::TerminalText => {
                let text = selected.expect("route checked terminal selection");
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
            CopySelectionRoute::CommandBlocks => self.block_copy_block(),
            CopySelectionRoute::None => self.set_status("Nothing selected"),
        }
    }

    fn paste_active_clipboard(&mut self) {
        if self.active_terminal_is_read_only() {
            self.set_status("Exited Agent terminals are read-only");
            return;
        }
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
                let active_session_id = self
                    .session_manager
                    .sessions()
                    .get(self.session_manager.active_index())
                    .map(|session| session.metadata.session_id.clone());
                let direct_input_blocked = active_session_id
                    .as_deref()
                    .is_none_or(|session_id| self.direct_input_is_blocked_for_session(session_id));
                let session = self.session_manager.get_active_session_mut();
                match crate::paste_text_into_session(
                    session,
                    text,
                    self.config.paste_confirm,
                    crate::PasteOrigin::Clipboard,
                    false,
                    direct_input_blocked,
                    &mut self.pending_paste_confirm,
                ) {
                    // 粘贴也是用户字节:与键盘输入一样丢弃 block 选中。
                    Ok(true) if self.pending_paste_confirm.is_none() => {
                        if let Some(session_id) = active_session_id {
                            self.clear_block_selection_for_session(&session_id);
                        }
                    }
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

    fn queue_terminal_control_input(&mut self, byte: u8) {
        if self.active_terminal_is_read_only() {
            self.set_status("Exited Agent terminals are read-only");
            return;
        }
        let (accepted, session_id) = {
            let session = self.session_manager.get_active_session_mut();
            (
                session.queue_input(&[byte]),
                session.metadata.session_id.clone(),
            )
        };
        if accepted_terminal_input_clears_block_selection(accepted, false) {
            self.clear_block_selection_for_session(&session_id);
        } else {
            self.set_status("Terminal input retry buffer is full");
        }
    }

    fn set_font_size_from_command(&mut self, ctx: &egui::Context, target_size: f32, action: &str) {
        let new_font_size = config::Config::clamp_font_size(target_size);
        if (new_font_size - self.config.font_size).abs() <= 0.01 {
            self.set_status(format!(
                "Font size is already at {:.0} pt",
                self.config.font_size
            ));
            return;
        }

        self.config.font_size = new_font_size;
        self.font_size_accumulator = 0.0;
        self.apply_font_size_change(ctx);
        self.schedule_config_save();
        self.set_status(format!("{action}: {:.0} pt", self.config.font_size));
    }

    fn adjust_opacity_from_command(&mut self, ctx: &egui::Context, delta: f32) {
        let new_opacity = (self.config.opacity + delta).clamp(0.05, 1.0);
        if (new_opacity - self.config.opacity).abs() <= f32::EPSILON {
            self.set_status(format!(
                "Opacity is already at {:.0}%",
                self.config.opacity * 100.0
            ));
            return;
        }

        self.config.opacity = new_opacity;
        // Surgical apply: only the renderers' opacity changes. The full
        // apply_runtime_config path re-applies pixels_per_point and rebuilds
        // fonts, which visibly rescales the whole UI on every keypress.
        self.renderer.opacity = new_opacity;
        for renderer in &mut self.pane_renderers {
            renderer.opacity = new_opacity;
        }
        ctx.request_repaint();
        self.schedule_config_save();
        self.set_status(format!("Opacity: {:.0}%", self.config.opacity * 100.0));
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
                self.new_tab();
            }
            keybindings::Command::SessionClose => {
                // 关 tab 连带关掉它所有的分屏窗格。
                let active_tab = self.tabs.active_index();
                if self.close_tab_synced(active_tab) {
                    self.schedule_session_save();
                } else {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return true;
                }
            }
            keybindings::Command::SessionNext => self.activate_next_session(),
            keybindings::Command::SessionPrev => self.activate_prev_session(),
            keybindings::Command::SessionJump(index) => {
                // Alt+N 选第 N 个 tab,与 tab 栏上看到的顺序一致。
                if !self.activate_tab(index) {
                    self.set_status(format!("Tab {} is not available", index + 1));
                }
            }
            keybindings::Command::SessionLast => {
                if let Some(last_tab) = self.tabs.len().checked_sub(1) {
                    self.activate_tab(last_tab);
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
            keybindings::Command::SearchNext => self.select_next_search_match(),
            keybindings::Command::SearchPrev => self.select_prev_search_match(),
            keybindings::Command::SearchHistoryPrev => {
                self.search_state.history_prev();
                self.refresh_search_matches();
            }
            keybindings::Command::SearchHistoryNext => {
                self.search_state.history_next();
                self.refresh_search_matches();
            }
            keybindings::Command::SearchReplaceToggle => self.search_replace_panel.toggle(),
            keybindings::Command::TerminalSendSigint => self.queue_terminal_control_input(0x03),
            keybindings::Command::TerminalSendEof => self.queue_terminal_control_input(0x04),
            keybindings::Command::TerminalClear => self.queue_terminal_control_input(0x0c),
            keybindings::Command::TerminalScrollUp => {
                if !self.block_context_scroll(crate::block_mode::SelectStep::Older) {
                    self.scroll_active_terminal(3);
                }
            }
            keybindings::Command::TerminalScrollDown => {
                if !self.block_context_scroll(crate::block_mode::SelectStep::Newer) {
                    self.scroll_active_terminal(-3);
                }
            }
            keybindings::Command::TerminalJumpPrevMark => {
                if self.block_scroll_selected_edge(false) {
                    return false;
                }
                let jumped = self.jump_adjacent_command(false);
                if !jumped {
                    self.set_status("No previous command mark");
                }
            }
            keybindings::Command::TerminalJumpNextMark => {
                if self.block_scroll_selected_edge(true) {
                    return false;
                }
                let jumped = self.jump_adjacent_command(true);
                if !jumped {
                    self.set_status("No next command mark");
                }
            }
            keybindings::Command::BlockJumpFirstFailed => self.block_jump_first_failed(),
            keybindings::Command::BlockCopyCommand => self.block_copy_command(),
            keybindings::Command::BlockCopyOutput => self.block_copy_output(),
            keybindings::Command::BlockRecallCommand => self.block_recall_command(),
            keybindings::Command::BlockSelectPrev => self.block_select_prev(),
            keybindings::Command::BlockSelectNext => self.block_select_next(),
            keybindings::Command::BlockSelectAll => self.block_select_all(),
            keybindings::Command::BlockReinputSelectedCommands => {
                self.block_reinput_selected_commands()
            }
            keybindings::Command::BlockCopyBlock => self.block_copy_block(),
            keybindings::Command::BlockCopyMarkdown => self.block_copy_markdown(),
            keybindings::Command::BlockJumpPrevFailed => self.block_jump_prev_failed(),
            keybindings::Command::BlockJumpNextFailed => self.block_jump_next_failed(),
            keybindings::Command::BlockSearchToggle => self.block_search_toggle(),
            keybindings::Command::BlockToggleBookmark => self.block_toggle_bookmark(),
            keybindings::Command::BlockJumpPrevBookmark => {
                self.block_jump_bookmark(crate::block_mode::SelectStep::Older)
            }
            keybindings::Command::BlockJumpNextBookmark => {
                self.block_jump_bookmark(crate::block_mode::SelectStep::Newer)
            }
            keybindings::Command::FontIncrease => {
                self.set_font_size_from_command(ctx, self.config.font_size + 1.0, "Font increased")
            }
            keybindings::Command::FontDecrease => {
                self.set_font_size_from_command(ctx, self.config.font_size - 1.0, "Font decreased")
            }
            keybindings::Command::FontReset => {
                self.set_font_size_from_command(ctx, config::DEFAULT_FONT_SIZE, "Font size reset")
            }
            keybindings::Command::OpacityIncrease => self.adjust_opacity_from_command(ctx, 0.025),
            keybindings::Command::OpacityDecrease => self.adjust_opacity_from_command(ctx, -0.025),
            keybindings::Command::TerminalSplitVertical => self.split_terminal(false),
            keybindings::Command::TerminalSplitHorizontal => self.split_terminal(true),
            keybindings::Command::TerminalClosePane => self.close_focused_pane_or_session(),
            keybindings::Command::PaneFocusNext => {
                if !self.layout_mut().focus_pane(layout::PaneDirection::Next) {
                    self.set_status("Only one pane is open");
                }
                self.sync_active_session_to_focused_pane();
            }
            keybindings::Command::PaneFocusPrev => {
                if !self.layout_mut().focus_pane(layout::PaneDirection::Prev) {
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
            keybindings::Command::PaneZoomToggle => {
                if self.layout_mut().toggle_focused_pane_zoom() {
                    self.force_resize_session = true;
                    ctx.request_repaint();
                    self.set_status(if self.layout().is_zoomed() {
                        "Focused pane zoomed"
                    } else {
                        "Pane zoom restored"
                    });
                } else {
                    self.set_status("Only one pane is open");
                }
            }
            keybindings::Command::PaneEqualize => {
                if self.layout_mut().equalize_splits() {
                    self.schedule_session_save();
                    ctx.request_repaint();
                    self.set_status("Pane dividers reset to 50/50");
                } else {
                    self.set_status("Pane dividers are already equal");
                }
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
                if self.sidebar.visible && self.sidebar.view == crate::sidebar::SidebarView::Files {
                    if let Some(error) = self.sidebar.refresh() {
                        self.set_status(format!("文件树刷新失败：{error}"));
                    }
                }
            }
            keybindings::Command::JshInstall => {
                self.install_or_update_jsh();
            }
            keybindings::Command::RemotePicker => {
                self.remote_picker.toggle();
                if self.remote_picker.is_open && self.config.remote_hosts.is_empty() {
                    self.set_status("配置里还没有 [[remote_hosts]]；面板里有可以照抄的示例");
                }
            }
            keybindings::Command::AgentToggle => {
                let session_id = self
                    .session_manager
                    .get_active_session_mut()
                    .metadata
                    .session_id
                    .clone();
                self.agent_panel.toggle(&self.config, session_id);
                self.set_status(if self.agent_panel.is_open {
                    "AI agent 已打开：每条命令都需要你批准后才会执行"
                } else {
                    "AI agent 已关闭"
                });
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

    /// 切换活跃会话并同步分屏布局。会话归某个 tab 的某个窗格所有，因此这里
    /// 先切到拥有它的 tab，再在 tab 内聚焦对应窗格——绝不把它搬进别的窗格，
    /// 那会让 tab 高亮、键盘输入和可见内容三者分离。
    pub fn activate_session(&mut self, index: usize) -> bool {
        let target_session_id = self
            .session_manager
            .sessions()
            .get(index)
            .map(|session| session.metadata.session_id.clone());
        if !self.session_manager.switch_session(index) {
            return false;
        }
        if let Some(tab_idx) = self.tabs.tab_of_session(index) {
            self.tabs.set_active(tab_idx);
            self.layout_mut().focus_session(index);
        }
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
            terminal.lock().clear_text_selection();
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
        let previous_results_session_id = self.search_state.results_session_id.clone();
        let previous_projection_message = self.search_state.projection_message.clone();
        let previous_hidden_zone = self.search_state.hidden_projection_zone;
        let previous_policy_revision = self.search_state.projection_policy_revision;
        let selected_match = self
            .search_state
            .matches
            .get(self.search_state.current_match_index)
            .copied();
        let (matches, error, truncated, grid_version, session_id, policy_revision) = {
            let session = self.session_manager.get_active_session_mut();
            let session_id = session.metadata.session_id.clone();
            let policy_revision = session.projection_policy.revision();
            let terminal = session.terminal.lock();
            let (matches, error, truncated) = search::SearchEngine::search(
                &terminal,
                &self.search_state.query,
                self.search_state.use_regex,
                self.search_state.case_sensitive,
            );
            (
                matches,
                error,
                truncated,
                terminal.get_grid_version(),
                session_id,
                policy_revision,
            )
        };
        self.search_state.matches = matches;
        self.search_state.error_message = error;
        self.search_state.results_truncated = truncated;
        self.search_state.current_match_index = selected_match
            .and_then(|selected| {
                self.search_state
                    .matches
                    .iter()
                    .position(|candidate| *candidate == selected)
            })
            .unwrap_or(0);
        let selected_survived = selected_match.is_some_and(|selected| {
            self.search_state
                .matches
                .get(self.search_state.current_match_index)
                .is_some_and(|current| *current == selected)
        });
        let diagnostic_context_survived = selected_survived
            && previous_results_session_id.as_deref() == Some(session_id.as_str())
            && previous_policy_revision == Some(policy_revision);
        self.search_state.projection_message = diagnostic_context_survived
            .then_some(previous_projection_message)
            .flatten();
        self.search_state.hidden_projection_zone = diagnostic_context_survived
            .then_some(previous_hidden_zone)
            .flatten();
        self.search_state.projection_policy_revision = diagnostic_context_survived
            .then_some(previous_policy_revision)
            .flatten();
        self.search_state.results_grid_version = Some(grid_version);
        self.search_state.results_session_idx = Some(session_idx);
        self.search_state.results_session_id = Some(session_id);
        self.search_state.results_refreshed_at = Some(std::time::Instant::now());
    }

    pub(crate) fn reveal_current_search_match(&mut self) {
        let active_session_id = self
            .session_manager
            .get_active_session_mut()
            .metadata
            .session_id
            .clone();
        if self.search_state.results_session_id.as_deref() != Some(active_session_id.as_str()) {
            self.refresh_search_matches();
        }
        let Some(search_match) = self
            .search_state
            .matches
            .get(self.search_state.current_match_index)
            .copied()
        else {
            return;
        };
        self.search_state.clear_projection_diagnostic();
        let block_mode = self.config.block_mode;
        let (location, policy_revision) = {
            let session = self.session_manager.get_active_session_mut();
            let terminal_arc = std::sync::Arc::clone(&session.terminal);
            let policy = &session.projection_policy;
            let policy_revision = policy.revision();
            let view_state = &mut session.projection_view_state;
            let mut terminal = terminal_arc.lock();
            let viewport = terminal.projected_viewport_with_state(
                crate::terminal::HistoryProjection::identity(),
                block_mode,
                policy,
                view_state,
            );
            if viewport.is_transformed() {
                (
                    terminal.reveal_buffer_anchor_in_projection(
                        policy,
                        view_state,
                        search_match.anchor(),
                    ),
                    policy_revision,
                )
            } else if terminal.scroll_to_buffer_anchor(search_match.anchor()) {
                (
                    crate::terminal::ProjectedBufferAnchorLocation::Identity,
                    policy_revision,
                )
            } else {
                (
                    crate::terminal::ProjectedBufferAnchorLocation::Unmapped,
                    policy_revision,
                )
            }
        };
        match location {
            crate::terminal::ProjectedBufferAnchorLocation::Hidden { zone_id } => {
                self.search_state.hidden_projection_zone = Some(zone_id);
                self.search_state.projection_message =
                    Some("Match is hidden in a collapsed block".to_owned());
                self.search_state.projection_policy_revision = Some(policy_revision);
            }
            crate::terminal::ProjectedBufferAnchorLocation::Unmapped => {
                self.search_state.projection_message =
                    Some("Match is no longer retained in the terminal".to_owned());
                self.search_state.projection_policy_revision = Some(policy_revision);
            }
            crate::terminal::ProjectedBufferAnchorLocation::Identity
            | crate::terminal::ProjectedBufferAnchorLocation::Visible { .. } => {}
        }
    }

    pub(super) fn reveal_hidden_search_match(&mut self) {
        let Some(zone_id) = self.search_state.hidden_projection_zone else {
            return;
        };
        let Some(search_match) = self
            .search_state
            .matches
            .get(self.search_state.current_match_index)
            .copied()
        else {
            self.search_state.clear_projection_diagnostic();
            return;
        };
        let (session_id, policy_revision) = {
            let session = self.session_manager.get_active_session_mut();
            (
                session.metadata.session_id.clone(),
                session.projection_policy.revision(),
            )
        };
        if !self
            .search_state
            .projection_diagnostic_is_current(&session_id, policy_revision)
        {
            self.search_state.clear_projection_diagnostic();
            self.reveal_current_search_match();
            return;
        }
        let still_hidden_by_same_zone = {
            let session = self.session_manager.get_active_session_mut();
            let terminal_arc = std::sync::Arc::clone(&session.terminal);
            let policy = &session.projection_policy;
            let view_state = &mut session.projection_view_state;
            let mut terminal = terminal_arc.lock();
            let viewport = terminal.projected_viewport_with_state(
                crate::terminal::HistoryProjection::identity(),
                self.config.block_mode,
                policy,
                view_state,
            );
            viewport.is_transformed()
                && matches!(
                    terminal.reveal_buffer_anchor_in_projection(
                        policy,
                        view_state,
                        search_match.anchor(),
                    ),
                    crate::terminal::ProjectedBufferAnchorLocation::Hidden {
                        zone_id: current_zone
                    } if current_zone == zone_id
                )
        };
        if !still_hidden_by_same_zone {
            self.search_state.clear_projection_diagnostic();
            self.reveal_current_search_match();
            return;
        }
        let changed = {
            let session = self.session_manager.get_active_session_mut();
            let changed = session.projection_policy.expand(zone_id);
            if changed {
                session.terminal.lock().clear_text_selection();
            }
            changed
        };
        if changed {
            self.search_state.clear_projection_diagnostic();
            self.reveal_current_search_match();
        } else {
            self.search_state.clear_projection_diagnostic();
        }
    }

    fn scroll_active_terminal(&mut self, lines: isize) {
        let block_mode = self.config.block_mode;
        let session = self.session_manager.get_active_session_mut();
        let terminal_arc = std::sync::Arc::clone(&session.terminal);
        let policy = &session.projection_policy;
        let view_state = &mut session.projection_view_state;
        let mut terminal = terminal_arc.lock();
        let viewport = terminal.projected_viewport_with_state(
            crate::terminal::HistoryProjection::identity(),
            block_mode,
            policy,
            view_state,
        );
        if viewport.is_transformed() {
            view_state.scroll(lines, &viewport);
        } else if !terminal.is_alt_buffer_active() {
            terminal.scroll(lines);
        }
    }

    fn jump_adjacent_command(&mut self, next: bool) -> bool {
        let block_mode = self.config.block_mode;
        let session = self.session_manager.get_active_session_mut();
        let terminal_arc = std::sync::Arc::clone(&session.terminal);
        let policy = &session.projection_policy;
        let view_state = &mut session.projection_view_state;
        let mut terminal = terminal_arc.lock();
        let viewport = terminal.projected_viewport_with_state(
            crate::terminal::HistoryProjection::identity(),
            block_mode,
            policy,
            view_state,
        );
        if !viewport.is_transformed() {
            return if next {
                terminal.jump_to_next_command()
            } else {
                terminal.jump_to_prev_command()
            };
        }

        let top_row = viewport.top_padding();
        let current_top = match viewport.row_kind(top_row) {
            Some(crate::terminal::ProjectedRowKind::CollapsedSummary { hidden_range, .. }) => {
                terminal.raw_cell_anchor_to_buffer_anchor(crate::terminal::RawCellAnchor {
                    row_id: hidden_range.start.row,
                    column: hidden_range.start.col,
                })
            }
            _ => viewport
                .view_row_absolute(top_row)
                .and_then(|absolute| terminal.absolute_to_buffer_anchor((absolute, 0))),
        }
        .map(|anchor| anchor.line_id)
        .unwrap_or(terminal.total_lines_scrolled);
        let cols = terminal.grid.row_len();
        let target = if next {
            terminal.command_records().iter().find(|record| {
                crate::block_mode::prompt_row_line_id(
                    record.prompt_start.line_id,
                    record.prompt_start.column,
                    cols,
                ) > current_top
            })
        } else {
            terminal.command_records().iter().rev().find(|record| {
                crate::block_mode::prompt_row_line_id(
                    record.prompt_start.line_id,
                    record.prompt_start.column,
                    cols,
                ) < current_top
            })
        };
        let anchor = target.map(|record| record.prompt_start);
        if transformed_next_command_returns_to_bottom(
            next,
            anchor.is_some(),
            view_state.offset_from_bottom(),
        ) {
            view_state.scroll_to_bottom();
            return true;
        }
        let Some(anchor) = anchor else {
            return false;
        };
        matches!(
            terminal.reveal_buffer_anchor_in_projection(policy, view_state, anchor),
            crate::terminal::ProjectedBufferAnchorLocation::Visible { .. }
        )
    }

    pub(super) fn select_next_search_match(&mut self) {
        self.search_state.next_match();
        self.reveal_current_search_match();
    }

    pub(super) fn select_prev_search_match(&mut self) {
        self.search_state.prev_match();
        self.reveal_current_search_match();
    }

    /// Next/Prev 走 tab,不走底层会话向量:分屏产生的会话属于某个 tab 内部,
    /// 让 Ctrl+Tab 轮询它们会把窗格当成 tab 来用。tab 内切窗格是 PaneNext/Prev。
    fn activate_next_session(&mut self) {
        if self.tabs.len() < 2 {
            return;
        }
        let next = (self.tabs.active_index() + 1) % self.tabs.len();
        self.activate_tab(next);
    }

    fn activate_prev_session(&mut self) {
        if self.tabs.len() < 2 {
            return;
        }
        let prev = (self.tabs.active_index() + self.tabs.len() - 1) % self.tabs.len();
        self.activate_tab(prev);
    }

    fn activate_previous_session(&mut self) -> bool {
        if !self.session_manager.switch_to_previous_active() {
            return false;
        }
        let index = self.session_manager.active_index();
        self.activate_session(index)
    }

    fn focus_physical_pane(&mut self, direction: layout::PaneDirection, label: &str) {
        if self.layout_mut().focus_pane(direction) {
            self.sync_active_session_to_focused_pane();
        } else {
            self.set_status(format!("No pane {label}"));
        }
    }

    fn resize_pane(&mut self, direction: layout::PaneDirection, label: &str) {
        const RESIZE_STEP: f32 = 0.05;
        if self.layout_mut().resize_split(direction, RESIZE_STEP) {
            self.schedule_session_save();
        } else {
            self.set_status(format!("Cannot resize pane {label}"));
        }
    }

    /// 关闭 pane 时同时关闭它拥有的 shell session。旧行为只从布局中摘掉
    /// pane，却把 PTY 留成隐藏 tab，既泄漏后台进程，也让 split 看起来像
    /// 在拼接已有 session。
    ///
    /// 关掉 tab 里最后一个窗格,就等于关掉这个 tab。
    fn close_focused_pane_or_session(&mut self) {
        if self.layout().pane_count() > 1 {
            let Some(closing_session_idx) = self.layout().focused_session_idx() else {
                self.set_status("No focused pane to close");
                return;
            };
            if let Err(error) = self.layout_mut().close_focused_pane() {
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

        let active_tab = self.tabs.active_index();
        if self.close_tab_synced(active_tab) {
            self.schedule_session_save();
        } else {
            self.set_status("Cannot close the last pane");
        }
    }

    /// 创建一个全新的 shell session，并从当前焦点 pane 原地分出新 pane。
    /// session 创建失败时不改变布局，布局更新失败时回滚刚创建的 session。
    fn split_terminal(&mut self, horizontal: bool) {
        if !self.layout().can_split() {
            self.set_status("No focused pane to split");
            return;
        }

        let minimum_pane_size = self.renderer.minimum_split_pane_size();
        if !self
            .layout()
            .can_split_focused_pane(horizontal, minimum_pane_size)
        {
            self.set_status("Pane is too small to split; resize it or choose a larger pane");
            return;
        }

        let old_len = self.session_manager.len();
        let new_session_idx = self.create_session_with_current_config(None, None);
        if self.session_manager.len() == old_len {
            self.set_status("Failed to create session for split");
            return;
        }

        match self.layout_mut().split(new_session_idx, horizontal) {
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
        let Some(idx) = self.layout().focused_session_idx() else {
            return;
        };
        if idx != self.session_manager.active_index() && self.activate_session(idx) {
            self.schedule_session_save();
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
                                self.select_next_search_match();
                            } else {
                                self.select_prev_search_match();
                            }
                        }
                        egui::Key::ArrowUp => {
                            self.search_state.history_prev();
                            self.refresh_search_matches();
                        }
                        egui::Key::ArrowDown => {
                            self.search_state.history_next();
                            self.refresh_search_matches();
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

    /// Handle keys the block-search picker owns (same routing pattern as the
    /// command palette; the `block:search` chord itself closes it through the
    /// modal-command path in `handle_keybindings`). Returns whether the
    /// picker owned this frame's input; it never requests a viewport close.
    pub fn handle_block_search_input(&mut self) -> bool {
        if !self.block_search.is_open {
            return false;
        }

        let events_copy = self.frame_events.clone();
        let mut confirm = None;
        for evt in &events_copy {
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = evt
            else {
                continue;
            };
            match key {
                egui::Key::Escape => self.block_search.close(),
                egui::Key::ArrowUp => self.block_search.select_prev(),
                egui::Key::ArrowDown => self.block_search.select_next(),
                egui::Key::I if modifiers.ctrl => {
                    self.block_search.case_sensitive = !self.block_search.case_sensitive;
                    self.block_search.computed_query = None;
                    self.refresh_block_search_hits();
                }
                egui::Key::R if modifiers.ctrl => {
                    self.block_search.regex = !self.block_search.regex;
                    self.block_search.computed_query = None;
                    self.refresh_block_search_hits();
                }
                egui::Key::W if modifiers.ctrl => {
                    self.block_search.whole_word = !self.block_search.whole_word;
                    self.block_search.computed_query = None;
                    self.refresh_block_search_hits();
                }
                egui::Key::O if modifiers.ctrl => {
                    self.block_search.scope = self.block_search.scope.cycled();
                    self.block_search.computed_query = None;
                    self.refresh_block_search_hits();
                }
                egui::Key::Enter => {
                    // Plain Enter keeps the accept-and-close contract.
                    // Shift+Enter reveals this hit, advances to the next one,
                    // and leaves the query open for walk-through review.
                    confirm = Some(modifiers.shift);
                    break;
                }
                _ => {}
            }
        }
        if let Some(keep_open) = confirm {
            self.block_search_accept(keep_open);
        }
        true
    }

    fn command_belongs_to_open_modal(&self, command: &keybindings::Command) -> bool {
        match command {
            keybindings::Command::SearchClose
            | keybindings::Command::SearchNext
            | keybindings::Command::SearchPrev
            | keybindings::Command::SearchHistoryPrev
            | keybindings::Command::SearchHistoryNext => self.search_state.is_open,
            keybindings::Command::ConfigClose | keybindings::Command::ConfigToggle => {
                self.config_panel.is_open
            }
            keybindings::Command::CommandPaletteToggle => self.command_palette.is_open,
            keybindings::Command::BlockSearchToggle => self.block_search.is_open,
            keybindings::Command::SearchReplaceToggle => self.search_replace_panel.is_open,
            _ => false,
        }
    }

    /// Queue a non-empty IME commit at its position in the original event
    /// batch. `Session::queue_input` is atomic and records user-input taint.
    /// Clearing selection here lets a later Ctrl+Up establish a genuinely
    /// newer selection, while a commit after Ctrl+Up clears it immediately.
    fn queue_ordered_ime_commit(&mut self, text: &str) -> bool {
        crate::debug_log!("[IME] Commit: {:?}", text);
        let queued = self
            .session_manager
            .get_active_session_mut()
            .queue_input(text.as_bytes());
        if queued {
            self.clear_block_selection();
        } else {
            log::warn!(
                "terminal input retry buffer full; IME commit retained by neither PTY nor UI"
            );
            self.status_message = "终端输入重试缓冲区已满，IME 文本未发送".to_string();
            self.status_expires_at =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(4));
        }
        queued
    }

    /// Interleave configurable commands and context-sensitive block keys in
    /// original event order. The second return value says a block selection was
    /// (re)established after the most recent accepted/deferred terminal input;
    /// the third reports a non-empty IME commit accepted directly into the
    /// session FIFO before the later keyboard encoder runs.
    pub fn handle_keybindings(
        &mut self,
        ctx: &egui::Context,
        ui_input_blocked: bool,
        terminal_input_blocked: bool,
    ) -> (bool, bool, bool) {
        if !self.config.block_mode && self.block_selection.is_some() {
            self.clear_block_selection();
        }

        let batch = ordered_key_presses(&self.frame_events, &self.keybindings);
        let mut terminal_input_seen = false;
        let mut selection_postdates_terminal_input = false;
        let mut accepted_ime_input = false;
        let mut handled_ime_event_indices = Vec::new();
        let dispatch = dispatch_ordered_key_presses(&batch.presses, |press| {
            if !terminal_input_blocked {
                for commit in &press.ime_commits_before {
                    if !ime_commit_can_queue_now(terminal_input_seen, commit) {
                        continue;
                    }
                    handled_ime_event_indices.push(commit.event_index);
                    if self.queue_ordered_ime_commit(&commit.text) {
                        accepted_ime_input = true;
                        selection_postdates_terminal_input = false;
                    }
                }
            }
            if !terminal_input_blocked && press.terminal_input_before {
                terminal_input_seen = true;
                selection_postdates_terminal_input = false;
            }
            if let Some(command) = press.command.clone() {
                crate::debug_log!(
                    "[KEYBINDING] Looking up: '{}' => {:?}",
                    build_keybinding_string(press.key, press.modifiers).unwrap_or_default(),
                    command
                );
                if ui_input_blocked && !self.command_belongs_to_open_modal(&command) {
                    return OrderedPressOutcome::Ignored;
                }
                if terminal_input_seen {
                    // Leave every later chord in frame_events once older input
                    // awaits the deferred encoder. Besides direct block replay,
                    // this prevents a newly opened modal from suppressing the
                    // older bytes when input blocking is recomputed below.
                    return OrderedPressOutcome::Ignored;
                }
                if !contextual_block_shortcut_available(
                    &command,
                    self.live_block_target().is_some(),
                ) {
                    // Contextual shortcuts with no immutable card target stay
                    // in frame_events for the terminal encoder. Command-palette
                    // invocation still dispatches normally and may show a toast.
                    return OrderedPressOutcome::Ignored;
                }

                let selection_before = self.block_selection.clone();
                if self.dispatch_command(ctx, command.clone()) {
                    return OrderedPressOutcome::CloseViewport;
                }
                let selection_command = matches!(
                    command,
                    keybindings::Command::TerminalScrollUp
                        | keybindings::Command::TerminalScrollDown
                        | keybindings::Command::BlockJumpFirstFailed
                        | keybindings::Command::BlockSelectPrev
                        | keybindings::Command::BlockSelectNext
                        | keybindings::Command::BlockSelectAll
                        | keybindings::Command::BlockJumpPrevFailed
                        | keybindings::Command::BlockJumpNextFailed
                        | keybindings::Command::BlockJumpPrevBookmark
                        | keybindings::Command::BlockJumpNextBookmark
                );
                if self.block_selection.is_some()
                    && (self.block_selection != selection_before || selection_command)
                {
                    selection_postdates_terminal_input = true;
                }

                // A modal opened by this exact press owns the rest of the
                // batch immediately. Its input handler ran before the open, so
                // later Enter/Escape/arrows are withheld until the next frame.
                if ui_input_blocked || self.terminal_input_blocked(ctx) {
                    OrderedPressOutcome::ClaimRest
                } else {
                    OrderedPressOutcome::Consumed
                }
            } else if terminal_input_blocked || terminal_input_seen {
                OrderedPressOutcome::Ignored
            } else if self.handle_block_selection_key_event(&press.event) {
                if self.block_selection.is_some() {
                    selection_postdates_terminal_input = true;
                }
                OrderedPressOutcome::Consumed
            } else {
                // This key remains for the deferred terminal encoder. Any
                // selection action before it is older and must be cleared once
                // the bytes are accepted; a later selection action can set the
                // bit again.
                terminal_input_seen = true;
                selection_postdates_terminal_input = false;
                OrderedPressOutcome::Ignored
            }
        });

        if !terminal_input_blocked && !dispatch.claimed_rest && !dispatch.close_requested {
            for commit in &batch.trailing_ime_commits {
                if !ime_commit_can_queue_now(terminal_input_seen, commit) {
                    continue;
                }
                handled_ime_event_indices.push(commit.event_index);
                if self.queue_ordered_ime_commit(&commit.text) {
                    accepted_ime_input = true;
                    selection_postdates_terminal_input = false;
                }
            }
            if batch.trailing_terminal_input {
                selection_postdates_terminal_input = false;
            }
        }

        // Commits queued above must not be encoded a second time by the later
        // keyboard pass. Reverse indices keep the original positions stable.
        handled_ime_event_indices.sort_unstable();
        handled_ime_event_indices.dedup();
        for event_index in handled_ime_event_indices.into_iter().rev() {
            self.frame_events.remove(event_index);
        }
        for (key, modifiers) in dispatch.consumed {
            consume_bound_key_event(&mut self.frame_events, key, modifiers);
        }
        (
            dispatch.close_requested,
            selection_postdates_terminal_input,
            accepted_ime_input,
        )
    }

    /// Handle one unbound context key for a visible active-pane block range.
    /// Enter returns false when the child owns it (running/alt screen) or the
    /// range has no commands; validation/backpressure failures are consumed and
    /// surfaced so they cannot submit unrelated prompt text.
    fn handle_block_selection_key_event(&mut self, event: &egui::Event) -> bool {
        if !self.config.block_mode {
            self.clear_block_selection();
            return false;
        }
        let (active_session_id, block_canvas_visible) = self
            .session_manager
            .sessions()
            .get(self.session_manager.active_index())
            .map(|session| {
                (
                    Some(session.metadata.session_id.clone()),
                    !session.terminal.lock().is_alt_buffer_active(),
                )
            })
            .unwrap_or((None, false));
        let has_selection = block_selection_context_available(
            self.config.block_mode,
            block_canvas_visible,
            active_session_id.as_ref().is_some_and(|session_id| {
                self.block_selection
                    .as_ref()
                    .is_some_and(|selection| &selection.session_id == session_id)
            }),
        );
        let Some(action) = block_selection_key_action(event, has_selection) else {
            return false;
        };
        match action {
            BlockSelectionKeyAction::Move(step) => self.block_move_selection(step, false),
            BlockSelectionKeyAction::Extend(step) => self.block_move_selection(step, true),
            BlockSelectionKeyAction::Reinput => self.block_reinput_selected_commands_from_enter(),
            BlockSelectionKeyAction::Clear => {
                self.clear_block_selection();
                true
            }
        }
    }

    /// 处理 IME 生命周期/预编辑状态与窗口标题。非空 Commit 的实际
    /// 入队由 `handle_keybindings` 按原始事件顺序完成，这样新打开的 modal
    /// 可以截断其后的 commit，而其前的 commit 已先进入 session FIFO。
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
                    egui::ImeEvent::Commit(_text) => {
                        terminal.clear_preedit();
                        crate::debug_log!("[IME] Commit state: {} bytes", _text.len());
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

        let private_title = self.tabs.flags(self.tabs.active_index()).private_title;
        let window_title = if private_title {
            "Private — Ember".to_string()
        } else {
            let reported_window_title = {
                let terminal = session.terminal.lock();
                terminal.window_title.clone()
            };
            let fallback_title = format!("{} — Ember", Self::session_cwd_title(session));
            super::window::safe_window_title(&reported_window_title, &fallback_title)
        };
        if window_title != self.last_window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(window_title.clone()));
            self.last_window_title = window_title;
        }
        has_preedit
    }

    /// 处理累积的 Ctrl+滚轮字体缩放。
    pub fn handle_font_zoom(&mut self, ctx: &egui::Context) {
        // Route Ctrl+wheel here for both single- and multi-pane layouts. The
        // terminal scroll paths explicitly ignore the same events below.
        // Keyboard zoom goes through configurable `font:*` commands instead.
        self.font_size_accumulator += ctrl_wheel_zoom_delta(&self.frame_events);

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
                        self.apply_font_size_change(ctx);
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
    fn transformed_next_after_last_mark_returns_to_live_bottom() {
        assert!(transformed_next_command_returns_to_bottom(true, false, 7));
        assert!(!transformed_next_command_returns_to_bottom(true, false, 0));
        assert!(!transformed_next_command_returns_to_bottom(true, true, 7));
        assert!(!transformed_next_command_returns_to_bottom(false, false, 7));
    }

    #[test]
    fn copy_prefers_terminal_text_then_whole_blocks() {
        assert_eq!(
            copy_selection_route(true, true),
            CopySelectionRoute::TerminalText
        );
        assert_eq!(
            copy_selection_route(false, true),
            CopySelectionRoute::CommandBlocks
        );
        assert_eq!(copy_selection_route(false, false), CopySelectionRoute::None);
        assert!(!contextual_block_shortcut_available(
            &keybindings::Command::BlockToggleBookmark,
            false,
        ));
        assert!(!contextual_block_shortcut_available(
            &keybindings::Command::BlockReinputSelectedCommands,
            false,
        ));
        assert!(contextual_block_shortcut_available(
            &keybindings::Command::BlockToggleBookmark,
            true,
        ));
    }

    fn key_press(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[derive(Debug, Default, PartialEq, Eq)]
    struct SimulatedOrderedRoute {
        trace: Vec<&'static str>,
        accepted_ime: Vec<String>,
        selected: bool,
        selection_postdates_input: bool,
        claimed_rest: bool,
    }

    /// Exercise the pure batch model with the same ordering decisions used by
    /// handle_keybindings. IME queue admission is assumed to succeed here; the
    /// Session FIFO's atomic cap behavior is covered in session tests.
    fn simulate_ime_selection_route(events: &[egui::Event]) -> SimulatedOrderedRoute {
        let bindings = keybindings::KeyBindings::default_bindings();
        let batch = ordered_key_presses(events, &bindings);
        let mut route = SimulatedOrderedRoute::default();
        let mut terminal_input_seen = false;
        let dispatch = dispatch_ordered_key_presses(&batch.presses, |press| {
            for commit in &press.ime_commits_before {
                if ime_commit_can_queue_now(terminal_input_seen, commit) {
                    route.accepted_ime.push(commit.text.clone());
                    route.selected = false;
                    route.selection_postdates_input = false;
                }
            }
            if press.terminal_input_before {
                terminal_input_seen = true;
                route.selection_postdates_input = false;
            }
            match press.command {
                Some(keybindings::Command::TerminalScrollUp) if terminal_input_seen => {
                    route.trace.push("deferred");
                    OrderedPressOutcome::Ignored
                }
                Some(keybindings::Command::TerminalScrollUp) => {
                    route.trace.push("select");
                    route.selected = true;
                    route.selection_postdates_input = true;
                    OrderedPressOutcome::Consumed
                }
                Some(keybindings::Command::CommandPaletteToggle) if terminal_input_seen => {
                    route.trace.push("deferred-modal");
                    OrderedPressOutcome::Ignored
                }
                Some(keybindings::Command::CommandPaletteToggle) => {
                    route.trace.push("open-palette");
                    OrderedPressOutcome::ClaimRest
                }
                _ => OrderedPressOutcome::Ignored,
            }
        });
        route.claimed_rest = dispatch.claimed_rest;
        if !dispatch.claimed_rest && !dispatch.close_requested {
            for commit in &batch.trailing_ime_commits {
                if ime_commit_can_queue_now(terminal_input_seen, commit) {
                    route.accepted_ime.push(commit.text.clone());
                    route.selected = false;
                    route.selection_postdates_input = false;
                }
            }
            if batch.trailing_terminal_input {
                route.selection_postdates_input = false;
            }
        }
        route
    }

    #[test]
    fn interactive_ui_surfaces_block_terminal_input() {
        assert!(!should_block_terminal_input(
            false, false, false, false, false, false, false
        ));
        // Each surface blocks on its own: search, settings, replace, paste
        // confirmation, palette, block-search picker, focused text edit.
        for index in 0..7 {
            let mut flags = [false; 7];
            flags[index] = true;
            let [search, config, replace, paste, palette, block_search, text_edit] = flags;
            assert!(
                should_block_terminal_input(
                    search,
                    config,
                    replace,
                    paste,
                    palette,
                    block_search,
                    text_edit
                ),
                "surface {index} must block terminal input"
            );
        }
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
    fn block_range_keys_are_contextual_and_leave_running_program_input_alone() {
        let key = |key, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };

        let up = key(egui::Key::ArrowUp, egui::Modifiers::NONE);
        let shift_up = key(egui::Key::ArrowUp, egui::Modifiers::SHIFT);
        let enter = key(egui::Key::Enter, egui::Modifiers::NONE);
        let escape = key(egui::Key::Escape, egui::Modifiers::NONE);

        // With no selection all four continue into the normal terminal path.
        for event in [&up, &shift_up, &enter, &escape] {
            assert_eq!(block_selection_key_action(event, false), None);
        }
        assert_eq!(
            block_selection_key_action(&up, true),
            Some(BlockSelectionKeyAction::Move(
                crate::block_mode::SelectStep::Older
            ))
        );
        assert_eq!(
            block_selection_key_action(&shift_up, true),
            Some(BlockSelectionKeyAction::Extend(
                crate::block_mode::SelectStep::Older
            ))
        );
        assert_eq!(
            block_selection_key_action(&enter, true),
            Some(BlockSelectionKeyAction::Reinput)
        );
        assert_eq!(
            block_selection_key_action(&escape, true),
            Some(BlockSelectionKeyAction::Clear)
        );

        let alt_up = key(egui::Key::ArrowUp, egui::Modifiers::ALT);
        let ctrl_enter = key(egui::Key::Enter, egui::Modifiers::CTRL);
        let shift_enter = key(egui::Key::Enter, egui::Modifiers::SHIFT);
        let shift_escape = key(egui::Key::Escape, egui::Modifiers::SHIFT);
        assert_eq!(block_selection_key_action(&alt_up, true), None);
        assert_eq!(block_selection_key_action(&ctrl_enter, true), None);
        assert_eq!(block_selection_key_action(&shift_enter, true), None);
        assert_eq!(block_selection_key_action(&shift_escape, true), None);
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
    fn bound_commands_consume_printable_text_and_all_shortcuts_in_the_batch() {
        let mut bindings = keybindings::KeyBindings::new();
        bindings
            .bindings
            .insert("shift+x".to_string(), "config:toggle".to_string());
        bindings
            .bindings
            .insert("alt+y".to_string(), "search:open".to_string());
        bindings
            .bindings
            .insert("ctrl+=".to_string(), "font:increase".to_string());
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let alt = egui::Modifiers {
            alt: true,
            ..Default::default()
        };
        let mut events = vec![
            egui::Event::Key {
                key: egui::Key::X,
                physical_key: Some(egui::Key::X),
                pressed: true,
                repeat: false,
                modifiers: shift,
            },
            egui::Event::Text("X".to_string()),
            egui::Event::Key {
                key: egui::Key::Y,
                physical_key: Some(egui::Key::Y),
                pressed: true,
                repeat: false,
                modifiers: alt,
            },
            egui::Event::Text("¥".to_string()),
            egui::Event::Key {
                key: egui::Key::Equals,
                physical_key: Some(egui::Key::Equals),
                pressed: true,
                repeat: false,
                modifiers: ctrl,
            },
        ];

        let batch = ordered_key_presses(&events, &bindings);
        assert_eq!(
            batch
                .presses
                .iter()
                .filter_map(|press| press.command.clone())
                .collect::<Vec<_>>(),
            vec![
                keybindings::Command::ConfigToggle,
                keybindings::Command::SearchOpen,
                keybindings::Command::FontIncrease
            ]
        );
        assert!(
            batch
                .presses
                .iter()
                .all(|press| !press.terminal_input_before),
            "Text paired with consumed printable shortcuts is not later PTY input"
        );
        assert!(!batch.trailing_terminal_input);
        for press in batch.presses {
            consume_bound_key_event(&mut events, press.key, press.modifiers);
        }
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn earlier_terminal_keys_defer_later_block_selection_instead_of_reordering() {
        let bindings = keybindings::KeyBindings::default_bindings();
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let key = |key, modifiers| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        };

        for first in [egui::Key::Enter, egui::Key::ArrowUp] {
            let batch = ordered_key_presses(
                &[
                    key(first, egui::Modifiers::NONE),
                    key(egui::Key::ArrowUp, ctrl),
                ],
                &bindings,
            );
            let mut selected = false;
            let mut terminal_input_seen = false;
            let mut trace = Vec::new();
            let result = dispatch_ordered_key_presses(&batch.presses, |press| {
                terminal_input_seen |= press.terminal_input_before;
                if press.command == Some(keybindings::Command::TerminalScrollUp) {
                    if terminal_input_seen {
                        trace.push("deferred-block-key");
                        OrderedPressOutcome::Ignored
                    } else {
                        selected = true;
                        trace.push("select");
                        OrderedPressOutcome::Consumed
                    }
                } else if block_selection_key_action(&press.event, selected).is_some() {
                    trace.push("block-context");
                    OrderedPressOutcome::Consumed
                } else {
                    terminal_input_seen = true;
                    trace.push("terminal");
                    OrderedPressOutcome::Ignored
                }
            });
            assert_eq!(trace, ["terminal", "deferred-block-key"]);
            assert!(result.consumed.is_empty());
            assert!(!selected);
        }

        // The forward order intentionally does the opposite: selection exists
        // by the time Enter is visited, so Enter becomes block recall.
        let batch = ordered_key_presses(
            &[
                key(egui::Key::ArrowUp, ctrl),
                key(egui::Key::Enter, egui::Modifiers::NONE),
            ],
            &bindings,
        );
        let mut selected = false;
        let mut trace = Vec::new();
        dispatch_ordered_key_presses(&batch.presses, |press| {
            if press.command == Some(keybindings::Command::TerminalScrollUp) {
                selected = true;
                trace.push("select");
            } else if block_selection_key_action(&press.event, selected)
                == Some(BlockSelectionKeyAction::Reinput)
            {
                trace.push("reinput");
            }
            OrderedPressOutcome::Consumed
        });
        assert_eq!(trace, ["select", "reinput"]);
    }

    #[test]
    fn earlier_text_defers_selected_reinput_and_trailing_text_is_recorded() {
        let bindings = keybindings::KeyBindings::default_bindings();
        let ctrl_shift = egui::Modifiers {
            ctrl: true,
            command: true,
            shift: true,
            ..Default::default()
        };
        let reinput = egui::Event::Key {
            key: egui::Key::I,
            physical_key: Some(egui::Key::I),
            pressed: true,
            repeat: false,
            modifiers: ctrl_shift,
        };
        let batch = ordered_key_presses(
            &[egui::Event::Text("echo old".to_owned()), reinput],
            &bindings,
        );
        assert_eq!(batch.presses.len(), 1);
        let press = &batch.presses[0];
        assert!(press.terminal_input_before);
        assert_eq!(
            press.command,
            Some(keybindings::Command::BlockReinputSelectedCommands)
        );
        let mut terminal_input_seen = false;
        let mut replay_dispatched = false;
        let result = dispatch_ordered_key_presses(&batch.presses, |press| {
            terminal_input_seen |= press.terminal_input_before;
            if terminal_input_seen {
                OrderedPressOutcome::Ignored
            } else {
                replay_dispatched = true;
                OrderedPressOutcome::Consumed
            }
        });
        assert!(!replay_dispatched);
        assert!(result.consumed.is_empty());

        let trailing = ordered_key_presses(
            &[
                egui::Event::Key {
                    key: egui::Key::ArrowUp,
                    physical_key: Some(egui::Key::ArrowUp),
                    pressed: true,
                    repeat: false,
                    modifiers: ctrl_shift,
                },
                egui::Event::Text("later".to_owned()),
            ],
            &bindings,
        );
        assert!(trailing.trailing_terminal_input);
    }

    #[test]
    fn ime_commit_and_block_selection_follow_both_event_orders() {
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let commit = egui::Event::Ime(egui::ImeEvent::Commit("你".to_owned()));

        let commit_then_select =
            simulate_ime_selection_route(&[commit.clone(), key_press(egui::Key::ArrowUp, ctrl)]);
        assert_eq!(commit_then_select.accepted_ime, ["你"]);
        assert_eq!(commit_then_select.trace, ["select"]);
        assert!(commit_then_select.selected);
        assert!(commit_then_select.selection_postdates_input);

        let select_then_commit =
            simulate_ime_selection_route(&[key_press(egui::Key::ArrowUp, ctrl), commit]);
        assert_eq!(select_then_commit.trace, ["select"]);
        assert_eq!(select_then_commit.accepted_ime, ["你"]);
        assert!(!select_then_commit.selected);
        assert!(!select_then_commit.selection_postdates_input);

        let empty_then_select = simulate_ime_selection_route(&[
            egui::Event::Ime(egui::ImeEvent::Commit(String::new())),
            key_press(egui::Key::ArrowUp, ctrl),
        ]);
        assert!(empty_then_select.accepted_ime.is_empty());
        assert_eq!(empty_then_select.trace, ["select"]);
        assert!(empty_then_select.selected);
    }

    #[test]
    fn modal_boundary_accepts_only_ime_commits_before_the_opener() {
        let ctrl_shift = egui::Modifiers {
            ctrl: true,
            command: true,
            shift: true,
            ..Default::default()
        };
        let opener = key_press(egui::Key::P, ctrl_shift);
        let commit = egui::Event::Ime(egui::ImeEvent::Commit("你".to_owned()));

        let opener_then_commit = simulate_ime_selection_route(&[opener.clone(), commit.clone()]);
        assert_eq!(opener_then_commit.trace, ["open-palette"]);
        assert!(opener_then_commit.claimed_rest);
        assert!(opener_then_commit.accepted_ime.is_empty());

        let commit_then_opener = simulate_ime_selection_route(&[commit, opener]);
        assert_eq!(commit_then_opener.trace, ["open-palette"]);
        assert!(commit_then_opener.claimed_rest);
        assert_eq!(commit_then_opener.accepted_ime, ["你"]);

        let mut terminal = crate::terminal::TerminalState::new(80, 24);
        terminal.ime_enabled = true;
        terminal.set_preedit("leaked-after-opener".to_owned(), 4);
        clear_terminal_preedit_for_ui_owner(&mut terminal, opener_then_commit.claimed_rest);
        assert!(!terminal.ime_enabled);
        assert!(terminal.preedit_text.is_empty());
    }

    #[test]
    fn accepted_paste_clears_selection_on_either_side_of_ctrl_up() {
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let paste = egui::Event::Paste("clipboard".to_owned());

        let paste_then_select =
            simulate_ime_selection_route(&[paste.clone(), key_press(egui::Key::ArrowUp, ctrl)]);
        assert_eq!(paste_then_select.trace, ["deferred"]);
        assert!(!paste_then_select.selected);
        assert!(accepted_terminal_input_clears_block_selection(
            true,
            paste_then_select.selection_postdates_input
        ));

        let select_then_paste =
            simulate_ime_selection_route(&[key_press(egui::Key::ArrowUp, ctrl), paste]);
        assert_eq!(select_then_paste.trace, ["select"]);
        assert!(select_then_paste.selected);
        assert!(!select_then_paste.selection_postdates_input);
        assert!(accepted_terminal_input_clears_block_selection(
            true,
            select_then_paste.selection_postdates_input
        ));
        assert!(!accepted_terminal_input_clears_block_selection(
            false, false
        ));
    }

    #[test]
    fn semantic_paste_claim_boundary_withholds_enter_and_ime_suffixes() {
        let enter = key_press(egui::Key::Enter, egui::Modifiers::NONE);
        let commit = egui::Event::Ime(egui::ImeEvent::Commit("你".to_owned()));

        let risky_paste_then_enter = [
            egui::Event::Paste("first\nsecond".to_owned()),
            enter.clone(),
        ];
        assert!(
            terminal_events_before_semantic_paste_claim(&risky_paste_then_enter, true).is_empty()
        );

        let paste_then_commit = [egui::Event::Paste("clipboard".to_owned()), commit];
        assert!(terminal_events_before_semantic_paste_claim(&paste_then_commit, true).is_empty());

        // Input before the semantic Paste remains reachable and therefore
        // precedes the OSC notification/confirmation boundary.
        let with_prefix = [
            egui::Event::Text("before".to_owned()),
            egui::Event::Paste("clipboard".to_owned()),
            enter,
        ];
        assert_eq!(
            terminal_events_before_semantic_paste_claim(&with_prefix, true),
            [egui::Event::Text("before".to_owned())]
        );
        assert_eq!(
            terminal_events_before_semantic_paste_claim(&with_prefix, false),
            with_prefix
        );
    }

    #[test]
    fn osc_paste_starts_only_at_a_clean_terminal_input_boundary() {
        let paste = egui::Event::Paste("clipboard".to_owned());
        assert!(osc_paste_route_is_clean(
            false,
            false,
            std::slice::from_ref(&paste)
        ));
        assert!(!osc_paste_route_is_clean(
            true,
            false,
            std::slice::from_ref(&paste)
        ));
        assert!(!osc_paste_route_is_clean(
            false,
            true,
            std::slice::from_ref(&paste)
        ));
        assert!(
            osc_paste_route_is_clean(
                false,
                false,
                &[paste.clone(), egui::Event::Text("suffix".to_owned())]
            ),
            "input after Paste is claimed rather than an older FIFO prefix"
        );

        let wheel = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 1.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        };
        let pointer_button = egui::Event::PointerButton {
            pos: egui::pos2(1.0, 1.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        };
        for prefix in [
            key_press(egui::Key::Enter, egui::Modifiers::NONE),
            egui::Event::Text("prefix".to_owned()),
            egui::Event::Ime(egui::ImeEvent::Commit("你".to_owned())),
            wheel,
            pointer_button,
            egui::Event::PointerMoved(egui::pos2(2.0, 2.0)),
        ] {
            assert!(!osc_paste_route_is_clean(
                false,
                false,
                &[prefix, paste.clone()]
            ));
        }

        for empty_prefix in [
            egui::Event::Text(String::new()),
            egui::Event::Ime(egui::ImeEvent::Commit(String::new())),
        ] {
            assert!(osc_paste_route_is_clean(
                false,
                false,
                &[empty_prefix, paste.clone()]
            ));
        }
    }

    #[test]
    fn fallback_paste_defers_behind_text_key_and_ime_prefixes() {
        let paste = egui::Event::Paste("safe".to_owned());
        for prefix in [
            egui::Event::Text("before".to_owned()),
            key_press(egui::Key::Enter, egui::Modifiers::NONE),
            egui::Event::Ime(egui::ImeEvent::Commit("先".to_owned())),
        ] {
            assert!(semantic_paste_direct_input_blocked(
                false,
                &[prefix, paste.clone()]
            ));
        }
        assert!(!semantic_paste_direct_input_blocked(
            false,
            std::slice::from_ref(&paste)
        ));
        assert!(semantic_paste_direct_input_blocked(true, &[paste]));
    }

    #[test]
    fn rejected_mouse_prefix_paste_keeps_pointer_route_but_accepted_paste_claims_it() {
        let paste = egui::Event::Paste("clipboard".to_owned());
        let pointer = egui::Event::PointerButton {
            pos: egui::pos2(1.0, 1.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        };
        let wheel = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 1.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        };

        for prefix in [pointer, wheel] {
            assert!(semantic_paste_has_mouse_prefix(&[
                prefix.clone(),
                paste.clone()
            ]));
            assert!(!semantic_paste_precedes_mouse_input(&[
                prefix,
                paste.clone()
            ]));
            assert!(!semantic_paste_pointer_input_blocked(true, true, false));
        }
        assert!(!semantic_paste_has_mouse_prefix(std::slice::from_ref(
            &paste
        )));
        let pointer_suffix = egui::Event::PointerButton {
            pos: egui::pos2(1.0, 1.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        };
        assert!(semantic_paste_precedes_mouse_input(&[
            paste.clone(),
            pointer_suffix
        ]));
        assert!(semantic_paste_pointer_input_blocked(true, false, true));
        assert!(semantic_paste_pointer_input_blocked(false, false, true));
    }

    #[test]
    fn retained_transcript_only_bypasses_its_own_read_only_pointer_boundary() {
        assert!(!retained_terminal_pointer_input_blocked(
            true, true, false, false
        ));
        assert!(retained_terminal_pointer_input_blocked(
            true, true, true, false
        ));
        assert!(retained_terminal_pointer_input_blocked(
            true, true, false, true
        ));
        assert!(retained_terminal_pointer_input_blocked(
            true, false, false, false
        ));
        assert!(!retained_terminal_pointer_input_blocked(
            false, false, false, false
        ));
    }

    #[test]
    fn rejected_keyboard_fifo_batch_does_not_clear_block_selection() {
        // `Session::queue_input` returns false atomically at its 8-MiB cap.
        // The final selection decision must use that admission result, not the
        // mere presence of encoded bytes in keyboard_input_buffer.
        let keyboard_input_accepted = false;
        let accepted_ime_input = false;
        let accepted_paste_input = false;
        let accepted_terminal_input =
            keyboard_input_accepted || accepted_ime_input || accepted_paste_input;
        assert!(!accepted_terminal_input_clears_block_selection(
            accepted_terminal_input,
            false
        ));
        assert!(accepted_terminal_input_clears_block_selection(true, false));
    }

    #[test]
    fn accepted_ctrl_d_retires_block_key_ownership_but_rejected_input_does_not() {
        assert!(accepted_terminal_input_clears_block_selection(true, false));
        assert!(!accepted_terminal_input_clears_block_selection(
            false, false
        ));
    }

    #[test]
    fn newly_opened_palette_claims_the_rest_of_the_same_batch() {
        let bindings = keybindings::KeyBindings::default_bindings();
        let ctrl_shift = egui::Modifiers {
            ctrl: true,
            command: true,
            shift: true,
            ..Default::default()
        };
        let key = |key, modifiers| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        };
        let batch = ordered_key_presses(
            &[
                key(egui::Key::P, ctrl_shift),
                key(egui::Key::ArrowUp, egui::Modifiers::NONE),
                key(egui::Key::Enter, egui::Modifiers::NONE),
            ],
            &bindings,
        );
        let mut trace = Vec::new();
        let result = dispatch_ordered_key_presses(&batch.presses, |press| {
            if press.command == Some(keybindings::Command::CommandPaletteToggle) {
                trace.push("open-palette");
                OrderedPressOutcome::ClaimRest
            } else {
                trace.push("leaked");
                OrderedPressOutcome::Consumed
            }
        });
        assert_eq!(trace, ["open-palette"]);
        assert_eq!(result.consumed, [(egui::Key::P, ctrl_shift)]);
    }

    #[test]
    fn disabled_block_mode_keeps_ctrl_scroll_and_terminal_keys() {
        let bindings = keybindings::KeyBindings::default_bindings();
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let key = |key, modifiers| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        };
        let batch = ordered_key_presses(
            &[
                key(egui::Key::ArrowUp, ctrl),
                key(egui::Key::ArrowUp, egui::Modifiers::NONE),
                key(egui::Key::Enter, egui::Modifiers::NONE),
            ],
            &bindings,
        );
        let block_mode = false;
        let stale_selection = true;
        let mut trace = Vec::new();
        dispatch_ordered_key_presses(&batch.presses, |press| {
            if press.command == Some(keybindings::Command::TerminalScrollUp) {
                trace.push(if block_mode { "select" } else { "scroll" });
            } else if block_selection_key_action(
                &press.event,
                block_selection_context_available(block_mode, true, stale_selection),
            )
            .is_some()
            {
                trace.push("block-context");
            } else {
                trace.push("terminal");
            }
            OrderedPressOutcome::Consumed
        });
        assert_eq!(trace, ["scroll", "terminal", "terminal"]);
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

    #[test]
    fn closing_a_modal_does_not_release_the_rest_of_its_input_batch() {
        assert!(terminal_input_blocked_after_commands(true, false, false));
        assert!(terminal_input_blocked_after_commands(false, true, false));
        assert!(terminal_input_blocked_after_commands(false, false, true));
        assert!(!terminal_input_blocked_after_commands(false, false, false));
    }

    #[test]
    fn paste_confirmation_keeps_focus_off_the_pty_when_replace_panel_stays_open() {
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: Some(egui::Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let blocked = should_block_terminal_input(
            false, // search
            false, // settings
            true,  // Find & Replace remains open behind the confirmation
            true,  // paste confirmation owns keyboard focus
            false, // command palette
            false, // block search picker
            false, // no unrelated text editor focus
        );

        assert!(blocked);
        assert!(routed_terminal_events(&[enter], blocked).is_empty());
    }
}
