//! Fractional wheel state belongs to one eligible native terminal route.
use jterm_core::wheel::WheelAccumulator;

#[derive(Default)]
pub(crate) struct AlternateScrollWheel {
    owner: Option<(String, u64, u32)>,
    pub(crate) accumulator: WheelAccumulator,
    pointer_over: bool,
    frame_start_pointer: Option<eframe::egui::Pos2>,
    frame_captured: bool,
    frame_events: Vec<eframe::egui::Event>,
    alternate_read: bool,
    alternate_claimed: bool,
    tracking_read: bool,
}
impl AlternateScrollWheel {
    /// Called even on frames with no wheel or no terminal keyboard ownership.
    pub(crate) fn sync(&mut self, route: Option<(&str, u64, u32)>) -> bool {
        let same = match (&self.owner, route) {
            (Some((session, generation, metric)), Some((next, next_generation, next_metric))) => {
                session == next && *generation == next_generation && *metric == next_metric
            }
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.accumulator.reset();
            self.owner = route.map(|(id, generation, metric)| (id.to_owned(), generation, metric));
        }
        self.owner.is_some()
    }

    pub(crate) fn capture_frame(
        &mut self,
        pointer: Option<eframe::egui::Pos2>,
        events: &[eframe::egui::Event],
    ) {
        use eframe::egui::Event;
        self.frame_start_pointer = pointer;
        self.frame_captured = true;
        self.alternate_read = false;
        self.alternate_claimed = false;
        self.tracking_read = false;
        self.frame_events.clear();
        self.frame_events.extend(
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        Event::PointerMoved(_)
                            | Event::PointerGone
                            | Event::WindowFocused(_)
                            | Event::MouseWheel { .. }
                    )
                })
                .cloned(),
        );
    }

    pub(crate) fn take_tracking_events(
        &mut self,
    ) -> Option<(Option<eframe::egui::Pos2>, Vec<eframe::egui::Event>)> {
        if self.tracking_read {
            return None;
        }
        self.tracking_read = true;
        if self.alternate_claimed || !self.frame_captured {
            return None;
        }
        Some((
            self.frame_start_pointer,
            std::mem::take(&mut self.frame_events),
        ))
    }
    pub(crate) fn recycle_tracking_events(&mut self, mut events: Vec<eframe::egui::Event>) {
        events.clear();
        if self.frame_events.is_empty() {
            self.frame_events = events;
        }
    }

    pub(crate) fn collect_frame(
        &mut self,
        route: Option<(&str, u64, u32)>,
        rect: Option<eframe::egui::Rect>,
        final_pointer_over: bool,
        frame_override: bool,
        accumulate: impl FnMut(
            &mut WheelAccumulator,
            isize,
            &eframe::egui::MouseWheelUnit,
            eframe::egui::Vec2,
        ) -> isize,
    ) -> isize {
        if self.alternate_read || self.tracking_read || !self.frame_captured {
            // A layout pass may change eligibility after raw input was used.
            // Observe that idle boundary without replaying any event/pulse.
            self.sync(route.filter(|_| final_pointer_over && !frame_override));
            self.pointer_over = final_pointer_over;
            return 0;
        }
        self.alternate_read = true;
        self.alternate_claimed = route.is_some();
        let events = std::mem::take(&mut self.frame_events);
        let steps = self.collect_events(
            route,
            rect,
            final_pointer_over,
            frame_override,
            &events,
            accumulate,
        );
        self.frame_events = events;
        steps
    }

    pub(crate) fn collect_events(
        &mut self,
        route: Option<(&str, u64, u32)>,
        rect: Option<eframe::egui::Rect>,
        final_pointer_over: bool,
        frame_override: bool,
        events: &[eframe::egui::Event],
        mut accumulate: impl FnMut(
            &mut WheelAccumulator,
            isize,
            &eframe::egui::MouseWheelUnit,
            eframe::egui::Vec2,
        ) -> isize,
    ) -> isize {
        use eframe::egui::Event;
        if route.is_none() {
            self.sync(None);
            self.pointer_over = final_pointer_over;
            return 0;
        }
        let has_pointer = events
            .iter()
            .any(|event| matches!(event, Event::PointerGone | Event::PointerMoved(_)));
        let mut over = if self.frame_captured {
            self.frame_start_pointer
                .zip(rect)
                .is_some_and(|(point, rect)| rect.contains(point))
        } else if has_pointer {
            self.pointer_over
        } else {
            final_pointer_over
        };
        // Even an idle frame observes modal/modifier/pointer/owner changes.
        if !events
            .iter()
            .any(|event| matches!(event, Event::MouseWheel { .. }))
        {
            self.sync(route.filter(|_| over && !frame_override));
        }
        let mut steps = 0;
        for event in events {
            if let Event::WindowFocused(false) = event {
                // Retire the old fraction without requiring a keyboard seat
                // before accepting later genuine pointer wheel input.
                self.sync(None);
                continue;
            }
            if let Some(next) = self.pointer_event(event, rect, route) {
                over = next;
                continue;
            }
            if let Event::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } = event
            {
                if modifiers.shift || modifiers.ctrl || !over {
                    self.sync(None);
                    continue;
                }
                self.sync(route);
                steps = accumulate(&mut self.accumulator, steps, unit, *delta);
            }
        }
        self.pointer_over = final_pointer_over;
        if !final_pointer_over || frame_override {
            self.sync(None);
        }
        steps
    }

    pub(crate) fn pointer_event(
        &mut self,
        event: &eframe::egui::Event,
        rect: Option<eframe::egui::Rect>,
        route: Option<(&str, u64, u32)>,
    ) -> Option<bool> {
        match event {
            eframe::egui::Event::PointerGone => {
                self.sync(None);
                Some(false)
            }
            eframe::egui::Event::PointerMoved(point) => {
                let over = rect.is_some_and(|rect| rect.contains(*point));
                self.sync(route.filter(|_| over));
                Some(over)
            }
            _ => None,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fraction_never_crosses_a_route_or_owner_boundary() {
        for boundary in ["pointer", "shift", "ctrl", "blocked", "blur"] {
            let mut wheel = AlternateScrollWheel::default();
            assert!(wheel.sync(Some(("first", 1, 12))));
            assert_eq!(wheel.accumulator.push(0.6, false), 0);
            assert!(!wheel.sync(None), "{boundary}");
            assert!(wheel.sync(Some(("first", 1, 12))));
            assert_eq!(wheel.accumulator.push(0.5, false), 0, "{boundary}");
        }
        for route in [("second", 1, 12), ("first", 2, 12), ("first", 1, 14)] {
            let mut wheel = AlternateScrollWheel::default();
            wheel.sync(Some(("first", 1, 12)));
            assert_eq!(wheel.accumulator.push(0.6, false), 0);
            wheel.sync(Some(route));
            assert_eq!(wheel.accumulator.push(0.5, false), 0);
        }
    }
    #[test]
    fn ordered_pointer_leave_return_clears_fraction_even_when_final_hover_is_inside() {
        use eframe::egui::{Event, Pos2, Rect, Vec2};
        let route = Some(("first", 1, 12));
        let rect = Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0)));
        let mut wheel = AlternateScrollWheel::default();
        wheel.sync(route);
        assert_eq!(wheel.accumulator.push(0.6, false), 0);
        assert_eq!(
            wheel.pointer_event(&Event::PointerGone, rect, route),
            Some(false)
        );
        assert_eq!(
            wheel.pointer_event(&Event::PointerMoved(Pos2::new(20.0, 20.0)), rect, route),
            Some(true)
        );
        assert_eq!(wheel.accumulator.push(0.5, false), 0);
        wheel.pointer_event(&Event::PointerMoved(Pos2::new(200.0, 20.0)), rect, route);
        wheel.pointer_event(&Event::PointerMoved(Pos2::new(20.0, 20.0)), rect, route);
        assert_eq!(wheel.accumulator.push(0.5, false), 0);
    }

    #[test]
    fn whole_pulses_survive_later_pointer_and_modifier_gaps_in_same_batch() {
        use eframe::egui::{Event, Modifiers, MouseWheelUnit, Pos2, Rect, Vec2};
        let route = Some(("first", 1, 12));
        let rect = Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0)));
        for (suffix, final_over) in [
            (Event::PointerGone, false),
            (
                Event::MouseWheel {
                    unit: MouseWheelUnit::Line,
                    phase: eframe::egui::TouchPhase::Move,
                    delta: Vec2::new(0.0, 0.4),
                    modifiers: Modifiers::SHIFT,
                },
                true,
            ),
        ] {
            let mut wheel = AlternateScrollWheel::default();
            wheel.sync(route);
            wheel.pointer_over = true;
            let steps = wheel.collect_events(
                route,
                rect,
                final_over,
                false,
                &[
                    Event::MouseWheel {
                        unit: MouseWheelUnit::Line,
                        phase: eframe::egui::TouchPhase::Move,
                        delta: Vec2::new(0.0, 1.4),
                        modifiers: Modifiers::NONE,
                    },
                    suffix,
                ],
                |acc, steps, _, delta| steps + acc.push(delta.y as f64, false) as isize,
            );
            assert_eq!(steps, 1);
            wheel.sync(route);
            assert_eq!(
                wheel.accumulator.push(0.7, false),
                0,
                "fraction retired, whole pulse preserved"
            );
        }
    }

    #[test]
    fn raw_frame_start_and_removed_ctrl_wheel_preserve_ordered_authority() {
        use eframe::egui::{Event, Modifiers, MouseWheelUnit, Pos2, Rect, Vec2};
        let route = Some(("first", 1, 12));
        let rect = Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0)));
        let pointer = Some(Pos2::new(20.0, 20.0));
        let wheel_event = |amount, modifiers| Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            phase: eframe::egui::TouchPhase::Move,
            delta: Vec2::new(0.0, amount),
            modifiers,
        };
        let accumulate =
            |acc: &mut WheelAccumulator, steps: isize, _: &MouseWheelUnit, delta: Vec2| {
                steps + acc.push(delta.y as f64, false) as isize
            };
        let mut wheel = AlternateScrollWheel::default();
        // A disarmed route does not erase the physical pointer position.
        wheel.sync(None);
        wheel.capture_frame(
            pointer,
            &[
                wheel_event(1.2, Modifiers::NONE),
                Event::PointerMoved(Pos2::new(21.0, 20.0)),
            ],
        );
        assert_eq!(wheel.collect_frame(route, rect, true, false, accumulate), 1);
        wheel.sync(None);
        wheel.sync(route);
        assert_eq!(wheel.accumulator.push(0.6, false), 0);
        let mut raw = vec![
            wheel_event(0.1, Modifiers::CTRL),
            wheel_event(0.5, Modifiers::NONE),
        ];
        wheel.capture_frame(pointer, &raw);
        raw.remove(0); // Existing font routing consumes Ctrl+wheel.
        assert_eq!(wheel.collect_frame(route, rect, true, false, accumulate), 0);
        wheel.capture_frame(
            pointer,
            &[
                Event::WindowFocused(false),
                Event::WindowFocused(true),
                wheel_event(0.6, Modifiers::NONE),
            ],
        );
        assert_eq!(wheel.collect_frame(route, rect, true, false, accumulate), 0);
    }

    #[test]
    fn focus_gap_retires_fraction_but_pointer_only_seat_can_start_new_motion() {
        use eframe::egui::{Event, Modifiers, MouseWheelUnit, Pos2, Rect, TouchPhase, Vec2};
        let route = Some(("first", 1, 12));
        let rect = Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0)));
        let pointer = Some(Pos2::new(20.0, 20.0));
        let event = |amount| Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            phase: TouchPhase::Move,
            delta: Vec2::new(0.0, amount),
            modifiers: Modifiers::NONE,
        };
        let accumulate =
            |acc: &mut WheelAccumulator, steps: isize, _: &MouseWheelUnit, delta: Vec2| {
                steps + acc.push(delta.y as f64, false) as isize
            };
        let mut wheel = AlternateScrollWheel::default();
        wheel.sync(route);
        assert_eq!(wheel.accumulator.push(0.6, false), 0);
        wheel.capture_frame(pointer, &[Event::WindowFocused(false), event(0.5)]);
        assert_eq!(wheel.collect_frame(route, rect, true, false, accumulate), 0);
        wheel.capture_frame(pointer, &[event(0.6)]);
        assert_eq!(wheel.collect_frame(route, rect, true, false, accumulate), 1);
    }

    #[test]
    fn raw_wheel_events_have_one_authorized_consumer_per_frame() {
        use eframe::egui::{Event, Modifiers, MouseWheelUnit, Pos2, Rect, TouchPhase, Vec2};
        let route = Some(("first", 1, 12));
        let pointer = Some(Pos2::new(20.0, 20.0));
        let rect = Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0)));
        let event = Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            phase: TouchPhase::Move,
            delta: Vec2::new(0.0, 1.0),
            modifiers: Modifiers::NONE,
        };
        let accumulate =
            |acc: &mut WheelAccumulator, steps: isize, _: &MouseWheelUnit, delta: Vec2| {
                steps + acc.push(delta.y as f64, false) as isize
            };
        let mut wheel = AlternateScrollWheel::default();
        wheel.capture_frame(pointer, &[event.clone()]);
        assert_eq!(wheel.collect_frame(route, rect, true, false, accumulate), 1);
        assert_eq!(wheel.collect_frame(route, rect, true, false, accumulate), 0);
        assert!(
            wheel.take_tracking_events().is_none(),
            "old alternate event cannot reroute after tracking enabled"
        );
        wheel.capture_frame(pointer, &[event]);
        assert_eq!(wheel.collect_frame(None, rect, true, false, accumulate), 0);
        let (_, events) = wheel.take_tracking_events().unwrap();
        assert_eq!(events.len(), 1);
        wheel.recycle_tracking_events(events);
        assert!(wheel.take_tracking_events().is_none());
        assert_eq!(wheel.collect_frame(route, rect, true, false, accumulate), 0);
    }

    #[test]
    fn repeated_layout_pass_disarms_fraction_without_replaying_input() {
        use eframe::egui::{Event, Modifiers, MouseWheelUnit, Pos2, Rect, TouchPhase, Vec2};
        let route = Some(("first", 1, 12));
        let pointer = Some(Pos2::new(20.0, 20.0));
        let rect = Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0)));
        let event = |amount| Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            phase: TouchPhase::Move,
            delta: Vec2::new(0.0, amount),
            modifiers: Modifiers::NONE,
        };
        let accumulate =
            |acc: &mut WheelAccumulator, steps: isize, _: &MouseWheelUnit, delta: Vec2| {
                steps + acc.push(delta.y as f64, false) as isize
            };
        let mut wheel = AlternateScrollWheel::default();
        wheel.capture_frame(pointer, &[event(0.6)]);
        assert_eq!(wheel.collect_frame(route, rect, true, false, accumulate), 0);
        assert_eq!(wheel.collect_frame(None, rect, true, false, accumulate), 0);
        assert!(
            wheel.take_tracking_events().is_none(),
            "already-claimed pulse cannot reroute"
        );
        wheel.capture_frame(pointer, &[event(0.5)]);
        assert_eq!(
            wheel.collect_frame(route, rect, true, false, accumulate),
            0,
            "modal pass retired old fraction"
        );
        assert_eq!(
            wheel.collect_frame(route, rect, true, false, accumulate),
            0,
            "second pass cannot count same partial twice"
        );
    }

    #[test]
    fn stable_route_retains_sub_line_motion_across_output_frames() {
        let mut wheel = AlternateScrollWheel::default();
        wheel.sync(Some(("first", 1, 12)));
        assert_eq!(wheel.accumulator.push(0.6, false), 0);
        wheel.sync(Some(("first", 1, 12)));
        assert_eq!(wheel.accumulator.push(0.5, false), 1);
    }
}
