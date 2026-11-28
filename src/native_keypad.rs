//! Keep physical keypad identity through the terminal's ordered input filters.
use crate::ui::{KeypadEncoding, RoutedKeypad};
use eframe::NativeKeypadEvent;
use egui::Event;
use jterm_engine::input::KeypadKey;
use winit::keyboard::KeyCode;

#[derive(Default)]
pub(crate) struct FrameKeypad(Vec<KeypadPress>);

struct KeypadPress {
    event: EventPosition,
    text: Option<EventPosition>,
    position: KeypadKey,
    center: bool,
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
                    if native.numeric_layer || native.first_event < focus_start {
                        return None;
                    }
                    let first = native.first_event;
                    let group = events.get(first..first.checked_add(native.event_count)?)?;
                    let text = match group {
                        [Event::Key { pressed: true, .. }] | [Event::Text(_)] => None,
                        [Event::Key { pressed: true, .. }, Event::Text(_)] => {
                            Some(EventPosition::capture(events, first + 1))
                        }
                        _ => return None,
                    };
                    Some(KeypadPress {
                        event: EventPosition::capture(events, first),
                        text,
                        position: keypad_position(native.key_code)?,
                        center: native.center,
                    })
                })
                .collect(),
        )
    }

    pub fn route(self, events: &[Event], application: bool) -> Vec<RoutedKeypad> {
        let mut routed = Vec::with_capacity(self.0.len());
        for press in self.0 {
            let encoding = if application {
                let Some(sequence) = press.position.application_sequence() else {
                    continue;
                };
                KeypadEncoding::Application(sequence)
            } else if press.center {
                KeypadEncoding::Center
            } else {
                continue;
            };
            let Some(event) = press.event.locate(events) else {
                continue;
            };
            let text = match press.text {
                Some(text) if text.locate(events) == Some(event + 1) => Some(event + 1),
                Some(_) => continue,
                None => None,
            };
            routed.push(RoutedKeypad {
                event,
                text,
                encoding,
            });
        }
        routed.sort_unstable_by_key(|press| press.event);
        routed
    }
}

impl EventPosition {
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
            batch().route(events, true),
            [RoutedKeypad {
                event: 1,
                text: None,
                encoding: KeypadEncoding::Application(*b"\x1bOM"),
            }]
        );
        assert!(batch().route(&events[1..], true).is_empty());
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
            batch().route(&raw.events, true),
            [RoutedKeypad {
                event: 0,
                text: Some(1),
                encoding: KeypadEncoding::Application(*b"\x1bOk"),
            }]
        );
        let remaining = [raw.events[0].clone(), raw.events[2].clone()];
        assert!(batch().route(&remaining, true).is_empty());
        assert!(batch().route(&raw.events, false).is_empty());
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
            batch.route(&raw.events, false),
            [RoutedKeypad {
                event: 5,
                text: None,
                encoding: KeypadEncoding::Center
            }]
        );
    }
}
