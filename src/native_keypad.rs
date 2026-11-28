//! Keep physical keypad identity through the terminal's ordered input filters.
use crate::ui::{
    is_modifier_only_key, normalized_modifiers, pty_text_payload, KeypadEncoding, RoutedKeypad,
};
use eframe::NativeKeypadEvent;
use egui::Event;
use jterm_engine::input::{encode_kitty_keypad_press, Key, KeypadKey};
use winit::keyboard::{Key as NativeKey, KeyCode, NamedKey};

#[derive(Default)]
pub(crate) struct FrameKeypad(Vec<KeypadPress>);

struct KeypadPress {
    anchor: KeypadAnchor,
    text: Option<EventPosition>,
    position: KeypadKey,
    center: bool,
    num_locked: bool,
    logical: Key,
    modifiers: egui::Modifiers,
}

enum KeypadAnchor {
    Event(EventPosition),
    Gap {
        left: Option<EventPosition>,
        right: Option<EventPosition>,
    },
}

struct EventPosition {
    event: Event,
    ordinal: usize,
    occurrences: usize,
}

impl FrameKeypad {
    pub fn capture(input: &egui::RawInput, native: Vec<NativeKeypadEvent>) -> Self {
        if !input.focused || native.is_empty() {
            return Self::default();
        }
        let events = &input.events;
        let focus_start = events
            .iter()
            .rposition(|event| matches!(event, Event::WindowFocused(false)))
            .map_or(0, |last| last + 1);
        Self(
            native
                .into_iter()
                .filter_map(|native| {
                    if native.first_event < focus_start {
                        return None;
                    }
                    let first = native.first_event;
                    let group = events.get(first..first.checked_add(native.event_count)?)?;
                    let (anchor, text) = match group {
                        [] => (KeypadAnchor::gap(events, first)?, None),
                        [Event::Key { pressed: true, .. }] | [Event::Text(_)] => (
                            KeypadAnchor::Event(EventPosition::capture(events, first)),
                            None,
                        ),
                        [Event::Key { pressed: true, .. }, Event::Text(_)] => (
                            KeypadAnchor::Event(EventPosition::capture(events, first)),
                            Some(EventPosition::capture(events, first + 1)),
                        ),
                        _ => return None,
                    };
                    Some(KeypadPress {
                        anchor,
                        text,
                        position: keypad_position(native.key_code)?,
                        center: native.center,
                        num_locked: native.numeric_layer,
                        logical: keypad_logical_key(&native.logical_key),
                        modifiers: native.modifiers,
                    })
                })
                .collect(),
        )
    }

    pub fn route(
        self,
        events: &[Event],
        application: bool,
        keyboard_flags: u16,
    ) -> Vec<RoutedKeypad> {
        let mut routed = Vec::with_capacity(self.0.len());
        for press in self.0 {
            let before_event = matches!(&press.anchor, KeypadAnchor::Gap { .. });
            let Some(event) = press.anchor.locate(events) else {
                continue;
            };
            let text = match press.text {
                Some(text) if text.locate(events) == Some(event + 1) => Some(event + 1),
                Some(_) => continue,
                None => None,
            };
            let encoding = if keyboard_flags & 0b1001 != 0 {
                let associated = if !before_event && keyboard_flags & 0b11000 == 0b11000 {
                    match events.get(text.unwrap_or(event)) {
                        Some(Event::Text(value)) => pty_text_payload(value),
                        _ => None,
                    }
                } else {
                    None
                };
                let Some(report) = encode_kitty_keypad_press(
                    press.position,
                    press.logical,
                    normalized_modifiers(press.modifiers),
                    press.num_locked,
                    keyboard_flags,
                    associated.as_deref(),
                ) else {
                    continue;
                };
                KeypadEncoding::Kitty(report)
            } else if keyboard_flags == 0
                && !before_event
                && !press.num_locked
                && !press.modifiers.any()
            {
                if application {
                    let Some(sequence) = press.position.application_sequence() else {
                        continue;
                    };
                    KeypadEncoding::Application(sequence)
                } else if press.center {
                    KeypadEncoding::Center
                } else {
                    continue;
                }
            } else {
                continue;
            };
            routed.push(RoutedKeypad {
                event,
                text,
                encoding,
                before_event,
            });
        }
        routed.sort_by_key(|press| (press.event, !press.before_event));
        routed
    }
}

impl KeypadAnchor {
    fn gap(events: &[Event], index: usize) -> Option<Self> {
        // Some modified keypad symbols have no egui Key or Text event.
        // Anchor their position to surviving neighbors without adding an
        // input event to the widgets or copying arbitrary text or paste data.
        let left = match events[..index]
            .iter()
            .rposition(|event| !gap_ignores(event))
        {
            Some(index) => Some(EventPosition::neighbor(events, index)?),
            None => None,
        };
        let right = match events[index..].iter().position(|event| !gap_ignores(event)) {
            Some(offset) => Some(EventPosition::neighbor(events, index + offset)?),
            None => None,
        };
        Some(Self::Gap { left, right })
    }

    fn locate(&self, events: &[Event]) -> Option<usize> {
        match self {
            Self::Event(event) => event.locate(events),
            Self::Gap { left, right } => {
                let first = match left {
                    Some(event) => event.locate(events)? + 1,
                    None => 0,
                };
                let last = match right {
                    Some(event) => event.locate(events)?,
                    None => events.len(),
                };
                (first <= last && events[first..last].iter().all(gap_ignores)).then_some(last)
            }
        }
    }
}

fn gap_ignores(event: &Event) -> bool {
    match event {
        Event::Key { key, pressed, .. } => !pressed || is_modifier_only_key(*key),
        Event::ModifiersChanged(_)
        | Event::WindowFocused(_)
        | Event::PointerMoved(_)
        | Event::PointerGone => true,
        _ => false,
    }
}

impl EventPosition {
    fn neighbor(events: &[Event], index: usize) -> Option<Self> {
        match events.get(index)? {
            Event::Key { .. } => Some(Self::capture(events, index)),
            Event::Text(text) if text.len() <= 4 => Some(Self::capture(events, index)),
            _ => None,
        }
    }

    fn capture(events: &[Event], index: usize) -> Self {
        let event = &events[index];
        let ordinal = events[..index]
            .iter()
            .filter(|other| same_event(event, other))
            .count();
        let occurrences = events[index..]
            .iter()
            .filter(|other| same_event(event, other))
            .count()
            + ordinal;
        Self {
            event: event.clone(),
            ordinal,
            occurrences,
        }
    }

    fn locate(&self, events: &[Event]) -> Option<usize> {
        let mut candidates = events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| same_event(&self.event, event).then_some(index));
        let index = candidates.nth(self.ordinal)?;
        // GUI filters can consume a logical Enter shared by two physical keys.
        // Keep the physical identity when occurrence order still identifies it.
        (self.ordinal + 1 + candidates.count() == self.occurrences).then_some(index)
    }
}

fn keypad_logical_key(key: &NativeKey) -> Key {
    match key {
        NativeKey::Named(NamedKey::ArrowLeft) => Key::ArrowLeft,
        NativeKey::Named(NamedKey::ArrowRight) => Key::ArrowRight,
        NativeKey::Named(NamedKey::ArrowUp) => Key::ArrowUp,
        NativeKey::Named(NamedKey::ArrowDown) => Key::ArrowDown,
        NativeKey::Named(NamedKey::PageUp) => Key::PageUp,
        NativeKey::Named(NamedKey::PageDown) => Key::PageDown,
        NativeKey::Named(NamedKey::Home) => Key::Home,
        NativeKey::Named(NamedKey::End) => Key::End,
        NativeKey::Named(NamedKey::Insert) => Key::Insert,
        NativeKey::Named(NamedKey::Delete) => Key::Delete,
        NativeKey::Named(NamedKey::Enter) => Key::Enter,
        NativeKey::Character(text) => text
            .chars()
            .next()
            .map_or(Key::Unidentified, Key::Character),
        _ => Key::Unidentified,
    }
}

fn same_event(left: &Event, right: &Event) -> bool {
    if let (
        Event::Key {
            key: left_key,
            physical_key: left_physical,
            pressed: left_pressed,
            modifiers: left_modifiers,
            ..
        },
        Event::Key {
            key: right_key,
            physical_key: right_physical,
            pressed: right_pressed,
            modifiers: right_modifiers,
            ..
        },
    ) = (left, right)
    {
        // egui computes repeat after the native converter records the event.
        (left_key, left_physical, left_pressed, left_modifiers)
            == (right_key, right_physical, right_pressed, right_modifiers)
    } else {
        left == right
    }
}

fn keypad_position(code: KeyCode) -> Option<KeypadKey> {
    Some(match code {
        KeyCode::Numpad0 => KeypadKey::Digit(0),
        KeyCode::Numpad1 => KeypadKey::Digit(1),
        KeyCode::Numpad2 => KeypadKey::Digit(2),
        KeyCode::Numpad3 => KeypadKey::Digit(3),
        KeyCode::Numpad4 => KeypadKey::Digit(4),
        KeyCode::Numpad5 => KeypadKey::Digit(5),
        KeyCode::Numpad6 => KeypadKey::Digit(6),
        KeyCode::Numpad7 => KeypadKey::Digit(7),
        KeyCode::Numpad8 => KeypadKey::Digit(8),
        KeyCode::Numpad9 => KeypadKey::Digit(9),
        KeyCode::NumpadDecimal => KeypadKey::Decimal,
        KeyCode::NumpadComma => KeypadKey::Separator,
        KeyCode::NumpadDivide => KeypadKey::Divide,
        KeyCode::NumpadMultiply => KeypadKey::Multiply,
        KeyCode::NumpadSubtract => KeypadKey::Subtract,
        KeyCode::NumpadAdd => KeypadKey::Add,
        KeyCode::NumpadEnter => KeypadKey::Enter,
        KeyCode::NumpadEqual => KeypadKey::Equal,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Key, Modifiers};

    fn key(key: Key) -> Event {
        Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    fn native(key_code: KeyCode, first_event: usize, event_count: usize) -> NativeKeypadEvent {
        NativeKeypadEvent {
            key_code,
            first_event,
            event_count,
            numeric_layer: false,
            center: false,
            logical_key: NativeKey::Named(NamedKey::Enter),
            modifiers: Modifiers::NONE,
        }
    }

    #[test]
    fn keypad_projection_keeps_adjacent_main_and_keypad_enter_distinct() {
        let raw = egui::RawInput {
            focused: true,
            events: vec![key(Key::Enter), key(Key::Enter), key(Key::Enter)],
            ..Default::default()
        };
        let events = &mut raw.events.clone();
        if let Event::Key { repeat, .. } = &mut events[1] {
            *repeat = true;
        }
        let batch = || FrameKeypad::capture(&raw, vec![native(KeyCode::NumpadEnter, 1, 1)]);
        assert_eq!(
            batch().route(events, true, 0),
            [RoutedKeypad {
                event: 1,
                text: None,
                encoding: KeypadEncoding::Application(*b"\x1bOM"),
                before_event: false,
            }]
        );
        assert!(batch().route(&events[1..], true, 0).is_empty());
    }

    #[test]
    fn keypad_projection_pairs_only_the_native_press_text() {
        let raw = egui::RawInput {
            focused: true,
            events: vec![
                key(Key::Plus),
                Event::Text("+".into()),
                Event::Text("+".into()),
            ],
            ..Default::default()
        };
        let batch = || FrameKeypad::capture(&raw, vec![native(KeyCode::NumpadAdd, 0, 2)]);
        assert_eq!(
            batch().route(&raw.events, true, 0),
            [RoutedKeypad {
                event: 0,
                text: Some(1),
                encoding: KeypadEncoding::Application(*b"\x1bOk"),
                before_event: false,
            }]
        );
        let remaining = [raw.events[0].clone(), raw.events[2].clone()];
        assert!(batch().route(&remaining, true, 0).is_empty());
        assert!(batch().route(&raw.events, false, 0).is_empty());
    }

    #[test]
    fn keypad_projection_excludes_numeric_text_and_pre_blur_keys() {
        let raw = egui::RawInput {
            focused: true,
            events: vec![
                key(Key::Enter),
                Event::WindowFocused(false),
                Event::WindowFocused(true),
                key(Key::Num7),
                Event::Text("7".into()),
                key(Key::Num5),
            ],
            ..Default::default()
        };
        let mut digit = native(KeyCode::Numpad7, 3, 2);
        digit.numeric_layer = true;
        let mut center = native(KeyCode::Numpad5, 5, 1);
        center.center = true;
        let batch = FrameKeypad::capture(
            &raw,
            vec![native(KeyCode::NumpadEnter, 0, 1), digit, center],
        );
        assert_eq!(
            batch.route(&raw.events, false, 0),
            [RoutedKeypad {
                event: 5,
                text: None,
                encoding: KeypadEncoding::Center,
                before_event: false,
            }]
        );
    }

    #[test]
    fn kitty_projection_preserves_a_modified_keypad_press_without_an_egui_key() {
        let raw = egui::RawInput {
            focused: true,
            events: vec![
                key(Key::Enter),
                key(Key::ControlLeft),
                Event::ModifiersChanged(Modifiers::CTRL),
                Event::ModifiersChanged(Modifiers::NONE),
                key(Key::Home),
            ],
            ..Default::default()
        };
        let mut multiply = native(KeyCode::NumpadMultiply, 3, 0);
        multiply.logical_key = NativeKey::Character("*".into());
        multiply.modifiers = Modifiers::CTRL;
        let batch = || FrameKeypad::capture(&raw, vec![multiply.clone()]);
        let filtered = [key(Key::Enter), key(Key::Home)];
        assert_eq!(
            batch().route(&filtered, true, 1),
            [RoutedKeypad {
                event: 1,
                text: None,
                encoding: KeypadEncoding::Kitty(b"\x1b[57411;5u".to_vec()),
                before_event: true,
            }]
        );
        assert!(batch().route(&filtered[1..], true, 1).is_empty());
        assert!(batch().route(&filtered, true, 0).is_empty());
    }

    #[test]
    fn kitty_projection_keeps_numeric_text_only_with_disambiguation() {
        let raw = egui::RawInput {
            focused: true,
            events: vec![key(Key::Num7), Event::Text("7".into())],
            ..Default::default()
        };
        let mut digit = native(KeyCode::Numpad7, 0, 2);
        digit.logical_key = NativeKey::Character("7".into());
        digit.numeric_layer = true;
        let batch = || FrameKeypad::capture(&raw, vec![digit.clone()]);
        assert!(batch().route(&raw.events, true, 1).is_empty());
        assert_eq!(
            batch().route(&raw.events, true, 24),
            [RoutedKeypad {
                event: 0,
                text: Some(1),
                encoding: KeypadEncoding::Kitty(b"\x1b[57406;129;55u".to_vec()),
                before_event: false,
            }]
        );
    }
}
