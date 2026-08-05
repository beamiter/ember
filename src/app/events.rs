// Event processing module

use super::state::TerminalApp;
use eframe::egui;

/// Tracks a V press until its matching release across raw-input batches.
///
/// `egui-winit` replaces a Ctrl/Cmd+V press with [`egui::Event::Paste`] when
/// it can read text from the clipboard. The matching V release is still a
/// regular key event and can arrive in a later batch. Remembering the press is
/// essential: otherwise that later release is indistinguishable from the
/// release left behind when an image-only Ctrl+V press was swallowed.
#[derive(Debug, Default)]
pub struct PasteKeyState {
    v_press_pending_release: bool,
}

impl PasteKeyState {
    pub fn reset(&mut self) {
        self.v_press_pending_release = false;
    }
}

/// 检查是否应该恢复终端快捷键事件
pub fn should_restore_terminal_shortcut_event(
    ctx: &egui::Context,
    modifiers: egui::Modifiers,
) -> bool {
    !ctx.text_edit_focused() && modifiers.command && !modifiers.alt
}

/// Recover the modifiers that accompanied a semantic paste shortcut.
///
/// `egui::Event::Paste` carries only clipboard text. If V and the modifiers
/// are released before eframe drains the window-event batch,
/// `RawInput::modifiers` already contains the final (usually empty) state.
/// Winit still emits V's release with its event-time modifiers, which lets us
/// distinguish Ctrl+Shift+V from an application's ordinary Ctrl+V.
pub fn semantic_paste_modifiers(
    events: &[egui::Event],
    fallback: egui::Modifiers,
) -> egui::Modifiers {
    if !events
        .iter()
        .any(|event| matches!(event, egui::Event::Paste(_)))
    {
        return fallback;
    }

    let release = events.iter().rev().find_map(|event| match event {
        egui::Event::Key {
            key: egui::Key::V,
            pressed: false,
            modifiers,
            ..
        } => Some(*modifiers),
        _ => None,
    });

    let Some(release) = release else {
        return fallback;
    };

    let mut recovered = fallback;
    recovered.alt |= release.alt;
    recovered.shift |= release.shift;
    recovered.ctrl |= release.ctrl;
    recovered.mac_cmd |= release.mac_cmd;
    recovered.command = true;
    if !cfg!(target_os = "macos") {
        // A semantic Paste event proves Ctrl/Cmd was held when V was pressed.
        // On Linux and Windows `command` is the Ctrl shortcut modifier.
        recovered.ctrl = true;
    }
    recovered
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

        // OSC 5522 owns the application's ordinary Ctrl+V paste event, but
        // Ctrl+Shift+V is ember's explicit host-text paste shortcut. Egui
        // represents both as Event::Paste, so keeping the shifted event here
        // would incorrectly send Codex an OSC 5522 MIME notification and make
        // it try an image paste instead of inserting the clipboard text.
        let explicit_text_paste = restore_shortcuts
            && modifiers.ctrl
            && modifiers.shift
            && matches!(event, egui::Event::Paste(_));
        if preserve_paste_event && matches!(event, egui::Event::Paste(_)) && !explicit_text_paste {
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

/// Restore a Ctrl+V press swallowed by egui-winit for image-only clipboards.
///
/// egui-winit emits only the release event when its text clipboard lookup has
/// no data. This also happens when Ctrl has already been released by the time
/// V's release reaches us, so the missing shortcut is identified from the
/// absent press/Paste events rather than its release modifiers.
pub fn restore_missing_image_paste_key_event(
    events: &mut Vec<egui::Event>,
    state: &mut PasteKeyState,
) -> bool {
    let mut swallowed_press_release = None;

    for event in events.iter() {
        match event {
            // A semantic paste replaces the Ctrl/Cmd+V press, but not its
            // release. Keep that press alive across frames so the release is
            // consumed instead of being turned into a second Ctrl+V.
            egui::Event::Paste(_) => state.v_press_pending_release = true,
            egui::Event::Key {
                key: egui::Key::V,
                pressed: true,
                ..
            } => state.v_press_pending_release = true,
            egui::Event::Key {
                key: egui::Key::V,
                pressed: false,
                modifiers,
                ..
            } => {
                if state.v_press_pending_release {
                    state.v_press_pending_release = false;
                } else if !modifiers.shift {
                    swallowed_press_release = Some(*modifiers);
                }
            }
            _ => {}
        }
    }

    let Some(mut modifiers) = swallowed_press_release else {
        return false;
    };

    // egui's global modifiers may be clear if Ctrl was released first. We are
    // restoring a swallowed Ctrl+V, so explicitly restore both fields used by
    // egui on Linux/Windows for the Ctrl modifier.
    modifiers.ctrl = true;
    modifiers.command = true;

    events.insert(
        0,
        egui::Event::Key {
            key: egui::Key::V,
            physical_key: Some(egui::Key::V),
            pressed: true,
            repeat: false,
            modifiers,
        },
    );
    true
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
        // canonical 存储形式是单词 `backslash`（jterm_core 的家族约定，
        // 让 TOML/JSON 配置无需转义）；这里直接产出 canonical 形式，
        // 绑定表查找就不会错过。
        egui::Key::Backslash => Some("backslash"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directional_shortcuts_use_the_keybinding_modifier_order() {
        let focus = egui::Modifiers {
            ctrl: true,
            alt: true,
            command: true,
            ..Default::default()
        };
        assert_eq!(
            build_keybinding_string(egui::Key::ArrowLeft, focus).as_deref(),
            Some("ctrl+alt+left")
        );

        let resize = egui::Modifiers {
            ctrl: true,
            shift: true,
            alt: true,
            command: true,
            ..Default::default()
        };
        assert_eq!(
            build_keybinding_string(egui::Key::ArrowDown, resize).as_deref(),
            Some("ctrl+shift+alt+down")
        );
    }

    #[test]
    fn sidebar_and_help_punctuation_chords_are_distinct() {
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        assert_eq!(
            build_keybinding_string(egui::Key::Backslash, ctrl).as_deref(),
            Some("ctrl+backslash")
        );

        let ctrl_shift = egui::Modifiers {
            shift: true,
            ..ctrl
        };
        assert_eq!(
            build_keybinding_string(egui::Key::Slash, ctrl_shift).as_deref(),
            Some("ctrl+shift+/")
        );
    }

    /// 运行时构造的查找键必须已经是 jterm_core 的 canonical 形式：
    /// `get_command` 虽会再规范化一次，但这里锁死"构造即 canonical"，
    /// 使查找永远不可能因拼写差异而错过绑定表键。
    #[test]
    fn build_keybinding_string_always_emits_core_canonical_chords() {
        let mod_sets = [
            egui::Modifiers::NONE,
            egui::Modifiers {
                ctrl: true,
                command: true,
                ..Default::default()
            },
            egui::Modifiers {
                ctrl: true,
                shift: true,
                command: true,
                ..Default::default()
            },
            egui::Modifiers {
                ctrl: true,
                shift: true,
                alt: true,
                command: true,
                ..Default::default()
            },
        ];
        for key in egui::Key::ALL {
            for modifiers in mod_sets {
                let Some(chord) = build_keybinding_string(*key, modifiers) else {
                    continue;
                };
                let parsed = jterm_core::keybindings::parse(&chord)
                    .unwrap_or_else(|e| panic!("chord {chord:?} must parse in core: {e}"));
                assert_eq!(
                    parsed.canonical(),
                    chord,
                    "build_keybinding_string output must be a canonical fixed point"
                );
            }
        }
    }

    #[test]
    fn font_shortcuts_round_trip_from_egui_events_to_default_commands() {
        let bindings = crate::keybindings::KeyBindings::default_bindings();
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let ctrl_shift = egui::Modifiers {
            shift: true,
            ..ctrl
        };

        for (key, modifiers, expected) in [
            (
                egui::Key::Equals,
                ctrl,
                crate::keybindings::Command::FontIncrease,
            ),
            (
                egui::Key::Plus,
                ctrl,
                crate::keybindings::Command::FontIncrease,
            ),
            (
                egui::Key::Plus,
                ctrl_shift,
                crate::keybindings::Command::FontIncrease,
            ),
            (
                egui::Key::Minus,
                ctrl,
                crate::keybindings::Command::FontDecrease,
            ),
            (
                egui::Key::Num0,
                ctrl,
                crate::keybindings::Command::FontReset,
            ),
        ] {
            let chord = build_keybinding_string(key, modifiers).unwrap();
            assert_eq!(bindings.get_command(&chord), Some(expected), "{chord}");
        }
    }
}
