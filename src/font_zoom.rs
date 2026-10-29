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

/// A current-frame UI registration owned by this application's Inspector.
/// Private fields prevent an arbitrary popup layer from claiming this route.
#[derive(Clone, Copy, Debug)]
pub(crate) struct InspectorDrawerOwner {
    rect: Rect,
    layer: LayerId,
}

#[derive(Default)]
pub(crate) struct FontZoom {
    pointer: Option<Pos2>,
    wheels: Vec<Wheel>,
    regions: Vec<Region>,
    inspector_drawer_layer: Option<LayerId>,
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
        self.inspector_drawer_layer = None;
    }

    pub fn register(&mut self, rect: Rect, layer: LayerId, group: FontGroup) {
        self.regions.push(Region { rect, layer, group });
    }

    /// Called only with the real response of the Inspector's modeless Area.
    /// It becomes an owner only after that layer has a measured UI region.
    pub fn identify_inspector_drawer(&mut self, layer: LayerId) {
        self.inspector_drawer_layer = Some(layer);
    }

    pub fn inspector_drawer_owner(&self) -> Option<InspectorDrawerOwner> {
        let layer = self.inspector_drawer_layer?;
        if layer.order != egui::Order::Middle {
            return None;
        }
        self.regions
            .iter()
            .rev()
            .find(|region| region.layer == layer && region.group == FontGroup::Ui)
            .map(|region| InspectorDrawerOwner {
                rect: region.rect,
                layer,
            })
    }

    /// The unified shell includes more than its former top/sidebar panels.
    /// Use this frame's measured terminal content, excluding its local header,
    /// so every remaining shell surface keeps UI font ownership. Floating
    /// surfaces still establish their own topmost layer before draining input.
    pub fn register_workspace(&mut self, workspace: Rect, main: Rect, layer: LayerId) {
        self.register(workspace, layer, FontGroup::Ui);
        self.register(main.intersect(workspace), layer, FontGroup::Main);
    }

    /// Half-open bounds give shared edges a single owner. Topmost-layer checking
    /// prevents a popup/window from zooming a pane hidden underneath it.
    #[cfg(test)]
    pub fn drain(&mut self, layer_at: impl FnMut(Pos2) -> Option<LayerId>) -> [f32; 2] {
        self.drain_with_main(layer_at, |_, _| false)
    }

    /// Let a main-pane owner consume its own zoom before accumulating the shell
    /// font delta. The callback runs only after the normal layer/region checks.
    pub fn drain_with_main(
        &mut self,
        mut layer_at: impl FnMut(Pos2) -> Option<LayerId>,
        mut on_main: impl FnMut(Pos2, f32) -> bool,
    ) -> [f32; 2] {
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
                    FontGroup::Main => {
                        if on_main(wheel.position, wheel.points) {
                            continue;
                        }
                        1
                    }
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
#[cfg(test)]
pub(crate) fn workspace_layer_at(ctx: &egui::Context, position: Pos2) -> Option<LayerId> {
    workspace_layer_at_with_inspector(ctx, position, None)
}

/// Only the application's measured, current-frame Inspector UI registration
/// may own a Middle layer. Every other covering popup/window stays blocking.
pub(crate) fn workspace_layer_at_with_inspector(
    ctx: &egui::Context,
    position: Pos2,
    inspector: Option<InspectorDrawerOwner>,
) -> Option<LayerId> {
    let layer = ctx.layer_id_at(position)?;
    let inspector = inspector.filter(|owner| {
        owner.layer == layer
            && position.x >= owner.rect.min.x
            && position.x < owner.rect.max.x
            && position.y >= owner.rect.min.y
            && position.y < owner.rect.max.y
    });
    ctx.memory(|memory| {
        if !memory.allows_interaction(layer) {
            return None;
        }
        let covered = memory.areas().visible_layer_ids().into_iter().any(|id| {
            matches!(id.order, egui::Order::Middle | egui::Order::Foreground)
                && inspector.is_none_or(|owner| owner.layer != id)
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

    fn inspector_frame(
        ctx: &egui::Context,
        router: &mut FontZoom,
        events: Vec<Event>,
        drawer: bool,
        registration: Option<FontGroup>,
        identify: bool,
        covering: Option<Order>,
    ) -> [f32; 2] {
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1356.0, 704.0))),
            events,
            ..Default::default()
        };
        router.collect(&mut input.events, input.focused);
        let mut delta = [0.0; 2];
        let mut output = ctx.run_ui(input, |ui| {
            router.begin_layout();
            let workspace = ui.max_rect();
            let root_layer = ui.layer_id();
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |_| {});
            router.register(workspace, root_layer, FontGroup::Main);
            if drawer {
                let response = egui::Area::new(Id::new("font-test-inspector"))
                    .order(Order::Middle)
                    .fixed_pos(Pos2::new(1050.0, 42.0))
                    .show(ctx, |ui| {
                        ui.allocate_exact_size(Vec2::new(306.0, 662.0), egui::Sense::hover());
                    });
                if let Some(group) = registration {
                    router.register(response.response.rect, response.response.layer_id, group);
                }
                if identify {
                    router.identify_inspector_drawer(response.response.layer_id);
                }
            }
            if let Some(order) = covering {
                let response = egui::Area::new(Id::new(("font-test-popup", order)))
                    .order(order)
                    .fixed_pos(Pos2::new(1100.0, 150.0))
                    .default_size(Vec2::new(200.0, 200.0))
                    .show(ctx, |ui| {
                        ui.allocate_exact_size(Vec2::new(200.0, 200.0), egui::Sense::hover());
                    });
                assert!(
                    response.response.rect.contains(Pos2::new(1150.0, 200.0)),
                    "Cover fixture must actually cover the wheel: {:?}",
                    response.response.rect
                );
            }
            let inspector = router.inspector_drawer_owner();
            delta = router.drain_with_main(
                |point| workspace_layer_at_with_inspector(ctx, point, inspector),
                |_, _| false,
            );
        });
        output.textures_delta.clear();
        delta
    }

    fn inspector_wheel() -> Vec<Event> {
        vec![
            Event::PointerMoved(Pos2::new(1150.0, 200.0)),
            wheel(MouseWheelUnit::Line, 1.0),
        ]
    }

    #[test]
    fn registered_inspector_drawer_routes_ui_through_the_real_provider() {
        let ctx = egui::Context::default();
        let mut router = FontZoom::default();
        for _ in 0..3 {
            inspector_frame(
                &ctx,
                &mut router,
                vec![],
                true,
                Some(FontGroup::Ui),
                true,
                None,
            );
        }
        assert_eq!(
            inspector_frame(
                &ctx,
                &mut router,
                inspector_wheel(),
                true,
                Some(FontGroup::Ui),
                true,
                None
            ),
            [1.0, 0.0]
        );
        assert_eq!(ctx.zoom_factor(), 1.0);
    }

    #[test]
    fn unregistered_unidentified_or_main_drawers_never_borrow_the_ui_route() {
        for (registration, identify) in [
            (None, true),
            (Some(FontGroup::Main), true),
            (Some(FontGroup::Ui), false),
        ] {
            let ctx = egui::Context::default();
            let mut router = FontZoom::default();
            for _ in 0..3 {
                inspector_frame(
                    &ctx,
                    &mut router,
                    vec![],
                    true,
                    registration,
                    identify,
                    None,
                );
            }
            assert_eq!(
                inspector_frame(
                    &ctx,
                    &mut router,
                    inspector_wheel(),
                    true,
                    registration,
                    identify,
                    None
                ),
                [0.0, 0.0]
            );
        }
    }

    #[test]
    fn inspector_drawer_never_bypasses_covering_popup_or_settings_layers() {
        for covering in [Order::Middle, Order::Foreground] {
            let ctx = egui::Context::default();
            let mut router = FontZoom::default();
            for _ in 0..3 {
                inspector_frame(
                    &ctx,
                    &mut router,
                    vec![],
                    true,
                    Some(FontGroup::Ui),
                    true,
                    None,
                );
            }
            // Include the first sizing frame, before the new overlay joins hit testing.
            assert_eq!(
                inspector_frame(
                    &ctx,
                    &mut router,
                    inspector_wheel(),
                    true,
                    Some(FontGroup::Ui),
                    true,
                    Some(covering)
                ),
                [0.0, 0.0]
            );
            for _ in 0..3 {
                inspector_frame(
                    &ctx,
                    &mut router,
                    vec![],
                    true,
                    Some(FontGroup::Ui),
                    true,
                    Some(covering),
                );
            }
            assert_eq!(
                inspector_frame(
                    &ctx,
                    &mut router,
                    inspector_wheel(),
                    true,
                    Some(FontGroup::Ui),
                    true,
                    Some(covering)
                ),
                [0.0, 0.0]
            );
        }
    }

    #[test]
    fn hidden_inspector_retires_its_font_owner_in_the_current_layout() {
        let ctx = egui::Context::default();
        let mut router = FontZoom::default();
        for _ in 0..3 {
            inspector_frame(
                &ctx,
                &mut router,
                vec![],
                true,
                Some(FontGroup::Ui),
                true,
                None,
            );
        }
        assert!(router.inspector_drawer_owner().is_some());
        for _ in 0..3 {
            inspector_frame(&ctx, &mut router, vec![], false, None, false, None);
        }
        assert!(router.inspector_drawer_owner().is_none());
        assert_eq!(
            inspector_frame(
                &ctx,
                &mut router,
                inspector_wheel(),
                false,
                None,
                false,
                None
            ),
            [0.0, 1.0]
        );
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
    fn editor_zoom_keeps_each_pointer_owner_and_leaves_shell_and_ui_independent() {
        let mut router = FontZoom::default();
        layout(&mut router);
        router.collect(
            &mut vec![
                Event::PointerMoved(Pos2::new(250.0, 100.0)),
                wheel(MouseWheelUnit::Line, 2.0),
                Event::PointerMoved(Pos2::new(450.0, 100.0)),
                wheel(MouseWheelUnit::Line, -1.0),
                Event::PointerMoved(Pos2::new(650.0, 100.0)),
                wheel(MouseWheelUnit::Line, 3.0),
                Event::PointerMoved(Pos2::new(20.0, 100.0)),
                wheel(MouseWheelUnit::Line, 4.0),
            ],
            true,
        );
        let mut editor_delta = [0.0; 2];
        let global = router.drain_with_main(
            |_| Some(layer()),
            |position, points| {
                if position.x >= 600.0 {
                    return false;
                }
                editor_delta[usize::from(position.x >= 400.0)] += points;
                true
            },
        );
        assert_eq!(editor_delta, [2.0, -1.0]);
        assert_eq!(global, [4.0, 3.0]);
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

    #[test]
    fn reference_shell_measures_ui_chrome_separately_from_terminal_content() {
        let ctx = egui::Context::default();
        let mut router = FontZoom::default();
        let mut measured = [Rect::NOTHING; 3];
        for width in [1356.0, 700.0] {
            let mut run = |events: Vec<Event>| {
                let mut input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 704.0))),
                    events,
                    ..Default::default()
                };
                router.collect(&mut input.events, input.focused);
                let mut delta = [0.0; 2];
                let mut output = ctx.run_ui(input, |ui| {
                    router.begin_layout();
                    let workspace = ui.max_rect();
                    egui::Panel::top("reference-titlebar")
                        .exact_size(42.0)
                        .show_separator_line(false)
                        .frame(egui::Frame::NONE)
                        .show(ui, |_| {});
                    egui::Panel::left("reference-rail")
                        .exact_size(55.0)
                        .show_separator_line(false)
                        .frame(egui::Frame::NONE)
                        .show(ui, |_| {});
                    if width > 1000.0 {
                        egui::Panel::left("reference-navigation")
                            .exact_size(237.0)
                            .show_separator_line(false)
                            .frame(egui::Frame::NONE)
                            .show(ui, |_| {});
                        egui::Panel::right("reference-inspector")
                            .exact_size(306.0)
                            .show_separator_line(false)
                            .frame(egui::Frame::NONE)
                            .show(ui, |_| {});
                    }
                    let main =
                        egui::CentralPanel::default()
                            .frame(egui::Frame::NONE)
                            .show(ui, |ui| {
                                let (header, _) = ui.allocate_exact_size(
                                    Vec2::new(ui.available_width(), 49.0),
                                    egui::Sense::hover(),
                                );
                                // Header controls paint into its already allocated
                                // rectangle. A compact child must not shrink that
                                // reservation and move the native terminal grid.
                                ui.scope_builder(
                                    egui::UiBuilder::new()
                                        .max_rect(header.shrink2(Vec2::new(28.0, 7.0))),
                                    |ui| {
                                        ui.allocate_exact_size(
                                            Vec2::splat(24.0),
                                            egui::Sense::hover(),
                                        );
                                    },
                                );
                                ui.advance_cursor_after_rect(header);
                                let bounds = ui.available_rect_before_wrap();
                                let content = Rect::from_min_max(
                                    bounds.min + Vec2::new(28.0, 12.0),
                                    bounds.max - Vec2::new(28.0, 11.0),
                                );
                                measured = [workspace, header, content];
                                content
                            });
                    // This is the same production registration used after the
                    // actual reference shell and its measured content render.
                    router.register_workspace(workspace, main.inner, ui.layer_id());
                    delta = router.drain(|p| workspace_layer_at(&ctx, p));
                    ctx.input(|input| {
                        assert_eq!(input.zoom_delta(), 1.0);
                        assert_eq!(input.smooth_scroll_delta, Vec2::ZERO);
                    });
                });
                output.textures_delta.clear();
                delta
            };
            for _ in 0..2 {
                assert_eq!(run(vec![]), [0.0, 0.0]);
            }
            // Measure once through a separate frame, then use real output
            // geometry rather than reconstructing the old top-bar variable.
            assert_eq!(
                run(vec![
                    Event::PointerMoved(Pos2::new(20.0, 20.0)),
                    wheel(MouseWheelUnit::Line, 1.0),
                    Event::PointerMoved(Pos2::new(20.0, 200.0)),
                    wheel(MouseWheelUnit::Line, 2.0),
                    Event::PointerMoved(Pos2::new(width * 0.5, 60.0)),
                    wheel(MouseWheelUnit::Line, 3.0),
                    Event::PointerMoved(Pos2::new(width * 0.5, 200.0)),
                    wheel(MouseWheelUnit::Line, 4.0),
                    Event::PointerMoved(Pos2::new(width - 10.0, 200.0)),
                    wheel(MouseWheelUnit::Line, 5.0),
                    Event::PointerMoved(Pos2::new(width + 10.0, 200.0)),
                    wheel(MouseWheelUnit::Line, 6.0),
                ]),
                [11.0, 4.0]
            );
            assert_eq!(measured[0].width(), width);
            assert_eq!(measured[1].height(), 49.0);
            assert!(measured[2].min.y >= measured[1].max.y + 12.0);
        }
    }
}
