//! Match native key presses to the input that survives terminal ownership filters.
use crate::ui::{
    is_modifier_only_key, normalized_key, normalized_modifiers, pty_text_payload,
    NativeKeyEncoding, RoutedKeyPress,
};
use eframe::{NativeKeyEvent, NativeKeyboardAnnotations, NativeTextKeyEvent};
use egui::Event;
use jterm_engine::input::{
    encode_kitty_keypad_press, encode_kitty_text_press, Key, KeyboardLocks, KeypadKey,
    KittyTextKey, KITTY_DISAMBIGUATE, KITTY_REPORT_ALL_KEYS, KITTY_REPORT_ASSOCIATED_TEXT,
};
use smallvec::SmallVec;
use winit::keyboard::{Key as NativeKey, KeyCode, NamedKey};

#[derive(Default)]
pub(crate) struct FrameKeyboard(SmallVec<[NativePress; 4]>);

struct NativePress {
    anchor: PressAnchor,
    text: Option<EventPosition>,
    kind: PressKind,
}

enum PressKind {
    Keypad {
        position: KeypadKey,
        center: bool,
        num_locked: bool,
        logical: Key,
        modifiers: egui::Modifiers,
    },
    Text(NativeTextKeyEvent),
}

enum PressAnchor {
    Event(EventPosition),
    Gap {
        left: Option<EventPosition>,
        right: Option<EventPosition>,
    },
}

struct EventPosition {
    identity: EventIdentity,
    ordinal: usize,
    occurrences: usize,
}

enum EventIdentity {
    Key {
        key: egui::Key,
        physical_key: Option<egui::Key>,
        pressed: bool,
        modifiers: egui::Modifiers,
    },
    Text(SmallVec<[u8; 8]>),
}

impl FrameKeyboard {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn capture(input: &egui::RawInput, native: NativeKeyboardAnnotations) -> Self {
        if !input.focused || native.events.is_empty() {
            return Self::default();
        }
        let events = &input.events;
        let focus_start = events
            .iter()
            .rposition(|event| matches!(event, Event::WindowFocused(false)))
            .map_or(0, |last| last + 1);
        Self(
            native
                .events
                .into_iter()
                .filter_map(|native| {
                    let (first, count) = native.event_range();
                    if first < focus_start {
                        return None;
                    }
                    let group = events.get(first..first.checked_add(count)?)?;
                    let (anchor, text) = match group {
                        [] => (PressAnchor::gap(events, first)?, None),
                        [Event::Key { pressed: true, .. }] | [Event::Text(_)] => (
                            PressAnchor::Event(EventPosition::capture(events, first)?),
                            None,
                        ),
                        [Event::Key { pressed: true, .. }, Event::Text(_)] => (
                            PressAnchor::Event(EventPosition::capture(events, first)?),
                            Some(EventPosition::capture(events, first + 1)?),
                        ),
                        _ => return None,
                    };
                    let kind = match native {
                        NativeKeyEvent::Keypad(native) => PressKind::Keypad {
                            position: keypad_position(native.key_code)?,
                            center: native.center,
                            num_locked: native.numeric_layer,
                            logical: keypad_logical_key(&native.logical_key),
                            modifiers: native.modifiers,
                        },
                        NativeKeyEvent::Text(native) => PressKind::Text(native),
                    };
                    Some(NativePress { anchor, text, kind })
                })
                .collect(),
        )
    }

    pub fn route(
        self,
        events: &[Event],
        application: bool,
        flags: u16,
    ) -> SmallVec<[RoutedKeyPress; 4]> {
        let mut routed = SmallVec::<[RoutedKeyPress; 4]>::new();
        let report_all = flags & KITTY_REPORT_ALL_KEYS != 0;
        let report_associated = report_all && flags & KITTY_REPORT_ASSOCIATED_TEXT != 0;
        for press in self.0 {
            if !report_all && matches!(press.kind, PressKind::Text(_)) {
                continue;
            }
            let before_event = matches!(&press.anchor, PressAnchor::Gap { .. });
            let text = press.text.as_ref().and_then(|text| text.locate(events));
            let Some(event) = press.anchor.locate(events) else {
                if let (true, Some(event)) = (report_all, text) {
                    // A host filter took the key. Its surviving text still
                    // belongs to that press and must not become a new report.
                    routed.push(RoutedKeyPress {
                        event,
                        text: None,
                        encoding: NativeKeyEncoding::SuppressText,
                        before_event: false,
                    });
                }
                continue;
            };
            if press.text.is_some() && text != Some(event + 1) {
                continue;
            }
            let associated = if report_associated && !before_event {
                match events.get(text.unwrap_or(event)) {
                    Some(Event::Text(value)) => pty_text_payload(value),
                    _ => None,
                }
            } else {
                None
            };
            let Some(encoding) =
                press
                    .kind
                    .encode(application, flags, before_event, associated.as_deref())
            else {
                continue;
            };
            routed.push(RoutedKeyPress {
                event,
                text,
                encoding,
                before_event,
            });
        }
        // Stable sorting preserves hardware order when several presses
        // produced no GUI event at the same position.
        routed.sort_by_key(|press| (press.event, !press.before_event));
        routed
    }
}

impl PressKind {
    fn encode(
        self,
        application: bool,
        flags: u16,
        before_event: bool,
        text: Option<&str>,
    ) -> Option<NativeKeyEncoding> {
        let (position, center, num_locked, logical, modifiers) = match self {
            Self::Text(native) => {
                let mut modifiers = normalized_modifiers(native.modifiers);
                modifiers.super_key = native.super_key;
                // XKB supplies printable strings even for Alt/Super
                // shortcuts. Those presses have no associated input text.
                let text = if cfg!(target_os = "linux") && (modifiers.alt || modifiers.super_key) {
                    None
                } else {
                    text
                };
                return encode_kitty_text_press(
                    Some(KittyTextKey {
                        base: native.base,
                        shifted: native.shifted,
                        base_layout: native.physical_key.and_then(base_layout_char),
                    }),
                    modifiers,
                    KeyboardLocks {
                        caps_lock: native.caps_lock,
                        num_lock: native.num_lock,
                    },
                    flags,
                    text,
                )
                .map(NativeKeyEncoding::Kitty);
            }
            Self::Keypad {
                position,
                center,
                num_locked,
                logical,
                modifiers,
            } => (position, center, num_locked, logical, modifiers),
        };
        if flags & (KITTY_REPORT_ALL_KEYS | u16::from(KITTY_DISAMBIGUATE)) != 0 {
            return encode_kitty_keypad_press(
                position,
                logical,
                normalized_modifiers(modifiers),
                num_locked,
                flags,
                text,
            )
            .map(NativeKeyEncoding::Kitty);
        }
        if flags != 0 || before_event || num_locked || modifiers.any() {
            None
        } else if application {
            position
                .application_sequence()
                .map(NativeKeyEncoding::Application)
        } else {
            center.then_some(NativeKeyEncoding::Center)
        }
    }
}

fn base_layout_char(key: egui::Key) -> Option<char> {
    Some(match key {
        egui::Key::Space => ' ',
        egui::Key::Comma => ',',
        egui::Key::Period => '.',
        egui::Key::Semicolon => ';',
        egui::Key::Backslash => '\\',
        egui::Key::Slash => '/',
        egui::Key::OpenBracket => '[',
        egui::Key::CloseBracket => ']',
        egui::Key::Backtick => '`',
        egui::Key::Quote => '\'',
        egui::Key::Minus => '-',
        egui::Key::Equals => '=',
        other => match normalized_key(other) {
            Key::Character(ch) => ch,
            _ => return None,
        },
    })
}

impl PressAnchor {
    fn gap(events: &[Event], index: usize) -> Option<Self> {
        // Some modified printable or keypad keys have no egui Key or Text event.
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
            Event::Key { .. } => Self::capture(events, index),
            Event::Text(text) if text.len() <= 4 => Self::capture(events, index),
            _ => None,
        }
    }

    fn capture(events: &[Event], index: usize) -> Option<Self> {
        let identity = match &events[index] {
            &Event::Key {
                key,
                physical_key,
                pressed,
                modifiers,
                ..
            } => EventIdentity::Key {
                key,
                physical_key,
                pressed,
                modifiers,
            },
            Event::Text(text) => EventIdentity::Text(SmallVec::from_slice(text.as_bytes())),
            _ => return None,
        };
        let ordinal = events[..index]
            .iter()
            .filter(|other| identity.matches(other))
            .count();
        let occurrences = events[index..]
            .iter()
            .filter(|other| identity.matches(other))
            .count()
            + ordinal;
        Some(Self {
            identity,
            ordinal,
            occurrences,
        })
    }

    fn locate(&self, events: &[Event]) -> Option<usize> {
        let mut candidates = events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| self.identity.matches(event).then_some(index));
        let index = candidates.nth(self.ordinal)?;
        // If a filter changed the number of identical events, their ordinal
        // no longer identifies which physical press survived.
        (self.ordinal + 1 + candidates.count() == self.occurrences).then_some(index)
    }
}

impl EventIdentity {
    fn matches(&self, event: &Event) -> bool {
        match (self, event) {
            (
                Self::Key {
                    key,
                    physical_key,
                    pressed,
                    modifiers,
                },
                Event::Key {
                    key: other_key,
                    physical_key: other_physical_key,
                    pressed: other_pressed,
                    modifiers: other_modifiers,
                    ..
                },
            ) => {
                // egui computes repeat after the native event was annotated.
                (key, physical_key, pressed, modifiers)
                    == (
                        other_key,
                        other_physical_key,
                        other_pressed,
                        other_modifiers,
                    )
            }
            (Self::Text(bytes), Event::Text(text)) => bytes.as_slice() == text.as_bytes(),
            _ => false,
        }
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
    use eframe::NativeKeypadEvent;
    use egui::{Key, Modifiers};

    fn capture_keypads(input: &egui::RawInput, keys: Vec<NativeKeypadEvent>) -> FrameKeyboard {
        FrameKeyboard::capture(
            input,
            NativeKeyboardAnnotations {
                events: keys.into_iter().map(NativeKeyEvent::Keypad).collect(),
            },
        )
    }

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
        let batch = || capture_keypads(&raw, vec![native(KeyCode::NumpadEnter, 1, 1)]);
        assert_eq!(
            batch().route(events, true, 0).as_slice(),
            &[RoutedKeyPress {
                event: 1,
                text: None,
                encoding: NativeKeyEncoding::Application(*b"\x1bOM"),
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
        let batch = || capture_keypads(&raw, vec![native(KeyCode::NumpadAdd, 0, 2)]);
        assert_eq!(
            batch().route(&raw.events, true, 0).as_slice(),
            &[RoutedKeyPress {
                event: 0,
                text: Some(1),
                encoding: NativeKeyEncoding::Application(*b"\x1bOk"),
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
        let batch = capture_keypads(
            &raw,
            vec![native(KeyCode::NumpadEnter, 0, 1), digit, center],
        );
        assert_eq!(
            batch.route(&raw.events, false, 0).as_slice(),
            &[RoutedKeyPress {
                event: 5,
                text: None,
                encoding: NativeKeyEncoding::Center,
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
        let batch = || capture_keypads(&raw, vec![multiply.clone()]);
        let filtered = [key(Key::Enter), key(Key::Home)];
        assert_eq!(
            batch().route(&filtered, true, 1).as_slice(),
            &[RoutedKeyPress {
                event: 1,
                text: None,
                encoding: NativeKeyEncoding::Kitty(b"\x1b[57411;5u".to_vec()),
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
        let batch = || capture_keypads(&raw, vec![digit.clone()]);
        assert!(batch().route(&raw.events, true, 1).is_empty());
        assert_eq!(
            batch().route(&raw.events, true, 24).as_slice(),
            &[RoutedKeyPress {
                event: 0,
                text: Some(1),
                encoding: NativeKeyEncoding::Kitty(b"\x1b[57406;129;55u".to_vec()),
                before_event: false,
            }]
        );
    }

    #[test]
    fn kitty_projection_pairs_printable_text_with_its_own_press() {
        let raw = egui::RawInput {
            focused: true,
            events: vec![
                key(Key::A),
                Event::Text("A".into()),
                key(Key::F3),
                Event::Text("A".into()),
                key(Key::Num3),
                Event::Text("#".into()),
            ],
            ..Default::default()
        };
        let a = NativeTextKeyEvent {
            first_event: 0,
            event_count: 2,
            base: 'a',
            shifted: None,
            physical_key: Some(Key::A),
            modifiers: Modifiers::NONE,
            super_key: false,
            caps_lock: true,
            num_lock: false,
        };
        let hash = NativeTextKeyEvent {
            first_event: 4,
            base: '3',
            shifted: Some('#'),
            physical_key: Some(Key::Num3),
            modifiers: Modifiers::SHIFT,
            caps_lock: false,
            ..a
        };
        let capture = || {
            FrameKeyboard::capture(
                &raw,
                NativeKeyboardAnnotations {
                    events: smallvec::smallvec![
                        NativeKeyEvent::Text(a),
                        NativeKeyEvent::Text(hash)
                    ],
                },
            )
        };
        let flags = 4 | KITTY_REPORT_ALL_KEYS | KITTY_REPORT_ASSOCIATED_TEXT;
        let shifted = |event| RoutedKeyPress {
            event,
            text: Some(event + 1),
            encoding: NativeKeyEncoding::Kitty(b"\x1b[51:35;2;35u".to_vec()),
            before_event: false,
        };
        assert_eq!(
            capture().route(&raw.events, false, flags).as_slice(),
            &[
                RoutedKeyPress {
                    event: 0,
                    text: Some(1),
                    encoding: NativeKeyEncoding::Kitty(b"\x1b[97;65;65u".to_vec()),
                    before_event: false,
                },
                shifted(4),
            ],
        );
        // A filter claimed Key(A). Suppress its Text(A) only, keeping the
        // independent Text(A) and the later shifted digit in their places.
        assert_eq!(
            capture().route(&raw.events[1..], false, flags).as_slice(),
            &[
                RoutedKeyPress {
                    event: 0,
                    text: None,
                    encoding: NativeKeyEncoding::SuppressText,
                    before_event: false,
                },
                shifted(3),
            ],
        );
        assert!(capture().route(&raw.events, false, 1).is_empty());
    }
}
