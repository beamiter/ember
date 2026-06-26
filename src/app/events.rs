// Event processing module

use super::state::TerminalApp;
use eframe::egui;

/// 检查是否应该恢复终端快捷键事件
pub fn should_restore_terminal_shortcut_event(
    ctx: &egui::Context,
    modifiers: egui::Modifiers,
) -> bool {
    !ctx.text_edit_focused() && modifiers.command && !modifiers.alt
}

/// 将快捷键事件转换为按键事件
pub fn shortcut_event_to_key_event(
    event: egui::Event,
    modifiers: egui::Modifiers,
) -> Option<egui::Event> {
    let key = match event {
        egui::Event::Copy => {
            crate::debug_log!("[EVENT] converting Copy to Key::C");
            egui::Key::C
        }
        egui::Event::Cut => {
            crate::debug_log!("[EVENT] converting Cut to Key::X");
            egui::Key::X
        }
        egui::Event::Paste(ref _content) => {
            crate::debug_log!("[EVENT] converting Paste to Key::V (content: {} bytes, modifiers: ctrl={} shift={} alt={})",
                             _content.len(), modifiers.ctrl, modifiers.shift, modifiers.alt);
            egui::Key::V
        }
        _ => return None,
    };

    Some(egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers,
    })
}

/// 规范化终端快捷键事件
pub fn normalize_terminal_shortcut_events(
    events: &mut Vec<egui::Event>,
    modifiers: egui::Modifiers,
    restore_shortcuts: bool,
    preserve_paste_event: bool,
) {
    crate::debug_log!(
        "[NORMALIZE] input: {} events, restore_shortcuts={}, preserve_paste_event={}",
        events.len(),
        restore_shortcuts,
        preserve_paste_event
    );

    let mut normalized_events = Vec::with_capacity(events.len());

    for event in events.drain(..) {
        match &event {
            egui::Event::Paste(_) => {
                crate::debug_log!("[NORMALIZE] found Paste event");
            }
            egui::Event::Copy => {
                crate::debug_log!("[NORMALIZE] found Copy event");
            }
            egui::Event::Cut => {
                crate::debug_log!("[NORMALIZE] found Cut event");
            }
            egui::Event::Key {
                key: _key,
                modifiers: _key_mods,
                pressed: _pressed,
                ..
            } => {
                crate::debug_log!(
                    "[NORMALIZE] found Key event: {:?} pressed={} ctrl={} shift={}",
                    _key,
                    _pressed,
                    _key_mods.ctrl,
                    _key_mods.shift
                );
            }
            _ => {}
        }

        if preserve_paste_event && matches!(event, egui::Event::Paste(_)) {
            crate::debug_log!("[NORMALIZE] preserving Paste (preserve_paste_event=true)");
            normalized_events.push(event);
            continue;
        }

        if restore_shortcuts
            && matches!(
                event,
                egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_)
            )
        {
            if let Some(key_event) = shortcut_event_to_key_event(event, modifiers) {
                crate::debug_log!("[NORMALIZE] converted to Key event via restore_shortcuts");
                normalized_events.push(key_event);
            }
            continue;
        }

        // 既不恢复为按键也不保留：丢弃语义剪贴板事件，避免泄漏进终端
        if matches!(
            event,
            egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_)
        ) {
            crate::debug_log!("[NORMALIZE] dropping semantic clipboard event");
            continue;
        }

        normalized_events.push(event);
    }

    *events = normalized_events;

    crate::debug_log!("[NORMALIZE] output: {} events", events.len());
}

/// 将 egui::Key 转换为字符串表示（零分配版本）
pub fn key_to_string(key: egui::Key) -> Option<&'static str> {
    match key {
        egui::Key::Enter => Some("return"),
        egui::Key::Escape => Some("escape"),
        egui::Key::Backspace => Some("backspace"),
        egui::Key::Tab => Some("tab"),
        egui::Key::ArrowUp => Some("up"),
        egui::Key::ArrowDown => Some("down"),
        egui::Key::ArrowLeft => Some("left"),
        egui::Key::ArrowRight => Some("right"),
        egui::Key::Home => Some("home"),
        egui::Key::End => Some("end"),
        egui::Key::Insert => Some("insert"),
        egui::Key::Delete => Some("delete"),
        egui::Key::PageUp => Some("pageup"),
        egui::Key::PageDown => Some("pagedown"),
        egui::Key::F1 => Some("f1"),
        egui::Key::F2 => Some("f2"),
        egui::Key::F3 => Some("f3"),
        egui::Key::F4 => Some("f4"),
        egui::Key::F5 => Some("f5"),
        egui::Key::F6 => Some("f6"),
        egui::Key::F7 => Some("f7"),
        egui::Key::F8 => Some("f8"),
        egui::Key::F9 => Some("f9"),
        egui::Key::F10 => Some("f10"),
        egui::Key::F11 => Some("f11"),
        egui::Key::F12 => Some("f12"),
        egui::Key::A => Some("a"),
        egui::Key::B => Some("b"),
        egui::Key::C => Some("c"),
        egui::Key::D => Some("d"),
        egui::Key::E => Some("e"),
        egui::Key::F => Some("f"),
        egui::Key::G => Some("g"),
        egui::Key::H => Some("h"),
        egui::Key::I => Some("i"),
        egui::Key::J => Some("j"),
        egui::Key::K => Some("k"),
        egui::Key::L => Some("l"),
        egui::Key::M => Some("m"),
        egui::Key::N => Some("n"),
        egui::Key::O => Some("o"),
        egui::Key::P => Some("p"),
        egui::Key::Q => Some("q"),
        egui::Key::R => Some("r"),
        egui::Key::S => Some("s"),
        egui::Key::T => Some("t"),
        egui::Key::U => Some("u"),
        egui::Key::V => Some("v"),
        egui::Key::W => Some("w"),
        egui::Key::X => Some("x"),
        egui::Key::Y => Some("y"),
        egui::Key::Z => Some("z"),
        egui::Key::Num0 => Some("0"),
        egui::Key::Num1 => Some("1"),
        egui::Key::Num2 => Some("2"),
        egui::Key::Num3 => Some("3"),
        egui::Key::Num4 => Some("4"),
        egui::Key::Num5 => Some("5"),
        egui::Key::Num6 => Some("6"),
        egui::Key::Num7 => Some("7"),
        egui::Key::Num8 => Some("8"),
        egui::Key::Num9 => Some("9"),
        egui::Key::Comma => Some(","),
        egui::Key::Period => Some("."),
        egui::Key::Plus => Some("+"),
        egui::Key::Minus => Some("-"),
        egui::Key::Slash => Some("/"),
        egui::Key::Backslash => Some("\\"),
        egui::Key::Semicolon => Some(";"),
        egui::Key::Quote => Some("'"),
        egui::Key::OpenBracket => Some("["),
        egui::Key::CloseBracket => Some("]"),
        egui::Key::Equals => Some("="),
        egui::Key::Backtick => Some("`"),
        _ => None,
    }
}

pub fn build_keybinding_string(key: egui::Key, modifiers: egui::Modifiers) -> Option<String> {
    let key_str = key_to_string(key)?;
    let mut buf = String::with_capacity(32);

    if modifiers.ctrl {
        buf.push_str("ctrl+");
    }
    if modifiers.shift {
        buf.push_str("shift+");
    }
    if modifiers.alt {
        buf.push_str("alt+");
    }
    #[cfg(target_os = "macos")]
    if modifiers.mac_cmd || modifiers.command_only() {
        buf.push_str("super+");
    }

    buf.push_str(key_str);
    Some(buf)
}

impl TerminalApp {
    // Event processing methods will be moved here
}
