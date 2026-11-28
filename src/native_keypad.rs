//! Match native key presses to the input that survives terminal ownership filters.
use crate::ui::{
    is_modifier_only_key, normalized_key, normalized_modifiers, pty_text_payload, HeldKittyKeys,
    KittyRoutedEvent, NativeKeyEncoding, RoutedKeyPress,
};
use eframe::{NativeKeyEvent, NativeKeyKind, NativeKeyPhase, NativeKeyboardAnnotations};
use egui::Event;
use jterm_engine::input::{
    encode_kitty_keypad_press, encode_kitty_text_press, kitty_keypad_report, kitty_modifier_report,
    kitty_named_report, kitty_text_report, Key, KeyboardLocks, KeypadKey, KittyEventType,
    KittyModifierKey, KittyTextKey, KITTY_REPORT_ALL_KEYS, KITTY_REPORT_ASSOCIATED_TEXT,
    KITTY_REPORT_EVENT_TYPES,
};
use smallvec::SmallVec;
use winit::keyboard::{Key as NativeKey, KeyCode, NamedKey, PhysicalKey};

#[derive(Default)]
pub(crate) struct FrameKeyboard(SmallVec<[NativePress; 4]>);

#[derive(Default)]
pub(crate) struct KittyKeyboardState {
    owner: Option<(String, u16, bool)>,
    held: HeldKittyKeys,
}

impl KittyKeyboardState {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn has_held_keys(&self) -> bool {
        !self.held.is_empty()
    }

    pub fn begin_batch(
        &mut self,
        terminal: &str,
        flags: u16,
        alternate_screen: bool,
        focused: bool,
    ) -> HeldKittyKeys {
        if !focused || flags & KITTY_REPORT_EVENT_TYPES == 0 {
            self.clear();
        } else if !self.targets(terminal, flags, alternate_screen) {
            self.held = HeldKittyKeys::default();
            self.owner = Some((terminal.to_owned(), flags, alternate_screen));
        }
        std::mem::take(&mut self.held)
    }

    pub fn admit_batch(&mut self, held: HeldKittyKeys) {
        if self.owner.is_some() {
            self.held = held;
        }
    }

    pub fn targets(&self, terminal: &str, flags: u16, alternate_screen: bool) -> bool {
        self.owner.as_ref().is_some_and(|(owner, mode, alternate)| {
            owner == terminal && *mode == flags && *alternate == alternate_screen
        })
    }
}

struct NativePress {
    anchor: PressAnchor,
    text: Option<EventPosition>,
    key: NativeKeyEvent,
}

enum PressAnchor {
    Event(EventPosition),
    Gap(Box<GapAnchors>),
}

struct GapAnchors {
    legacy: Option<GapPosition>,
    ordered: Option<GapPosition>,
}

struct GapPosition {
    left: Option<EventPosition>,
    right: Option<EventPosition>,
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
    Clipboard(egui::Key),
}

impl FrameKeyboard {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn has_modifier_press(&self) -> bool {
        self.0.iter().any(|press| match press.key.kind {
            NativeKeyKind::Named(key) => modifier_key(key, press.key.physical_key).is_some(),
            _ => false,
        })
    }

    pub fn held_release_positions(
        &self,
        events: &[Event],
        state: &KittyKeyboardState,
    ) -> SmallVec<[usize; 4]> {
        self.0
            .iter()
            .filter(|press| {
                press.key.phase == NativeKeyPhase::Release
                    && state.held.contains(press.key.physical_key)
            })
            .filter_map(|press| press.anchor.locate(events, true))
            .collect()
    }

    pub fn capture(
        input: &egui::RawInput,
        native: NativeKeyboardAnnotations,
        has_held_keys: bool,
    ) -> Self {
        if !input.focused || native.events.is_empty() {
            return Self::default();
        }
        let events = &input.events;
        let can_report_release = has_held_keys
            || native
                .events
                .iter()
                .any(|event| event.phase == NativeKeyPhase::Press);
        let focus_start = events
            .iter()
            .rposition(|event| matches!(event, Event::WindowFocused(false)))
            .map_or(0, |last| last + 1);
        Self(
            native
                .events
                .into_iter()
                .filter_map(|native| {
                    let first = native.first_event;
                    if first < focus_start
                        || (!can_report_release && native.phase == NativeKeyPhase::Release)
                    {
                        return None;
                    }
                    let group = events.get(first..first.checked_add(native.event_count)?)?;
                    let down = native.phase != NativeKeyPhase::Release;
                    let (anchor, text) = match group {
                        [] => (PressAnchor::gap(events, first)?, None),
                        [Event::Key { pressed, .. }] if *pressed == down => (
                            PressAnchor::Event(EventPosition::capture(events, first)?),
                            None,
                        ),
                        [Event::Text(_)] if down => (
                            PressAnchor::Event(EventPosition::capture(events, first)?),
                            None,
                        ),
                        [Event::Copy | Event::Cut | Event::Paste(_)] if down => (
                            PressAnchor::Event(EventPosition::capture(events, first)?),
                            None,
                        ),
                        [Event::Key { pressed: true, .. }, Event::Text(_)] if down => (
                            PressAnchor::Event(EventPosition::capture(events, first)?),
                            Some(EventPosition::capture(events, first + 1)?),
                        ),
                        _ => return None,
                    };
                    Some(NativePress {
                        anchor,
                        text,
                        key: native,
                    })
                })
                .collect(),
        )
    }

    /// Transfer physical identity only when the host actually restores the
    /// clipboard shortcut as a terminal key press.
    pub fn normalize_clipboard_shortcuts(
        &mut self,
        events: &mut Vec<Event>,
        modifiers: egui::Modifiers,
        restore: bool,
        preserve_paste: bool,
        ui_owns_clipboard: bool,
    ) {
        use crate::app::events::{
            normalize_terminal_shortcut_events, normalize_terminal_shortcut_events_with_origin,
        };
        let mut clipboard: SmallVec<[(usize, Option<usize>, &mut NativePress); 4]> = self
            .0
            .iter_mut()
            .filter_map(|press| {
                let position = press.clipboard_position()?.locate(events)?;
                Some((position, None, press))
            })
            .collect();
        if clipboard.is_empty() {
            normalize_terminal_shortcut_events(
                events,
                modifiers,
                restore,
                preserve_paste,
                ui_owns_clipboard,
            );
            return;
        }
        normalize_terminal_shortcut_events_with_origin(
            events,
            modifiers,
            restore,
            preserve_paste,
            ui_owns_clipboard,
            |source, destination| {
                if let Some((_, mapped, _)) =
                    clipboard.iter_mut().find(|(index, _, _)| *index == source)
                {
                    *mapped = Some(destination);
                }
            },
        );
        for (_, destination, press) in clipboard {
            if let Some(position) =
                destination.and_then(|index| EventPosition::capture(events, index))
            {
                press.anchor = PressAnchor::Event(position);
            }
        }
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
        let report_events = flags & KITTY_REPORT_EVENT_TYPES != 0;
        let ordered = report_events || report_all;
        for press in self.0 {
            if press.clipboard_position().is_some() {
                continue;
            }
            if !report_events
                && (press.key.phase == NativeKeyPhase::Release
                    || (!report_all
                        && matches!(
                            press.key.kind,
                            NativeKeyKind::Text(_) | NativeKeyKind::Named(_)
                        )))
            {
                continue;
            }
            let before_event = matches!(&press.anchor, PressAnchor::Gap(_));
            let text = press.text.as_ref().and_then(|text| text.locate(events));
            let Some(event) = press.anchor.locate(events, ordered) else {
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
            let associated = if report_associated && !before_event && !report_events {
                match events.get(text.unwrap_or(event)) {
                    Some(Event::Text(value)) => pty_text_payload(value),
                    _ => None,
                }
            } else {
                None
            };
            let Some(encoding) = press.encode(
                application,
                flags,
                before_event,
                associated.as_deref(),
                !before_event && matches!(events.get(text.unwrap_or(event)), Some(Event::Text(_))),
            ) else {
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

impl NativePress {
    fn clipboard_position(&self) -> Option<&EventPosition> {
        match &self.anchor {
            PressAnchor::Event(position)
                if matches!(position.identity, EventIdentity::Clipboard(_)) =>
            {
                Some(position)
            }
            _ => None,
        }
    }

    fn encode(
        self,
        application: bool,
        flags: u16,
        before_event: bool,
        text: Option<&str>,
        produces_text: bool,
    ) -> Option<NativeKeyEncoding> {
        let native = self.key;
        let mut modifiers = normalized_modifiers(native.modifiers);
        modifiers.super_key = native.super_key;
        let report_all = flags & KITTY_REPORT_ALL_KEYS != 0;
        let locks = if report_all {
            KeyboardLocks {
                caps_lock: native.caps_lock,
                num_lock: native.num_lock,
            }
        } else {
            KeyboardLocks::default()
        };
        let alt_shortcut = cfg!(target_os = "linux") && (modifiers.alt || modifiers.super_key);
        let text = text.filter(|_| !alt_shortcut);
        let event = match native.phase {
            NativeKeyPhase::Press => KittyEventType::Press,
            NativeKeyPhase::Repeat => KittyEventType::Repeat,
            NativeKeyPhase::Release => KittyEventType::Release,
        };
        if flags & KITTY_REPORT_EVENT_TYPES != 0 {
            let report = match native.kind {
                NativeKeyKind::Keypad(pad) => kitty_keypad_report(
                    keypad_position(pad.key_code)?,
                    keypad_logical_key(&pad.logical_key),
                    modifiers,
                    flags,
                ),
                NativeKeyKind::Text(text) if report_all || !produces_text || alt_shortcut => {
                    kitty_text_report(text_key(text), modifiers, flags)
                }
                NativeKeyKind::Named(key) => {
                    if let Some(key) = modifier_key(key, native.physical_key) {
                        kitty_modifier_report(key, flags)
                    } else {
                        kitty_named_report(named_key(key)?, modifiers, flags)
                    }
                }
                NativeKeyKind::Text(_) | NativeKeyKind::Release => None,
            };
            if event == KittyEventType::Press && report.is_none() {
                return None;
            }
            return Some(NativeKeyEncoding::Lifecycle(KittyRoutedEvent {
                physical_key: native.physical_key,
                report,
                event,
                modifiers,
                locks,
                associates_text: !alt_shortcut,
            }));
        }
        match native.kind {
            NativeKeyKind::Text(native) => {
                encode_kitty_text_press(Some(text_key(native)), modifiers, locks, flags, text)
                    .map(NativeKeyEncoding::Kitty)
            }
            NativeKeyKind::Keypad(pad) => {
                let position = keypad_position(pad.key_code)?;
                if flags != 0 {
                    return encode_kitty_keypad_press(
                        position,
                        keypad_logical_key(&pad.logical_key),
                        modifiers,
                        pad.numeric_layer,
                        flags,
                        text,
                    )
                    .map(NativeKeyEncoding::Kitty);
                }
                if before_event || pad.numeric_layer || pad.modifiers.any() {
                    None
                } else if application {
                    position
                        .application_sequence()
                        .map(NativeKeyEncoding::Application)
                } else {
                    pad.center.then_some(NativeKeyEncoding::Center)
                }
            }
            NativeKeyKind::Named(key) => {
                kitty_modifier_report(modifier_key(key, native.physical_key)?, flags)?
                    .encode(KittyEventType::Press, modifiers, locks, flags, None)
                    .map(NativeKeyEncoding::Kitty)
            }
            NativeKeyKind::Release => None,
        }
    }
}

fn text_key(native: eframe::NativeTextKeyEvent) -> KittyTextKey {
    KittyTextKey {
        base: native.base,
        shifted: native.shifted,
        base_layout: native.physical_key.and_then(base_layout_char),
    }
}

fn modifier_key(key: NamedKey, physical: PhysicalKey) -> Option<KittyModifierKey> {
    use KittyModifierKey as Modifier;
    Some(match (key, physical) {
        (NamedKey::CapsLock, _) => Modifier::CapsLock,
        (NamedKey::ScrollLock, _) => Modifier::ScrollLock,
        (NamedKey::NumLock, _) => Modifier::NumLock,
        (NamedKey::Shift, PhysicalKey::Code(KeyCode::ShiftRight)) => Modifier::ShiftRight,
        (NamedKey::Shift, _) => Modifier::ShiftLeft,
        (NamedKey::Control, PhysicalKey::Code(KeyCode::ControlRight)) => Modifier::ControlRight,
        (NamedKey::Control, _) => Modifier::ControlLeft,
        (NamedKey::Alt | NamedKey::AltGraph, PhysicalKey::Code(KeyCode::AltRight)) => {
            Modifier::AltRight
        }
        (NamedKey::Alt | NamedKey::AltGraph, _) => Modifier::AltLeft,
        (NamedKey::Super | NamedKey::Meta, PhysicalKey::Code(KeyCode::SuperRight)) => {
            Modifier::SuperRight
        }
        (NamedKey::Super | NamedKey::Meta, _) => Modifier::SuperLeft,
        _ => return None,
    })
}

fn named_key(key: NamedKey) -> Option<Key> {
    Some(match key {
        NamedKey::Escape => Key::Escape,
        NamedKey::Enter => Key::Enter,
        NamedKey::Tab => Key::Tab,
        NamedKey::Backspace => Key::Backspace,
        NamedKey::ArrowUp => Key::ArrowUp,
        NamedKey::ArrowDown => Key::ArrowDown,
        NamedKey::ArrowLeft => Key::ArrowLeft,
        NamedKey::ArrowRight => Key::ArrowRight,
        NamedKey::Home => Key::Home,
        NamedKey::End => Key::End,
        NamedKey::PageUp => Key::PageUp,
        NamedKey::PageDown => Key::PageDown,
        NamedKey::Insert => Key::Insert,
        NamedKey::Delete => Key::Delete,
        NamedKey::F1 => Key::F1,
        NamedKey::F2 => Key::F2,
        NamedKey::F3 => Key::F3,
        NamedKey::F4 => Key::F4,
        NamedKey::F5 => Key::F5,
        NamedKey::F6 => Key::F6,
        NamedKey::F7 => Key::F7,
        NamedKey::F8 => Key::F8,
        NamedKey::F9 => Key::F9,
        NamedKey::F10 => Key::F10,
        NamedKey::F11 => Key::F11,
        NamedKey::F12 => Key::F12,
        _ => return None,
    })
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
        let legacy = GapPosition::capture(events, index, false);
        let ordered = GapPosition::capture(events, index, true);
        if legacy.is_none() && ordered.is_none() {
            return None;
        }
        Some(Self::Gap(Box::new(GapAnchors { legacy, ordered })))
    }

    fn locate(&self, events: &[Event], ordered: bool) -> Option<usize> {
        match self {
            Self::Event(event) => event.locate(events),
            Self::Gap(anchors) => {
                if ordered {
                    anchors.ordered.as_ref()?.locate(events, true)
                } else {
                    anchors.legacy.as_ref()?.locate(events, false)
                }
            }
        }
    }
}

impl GapPosition {
    fn capture(events: &[Event], index: usize, ordered: bool) -> Option<Self> {
        // A key lacking a GUI event needs a surviving neighbor on each
        // available side. In event-reporting modes modifier keys and key
        // releases can have wire bytes of their own.
        let left = match events[..index]
            .iter()
            .rposition(|event| !gap_ignores(event, ordered))
        {
            Some(index) => Some(EventPosition::neighbor(events, index)?),
            None => None,
        };
        let right = match events[index..]
            .iter()
            .position(|event| !gap_ignores(event, ordered))
        {
            Some(offset) => Some(EventPosition::neighbor(events, index + offset)?),
            None => None,
        };
        Some(Self { left, right })
    }

    fn locate(&self, events: &[Event], ordered: bool) -> Option<usize> {
        let first = match &self.left {
            Some(event) => event.locate(events)? + 1,
            None => 0,
        };
        let last = match &self.right {
            Some(event) => event.locate(events)?,
            None => events.len(),
        };
        (first <= last
            && events[first..last]
                .iter()
                .all(|event| gap_ignores(event, ordered)))
        .then_some(last)
    }
}

fn gap_ignores(event: &Event, ordered: bool) -> bool {
    match event {
        Event::Key { key, pressed, .. } => !ordered && (!pressed || is_modifier_only_key(*key)),
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
            event => EventIdentity::Clipboard(clipboard_key(event)?),
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
            (Self::Clipboard(key), event) => clipboard_key(event) == Some(*key),
            _ => false,
        }
    }
}

fn clipboard_key(event: &Event) -> Option<egui::Key> {
    match event {
        Event::Copy => Some(egui::Key::C),
        Event::Cut => Some(egui::Key::X),
        Event::Paste(_) => Some(egui::Key::V),
        _ => None,
    }
}

fn keypad_logical_key(key: &NativeKey) -> Key {
    match key {
        NativeKey::Named(key) => named_key(*key).unwrap_or(Key::Unidentified),
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
                events: keys
                    .into_iter()
                    .map(|pad| NativeKeyEvent {
                        phase: NativeKeyPhase::Press,
                        first_event: pad.first_event,
                        event_count: pad.event_count,
                        physical_key: PhysicalKey::Code(pad.key_code),
                        modifiers: pad.modifiers,
                        super_key: false,
                        caps_lock: false,
                        num_lock: pad.numeric_layer,
                        kind: NativeKeyKind::Keypad(pad),
                    })
                    .collect(),
            },
            false,
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
        let a = NativeKeyEvent {
            phase: NativeKeyPhase::Press,
            first_event: 0,
            event_count: 2,
            kind: NativeKeyKind::Text(eframe::NativeTextKeyEvent {
                base: 'a',
                shifted: None,
                physical_key: Some(Key::A),
            }),
            physical_key: PhysicalKey::Code(KeyCode::KeyA),
            modifiers: Modifiers::NONE,
            super_key: false,
            caps_lock: true,
            num_lock: false,
        };
        let hash = NativeKeyEvent {
            first_event: 4,
            kind: NativeKeyKind::Text(eframe::NativeTextKeyEvent {
                base: '3',
                shifted: Some('#'),
                physical_key: Some(Key::Num3),
            }),
            physical_key: PhysicalKey::Code(KeyCode::Digit3),
            modifiers: Modifiers::SHIFT,
            caps_lock: false,
            ..a.clone()
        };
        let capture = || {
            FrameKeyboard::capture(
                &raw,
                NativeKeyboardAnnotations {
                    events: smallvec::smallvec![a.clone(), hash.clone()],
                },
                false,
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

    #[test]
    fn kitty_copy_annotation_is_projected_only_for_an_admitted_shortcut() {
        let input = egui::RawInput {
            focused: true,
            events: vec![Event::Copy],
            ..Default::default()
        };
        let modifier = Modifiers {
            ctrl: true,
            command: true,
            ..Modifiers::NONE
        };
        let key = NativeKeyEvent {
            first_event: 0,
            event_count: 1,
            physical_key: PhysicalKey::Code(KeyCode::KeyC),
            phase: NativeKeyPhase::Press,
            kind: NativeKeyKind::Text(eframe::NativeTextKeyEvent {
                base: 'c',
                shifted: None,
                physical_key: Some(Key::C),
            }),
            modifiers: modifier,
            super_key: false,
            caps_lock: false,
            num_lock: false,
        };
        for ui in [true, false] {
            let mut batch = FrameKeyboard::capture(
                &input,
                NativeKeyboardAnnotations {
                    events: smallvec::smallvec![key.clone()],
                },
                false,
            );
            let mut admitted = input.events.clone();
            batch.normalize_clipboard_shortcuts(&mut admitted, modifier, true, false, ui);
            let keys = batch.route(&admitted, false, 3);
            if ui {
                assert!(keys.is_empty());
            } else {
                assert!(
                    matches!(&keys[0].encoding, NativeKeyEncoding::Lifecycle(event)
                    if event.physical_key == PhysicalKey::Code(KeyCode::KeyC))
                );
            }
        }
    }
}
