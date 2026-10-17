//! Pointer-owned font zoom. Intercept raw wheel input before egui can turn it
//! into global zoom or scroll velocity, then route against this frame's layout.
use egui::{Event, LayerId, MouseWheelUnit, Pos2, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FontGroup {
    Ui,
    Main,
}

#[derive(Clone, Copy, Debug)]
struct Wheel {
    position: Pos2,
    points: f32,
}

#[derive(Clone, Copy, Debug)]
struct Region {
    rect: Rect,
    layer: LayerId,
    group: FontGroup,
}

#[derive(Default)]
pub(crate) struct FontZoom {
    pointer: Option<Pos2>,
    wheels: Vec<Wheel>,
    regions: Vec<Region>,
}

impl FontZoom {
    /// Replay pointer movement in event order, not the batch's final position.
    /// Ctrl+Alt wheel is reserved, but must not leak into ordinary scrolling.
    pub fn collect(&mut self, events: &mut Vec<Event>, focused: bool) {
        // An early-returned frame must not replay its wheel against a later layout.
        self.wheels.clear();
        if !focused {
            self.pointer = None;
            self.wheels.clear();
        }
        events.retain(|event| match event {
            Event::PointerMoved(pos) => {
                self.pointer = focused.then_some(*pos);
                true
            }
            Event::PointerGone => {
                self.pointer = None;
                true
            }
            Event::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } if modifiers.ctrl => {
                if focused && !modifiers.alt && delta.y.is_finite() {
                    if let Some(position) = self.pointer {
                        let points = match unit {
                            MouseWheelUnit::Point => delta.y / 50.0,
                            MouseWheelUnit::Line | MouseWheelUnit::Page => delta.y,
                        };
                        if points != 0.0 {
                            self.wheels.push(Wheel { position, points });
                        }
                    }
                }
                false
            }
            _ => true,
        });
    }

    pub fn begin_layout(&mut self) {
        self.regions.clear();
    }

    pub fn register(&mut self, rect: Rect, layer: LayerId, group: FontGroup) {
        self.regions.push(Region { rect, layer, group });
    }

    /// Half-open bounds give shared edges a single owner. Topmost-layer checking
    /// prevents a popup/window from zooming a pane hidden underneath it.
    pub fn drain(&mut self, mut layer_at: impl FnMut(Pos2) -> Option<LayerId>) -> [f32; 2] {
        // Accumulate in f64 so finite extreme input cannot overflow into NaN.
        let mut delta = [0.0_f64; 2];
        for wheel in self.wheels.drain(..) {
            let top = layer_at(wheel.position);
            if let Some(region) = self.regions.iter().rev().find(|region| {
                let p = wheel.position;
                p.x >= region.rect.min.x
                    && p.x < region.rect.max.x
                    && p.y >= region.rect.min.y
                    && p.y < region.rect.max.y
                    && top == Some(region.layer)
            }) {
                let index = match region.group {
                    FontGroup::Ui => 0,
                    FontGroup::Main => 1,
                };
                delta[index] += f64::from(wheel.points);
            }
        }
        delta.map(|value| value.clamp(-f64::from(f32::MAX), f64::from(f32::MAX)) as f32)
    }
}

/// Include newly opened sizing-pass surfaces, which egui deliberately excludes
/// from hit testing until the next frame. They still must not forward zoom to
/// a terminal underneath. Tooltip/debug paint layers do not own wheel input.
pub(crate) fn workspace_layer_at(ctx: &egui::Context, position: Pos2) -> Option<LayerId> {
    let layer = ctx.layer_id_at(position)?;
    ctx.memory(|memory| {
        if !memory.allows_interaction(layer) {
            return None;
        }
        let covered = memory.areas().visible_layer_ids().into_iter().any(|id| {
            matches!(id.order, egui::Order::Middle | egui::Order::Foreground)
                && memory
                    .area_rect(id.id)
                    .is_some_and(|rect| rect.contains(position))
        });
        (!covered).then_some(layer)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Id, Modifiers, Order, Vec2};

    fn layer() -> LayerId {
        LayerId::new(Order::Background, Id::new("workspace"))
    }
    fn wheel(unit: MouseWheelUnit, y: f32) -> Event {
        Event::MouseWheel {
            unit,
            delta: Vec2::new(0.0, y),
            modifiers: Modifiers::CTRL,
            phase: egui::TouchPhase::Move,
        }
    }
    fn layout(router: &mut FontZoom) {
        router.begin_layout();
        router.register(
            Rect::from_min_max(Pos2::ZERO, Pos2::new(800.0, 50.0)),
            layer(),
            FontGroup::Ui,
        );
        router.register(
            Rect::from_min_max(Pos2::new(0.0, 50.0), Pos2::new(200.0, 600.0)),
            layer(),
            FontGroup::Ui,
        );
        router.register(
            Rect::from_min_max(Pos2::new(200.0, 50.0), Pos2::new(800.0, 600.0)),
            layer(),
            FontGroup::Main,
        );
    }
    #[test]
    fn each_wheel_uses_its_pointer_in_event_order_and_is_consumed_once() {
        let mut router = FontZoom::default();
        let mut events = vec![
            Event::PointerMoved(Pos2::new(20.0, 100.0)),
            wheel(MouseWheelUnit::Line, 1.0),
            Event::PointerMoved(Pos2::new(400.0, 100.0)),
            wheel(MouseWheelUnit::Line, -2.0),
            Event::PointerMoved(Pos2::new(400.0, 20.0)),
            wheel(MouseWheelUnit::Line, 3.0),
        ];
        router.collect(&mut events, true);
        assert!(!events.iter().any(|e| matches!(e, Event::MouseWheel { .. })));
        layout(&mut router);
        assert_eq!(router.drain(|_| Some(layer())), [4.0, -2.0]);
        assert_eq!(router.drain(|_| Some(layer())), [0.0, 0.0]);
    }
    #[test]
    fn smooth_wheel_is_frame_rate_independent_and_symmetric() {
        let mut router = FontZoom::default();
        layout(&mut router);
        let mut total = 0.0;
        for _ in 0..100 {
            router.collect(
                &mut vec![
                    Event::PointerMoved(Pos2::new(400.0, 100.0)),
                    wheel(MouseWheelUnit::Point, 0.5),
                ],
                true,
            );
            total += router.drain(|_| Some(layer()))[1];
        }
        assert!((total - 1.0).abs() < 0.0001);
        router.collect(&mut vec![wheel(MouseWheelUnit::Point, -50.0)], true);
        assert_eq!(router.drain(|_| Some(layer())), [0.0, -1.0]);
        assert_eq!(router.drain(|_| Some(layer())), [0.0, 0.0]);
    }
    #[test]
    fn shared_edges_splits_and_current_layout_have_one_owner() {
        let mut router = FontZoom::default();
        let mut events = Vec::new();
        for pos in [
            Pos2::new(200.0, 50.0),
            Pos2::new(400.0, 200.0),
            Pos2::new(700.0, 200.0),
        ] {
            events.extend([Event::PointerMoved(pos), wheel(MouseWheelUnit::Line, 1.0)]);
        }
        router.collect(&mut events, true);
        layout(&mut router);
        assert_eq!(router.drain(|_| Some(layer())), [0.0, 3.0]);
        router.collect(
            &mut vec![
                Event::PointerMoved(Pos2::new(100.0, 100.0)),
                wheel(MouseWheelUnit::Line, 1.0),
            ],
            true,
        );
        router.begin_layout(); // Sidebar hidden this frame; do not use its old rectangle.
        router.register(
            Rect::from_min_max(Pos2::new(0.0, 50.0), Pos2::new(800.0, 600.0)),
            layer(),
            FontGroup::Main,
        );
        assert_eq!(router.drain(|_| Some(layer())), [0.0, 1.0]);
    }
    #[test]
    fn extreme_finite_wheels_remain_finite_and_balanced() {
        let mut router = FontZoom::default();
        layout(&mut router);
        router.collect(
            &mut vec![
                Event::PointerMoved(Pos2::new(400.0, 100.0)),
                wheel(MouseWheelUnit::Line, f32::MAX),
                wheel(MouseWheelUnit::Line, f32::MAX),
                wheel(MouseWheelUnit::Line, -f32::MAX),
                wheel(MouseWheelUnit::Line, -f32::MAX),
            ],
            true,
        );
        assert_eq!(router.drain(|_| Some(layer())), [0.0, 0.0]);
        router.collect(
            &mut vec![
                wheel(MouseWheelUnit::Line, f32::MAX),
                wheel(MouseWheelUnit::Line, f32::MAX),
            ],
            true,
        );
        assert_eq!(router.drain(|_| Some(layer())), [0.0, f32::MAX]);
    }

    #[test]
    fn resized_sidebar_and_aborted_frames_do_not_reuse_stale_targets() {
        let mut router = FontZoom::default();
        layout(&mut router);
        router.collect(
            &mut vec![
                Event::PointerMoved(Pos2::new(250.0, 100.0)),
                wheel(MouseWheelUnit::Line, 1.0),
            ],
            true,
        );
        router.begin_layout();
        router.register(
            Rect::from_min_max(Pos2::new(0.0, 50.0), Pos2::new(300.0, 600.0)),
            layer(),
            FontGroup::Ui,
        );
        router.register(
            Rect::from_min_max(Pos2::new(300.0, 50.0), Pos2::new(800.0, 600.0)),
            layer(),
            FontGroup::Main,
        );
        assert_eq!(router.drain(|_| Some(layer())), [1.0, 0.0]);
        router.collect(&mut vec![wheel(MouseWheelUnit::Line, 1.0)], true);
        // Simulate a frame returning before render_ui (e.g. a closing tab).
        router.collect(&mut vec![], true);
        layout(&mut router);
        assert_eq!(router.drain(|_| Some(layer())), [0.0, 0.0]);
    }

    #[test]
    fn floating_layers_unknown_pointer_and_viewport_edges_never_zoom_underneath() {
        let mut router = FontZoom::default();
        layout(&mut router);
        router.collect(&mut vec![wheel(MouseWheelUnit::Line, 1.0)], true);
        assert_eq!(router.drain(|_| Some(layer())), [0.0, 0.0]);
        router.collect(
            &mut vec![
                Event::PointerMoved(Pos2::new(400.0, 100.0)),
                wheel(MouseWheelUnit::Line, 1.0),
            ],
            true,
        );
        assert_eq!(
            router.drain(|_| Some(LayerId::new(Order::Foreground, Id::new("popup")))),
            [0.0, 0.0]
        );
        for pos in [
            Pos2::new(800.0, 100.0),
            Pos2::new(-1.0, 20.0),
            Pos2::new(200.0, 600.0),
        ] {
            router.collect(
                &mut vec![Event::PointerMoved(pos), wheel(MouseWheelUnit::Page, 1.0)],
                true,
            );
        }
        assert_eq!(router.drain(|_| Some(layer())), [0.0, 0.0]);
        router.collect(
            &mut vec![Event::PointerGone, wheel(MouseWheelUnit::Line, 1.0)],
            true,
        );
        assert_eq!(router.drain(|_| Some(layer())), [0.0, 0.0]);
    }
    #[test]
    fn plain_scroll_survives_but_reserved_and_invalid_ctrl_wheel_do_not() {
        let mut router = FontZoom::default();
        layout(&mut router);
        let plain = Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: Vec2::new(0.0, 1.0),
            modifiers: Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        };
        let mut events = vec![
            Event::PointerMoved(Pos2::new(400.0, 100.0)),
            plain.clone(),
            wheel(MouseWheelUnit::Line, f32::NAN),
            Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: Vec2::new(0.0, 1.0),
                modifiers: Modifiers {
                    ctrl: true,
                    alt: true,
                    ..Modifiers::NONE
                },
                phase: egui::TouchPhase::Move,
            },
        ];
        router.collect(&mut events, true);
        assert_eq!(
            events,
            vec![Event::PointerMoved(Pos2::new(400.0, 100.0)), plain]
        );
        assert_eq!(router.drain(|_| Some(layer())), [0.0, 0.0]);
        router.collect(&mut vec![wheel(MouseWheelUnit::Line, 1.0)], false);
        assert_eq!(router.drain(|_| Some(layer())), [0.0, 0.0]);
    }

    #[test]
    fn real_egui_panels_consume_zoom_without_global_scaling_or_scroll() {
        let ctx = egui::Context::default();
        let mut router = FontZoom::default();
        let mut focused_text = String::new();
        let mut sidebar_rect = Rect::NOTHING;
        let mut main_rect = Rect::NOTHING;
        let mut top_rect = Rect::NOTHING;
        let mut run = |events: Vec<Event>, popup: bool| {
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
                events,
                ..Default::default()
            };
            router.collect(&mut input.events, input.focused);
            let mut delta = [0.0; 2];
            let mut output = ctx.run_ui(input, |ui| {
                router.begin_layout();
                let layer = ui.layer_id();
                top_rect = egui::Panel::top("top")
                    .exact_size(50.0)
                    .show(ui, |ui| {
                        ui.label("Top");
                    })
                    .response
                    .rect;
                router.register(top_rect, layer, FontGroup::Ui);
                sidebar_rect = egui::Panel::left("side")
                    .exact_size(200.0)
                    .show(ui, |ui| {
                        // Deliberately keep keyboard focus in the other group.
                        ui.text_edit_singleline(&mut focused_text).request_focus();
                    })
                    .response
                    .rect;
                router.register(sidebar_rect, layer, FontGroup::Ui);
                main_rect = egui::CentralPanel::default()
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for _ in 0..100 {
                                ui.label("main content");
                            }
                        });
                    })
                    .response
                    .rect;
                router.register(main_rect, layer, FontGroup::Main);
                if popup {
                    egui::Area::new(Id::new("cover"))
                        .order(Order::Foreground)
                        .fixed_pos(Pos2::new(300.0, 100.0))
                        .show(&ctx, |ui| {
                            ui.allocate_exact_size(Vec2::new(200.0, 200.0), egui::Sense::hover());
                        });
                }
                delta = router.drain(|p| workspace_layer_at(&ctx, p));
                ctx.input(|i| {
                    assert!(!i
                        .raw
                        .events
                        .iter()
                        .any(|event| matches!(event, Event::MouseWheel { .. })));
                    assert_eq!(i.smooth_scroll_delta, Vec2::ZERO);
                    assert_eq!(i.zoom_delta(), 1.0);
                });
            });
            output.textures_delta.clear();
            delta
        };
        for _ in 0..3 {
            assert_eq!(run(vec![], false), [0.0, 0.0]);
        }
        assert_eq!(
            run(
                vec![
                    Event::PointerMoved(Pos2::new(400.0, 300.0)),
                    wheel(MouseWheelUnit::Line, 1.0)
                ],
                false
            ),
            [0.0, 1.0]
        );
        assert_eq!(
            run(
                vec![
                    Event::PointerMoved(Pos2::new(100.0, 300.0)),
                    wheel(MouseWheelUnit::Line, -1.0)
                ],
                false
            ),
            [-1.0, 0.0]
        );
        assert_eq!(
            run(
                vec![
                    Event::PointerMoved(Pos2::new(400.0, 20.0)),
                    wheel(MouseWheelUnit::Line, 1.0)
                ],
                false
            ),
            [1.0, 0.0]
        );
        assert_eq!(
            run(
                vec![
                    Event::PointerMoved(Pos2::new(350.0, 150.0)),
                    wheel(MouseWheelUnit::Line, 1.0)
                ],
                true
            ),
            [0.0, 0.0]
        );
        for _ in 0..3 {
            run(vec![], true);
        }
        assert_eq!(
            run(
                vec![
                    Event::PointerMoved(Pos2::new(350.0, 150.0)),
                    wheel(MouseWheelUnit::Line, 1.0)
                ],
                true
            ),
            [0.0, 0.0]
        );
        assert_eq!(ctx.zoom_factor(), 1.0);
    }
}
