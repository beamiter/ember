//! Ember's glass presentation adapter. Shared core and terminal authority stay unchanged.
pub use crate::glass_shell_tokens as token;
use egui::{Color32, Rect as EguiRect, Stroke};
pub use jterm_core::reference_shell::*;

pub const TITLEBAR_HEIGHT: f32 = token::TOPBAR_HEIGHT;
pub const RAIL_WIDTH: f32 = token::RAIL_WIDTH;
pub const NAVIGATION_WIDTH: f32 = token::NAVIGATION_WIDTH;
pub const INSPECTOR_WIDTH: f32 = token::INSPECTOR_WIDTH;
pub const CANVAS_HEADER_HEIGHT: f32 = token::HEADER_HEIGHT;
pub const COMPOSER_HEIGHT: f32 = token::TOOLBAR_HEIGHT;
pub const COMPOSER_INSET: f32 = token::TOOLBAR_INSET_LEFT;
pub const COMPOSER_BOTTOM: f32 = 0.0;
pub const COMPOSER_RADIUS: f32 = token::PANEL_RADIUS;
pub const CARD_RADIUS: f32 = token::PANEL_RADIUS;
pub const CONTROL_RADIUS: f32 = token::CONTROL_RADIUS;
pub const CONTROL_HEIGHT: f32 = token::TOOL_BUTTON_HEIGHT;
pub const METADATA_GAP: f32 = token::TOOL_GAP;
pub const COMPOSER_CONTROL_BOTTOM: f32 = 5.0;
pub const COMPOSER_CONTROL_LEFT: f32 = 6.0;
pub const COMPOSER_CONTROL_RIGHT: f32 = 6.0;
pub const COMPOSER_PLUS_WIDTH: f32 = 57.0;
pub const COMPOSER_ATTACH_WIDTH: f32 = 91.0;
pub const COMPOSER_IMAGE_WIDTH: f32 = 89.0;
pub const COMPOSER_CODE_WIDTH: f32 = 89.0;
pub const COMPOSER_TERMINAL_WIDTH: f32 = 88.0;
pub const COMPOSER_TOOLS_WIDTH: f32 = 88.0;
pub const COMPOSER_SEND_WIDTH: f32 = 32.0;
pub const COMPOSER_MODEL_MIN_WIDTH: f32 = 180.0;
pub const COMPOSER_SECONDARY_MIN_WIDTH: f32 = 850.0;
pub const COMPOSER_MODEL_MIN_AVAILABLE: f32 = 560.0;
pub const COMPOSER_TERMINAL_MIN_AVAILABLE: f32 = 280.0;
pub const MODE_CONTROL_HEIGHT: f32 = 32.0;
pub const MODE_CONTROL_TOP: f32 = 4.0;
pub const MODE_CONTROL_MIN_WIDTH: f32 = 64.0;
pub const SELECTED_RAIL_SIZE: f32 = 32.0;
pub const PROFILE_DIAMETER: f32 = 32.0;
pub const NAVIGATION_PADDING: f32 = 10.0;
pub const NAVIGATION_ROW_HEIGHT: f32 = 32.0;
pub const NAVIGATION_ROW_GAP: f32 = 4.0;
pub const WORKSPACE_SELECTOR_TOP: f32 = 108.0;
pub const WORKSPACE_SELECTOR_HEIGHT: f32 = 36.0;
pub const NAVIGATION_ITEMS_TOP: f32 = 153.0;
pub const REFERENCE_RECENT_TOP: f32 = 398.0;
pub const REFERENCE_SPACES_TOP: f32 = 620.0;
pub const RECENT_ROW_HEIGHT: f32 = 42.0;
pub const SPACES_ROW_HEIGHT: f32 = 46.0;
pub const NAVIGATION_STATUS_HEIGHT: f32 = 32.0;
pub const NAVIGATION_STATUS_TEXT_BOTTOM: f32 = 28.0;
pub const HEADER_ACTION_SIZE: f32 = 28.0;
pub const HEADER_ACTION_GAP: f32 = 4.0;
pub const INSPECTOR_TAB_WIDTH: f32 = 80.0;
pub const INSPECTOR_INSET: f32 = 10.0;
pub const INSPECTOR_METADATA_ROW_HEIGHT: f32 = 28.0;
pub const INSPECTOR_CURRENT_HEIGHT: f32 = 58.0;
pub const INSPECTOR_PROGRESS_TARGET_Y: f32 = 238.0;
pub const INSPECTOR_METADATA_TARGET_Y: f32 = 282.0;
pub const INSPECTOR_CURRENT_TARGET_Y: f32 = 453.0;
pub const INSPECTOR_OUTPUT_HEIGHT: f32 = 170.0;

pub mod type_size {
    pub use jterm_core::reference_shell::type_size::*;
    pub const PAGE_TITLE: f32 = 16.0;
    pub const CONTROL: f32 = 14.0;
    pub const SECTION: f32 = 13.0;
}

pub fn rgba([r, g, b, a]: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// Semantic palette adaptation, including background alpha only. Foregrounds stay opaque.
pub fn paint_color(value: [u8; 3]) -> Color32 {
    use jterm_core::reference_shell::color as c;
    let mapped = if value == c::TITLEBAR {
        token::TOP
    } else if value == c::RAIL {
        token::RAIL
    } else if value == c::NAVIGATION {
        token::NAVIGATION
    } else if value == c::INSPECTOR {
        token::INSPECTOR
    } else if value == c::INSPECTOR_CARD {
        token::INSPECTOR_CARD
    } else if value == c::CANVAS || value == c::CANVAS_COOL {
        token::CENTER
    } else if value == c::COMPOSER {
        token::COMPOSER
    } else if value == c::TERMINAL {
        token::TERMINAL
    } else if value == c::CONTROL_SURFACE || value == c::ACCENT_PALE {
        token::BUTTON
    } else if value == c::NAVIGATION_SELECTED || value == c::USER_BUBBLE {
        token::SELECTED_NAV
    } else if value == c::DARK_BORDER || value == c::LIGHT_BORDER || value == c::CONTROL_BORDER {
        token::BORDER
    } else if value == c::CHROME_TEXT || value == c::CANVAS_TEXT || value == c::MESSAGE_USER_TEXT {
        token::TEXT
    } else if value == c::CHROME_MUTED
        || value == c::CANVAS_MUTED
        || value == c::MESSAGE_BODY
        || value == c::MESSAGE_STEP_TEXT
    {
        token::MUTED_TEXT
    } else if value == c::DISABLED_TEXT {
        token::DISABLED_TEXT
    } else if value == c::ACCENT {
        token::ACCENT_TOP
    } else {
        return Color32::from_rgb(value[0], value[1], value[2]);
    };
    rgba(mapped)
}

pub fn paint_panel(painter: &egui::Painter, rect: EguiRect, fill: [u8; 4]) {
    if !rect.is_positive() {
        return;
    }
    painter.rect_filled(rect, token::PANEL_RADIUS, rgba(fill));
    painter.rect_stroke(
        rect,
        token::PANEL_RADIUS,
        Stroke::new(token::BORDER_WIDTH, rgba(token::BORDER)),
        egui::StrokeKind::Inside,
    );
}

/// Isolate intrinsic widget sizes from the rectangle already allocated to
/// workspace chrome. egui's max_rect is a layout hint: scope_builder would
/// propagate oversized content back into a fixed Panel and shift its edge.
pub fn bounded_ui_at<R>(
    ui: &mut egui::Ui,
    rect: EguiRect,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("bounded-workspace-content")
            .max_rect(rect),
    );
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    let result = contents(&mut child);
    // Allocate the actual fixed rectangle, never the child's intrinsic min_rect.
    ui.advance_cursor_after_rect(rect);
    result
}

pub fn bounded_ui<R>(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let rect = ui.available_rect_before_wrap();
    bounded_ui_at(ui, rect, contents)
}

pub trait BoundedPanel {
    fn show_bounded<R>(
        self,
        ui: &mut egui::Ui,
        contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::InnerResponse<R>;
}

impl BoundedPanel for egui::Panel {
    fn show_bounded<R>(
        self,
        ui: &mut egui::Ui,
        contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::InnerResponse<R> {
        self.show(ui, |ui| bounded_ui(ui, contents))
    }
}

impl BoundedPanel for egui::CentralPanel {
    fn show_bounded<R>(
        self,
        ui: &mut egui::Ui,
        contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::InnerResponse<R> {
        self.show(ui, |ui| bounded_ui(ui, contents))
    }
}

/// The projection reserves this one band, including the gap above real input.
/// This is presentation chrome only; native cells/PTY bytes remain untouched.
pub fn footer_rows(padding: f32, line_height: f32) -> usize {
    footer_rows_with_height(padding, line_height, CONTROL_HEIGHT)
}

pub fn footer_rows_with_height(padding: f32, line_height: f32, control_height: f32) -> usize {
    let reserve = (control_height + COMPOSER_CONTROL_BOTTOM + 6.0 - padding).max(control_height);
    (reserve / line_height.max(1.0)).ceil().max(1.0) as usize
}

/// Keep55% of the native reading pane and at least three rows on short windows.
/// Beyond this cap the caller keeps the whole real input in the native viewport.
pub fn editable_input_limit(outer_height: f32, line_height: f32) -> f32 {
    editable_input_limit_with_height(outer_height, line_height, COMPOSER_HEIGHT)
}

pub fn editable_input_limit_with_height(
    outer_height: f32,
    line_height: f32,
    tools_height: f32,
) -> f32 {
    let raw = (outer_height - tools_height - token::TERMINAL_FOOTER_GAP).max(0.0);
    180.0_f32
        .min(raw * 0.45)
        .min((raw - 3.0 * line_height).max(0.0))
}

#[derive(Clone, Copy, Debug)]
pub struct DockGeometry {
    pub frame: EguiRect,
    pub controls: EguiRect,
    pub history_bottom: f32,
}

/// Both the native input clip and canvas use the same actual editor boundary.
pub fn editor_dock(outer: EguiRect, native_editor_top: f32) -> DockGeometry {
    editor_dock_with_height(outer, native_editor_top, COMPOSER_HEIGHT)
}

pub fn editor_dock_with_height(
    outer: EguiRect,
    native_editor_top: f32,
    tools_height: f32,
) -> DockGeometry {
    let control_height = (tools_height - 10.0).max(CONTROL_HEIGHT);
    let top = (outer.bottom() - tools_height)
        .min(native_editor_top - 6.0)
        .max(outer.top());
    let frame = EguiRect::from_min_max(egui::pos2(outer.left(), top), outer.max);
    let controls = EguiRect::from_min_max(
        egui::pos2(
            frame.left() + COMPOSER_CONTROL_LEFT,
            frame.bottom() - COMPOSER_CONTROL_BOTTOM - control_height,
        ),
        frame.max - egui::vec2(COMPOSER_CONTROL_RIGHT, COMPOSER_CONTROL_BOTTOM),
    );
    DockGeometry {
        frame,
        controls,
        history_bottom: (top - token::TERMINAL_FOOTER_GAP).max(outer.top()),
    }
}

pub fn chrome_control_height(font_size: f32) -> f32 {
    let scale = if font_size.is_finite() {
        (font_size / 14.0).clamp(1.0, 72.0 / 14.0)
    } else {
        1.0
    };
    (16.5 * scale * 1.3 + 8.0)
        .ceil()
        .max(token::TOOL_BUTTON_HEIGHT)
}

pub const NAVIGATION_MIN_WIDTH: f32 = 180.0;
pub const NAVIGATION_MAX_WIDTH: f32 = 480.0;
pub const INSPECTOR_MIN_WIDTH: f32 = 260.0;
pub const INSPECTOR_MAX_WIDTH: f32 = 520.0;
pub const CENTER_MIN_WIDTH: f32 = 300.0;

/// Requested widths remain independent of responsive effective allocation.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SidebarWidths {
    pub navigation: Option<f32>,
    pub inspector: Option<f32>,
}

impl SidebarWidths {
    fn navigation(self, adaptive: f32) -> f32 {
        self.navigation
            .filter(|value| value.is_finite())
            .unwrap_or(adaptive)
            .clamp(NAVIGATION_MIN_WIDTH, NAVIGATION_MAX_WIDTH)
    }
    pub fn inspector(self) -> f32 {
        self.inspector
            .filter(|value| value.is_finite())
            .unwrap_or(INSPECTOR_WIDTH)
            .clamp(INSPECTOR_MIN_WIDTH, INSPECTOR_MAX_WIDTH)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub titlebar: Rect,
    pub rail: Rect,
    pub navigation: Option<Rect>,
    pub center: Rect,
    pub header: Rect,
    pub inspector: Option<Rect>,
    pub composer: Rect,
    viewport_height: f32,
    requested: SidebarWidths,
    navigation_open: bool,
    inspector_open: bool,
}
impl Layout {
    pub fn for_size(width: f32, height: f32) -> Self {
        Self::with_sidebar_widths(width, height, SidebarWidths::default())
    }
    pub fn with_sidebar_widths(width: f32, height: f32, requested: SidebarWidths) -> Self {
        Self::build(width, height, true, true, requested)
    }
    fn build(
        width: f32,
        height: f32,
        navigation_open: bool,
        inspector_open: bool,
        requested: SidebarWidths,
    ) -> Self {
        let width = if width.is_finite() {
            width.max(0.0)
        } else {
            1612.0
        };
        let height = if height.is_finite() {
            height.max(0.0)
        } else {
            976.0
        };
        let (rail, nav) = (RAIL_WIDTH, NAVIGATION_WIDTH);
        let docked_inspector = inspector_open && width >= token::INSPECTOR_DRAWER_BREAKPOINT;
        let available = (width
            - token::OUTER_LEFT
            - rail
            - token::LEFT_GAP
            - token::OUTER_RIGHT
            - CENTER_MIN_WIDTH
            - if docked_inspector {
                token::INSPECTOR_GAP
            } else {
                0.0
            })
        .max(0.0);
        let navigation_visible = navigation_open
            && available
                >= NAVIGATION_MIN_WIDTH
                    + if docked_inspector {
                        INSPECTOR_MIN_WIDTH
                    } else {
                        0.0
                    };
        // Keep the navigation request stable and reserve the inspector's
        // minimum before fitting it. Inspector allocation uses the remaining
        // budget. Drag distances therefore do not scale or rebalance both
        // requested widths when the window is constrained.
        let nav = if navigation_visible {
            requested.navigation(nav).min(
                (available
                    - if docked_inspector {
                        INSPECTOR_MIN_WIDTH
                    } else {
                        0.0
                    })
                .max(NAVIGATION_MIN_WIDTH),
            )
        } else {
            0.0
        };
        let inspector_width = if docked_inspector {
            requested
                .inspector()
                .min((available - nav).max(INSPECTOR_MIN_WIDTH))
        } else {
            0.0
        };
        let bottom = if height < token::SHORT_HEIGHT {
            token::SHORT_BOTTOM_GAP
        } else {
            token::MAIN_BOTTOM_GAP
        };
        let nav_bottom = if height < token::SHORT_HEIGHT {
            token::SHORT_BOTTOM_GAP
        } else {
            token::NAVIGATION_BOTTOM_GAP
        };
        let inspector = docked_inspector;
        let center_x = token::OUTER_LEFT + rail + nav + token::LEFT_GAP;
        let right = width
            - token::OUTER_RIGHT
            - if inspector {
                inspector_width + token::INSPECTOR_GAP
            } else {
                0.0
            };
        let center = Rect {
            x: center_x,
            y: token::MAIN_TOP,
            width: (right - center_x).max(0.0),
            height: (height - token::MAIN_TOP - bottom).max(0.0),
        };
        let header = Rect {
            height: if height < token::SHORT_HEIGHT {
                token::SHORT_HEADER_HEIGHT
            } else {
                CANVAS_HEADER_HEIGHT
            },
            ..center
        };
        Self {
            titlebar: Rect {
                x: 0.0,
                y: 0.0,
                width,
                height: TITLEBAR_HEIGHT.min(height),
            },
            rail: Rect {
                x: token::OUTER_LEFT,
                y: TITLEBAR_HEIGHT,
                width: rail,
                height: (height - TITLEBAR_HEIGHT - nav_bottom).max(0.0),
            },
            navigation: (nav > 0.0).then_some(Rect {
                x: token::OUTER_LEFT + rail,
                y: TITLEBAR_HEIGHT,
                width: nav,
                height: (height - TITLEBAR_HEIGHT - nav_bottom).max(0.0),
            }),
            center,
            header,
            viewport_height: height,
            requested,
            navigation_open,
            inspector_open,
            inspector: inspector.then_some(Rect {
                x: right + token::INSPECTOR_GAP,
                width: inspector_width,
                ..center
            }),
            composer: Rect {
                x: center.x + token::TOOLBAR_INSET_LEFT,
                y: (center.y + center.height - COMPOSER_HEIGHT).max(center.y),
                width: (center.width - token::TOOLBAR_INSET_LEFT - token::TOOLBAR_INSET_RIGHT)
                    .max(0.0),
                height: COMPOSER_HEIGHT.min(center.height),
            },
        }
    }
    /// UI text grows only its own chrome rows, before native sizing and paint.
    pub fn with_chrome_font_size(mut self, font_size: f32) -> Self {
        let control = chrome_control_height(font_size);
        let title = token::TOPBAR_HEIGHT
            .max(control + 8.0)
            .min(self.viewport_height);
        let main_top = (title + token::MAIN_TOP - token::TOPBAR_HEIGHT).min(self.viewport_height);
        let delta = (main_top - self.center.y).max(0.0);
        self.titlebar.height = title;
        self.rail.y = title;
        self.rail.height = (self.viewport_height - title - token::NAVIGATION_BOTTOM_GAP).max(0.0);
        if let Some(nav) = &mut self.navigation {
            nav.y = title;
            nav.height = self.rail.height;
        }
        self.center.y = main_top;
        self.center.height = (self.center.height - delta).max(0.0);
        if let Some(inspector) = &mut self.inspector {
            inspector.y = main_top;
            inspector.height = self.center.height;
        }
        self.header.y = main_top;
        self.header.height = token::HEADER_HEIGHT
            .max(control + 4.0)
            .min(self.center.height);
        self.composer.height = token::TOOLBAR_HEIGHT
            .max(control + 10.0)
            .min((self.center.height - self.header.height).max(0.0));
        self.composer.y = self.center.y + self.center.height - self.composer.height;
        self
    }

    pub fn without_inspector(self) -> Self {
        Self::build(
            self.titlebar.width,
            self.viewport_height,
            self.navigation_open,
            false,
            self.requested,
        )
    }
    pub fn without_navigation(self) -> Self {
        Self::build(
            self.titlebar.width,
            self.viewport_height,
            false,
            self.inspector_open,
            self.requested,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarEdge {
    Navigation,
    Inspector,
}

#[derive(Clone, Copy, Debug)]
struct SidebarResizeGesture {
    edge: SidebarEdge,
    origin_x: f32,
    effective_width: f32,
    available_width: f32,
    original: SidebarWidths,
}

#[derive(Default)]
pub struct SidebarResizeState {
    preview: Option<SidebarWidths>,
    gesture: Option<SidebarResizeGesture>,
    suppress_until_release: bool,
}

#[derive(Default)]
pub struct SidebarResizeOutcome {
    pub pointer_claimed: bool,
    pub persist: bool,
    pub edited_edge: Option<SidebarEdge>,
}

/// The docked handles live entirely in the existing panel gaps. A compact
/// drawer owns its inner left border, away from both content and terminal input.
pub fn sidebar_resize_handles(
    layout: Layout,
    drawer: Option<EguiRect>,
) -> Vec<(SidebarEdge, EguiRect, f32)> {
    let mut handles = Vec::new();
    if let Some(nav) = layout.navigation {
        let x = nav.x + nav.width + token::LEFT_GAP / 2.0;
        let bottom = (nav.y + nav.height).min(layout.center.y + layout.center.height);
        let rect = EguiRect::from_min_max(
            egui::pos2(x - token::LEFT_GAP.min(8.0) / 2.0, nav.y + 18.0),
            egui::pos2(x + token::LEFT_GAP.min(8.0) / 2.0, bottom - 18.0),
        );
        if rect.is_positive() {
            handles.push((SidebarEdge::Navigation, rect, nav.width));
        }
    }
    if let Some(inspector) = layout.inspector {
        let x = inspector.x - token::INSPECTOR_GAP / 2.0;
        let rect = EguiRect::from_min_max(
            egui::pos2(x - token::INSPECTOR_GAP.min(8.0) / 2.0, inspector.y + 18.0),
            egui::pos2(
                x + token::INSPECTOR_GAP.min(8.0) / 2.0,
                inspector.y + inspector.height - 18.0,
            ),
        );
        if rect.is_positive() {
            handles.push((SidebarEdge::Inspector, rect, inspector.width));
        }
    } else if let Some(drawer) = drawer {
        let rect = EguiRect::from_min_max(
            drawer.min + egui::vec2(0.0, 18.0),
            egui::pos2(drawer.left() + 8.0, drawer.bottom() - 18.0),
        );
        if rect.is_positive() {
            handles.push((SidebarEdge::Inspector, rect, drawer.width()));
        }
    }
    handles
}

impl SidebarResizeState {
    /// Live allocation is not part of serializable Config until a drag commits.
    pub fn preview_widths(&self) -> Option<SidebarWidths> {
        self.preview
    }

    pub fn interact(
        &mut self,
        ctx: &egui::Context,
        layout: Layout,
        drawer: Option<EguiRect>,
        widths: &mut SidebarWidths,
        enabled: bool,
    ) -> SidebarResizeOutcome {
        let before = *widths;
        let mut outcome = SidebarResizeOutcome {
            pointer_claimed: self.suppress_until_release,
            persist: false,
            edited_edge: None,
        };
        let (down, released, focused, position, press_origin) = ctx.input(|input| {
            (
                input.pointer.primary_down(),
                input.pointer.button_released(egui::PointerButton::Primary),
                input.viewport().focused.unwrap_or(true),
                input.pointer.latest_pos(),
                input.pointer.press_origin(),
            )
        });
        let handles = sidebar_resize_handles(layout, drawer);
        let target_disappeared = self
            .gesture
            .is_some_and(|gesture| !handles.iter().any(|(edge, _, _)| *edge == gesture.edge));
        if !enabled || !focused || target_disappeared {
            if let Some(gesture) = self.gesture.take() {
                *widths = gesture.original;
            }
        } else {
            for (edge, rect, effective_width) in handles {
                let response = egui::Area::new(egui::Id::new(("glass-sidebar-resize", edge as u8)))
                    .order(egui::Order::Foreground)
                    .fixed_pos(rect.min)
                    .movable(false)
                    .constrain_to(ctx.viewport_rect())
                    .show(ctx, |ui| {
                        ui.set_min_size(rect.size());
                        ui.set_max_size(rect.size());
                        ui.set_clip_rect(rect);
                        let (_, response) = ui.allocate_exact_size(
                            rect.size(),
                            egui::Sense::CLICK | egui::Sense::DRAG,
                        );
                        response
                    })
                    .inner;
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::DragValue,
                        true,
                        match edge {
                            SidebarEdge::Navigation => "Resize navigation sidebar",
                            SidebarEdge::Inspector => "Resize inspector sidebar",
                        },
                    )
                });
                if response.is_pointer_button_down_on() {
                    self.suppress_until_release = true;
                    outcome.pointer_claimed = true;
                }
                if response.double_clicked() {
                    let preference = match edge {
                        SidebarEdge::Navigation => &mut widths.navigation,
                        SidebarEdge::Inspector => &mut widths.inspector,
                    };
                    if preference.take().is_some() {
                        outcome.persist = true;
                        outcome.edited_edge = Some(edge);
                    }
                }
                if response.drag_started_by(egui::PointerButton::Primary) && self.gesture.is_none()
                {
                    if let Some(origin) = press_origin.or(position) {
                        self.gesture = Some(SidebarResizeGesture {
                            edge,
                            origin_x: origin.x,
                            effective_width,
                            // Capture the budget once, so an opposite panel's
                            // expansion during shrink cannot ratchet this grip.
                            available_width: if edge == SidebarEdge::Inspector && drawer.is_some() {
                                (ctx.viewport_rect().width()
                                    - layout.rail.width
                                    - token::OUTER_LEFT
                                    - token::OUTER_RIGHT)
                                    .max(0.0)
                            } else {
                                effective_width + (layout.center.width - CENTER_MIN_WIDTH).max(0.0)
                            },
                            original: *widths,
                        });
                    }
                }
                let active = self.gesture.is_some_and(|gesture| gesture.edge == edge);
                if response.hovered() || active {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                // A quiet grab indicator becomes clear on hover, without a
                // hit region extending into either pane's content.
                let color = if active {
                    rgba(token::ACCENT_TOP)
                } else if response.hovered() {
                    rgba(token::MUTED_TEXT)
                } else {
                    rgba(token::BORDER).gamma_multiply(0.45)
                };
                let grip = EguiRect::from_center_size(
                    rect.center(),
                    egui::vec2(
                        if active || response.hovered() {
                            3.0
                        } else {
                            2.0
                        },
                        36.0_f32.min(rect.height()),
                    ),
                );
                ctx.layer_painter(response.layer_id)
                    .rect_filled(grip, 2, color);
                response.on_hover_text("Drag to resize · Double-click to reset width");
            }
            if let Some(gesture) = self.gesture {
                if let Some(position) = position.filter(|_| down || released) {
                    let delta = position.x - gesture.origin_x;
                    let requested = match gesture.edge {
                        SidebarEdge::Navigation => (gesture.effective_width + delta).clamp(
                            NAVIGATION_MIN_WIDTH.min(gesture.available_width),
                            NAVIGATION_MAX_WIDTH.min(gesture.available_width),
                        ),
                        SidebarEdge::Inspector => (gesture.effective_width - delta).clamp(
                            INSPECTOR_MIN_WIDTH.min(gesture.available_width),
                            INSPECTOR_MAX_WIDTH.min(gesture.available_width),
                        ),
                    };
                    match gesture.edge {
                        SidebarEdge::Navigation => widths.navigation = Some(requested),
                        SidebarEdge::Inspector => widths.inspector = Some(requested),
                    }
                }
                if !down {
                    if *widths != gesture.original {
                        outcome.persist = true;
                        outcome.edited_edge = Some(gesture.edge);
                    }
                    self.gesture = None;
                }
            }
        }
        self.preview = self.gesture.map(|_| *widths);
        outcome.pointer_claimed |= self.suppress_until_release;
        if !down {
            self.suppress_until_release = false;
        }
        if *widths != before {
            ctx.request_repaint();
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pointer_frame(
        context: &egui::Context,
        state: &mut SidebarResizeState,
        widths: &mut SidebarWidths,
        size: egui::Vec2,
        events: Vec<egui::Event>,
    ) -> SidebarResizeOutcome {
        let mut outcome = SidebarResizeOutcome::default();
        let input = egui::RawInput {
            screen_rect: Some(EguiRect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..Default::default()
        };
        let output = context.run_ui(input, |_| {
            let layout = Layout::with_sidebar_widths(size.x, size.y, *widths);
            outcome = state.interact(context, layout, None, widths, true);
        });
        output.drop_without_applying_deltas();
        outcome
    }

    fn pointer_button(position: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn resized_sidebars_share_center_budget_and_restore_requested_sizes() {
        let requested = SidebarWidths {
            navigation: Some(480.0),
            inspector: Some(520.0),
        };
        for width in [1612.0, 1180.0, 1000.0, 800.0, 1612.0] {
            let layout = Layout::with_sidebar_widths(width, 850.0, requested);
            assert!(layout.center.width >= CENTER_MIN_WIDTH - 0.01);
            if let Some(nav) = layout.navigation {
                assert!((NAVIGATION_MIN_WIDTH..=NAVIGATION_MAX_WIDTH).contains(&nav.width));
                assert!(nav.x + nav.width < layout.center.x);
            }
            if let Some(inspector) = layout.inspector {
                assert!((INSPECTOR_MIN_WIDTH..=INSPECTOR_MAX_WIDTH).contains(&inspector.width));
                assert!(layout.center.x + layout.center.width < inspector.x);
            }
            for (_, grip, _) in sidebar_resize_handles(layout, None) {
                let center = EguiRect::from_min_size(
                    egui::pos2(layout.center.x, layout.center.y),
                    egui::vec2(layout.center.width, layout.center.height),
                );
                assert!(
                    !grip.intersect(center).is_positive(),
                    "grips must not take terminal selection pixels"
                );
            }
        }
        let wide = Layout::with_sidebar_widths(1612.0, 850.0, requested);
        assert_eq!(wide.navigation.unwrap().width, 480.0);
        assert_eq!(wide.inspector.unwrap().width, 520.0);
        assert!(wide.without_navigation().navigation.is_none());
        assert_eq!(wide.without_inspector().navigation.unwrap().width, 480.0);
        assert_eq!(wide.requested, requested);
    }

    #[test]
    fn both_sidebar_handles_follow_real_pointer_drags_and_commit_on_release() {
        for edge in [SidebarEdge::Navigation, SidebarEdge::Inspector] {
            let context = egui::Context::default();
            let mut state = SidebarResizeState::default();
            let mut widths = SidebarWidths::default();
            let size = egui::vec2(1612.0, 850.0);
            for _ in 0..3 {
                pointer_frame(&context, &mut state, &mut widths, size, vec![]);
            }
            let layout = Layout::with_sidebar_widths(size.x, size.y, widths);
            let (_, rect, original) = sidebar_resize_handles(layout, None)
                .into_iter()
                .find(|(side, _, _)| *side == edge)
                .unwrap();
            let start = rect.center();
            let press = pointer_frame(
                &context,
                &mut state,
                &mut widths,
                size,
                vec![
                    egui::Event::PointerMoved(start),
                    pointer_button(start, true),
                ],
            );
            assert!(press.pointer_claimed);
            assert_eq!(widths, SidebarWidths::default());
            let end = start
                + egui::vec2(
                    if edge == SidebarEdge::Navigation {
                        30.0
                    } else {
                        -30.0
                    },
                    0.0,
                );
            let moved = pointer_frame(
                &context,
                &mut state,
                &mut widths,
                size,
                vec![egui::Event::PointerMoved(end)],
            );
            assert!(moved.pointer_claimed);
            assert!(!moved.persist, "do not write config during a drag");
            let chosen = match edge {
                SidebarEdge::Navigation => widths.navigation.unwrap(),
                SidebarEdge::Inspector => widths.inspector.unwrap(),
            };
            assert!((chosen - original - 30.0).abs() < 0.1);
            assert_eq!(
                match edge {
                    SidebarEdge::Navigation => widths.inspector,
                    SidebarEdge::Inspector => widths.navigation,
                },
                None
            );
            let released = pointer_frame(
                &context,
                &mut state,
                &mut widths,
                size,
                vec![pointer_button(end, false)],
            );
            assert!(released.pointer_claimed && released.persist);
            assert_eq!(released.edited_edge, Some(edge));
            assert!(
                !pointer_frame(&context, &mut state, &mut widths, size, vec![]).pointer_claimed
            );
        }
    }

    #[test]
    fn constrained_drag_tracks_and_reverses_without_rebalancing_opposite_request() {
        for edge in [SidebarEdge::Navigation, SidebarEdge::Inspector] {
            let context = egui::Context::default();
            let mut state = SidebarResizeState::default();
            let mut widths = SidebarWidths {
                navigation: Some(480.0),
                inspector: Some(520.0),
            };
            let size = egui::vec2(1180.0, 850.0);
            for _ in 0..3 {
                pointer_frame(&context, &mut state, &mut widths, size, vec![]);
            }
            let initial = Layout::with_sidebar_widths(size.x, size.y, widths);
            let (_, rect, original) = sidebar_resize_handles(initial, None)
                .into_iter()
                .find(|(side, _, _)| *side == edge)
                .unwrap();
            let start = rect.center();
            pointer_frame(
                &context,
                &mut state,
                &mut widths,
                size,
                vec![
                    egui::Event::PointerMoved(start),
                    pointer_button(start, true),
                ],
            );
            let shrink = start
                + egui::vec2(
                    if edge == SidebarEdge::Navigation {
                        -30.0
                    } else {
                        30.0
                    },
                    0.0,
                );
            pointer_frame(
                &context,
                &mut state,
                &mut widths,
                size,
                vec![egui::Event::PointerMoved(shrink)],
            );
            let smaller = Layout::with_sidebar_widths(size.x, size.y, widths);
            let effective = if edge == SidebarEdge::Navigation {
                smaller.navigation.unwrap().width
            } else {
                smaller.inspector.unwrap().width
            };
            assert!((effective - (original - 30.0)).abs() < 0.1);
            pointer_frame(
                &context,
                &mut state,
                &mut widths,
                size,
                vec![egui::Event::PointerMoved(start)],
            );
            let restored = Layout::with_sidebar_widths(size.x, size.y, widths);
            assert!(
                (restored.navigation.unwrap().width - initial.navigation.unwrap().width).abs()
                    < 0.1
            );
            assert!(
                (restored.inspector.unwrap().width - initial.inspector.unwrap().width).abs() < 0.1
            );
            assert_eq!(
                if edge == SidebarEdge::Navigation {
                    widths.inspector
                } else {
                    widths.navigation
                },
                Some(if edge == SidebarEdge::Navigation {
                    520.0
                } else {
                    480.0
                })
            );
            let released = pointer_frame(
                &context,
                &mut state,
                &mut widths,
                size,
                vec![
                    egui::Event::PointerMoved(egui::pos2(-100.0, 100.0)),
                    pointer_button(egui::pos2(-100.0, 100.0), false),
                ],
            );
            assert!(
                released.pointer_claimed,
                "release outside the handle still belongs to resizing"
            );
        }
    }

    #[test]
    fn no_motion_click_keeps_clamped_preference_for_window_growth() {
        let context = egui::Context::default();
        let mut state = SidebarResizeState::default();
        let mut widths = SidebarWidths {
            navigation: Some(480.0),
            inspector: Some(520.0),
        };
        let initial = widths;
        let size = egui::vec2(800.0, 850.0);
        for _ in 0..3 {
            pointer_frame(&context, &mut state, &mut widths, size, vec![]);
        }
        let (_, rect, effective) =
            sidebar_resize_handles(Layout::with_sidebar_widths(size.x, size.y, widths), None)[0];
        assert!(effective < 480.0);
        let point = rect.center();
        pointer_frame(
            &context,
            &mut state,
            &mut widths,
            size,
            vec![
                egui::Event::PointerMoved(point),
                pointer_button(point, true),
            ],
        );
        let released = pointer_frame(
            &context,
            &mut state,
            &mut widths,
            size,
            vec![pointer_button(point, false)],
        );
        assert!(!released.persist);
        assert_eq!(widths, initial);
    }

    #[test]
    fn glass_reference_geometry_and_hidden_inspector_budget() {
        let l = Layout::for_size(1612.0, 976.0);
        assert_eq!(
            (
                l.rail.x,
                l.navigation.unwrap().x,
                l.center.x,
                l.inspector.unwrap().x
            ),
            (4.0, 48.0, 270.0, 1320.0)
        );
        assert_eq!(
            (
                l.composer.x,
                l.composer.y,
                l.composer.width,
                l.composer.height
            ),
            (270.0, 928.0, 1044.0, 42.0)
        );
        let hidden = l.without_inspector();
        assert_eq!(hidden.center.width - l.center.width, 294.0);
        assert_eq!(hidden.composer.y, l.composer.y);
        assert!(Layout::for_size(800.0, 640.0).inspector.is_none());
    }

    #[test]
    fn compact_chrome_grows_for_ui_fonts_without_changing_sidebar_intent() {
        let compact = Layout::for_size(1612.0, 976.0).with_chrome_font_size(14.0);
        assert_eq!(
            (
                compact.titlebar.height,
                compact.header.height,
                compact.composer.height
            ),
            (40.0, 36.0, 42.0)
        );
        for font in [14.0, 18.0, 24.0, 32.0, 40.0, 72.0] {
            for height in [400.0, 700.0, 976.0] {
                let layout = Layout::for_size(1280.0, height).with_chrome_font_size(font);
                let control = chrome_control_height(font);
                assert!(layout.titlebar.height >= control + 8.0);
                assert!(layout.header.height >= control + 4.0);
                assert!(layout.composer.height >= control + 10.0);
                assert!(layout.composer.y >= layout.header.y + layout.header.height);
                assert!(layout.center.width >= CENTER_MIN_WIDTH);
                assert_eq!(layout.rail.width, 44.0);
                assert_eq!(layout.navigation.unwrap().width, 216.0);
                assert_eq!(layout.inspector.unwrap().width, 288.0);
                assert_eq!(layout.composer.y + layout.composer.height, height - 6.0);
            }
        }
        let ctx = egui::Context::default();
        let mut shapes = Vec::new();
        let output = ctx.run_ui(Default::default(), |ui| {
            paint_panel(
                ui.painter(),
                EguiRect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 200.0)),
                token::CENTER,
            );
        });
        for shape in &output.shapes {
            if let egui::Shape::Rect(rect) = &shape.shape {
                if rect.rect.size() == egui::vec2(300.0, 200.0) {
                    shapes.push(rect);
                }
            }
        }
        assert_eq!(
            shapes.len(),
            2,
            "one fill and one thin edge; no nested glow frames"
        );
        assert!(shapes.iter().all(|rect| rect.stroke.width <= 1.0));
        output.drop_without_applying_deltas();
    }

    #[test]
    fn real_input_and_tool_band_share_one_nonoverlapping_budget() {
        for height in [759.0, 477.0, 350.0] {
            let outer =
                EguiRect::from_min_size(egui::pos2(349.0, 171.0), egui::vec2(892.0, height));
            for line in [12.0, 18.0, 23.0, 32.0, 72.0] {
                let padding = 20.0;
                let editor_bottom =
                    outer.bottom() - padding - footer_rows(padding, line) as f32 * line;
                for editor_height in [line, 4.0 * line, 180.0] {
                    let editor_top = editor_bottom - editor_height;
                    if editor_height > editable_input_limit(height, line) {
                        let raw_bottom =
                            outer.bottom() - COMPOSER_HEIGHT - token::TERMINAL_FOOTER_GAP;
                        assert!(raw_bottom > outer.top());
                        assert_eq!(
                            outer.bottom() - COMPOSER_HEIGHT + COMPOSER_HEIGHT,
                            outer.bottom()
                        );
                        continue; // Whole native viewport owns the long input; no dock clipping.
                    }
                    assert!(editor_top >= outer.top() + 6.0);
                    let dock = editor_dock(outer, editor_top);
                    assert!(dock.history_bottom <= editor_top);
                    assert!(editor_bottom + 6.0 <= dock.controls.top());
                    assert_eq!(dock.controls.height(), 32.0);
                    assert_eq!(dock.frame.bottom(), outer.bottom());
                    assert!(dock.frame.height() >= 42.0);
                }
            }
        }
    }
}
