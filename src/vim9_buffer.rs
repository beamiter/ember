//! The Vim buffer's native text surface.
//!
//! Its input is the authenticated Vim buffer protocol, never ANSI cells or a
//! terminal palette. Logical screen columns are retained for Vim hit testing;
//! egui owns glyph shaping, RGB backgrounds and all text decorations.

use egui::{self, Color32, FontId, Rect, Stroke};
use jterm_core::vim9::{
    Vim9BufferPosition, Vim9Command, Vim9CursorBlink, Vim9InputSnapshot, Vim9PixelRect,
    Vim9PixelScroll, Vim9PointerPhase, Vim9RenderFrame, Vim9RenderMetrics, Vim9RenderRect,
    Vim9RenderStyle, Vim9RenderWindow, Vim9ScrollAnimation, Vim9Session, Vim9Viewport,
    Vim9ViewportAnchor,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

#[cfg(all(test, unix))]
#[path = "vim9_native_input_tests.rs"]
mod native_input_tests;

/// A single source-bound transform shared by native input and paint. The
/// application owns admission; the painter only samples these bounded offsets.
#[derive(Default)]
pub struct DirectInput {
    scrolls: HashMap<i64, Vim9PixelScroll>,
    suppress_fraction: bool,
    pointer: Option<NativePointer>,
    discarded_pointer: bool,
    next_gesture: u64,
    last_press: Option<(f64, egui::Pos2)>,
    presented: Option<PresentedFrame>,
    presentation_retired: bool,
}

struct PresentedFrame {
    frame: Arc<Vim9RenderFrame>,
    metrics: Vim9RenderMetrics,
    offsets: HashMap<i64, f32>,
    viewport: egui::ViewportId,
    frame_number: u64,
}

struct NativePointer {
    gesture: u64,
    target: Vim9BufferPosition,
    metrics: Vim9RenderMetrics,
    cancelled: bool,
    request: u64,
    queued: Option<(Vim9BufferPosition, Vim9PointerPhase)>,
    released: bool,
}

impl DirectInput {
    pub fn set_smooth(&mut self, enabled: bool) {
        if self.suppress_fraction == enabled {
            self.cancel();
            self.suppress_fraction = !enabled;
        }
    }

    /// A cancelled press still owns its physical release. Never turn the tail
    /// of a semantic drag into an orphan legacy mouse report after Escape.
    pub fn cancel(&mut self) {
        self.scrolls.clear();
        // Keep the visible image's ownership until its replacement is painted.
        // A key/toolbar action may cancel before this frame's pointer admission.
        self.presentation_retired = true;
        if let Some(pointer) = &mut self.pointer {
            pointer.cancelled = true;
            pointer.queued = None;
        }
    }

    pub fn pointer_captured(&self) -> bool {
        self.pointer.is_some() || self.discarded_pointer
    }

    pub fn finish_cancelled_pointer(&mut self, down: bool) -> bool {
        let captured = self.pointer_captured();
        if !down {
            self.pointer = None;
            self.discarded_pointer = false;
        }
        captured
    }

    /// Unsupported clicks over a translated image settle it first. Their
    /// press and physical tail must never become legacy cells in a new image.
    pub fn discard_press(&mut self) {
        self.cancel();
        self.pointer = None;
        self.discarded_pointer = true;
    }

    pub fn discarding_pointer(&self) -> bool {
        self.discarded_pointer
    }

    /// The frame and completion set must be one bridge observation. A newer
    /// completion paired with an old frame would falsely retire its fraction.
    /// Painting repeats this reconciliation so a late exact ack is rebased
    /// before the acknowledged image is submitted.
    pub fn reconcile(&mut self, snapshot: &Vim9InputSnapshot, geometry: Vim9RenderMetrics) {
        let Some(frame) = &snapshot.frame else {
            self.cancel();
            return;
        };
        self.reconcile_with(frame, geometry, |id| snapshot.command_pending(id));
    }

    fn reconcile_with(
        &mut self,
        frame: &Vim9RenderFrame,
        geometry: Vim9RenderMetrics,
        pending: impl Fn(u64) -> bool,
    ) {
        let topology_changed = self
            .presented
            .as_ref()
            .is_some_and(|presented| !same_layout(presented, frame, geometry));
        if topology_changed || !frame.supported || frame.completion.is_some() {
            self.cancel();
            return;
        }
        self.scrolls.retain(|window, scroll| {
            frame
                .windows
                .iter()
                .find(|view| view.window == *window)
                .is_some_and(|view| {
                    let in_flight = scroll.request_id().is_some_and(&pending);
                    scroll.reconcile(view, geometry, in_flight)
                })
        });
        if let Some(pointer) = &mut self.pointer {
            let source_current = frame.windows.iter().any(|window| {
                window.native_pointer
                    && (window.window, window.buffer, window.changedtick)
                        == (
                            pointer.target.window,
                            pointer.target.buffer,
                            pointer.target.changedtick,
                        )
            });
            pointer.cancelled |= !source_current || pointer.metrics != geometry;
        }
    }

    pub fn animating(&self) -> bool {
        self.scrolls.values().any(Vim9PixelScroll::is_animating)
    }

    pub fn needs_wake(&self, session: &Vim9Session) -> bool {
        self.scrolls
            .values()
            .any(|scroll| scroll.request_id().is_some() || scroll.is_animating())
            || self.pointer.as_ref().is_some_and(|pointer| {
                !pointer.cancelled
                    && (pointer.queued.is_some() || session.command_pending(pointer.request))
            })
    }

    /// Retry only unsent motion from this gesture, after its exact semantic
    /// acknowledgement. A rejected request drops the local transform.
    pub fn flush_scroll(&mut self, session: &Vim9Session) {
        self.scrolls.retain(|_, scroll| {
            let Some(command) = scroll.command() else {
                return true;
            };
            match session.send(command) {
                Ok(id) => {
                    scroll.submitted(id);
                    true
                }
                Err(_) => false,
            }
        });
    }

    pub fn wheel(
        &mut self,
        frame: &Vim9RenderFrame,
        window: i64,
        geometry: Vim9RenderMetrics,
        delta: f64,
        session: &Vim9Session,
    ) -> bool {
        self.wheel_with(frame, window, geometry, delta, session, false)
    }

    pub fn wheel_discrete(
        &mut self,
        frame: &Vim9RenderFrame,
        window: i64,
        geometry: Vim9RenderMetrics,
        delta: f64,
        session: &Vim9Session,
    ) -> bool {
        self.wheel_with(frame, window, geometry, delta, session, true)
    }

    fn wheel_with(
        &mut self,
        frame: &Vim9RenderFrame,
        window: i64,
        geometry: Vim9RenderMetrics,
        delta: f64,
        session: &Vim9Session,
        discrete: bool,
    ) -> bool {
        if frame.completion.is_some() {
            self.cancel();
            return false;
        }
        let Some(view) = frame.windows.iter().find(|view| view.window == window) else {
            return false;
        };
        if let std::collections::hash_map::Entry::Vacant(entry) = self.scrolls.entry(window) {
            let Some(scroll) = Vim9PixelScroll::new(view, geometry) else {
                return false;
            };
            entry.insert(scroll);
        }
        let scroll = self
            .scrolls
            .get_mut(&window)
            .expect("bound window inserted above");
        if discrete && !self.suppress_fraction {
            scroll.push_wheel(delta);
        } else {
            scroll.push(delta);
        }
        if let Some(command) = scroll.command() {
            match session.send(command) {
                Ok(id) => scroll.submitted(id),
                Err(_) => {
                    self.scrolls.remove(&window);
                    return false;
                }
            }
        }
        true
    }

    pub fn offset(&self, frame: &Vim9RenderFrame, window: i64, geometry: Vim9RenderMetrics) -> f32 {
        if self.suppress_fraction {
            return 0.0;
        }
        self.scrolls
            .get(&window)
            .map_or(0.0, |scroll| scroll.offset(frame, geometry))
    }

    pub fn remember_presentation(
        &mut self,
        frame: Arc<Vim9RenderFrame>,
        geometry: Vim9RenderMetrics,
        ctx: &egui::Context,
    ) {
        let offsets = frame
            .windows
            .iter()
            .map(|window| (window.window, self.offset(&frame, window.window, geometry)))
            .collect();
        self.presented = Some(PresentedFrame {
            frame,
            metrics: geometry,
            offsets,
            viewport: ctx.viewport_id(),
            frame_number: ctx.cumulative_frame_nr(),
        });
        self.presentation_retired = false;
    }

    pub fn forget_presentation(&mut self) {
        self.presented = None;
        self.presentation_retired = false;
    }

    /// Hidden tabs and removed panes do not paint a replacement into this
    /// editor. Their old image stops owning new input once another completed
    /// egui frame has replaced it. An already captured release still belongs
    /// to its original gesture, including across an editor switch.
    pub fn retire_unpainted(&mut self, ctx: &egui::Context) {
        let obsolete = self.presented.as_ref().is_some_and(|presented| {
            presented.viewport != ctx.viewport_id()
                || presented.frame_number.saturating_add(1) < ctx.cumulative_frame_nr()
        });
        if obsolete {
            self.cancel();
            self.forget_presentation();
        }
    }

    pub fn presented_fraction_at(&self, point: egui::Pos2) -> bool {
        self.presented.as_ref().is_some_and(|presented| {
            presented.frame.windows.iter().any(|window| {
                presented
                    .offsets
                    .get(&window.window)
                    .copied()
                    .unwrap_or(0.0)
                    != 0.0
                    && presented
                        .metrics
                        .rect(&window.view)
                        .is_some_and(|rect| rect.contains(point.x, point.y))
            })
        })
    }

    /// A wheel over an obsolete source must not fall through to PTY cells in
    /// its unseen replacement. Unsupported but unchanged sources still use
    /// Vim's normal mouse route.
    pub fn stale_presentation_at(
        &self,
        current: &Vim9RenderFrame,
        geometry: Vim9RenderMetrics,
        point: egui::Pos2,
    ) -> bool {
        let Some(presented) = &self.presented else {
            return false;
        };
        let Some(old) = presented.frame.windows.iter().find(|window| {
            presented
                .metrics
                .rect(&window.view)
                .is_some_and(|rect| rect.contains(point.x, point.y))
        }) else {
            return false;
        };
        self.presentation_retired
            || !same_layout(presented, current, geometry)
            || !current.windows.iter().any(|new| same_source(old, new))
    }

    /// Source hits are frozen with the image submitted to egui. A newer bridge
    /// frame or scroll acknowledgement cannot move a click before repaint.
    fn presented_anchor(
        &self,
        current: &Vim9RenderFrame,
        geometry: Vim9RenderMetrics,
        point: egui::Pos2,
    ) -> Option<Vim9ViewportAnchor> {
        let presented = self.presented.as_ref()?;
        if self.presentation_retired
            || !current.supported
            || current.completion.is_some()
            || !same_layout(presented, current, geometry)
        {
            return None;
        }
        presented
            .frame
            .windows
            .iter()
            .filter(|window| window.native_pointer)
            .find_map(|window| {
                let still_current = current
                    .windows
                    .iter()
                    .any(|new| new.native_pointer && same_source(window, new));
                if !still_current {
                    return None;
                }
                let offset = presented
                    .offsets
                    .get(&window.window)
                    .copied()
                    .unwrap_or(0.0);
                Vim9Viewport::new(&presented.frame, window.window, geometry, offset)?
                    .anchor_at(point.x, point.y)
            })
    }

    pub fn wheel_window_at(
        &self,
        current: &Vim9RenderFrame,
        geometry: Vim9RenderMetrics,
        point: egui::Pos2,
    ) -> Option<i64> {
        let presented = self.presented.as_ref()?;
        if self.presentation_retired
            || !current.supported
            || current.completion.is_some()
            || !same_layout(presented, current, geometry)
        {
            return None;
        }
        presented.frame.windows.iter().find_map(|old| {
            let same_source = current
                .windows
                .iter()
                .any(|new| new.native_pointer && same_source(old, new));
            (old.native_pointer
                && same_source
                && geometry.rect(&old.view)?.contains(point.x, point.y))
            .then_some(old.window)
        })
    }

    pub fn press(
        &mut self,
        frame: &Vim9RenderFrame,
        geometry: Vim9RenderMetrics,
        point: egui::Pos2,
        now: f64,
        session: &Vim9Session,
    ) -> bool {
        if frame.completion.is_some() {
            self.cancel();
            return false;
        }
        let repeated = self.last_press.is_some_and(|(time, pos)| {
            now >= time && now - time <= 0.5 && pos.distance(point) <= 6.0
        });
        self.last_press = Some((now, point));
        // Static clicks keep Vim's existing mouse mappings and multi-click
        // behavior. Source bytes are needed while a fraction is actually shown.
        if !self.presented_fraction_at(point) {
            return false;
        }
        if repeated {
            self.discard_press();
            return true;
        }
        let Some(anchor) = self.presented_anchor(frame, geometry, point) else {
            self.discard_press();
            return true;
        };
        self.next_gesture = self.next_gesture.saturating_add(1).max(1);
        let gesture = self.next_gesture;
        let Ok(request) = session.send(pointer_command(
            &anchor.position,
            gesture,
            Vim9PointerPhase::Press,
        )) else {
            self.discard_press();
            return true;
        };
        for scroll in self.scrolls.values_mut() {
            scroll.stop_animation();
        }
        self.pointer = Some(NativePointer {
            gesture,
            target: anchor.position,
            metrics: geometry,
            cancelled: false,
            request,
            queued: None,
            released: false,
        });
        true
    }

    pub fn pointer(
        &mut self,
        frame: &Vim9RenderFrame,
        geometry: Vim9RenderMetrics,
        point: Option<egui::Pos2>,
        released: bool,
        session: &Vim9Session,
    ) -> bool {
        let Some(mut pointer) = self.pointer.take() else {
            return false;
        };
        if !pointer.cancelled && !pointer.released {
            let target = point.and_then(|point| self.presented_anchor(frame, geometry, point));
            let target = target
                .filter(|anchor| {
                    (
                        anchor.position.window,
                        anchor.position.buffer,
                        anchor.position.changedtick,
                    ) == (
                        pointer.target.window,
                        pointer.target.buffer,
                        pointer.target.changedtick,
                    )
                })
                .map(|anchor| anchor.position);
            if released {
                pointer.queued =
                    Some((target.unwrap_or(pointer.target), Vim9PointerPhase::Release));
                pointer.released = true;
            } else if let Some(target) = target.filter(|target| *target != pointer.target) {
                pointer.queued = Some((target, Vim9PointerPhase::Drag));
                pointer.target = target;
            }
        }
        let mut complete = pointer.cancelled && released;
        if !pointer.cancelled && !session.command_pending(pointer.request) {
            if let Some((target, phase)) = pointer.queued.take() {
                match session.send(pointer_command(&target, pointer.gesture, phase)) {
                    Ok(id) => {
                        pointer.request = id;
                        pointer.target = target;
                    }
                    Err(_) => {
                        pointer.cancelled = true;
                        complete = pointer.released;
                    }
                }
            } else if pointer.released {
                complete = true;
            }
        }
        if !complete {
            self.pointer = Some(pointer);
        }
        true
    }
}

fn same_layout(
    presented: &PresentedFrame,
    current: &Vim9RenderFrame,
    geometry: Vim9RenderMetrics,
) -> bool {
    presented.metrics == geometry
        && presented.frame.rows == current.rows
        && presented.frame.columns == current.columns
        && presented.frame.windows.len() == current.windows.len()
        && presented
            .frame
            .windows
            .iter()
            .zip(&current.windows)
            .all(|(old, new)| old.window == new.window && old.view == new.view)
}

fn same_source(old: &Vim9RenderWindow, new: &Vim9RenderWindow) -> bool {
    (
        old.window,
        old.buffer,
        old.changedtick,
        &old.view,
        old.leftcol,
        old.textoff,
    ) == (
        new.window,
        new.buffer,
        new.changedtick,
        &new.view,
        new.leftcol,
        new.textoff,
    )
}

fn pointer_command(
    position: &Vim9BufferPosition,
    gesture: u64,
    phase: Vim9PointerPhase,
) -> Vim9Command {
    Vim9Command::Pointer {
        window: position.window,
        buffer: position.buffer,
        changedtick: position.changedtick,
        line: position.line,
        byte_column: position.byte_column,
        gesture,
        phase,
    }
}

/// Acknowledged Vim frames are the source of truth. While a short scroll
/// settles, retain its old edge rows so no empty band flashes at the boundary.
/// State is bound to the editor's identity, never a renderer's reused pane.
#[derive(Default)]
pub(crate) struct ScrollPresentation {
    cursor_blink: std::cell::RefCell<Vim9CursorBlink>,
    identity: Option<uuid::Uuid>,
    current: Option<Arc<Vim9RenderFrame>>,
    windows: HashMap<i64, WindowScroll>,
    line_height: f32,
    suspend_until: f64,
}

struct WindowScroll {
    animation: Vim9ScrollAnimation,
    /// Covering outgoing frame followed by newer candidates for a long gesture.
    sources: VecDeque<(Arc<Vim9RenderFrame>, u64)>,
}

impl ScrollPresentation {
    pub(crate) fn update(
        &mut self,
        identity: Option<uuid::Uuid>,
        frame: Option<&Arc<Vim9RenderFrame>>,
        line_height: f32,
        now: f64,
        enabled: bool,
        direct_input: bool,
    ) {
        if self.identity != identity || frame.is_none() {
            self.cursor_blink.borrow_mut().reset();
        }
        if direct_input {
            self.cursor_blink.borrow_mut().reset();
            // A click must address the cells actually visible in Vim. Do not
            // animate a delayed acknowledgement of keyboard navigation either.
            self.suspend_until = now + 0.15;
        }
        self.windows
            .retain(|_, scroll| scroll.animation.is_active(now));
        let reset = !enabled
            || now < self.suspend_until
            || identity.is_none()
            || self.identity != identity
            || self.line_height != line_height
            || frame.is_none()
            || frame.is_some_and(|frame| frame.completion.is_some());
        if reset {
            self.windows.clear();
        } else if let (Some(old), Some(new)) = (&self.current, frame) {
            if old.revision != new.revision {
                let same_layout = old.rows == new.rows
                    && old.columns == new.columns
                    && old.default_style == new.default_style
                    && old.windows.len() == new.windows.len()
                    && old.windows.iter().zip(&new.windows).all(|(a, b)| {
                        a.window == b.window
                            && a.buffer == b.buffer
                            && a.changedtick == b.changedtick
                            && a.view == b.view
                            && a.leftcol == b.leftcol
                            && a.textoff == b.textoff
                    });
                if !same_layout {
                    self.windows.clear();
                } else {
                    for (a, b) in old.windows.iter().zip(&new.windows) {
                        let delta = b.topline as f64 - a.topline as f64;
                        if delta == 0.0 {
                            continue;
                        }
                        // Large jumps, empty/legacy viewports and invalid
                        // geometry settle immediately instead of flying past.
                        if !delta.is_finite()
                            || delta.abs() >= b.view.rows as f64
                            || a.topline == 0
                            || !line_height.is_finite()
                            || line_height <= 0.0
                        {
                            self.windows.remove(&b.window);
                            continue;
                        }
                        let entry = self
                            .windows
                            .entry(b.window)
                            .or_insert_with(|| WindowScroll {
                                animation: Vim9ScrollAnimation::default(),
                                sources: VecDeque::new(),
                            });
                        let pixels = delta * f64::from(line_height);
                        entry.animation.retarget(pixels, now);
                        entry.sources.push_back((Arc::clone(old), a.topline));
                        let offset = entry.animation.offset(now);
                        let height = b.view.rows as f64 * f64::from(line_height);
                        let covering_source = entry.sources.iter().rposition(|(_, top)| {
                            let distance =
                                (b.topline as f64 - *top as f64) * f64::from(line_height);
                            if offset >= 0.0 {
                                offset <= distance && distance <= height
                            } else {
                                -height <= distance && distance <= offset
                            }
                        });
                        if let Some(source) = covering_source.filter(|_| offset.abs() < height) {
                            entry.sources.drain(..source);
                            // Preserve the covering source and recent candidates.
                            // Gesture distance is unbounded; retained frames aren't.
                            const MAX_EDGE_SOURCES: usize = 33;
                            if entry.sources.len() > MAX_EDGE_SOURCES {
                                entry
                                    .sources
                                    .drain(1..entry.sources.len() - MAX_EDGE_SOURCES + 1);
                            }
                        } else {
                            self.windows.remove(&b.window);
                        }
                    }
                }
            }
        }
        self.identity = identity;
        self.line_height = line_height;
        self.current = frame.cloned();
    }

    pub(crate) fn offset(&self, window: i64, now: f64) -> f32 {
        self.windows
            .get(&window)
            .map_or(0.0, |scroll| scroll.animation.offset(now) as f32)
    }

    pub(crate) fn active(&self, now: f64) -> bool {
        self.windows
            .values()
            .any(|scroll| scroll.animation.is_active(now))
    }
}

fn gui_rect(rect: Vim9PixelRect) -> Rect {
    Rect::from_min_size(
        egui::pos2(rect.x, rect.y),
        egui::vec2(rect.width, rect.height),
    )
}

fn metrics(content: Rect, cell_width: f32, line_height: f32) -> Vim9RenderMetrics {
    Vim9RenderMetrics {
        origin_x: content.left(),
        origin_y: content.top(),
        cell_width,
        line_height,
    }
}

/// Egui reports movement of the content, while Vim expects positive lines
/// towards the end of the file. Precision devices keep their pixel distance.
#[allow(dead_code)] // The application binary admits events; the library exposes the painter.
pub(crate) fn wheel_lines(event: &egui::Event, line_height: f32, rows: usize) -> Option<f64> {
    let egui::Event::MouseWheel {
        unit,
        delta,
        modifiers,
        ..
    } = event
    else {
        return None;
    };
    if modifiers.ctrl
        || modifiers.command
        || modifiers.shift
        || modifiers.alt
        || !delta.y.is_finite()
        || delta.y == 0.0
        || delta.x.abs() > delta.y.abs()
        || !line_height.is_finite()
        || line_height <= 0.0
    {
        return None;
    }
    Some(
        -f64::from(delta.y)
            * match unit {
                egui::MouseWheelUnit::Line => 3.0,
                egui::MouseWheelUnit::Point => f64::from(line_height).recip(),
                egui::MouseWheelUnit::Page => rows.max(1) as f64,
            },
    )
}

/// The native caret and the OS IME candidate window share the same fractional
/// coordinates. Terminal-grid rounding must not move a GUI overlay by a cell.
pub(crate) fn cursor_rect(
    frame: &Vim9RenderFrame,
    content: Rect,
    cell_width: f32,
    line_height: f32,
    offset_y: f32,
) -> Option<Rect> {
    let rect = metrics(content, cell_width, line_height).cursor_rect(&frame.cursor)?;
    let view = frame
        .paint_views()
        .find(|view| view.contains(frame.cursor.row, frame.cursor.column))?;
    let clip = gui_rect(metrics(content, cell_width, line_height).rect(view)?).intersect(content);
    let rect = gui_rect(rect)
        .translate(egui::vec2(0.0, offset_y))
        .intersect(clip);
    rect.is_positive().then_some(rect)
}

pub(crate) fn usable(frame: &Vim9RenderFrame, rows: usize, columns: usize) -> bool {
    frame.supported
        && frame.rows == rows
        && frame.columns == columns
        && frame.paint_views().all(|view| {
            view.rows > 0
                && view.columns > 0
                && view.row.saturating_add(view.rows) <= rows
                && view.column.saturating_add(view.columns) <= columns
        })
}

pub(crate) fn covers(frame: &Vim9RenderFrame, row: usize, column: usize) -> bool {
    frame.covers(row, column)
}

fn rgb(value: u32) -> Color32 {
    Color32::from_rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

fn colors(style: &Vim9RenderStyle, normal: &Vim9RenderStyle) -> (Color32, Color32, Color32) {
    // Unspecified GUI Normal colors use a neutral native-editor theme. Neither
    // terminal indexed colors nor OSC palette mutations participate here.
    let mut fg = rgb(style.foreground.or(normal.foreground).unwrap_or(0xd7dce2));
    let mut bg = rgb(style.background.or(normal.background).unwrap_or(0x15191f));
    if style.reverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    (
        fg,
        bg,
        style.special.or(normal.special).map(rgb).unwrap_or(fg),
    )
}

/// Paint each buffer rectangle independently. Vim's command/status rows and
/// split separators remain in the compatibility renderer between rectangles.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint(
    painter: &egui::Painter,
    frame: &Vim9RenderFrame,
    content: Rect,
    cell_width: f32,
    line_height: f32,
    font_size: f32,
    focused: bool,
    cursor_on: bool,
    scroll: &ScrollPresentation,
    now: f64,
    direct: Option<&DirectInput>,
) {
    if !metrics(content, cell_width, line_height).is_valid()
        || !content.is_positive()
        || !font_size.is_finite()
        || font_size <= 0.0
    {
        return;
    }
    let cursor_on = if frame.cursor.appearance.is_some() {
        let mut blink = scroll.cursor_blink.borrow_mut();
        let (on, next) = blink.sample(frame, focused, std::time::Instant::now());
        if let Some(delay) = next {
            painter.ctx().request_repaint_after(delay);
        }
        on
    } else {
        cursor_on
    };
    if let Some(direct) = direct.filter(|_| !frame.windows.is_empty()) {
        paint_direct(
            painter,
            frame,
            content,
            cell_width,
            line_height,
            font_size,
            focused,
            cursor_on,
            direct,
        );
        paint_completion(painter, frame, content, cell_width, line_height, font_size);
        return;
    }
    for view in frame.paint_views() {
        let (window, top) = frame
            .windows
            .iter()
            .find(|window| &window.view == view)
            .map_or((frame.window, 0), |window| (window.window, window.topline));
        let offset = scroll.offset(window, now);
        if let Some(scroll) = scroll.windows.get(&window).filter(|_| offset != 0.0) {
            if let Some((previous, source_top)) = scroll.sources.front() {
                if let Some(old_view) = previous.windows.iter().find(|old| old.window == window) {
                    let distance = (top as f64 - *source_top as f64) * f64::from(line_height);
                    paint_view(
                        painter,
                        previous,
                        &old_view.view,
                        content,
                        cell_width,
                        line_height,
                        font_size,
                        focused,
                        false,
                        offset - distance as f32,
                        false,
                    );
                }
            }
        }
        paint_view(
            painter,
            frame,
            view,
            content,
            cell_width,
            line_height,
            font_size,
            focused,
            cursor_on,
            offset,
            true,
        );
    }
    paint_completion(painter, frame, content, cell_width, line_height, font_size);
}

/// Completion owns a screen rectangle above every split, including Vim's
/// status and separator rows. It never inherits a buffer's scrolling offset.
fn paint_completion(
    painter: &egui::Painter,
    frame: &Vim9RenderFrame,
    content: Rect,
    cell_width: f32,
    line_height: f32,
    font_size: f32,
) {
    let Some(completion) = &frame.completion else {
        return;
    };
    let geometry = metrics(content, cell_width, line_height);
    let Some(view) = geometry.rect(&completion.view).map(gui_rect) else {
        return;
    };
    let clip = view.intersect(content);
    if !clip.is_positive() {
        return;
    }
    // Elevation belongs to the editor overlay, not to any scrolling split.
    // Keep Vim's menu rectangle unchanged; its opaque fill masks the shadow's
    // interior while the editor clip contains the four-point outer falloff.
    let overlay = painter.with_clip_rect(content);
    for (spread, alpha) in [(4.0, 4), (3.0, 5), (2.0, 7), (1.0, 10)] {
        overlay.rect_filled(view.expand(spread), 0.0, Color32::from_black_alpha(alpha));
    }
    let painter = overlay.with_clip_rect(clip);
    let (foreground, background, _) = colors(&completion.default_style, &frame.default_style);
    painter.rect_filled(view, 0.0, background);
    for run in &completion.runs {
        if let Some(rect) = geometry.cell_rect(run.row, run.column, run.cells) {
            let (_, background, _) = colors(&run.style, &completion.default_style);
            painter.rect_filled(gui_rect(rect), 0.0, background);
        }
    }
    for run in &completion.runs {
        if let Some(rect) = geometry.cell_rect(run.row, run.column, run.cells) {
            paint_run(
                &painter,
                run,
                &completion.default_style,
                gui_rect(rect),
                clip,
                cell_width,
                line_height,
                font_size,
            );
        }
    }
    let pixel = painter.ctx().pixels_per_point().max(0.1).recip();
    painter.rect_stroke(
        view,
        0.0,
        Stroke::new(pixel, foreground.gamma_multiply(0.20)),
        egui::StrokeKind::Inside,
    );
}

/// The direct fractional transform owns both sampled edge rows and the caret.
pub(crate) fn direct_cursor_rect(
    frame: &Vim9RenderFrame,
    content: Rect,
    cell_width: f32,
    line_height: f32,
    direct: &DirectInput,
) -> Option<Rect> {
    let geometry = metrics(content, cell_width, line_height);
    Vim9Viewport::new(
        frame,
        frame.window,
        geometry,
        direct.offset(frame, frame.window, geometry),
    )?
    .cursor_rect()
    .map(gui_rect)
    .map(|rect| rect.intersect(content))
    .filter(Rect::is_positive)
}

#[allow(clippy::too_many_arguments)]
fn paint_direct(
    painter: &egui::Painter,
    frame: &Vim9RenderFrame,
    content: Rect,
    cell_width: f32,
    line_height: f32,
    font_size: f32,
    focused: bool,
    cursor_on: bool,
    direct: &DirectInput,
) {
    let geometry = metrics(content, cell_width, line_height);
    let (normal_fg, normal_bg, _) = colors(&frame.default_style, &frame.default_style);
    for window in &frame.windows {
        let Some(viewport) = Vim9Viewport::new(
            frame,
            window.window,
            geometry,
            direct.offset(frame, window.window, geometry),
        ) else {
            continue;
        };
        let view = gui_rect(viewport.clip()).intersect(content);
        if !view.is_positive() {
            continue;
        }
        let painter = painter.with_clip_rect(view);
        // Fill the stationary split once, including empty sampled margin rows.
        // Glyph rectangles retain their full height while only clipping changes.
        painter.rect_filled(view, 0.0, normal_bg);
        for projected in viewport.runs() {
            let (_, bg, _) = colors(&projected.run.style, &frame.default_style);
            painter.rect_filled(gui_rect(projected.rect), 0.0, bg);
        }
        for projected in viewport.runs() {
            paint_run(
                &painter,
                projected.run,
                &frame.default_style,
                gui_rect(projected.rect),
                view,
                cell_width,
                line_height,
                font_size,
            );
        }
        if window.window == frame.window && (!focused || cursor_on) {
            if let Some(cursor) = viewport.cursor_rect().map(gui_rect) {
                if !paint_native_cursor(
                    &painter,
                    frame,
                    cursor,
                    geometry,
                    font_size,
                    focused,
                    viewport.runs().map(|p| (p.run, gui_rect(p.rect))),
                ) {
                    paint_caret(&painter, cursor, &frame.mode, focused, normal_fg);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_native_cursor<'a>(
    painter: &egui::Painter,
    frame: &Vim9RenderFrame,
    cell: Rect,
    geometry: Vim9RenderMetrics,
    font_size: f32,
    focused: bool,
    runs: impl Iterator<Item = (&'a jterm_core::vim9::Vim9RenderRun, Rect)>,
) -> bool {
    let Some(appearance) = &frame.cursor.appearance else {
        return false;
    };
    let runs: Vec<_> = runs.filter(|(_, rect)| rect.intersects(cell)).collect();
    let under = runs
        .iter()
        .find(|(_, rect)| rect.contains(cell.center()))
        .map_or(&frame.default_style, |(run, _)| &run.style);
    let (fg, bg) = appearance.colors(under, &frame.default_style);
    if !focused {
        painter.rect_stroke(
            cell.shrink(0.5),
            0.0,
            Stroke::new(1.0, rgb(bg)),
            egui::StrokeKind::Inside,
        );
        return true;
    }
    let caret = gui_rect(appearance.paint_rect(
        Vim9PixelRect {
            x: cell.left(),
            y: cell.top(),
            width: cell.width(),
            height: cell.height(),
        },
        geometry.cell_width,
        painter.ctx().pixels_per_point(),
    ));
    painter.rect_filled(caret, 0.0, rgb(bg));
    for (run, rect) in runs {
        let mut ink = run.clone();
        ink.style.foreground = Some(fg);
        ink.style.background = Some(bg);
        ink.style.special = Some(fg);
        ink.style.reverse = false;
        paint_run(
            painter,
            &ink,
            &frame.default_style,
            rect,
            caret,
            geometry.cell_width,
            geometry.line_height,
            font_size,
        );
    }
    true
}

fn paint_caret(painter: &egui::Painter, cursor: Rect, mode: &str, focused: bool, color: Color32) {
    if focused && mode.starts_with('i') {
        let pixel = painter.ctx().pixels_per_point().max(0.1).recip();
        let width = (2.0 / pixel).round().max(1.0) * pixel;
        painter.rect_filled(
            Rect::from_min_size(
                cursor.min,
                egui::vec2(width.min(cursor.width()), cursor.height()),
            ),
            0.0,
            color,
        );
    } else if focused && mode.starts_with('R') {
        let height = (cursor.height() * 0.2).max(2.0).min(cursor.height());
        painter.rect_filled(
            Rect::from_min_max(
                egui::pos2(cursor.left(), cursor.bottom() - height),
                cursor.max,
            ),
            0.0,
            color,
        );
    } else {
        if focused {
            painter.rect_filled(cursor, 1.0, color.gamma_multiply(0.22));
        }
        painter.rect_stroke(
            cursor.shrink(0.5),
            1.0,
            Stroke::new(1.0, color),
            egui::StrokeKind::Inside,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_view(
    painter: &egui::Painter,
    frame: &Vim9RenderFrame,
    logical_view: &Vim9RenderRect,
    content: Rect,
    cell_width: f32,
    line_height: f32,
    font_size: f32,
    focused: bool,
    cursor_on: bool,
    offset_y: f32,
    draw_cursor: bool,
) {
    let mut geometry = metrics(content, cell_width, line_height);
    let Some(view) = geometry.rect(logical_view).map(gui_rect) else {
        return;
    };
    let view = view.intersect(content);
    if !view.is_positive() {
        return;
    }
    let painter = painter.with_clip_rect(view);
    geometry.origin_y += offset_y;
    let (normal_fg, normal_bg, _) = colors(&frame.default_style, &frame.default_style);
    painter.rect_filled(view.translate(egui::vec2(0.0, offset_y)), 0.0, normal_bg);
    // Backgrounds precede every glyph so a later style run cannot erase the
    // preceding italic glyph's natural overhang.
    for run in &frame.runs {
        if logical_view.contains(run.row, run.column) && run.cells > 0 {
            let (_, bg, _) = colors(&run.style, &frame.default_style);
            if let Some(rect) = geometry.cell_rect(run.row, run.column, run.cells) {
                painter.rect_filled(gui_rect(rect), 0.0, bg);
            }
        }
    }
    for run in &frame.runs {
        if !logical_view.contains(run.row, run.column) || run.cells == 0 {
            continue;
        }
        let Some(run_rect) = geometry
            .cell_rect(run.row, run.column, run.cells)
            .map(gui_rect)
        else {
            continue;
        };
        paint_run(
            &painter,
            run,
            &frame.default_style,
            run_rect,
            view,
            cell_width,
            line_height,
            font_size,
        );
    }
    if draw_cursor
        && frame.cursor.visible
        && logical_view.contains(frame.cursor.row, frame.cursor.column)
        && (!focused || cursor_on)
    {
        let Some(cursor) = geometry.cursor_rect(&frame.cursor).map(gui_rect) else {
            return;
        };
        if paint_native_cursor(
            &painter,
            frame,
            cursor,
            geometry,
            font_size,
            focused,
            frame
                .runs
                .iter()
                .filter(|r| logical_view.contains(r.row, r.column))
                .filter_map(|r| {
                    geometry
                        .cell_rect(r.row, r.column, r.cells)
                        .map(|rect| (r, gui_rect(rect)))
                }),
        ) {
            return;
        }
        paint_caret(&painter, cursor, &frame.mode, focused, normal_fg);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_run(
    painter: &egui::Painter,
    run: &jterm_core::vim9::Vim9RenderRun,
    normal: &Vim9RenderStyle,
    run_rect: Rect,
    view: Rect,
    cell_width: f32,
    line_height: f32,
    font_size: f32,
) {
    let (fg, _, special) = colors(&run.style, normal);
    let clip = run_rect
        .expand2(egui::vec2(font_size * 0.25, 0.0))
        .intersect(view);
    let run_painter = painter.with_clip_rect(clip);
    let font = if run.style.bold {
        FontId::new(font_size, egui::FontFamily::Name("vim9-buffer-bold".into()))
    } else {
        FontId::monospace(font_size)
    };
    let format = egui::TextFormat {
        font_id: font,
        color: fg,
        italics: run.style.italic,
        ..Default::default()
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(&run.text, 0.0, format.clone());
    let galley = run_painter.layout_job(job);
    // A configured or bold fallback face can be proportional even though
    // the family is named Monospace. Keep Vim's cursor/hit testing aligned
    // instead of letting the shaped advances accumulate across that run.
    let place_ascii_cells = run.text.is_ascii()
        && run.text.len() == run.cells
        && !ascii_columns_match(&galley, run.cells, cell_width);
    if place_ascii_cells {
        for (column, ch) in run.text.chars().enumerate() {
            let mut job = egui::text::LayoutJob::default();
            let mut utf8 = [0; 4];
            job.append(ch.encode_utf8(&mut utf8), 0.0, format.clone());
            let glyph = run_painter.layout_job(job);
            let y = run_rect.top() + (line_height - glyph.size().y) * 0.5;
            run_painter.galley(
                egui::pos2(run_rect.left() + column as f32 * cell_width, y),
                glyph,
                fg,
            );
        }
    } else {
        let text_y = run_rect.top() + (line_height - galley.size().y) * 0.5;
        run_painter.galley(egui::pos2(run_rect.left(), text_y), galley, fg);
    }
    let pixel = painter.ctx().pixels_per_point().max(0.1).recip();
    let thickness = ((font_size / 14.0).clamp(1.0, 2.0) / pixel).round() * pixel;
    let underline_y = run_rect.bottom() - thickness * 1.5;
    if run.style.undercurl {
        let points = undercurl_points(run_rect, underline_y, thickness, pixel);
        run_painter.add(egui::Shape::line(points, Stroke::new(thickness, special)));
    } else if run.style.underline {
        run_painter.line_segment(
            [
                egui::pos2(run_rect.left(), underline_y),
                egui::pos2(run_rect.right(), underline_y),
            ],
            Stroke::new(thickness, special),
        );
    }
    if run.style.strikethrough {
        run_painter.line_segment(
            [
                egui::pos2(run_rect.left(), run_rect.center().y),
                egui::pos2(run_rect.right(), run_rect.center().y),
            ],
            Stroke::new(thickness, special),
        );
    }
}

/// Continuous screen-space phase keeps adjoining syntax runs on the same
/// smooth wave; sampling follows physical pixels instead of a jagged zigzag.
fn undercurl_points(rect: Rect, baseline: f32, thickness: f32, pixel: f32) -> Vec<egui::Pos2> {
    let wavelength = (thickness * 6.0).max(4.0 * pixel);
    let count = (rect.width() / pixel.max(0.5)).ceil().clamp(1.0, 16_384.0) as usize;
    (0..=count)
        .map(|index| {
            let x = rect.left() + rect.width() * index as f32 / count as f32;
            let phase = x / wavelength * std::f32::consts::TAU;
            egui::pos2(x, baseline - thickness * (0.75 + phase.sin() * 0.75))
        })
        .collect()
}

fn ascii_columns_match(galley: &egui::Galley, cells: usize, cell_width: f32) -> bool {
    galley.rows.len() == 1
        && galley.rows[0].glyphs.len() == cells
        && galley.rows[0]
            .glyphs
            .iter()
            .enumerate()
            .all(|(column, glyph)| (glyph.pos.x - column as f32 * cell_width).abs() <= 0.75)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct_frame(top: u64) -> Arc<Vim9RenderFrame> {
        use jterm_core::vim9::{
            Vim9RenderCursor, Vim9RenderMarginRow, Vim9RenderRun, Vim9RenderWindow,
        };
        let view = Vim9RenderRect {
            row: 0,
            column: 0,
            rows: 4,
            columns: 10,
        };
        let cursor = Vim9RenderCursor {
            appearance: None,
            row: 1,
            column: 1,
            width: 1,
            visible: true,
            line: top + 1,
            byte_column: 1,
        };
        let primary = Vim9RenderWindow {
            window: 1,
            buffer: 1,
            changedtick: 3,
            view: view.clone(),
            native_pointer: true,
            topline: top,
            botline: top + 3,
            line_count: 100,
            line_lengths: vec![4; 4],
            cursor: cursor.clone(),
            overscan: vec![
                Vim9RenderMarginRow {
                    row: 4,
                    line: top + 4,
                    byte_length: 4,
                    runs: vec![Vim9RenderRun {
                        window: 1,
                        column: 0,
                        text: "edge".into(),
                        cells: 4,
                        line: top + 4,
                        byte_start: 0,
                        byte_end: 4,
                        style: Vim9RenderStyle {
                            foreground: Some(0x19c3a7),
                            background: Some(0x123456),
                            ..Default::default()
                        },
                        ..Default::default()
                    }],
                },
                Vim9RenderMarginRow {
                    row: 5,
                    line: top + 5,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let secondary = Vim9RenderWindow {
            window: 2,
            buffer: 2,
            changedtick: 1,
            view: Vim9RenderRect {
                column: 11,
                ..view.clone()
            },
            topline: 1,
            botline: 4,
            line_count: 20,
            line_lengths: vec![4; 4],
            native_pointer: true,
            ..Default::default()
        };
        Arc::new(Vim9RenderFrame {
            revision: top,
            supported: true,
            rows: 6,
            columns: 22,
            mode: "i".into(),
            window: 1,
            buffer: 1,
            changedtick: 3,
            default_style: Vim9RenderStyle {
                foreground: Some(0xddeeff),
                background: Some(0x101820),
                ..Default::default()
            },
            view,
            cursor,
            windows: vec![primary, secondary],
            runs: (0..4)
                .map(|row| Vim9RenderRun {
                    window: 1,
                    row,
                    column: 0,
                    text: "ABCD".into(),
                    cells: 4,
                    line: top + row as u64,
                    byte_start: 0,
                    byte_end: 4,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
    }

    fn direct_fixture(
        frame: &Arc<Vim9RenderFrame>,
        geometry: Vim9RenderMetrics,
        lines: f64,
    ) -> DirectInput {
        let mut direct = DirectInput::default();
        let mut scroll = Vim9PixelScroll::new(&frame.windows[0], geometry).unwrap();
        scroll.push(lines);
        direct.scrolls.insert(1, scroll);
        direct.remember_presentation(Arc::clone(frame), geometry, &egui::Context::default());
        direct
    }

    #[test]
    fn completion_cancels_buffer_motion_and_paints_above_every_split() {
        let mut frame = direct_frame(10);
        let content = Rect::from_min_size(egui::pos2(7.25, 12.5), egui::vec2(220.0, 120.0));
        let geometry = metrics(content, 10.0, 20.0);
        let mut direct = direct_fixture(&frame, geometry, 0.3125);
        assert_ne!(direct.offset(&frame, 1, geometry), 0.0);
        Arc::make_mut(&mut frame).completion = Some(
            serde_json::from_value(serde_json::json!({
                "view": {"row":1,"column":8,"rows":2,"columns":10},
                "default_style": {"foreground":0x19c3a7,"background":0x294761},
                "runs": [
                    {"row":1,"column":8,"cells":10,"text":"          ",
                     "style":{"background":0x8351a7}},
                    {"row":1,"column":8,"cells":4,"text":"menu"},
                    {"row":2,"column":8,"cells":4,"text":"中文"}
                ]
            }))
            .unwrap(),
        );
        direct.reconcile_with(&frame, geometry, |_| false);
        assert_eq!(direct.offset(&frame, 1, geometry), 0.0);
        assert!(direct
            .wheel_window_at(&frame, geometry, content.center())
            .is_none());
        let popup = Rect::from_min_size(
            content.min + egui::vec2(80.0, 20.0),
            egui::vec2(100.0, 40.0),
        );
        for direct_path in [false, true] {
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(Default::default(), |ui| {
                paint(
                    &ui.ctx().layer_painter(egui::LayerId::background()),
                    &frame,
                    content,
                    10.0,
                    20.0,
                    14.0,
                    true,
                    true,
                    &ScrollPresentation::default(),
                    0.0,
                    direct_path.then_some(&direct),
                );
            });
            output.textures_delta.clear();
            let popup_index = output
                .shapes
                .iter()
                .position(|clipped| {
                    clipped.clip_rect == popup
                        && matches!(&clipped.shape, egui::Shape::Rect(shape)
                        if shape.rect == popup && shape.fill == rgb(0x294761))
                })
                .expect("completion must span both splits and their separator");
            let buffer_indices = output
                .shapes
                .iter()
                .enumerate()
                .filter_map(|(index, clipped)| {
                    matches!(&clipped.shape, egui::Shape::Rect(shape)
                    if shape.fill == rgb(0x101820))
                    .then_some(index)
                })
                .collect::<Vec<_>>();
            assert!(!buffer_indices.is_empty());
            assert!(buffer_indices.into_iter().all(|index| index < popup_index));
            assert!(
                output.shapes.iter().any(|clipped| {
                    clipped.clip_rect == popup
                        && matches!(&clipped.shape, egui::Shape::Rect(shape)
                        if shape.fill == rgb(0x8351a7))
                }),
                "selected menu row must retain its GUI background"
            );
            let shadow_index = output
                .shapes
                .iter()
                .position(|clipped| {
                    clipped.clip_rect == content
                        && matches!(&clipped.shape, egui::Shape::Rect(shape)
                        if shape.rect == popup.expand(4.0)
                            && shape.fill == Color32::from_black_alpha(4))
                })
                .expect("popup elevation must use the full editor clip");
            assert!(shadow_index < popup_index);
            assert!(output.shapes[..shadow_index].iter().any(|clipped| {
                matches!(&clipped.shape, egui::Shape::Rect(shape)
                    if shape.fill == rgb(0x101820))
            }));
            let border_index = output
                .shapes
                .iter()
                .position(|clipped| {
                    matches!(&clipped.shape, egui::Shape::Rect(shape)
                    if shape.rect == popup && shape.stroke.color == rgb(0x19c3a7).gamma_multiply(0.20))
                })
                .expect("the menu has a restrained outline");
            assert!(output.shapes[popup_index..border_index]
                .iter()
                .any(|clipped| matches!(&clipped.shape, egui::Shape::Text(_))));
        }
    }

    #[test]
    fn completion_elevation_preserves_clipped_menu_geometry_at_high_dpi() {
        for scale in [1.0, 1.25, 2.0] {
            let mut frame = (*direct_frame(10)).clone();
            frame.completion = Some(
                serde_json::from_value(serde_json::json!({
                    "view": {"row":5,"column":20,"rows":2,"columns":4},
                    "default_style": {"foreground":0x19c3a7,"background":0x294761},
                    "runs": [{"row":5,"column":20,"cells":4,"text":"menu"}]
                }))
                .unwrap(),
            );
            // Model an editor clipped midway through a menu's right/bottom
            // edge, as can happen while its container is being resized.
            let content = Rect::from_min_size(egui::pos2(7.25, 12.5), egui::vec2(220.0, 120.0));
            let popup = Rect::from_min_size(
                content.min + egui::vec2(200.0, 100.0),
                egui::vec2(40.0, 40.0),
            );
            let clip = popup.intersect(content);
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(scale);
            let mut output = ctx.run_ui(Default::default(), |ui| {
                assert_eq!(ui.ctx().pixels_per_point(), scale);
                paint_completion(
                    &ui.ctx().layer_painter(egui::LayerId::background()),
                    &frame,
                    content,
                    10.0,
                    20.0,
                    14.0,
                );
            });
            output.textures_delta.clear();
            assert!(output.shapes.iter().any(|clipped| {
                clipped.clip_rect == clip
                    && matches!(&clipped.shape, egui::Shape::Rect(shape)
                    if shape.rect == popup && shape.fill == rgb(0x294761))
            }));
            for (spread, alpha) in [(4.0, 4), (3.0, 5), (2.0, 7), (1.0, 10)] {
                assert!(output.shapes.iter().any(|clipped| {
                    clipped.clip_rect == content
                        && matches!(&clipped.shape, egui::Shape::Rect(shape)
                        if shape.rect == popup.expand(spread)
                            && shape.fill == Color32::from_black_alpha(alpha))
                }));
            }
            assert!(
                output.shapes.iter().any(|clipped| {
                    clipped.clip_rect == clip
                        && matches!(&clipped.shape, egui::Shape::Rect(shape)
                    if shape.rect == popup
                        && shape.stroke_kind == egui::StrokeKind::Inside
                        && shape.stroke.width == 1.0 / scale
                        && shape.stroke.color == rgb(0x19c3a7).gamma_multiply(0.20))
                }),
                "clipping must not move the outline to a false menu edge"
            );
            let text: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(shape) => Some(shape),
                    _ => None,
                })
                .collect();
            assert_eq!(text[0].pos.x, popup.left());
            assert_eq!(
                text.iter()
                    .map(|shape| shape.galley.job.text.as_str())
                    .collect::<String>(),
                "menu"
            );
            assert!(text.iter().all(|shape| shape
                .galley
                .job
                .sections
                .iter()
                .all(|section| section.format.color == rgb(0x19c3a7))));
        }
    }

    #[test]
    fn direct_projection_paints_margin_and_ime_at_the_same_fraction() {
        let frame = direct_frame(10);
        let content = Rect::from_min_size(egui::pos2(7.25, 12.5), egui::vec2(220.0, 120.0));
        let geometry = metrics(content, 10.0, 20.0);
        let direct = direct_fixture(&frame, geometry, 0.3125);
        assert_eq!(direct.offset(&frame, 1, geometry), -6.25);
        assert_eq!(direct.offset(&frame, 2, geometry), 0.0);
        let ime = direct_cursor_rect(&frame, content, 10.0, 20.0, &direct).unwrap();
        assert_eq!(ime.min, content.min + egui::vec2(10.0, 13.75));
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            paint(
                &ui.ctx().layer_painter(egui::LayerId::background()),
                &frame,
                content,
                10.0,
                20.0,
                14.0,
                true,
                true,
                &ScrollPresentation::default(),
                0.0,
                Some(&direct),
            );
        });
        output.textures_delta.clear();
        let view = gui_rect(geometry.rect(&frame.windows[0].view).unwrap());
        assert!(
            output.shapes.iter().any(|clipped| matches!(&clipped.shape,
            egui::Shape::Rect(shape) if shape.fill == rgb(0x123456)
                && shape.rect.top() == view.bottom() - 6.25 && clipped.clip_rect == view)),
            "the sampled bottom edge must paint inside the owning split"
        );
        assert!(
            output.shapes.iter().any(|clipped| matches!(&clipped.shape,
            egui::Shape::Text(shape) if shape.galley.job.sections.iter().any(|section| section.format.color == rgb(0x19c3a7))
                && clipped.clip_rect.bottom() == view.bottom())),
            "fractional input must show real margin glyphs before a whole-line command"
        );
        assert!(
            output.shapes.iter().any(|clipped| matches!(&clipped.shape,
            egui::Shape::Rect(shape) if shape.fill == rgb(0xddeeff) && shape.rect.min == ime.min)),
            "the candidate anchor and painted caret must agree exactly"
        );
    }

    #[test]
    fn source_hit_keeps_the_presented_image_across_a_newer_acknowledgement() {
        let old = direct_frame(10);
        let content = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(220.0, 120.0));
        let geometry = metrics(content, 10.0, 20.0);
        let mut direct = direct_fixture(&old, geometry, 1.5);
        // The live scroll transaction can settle before egui paints again.
        direct.scrolls.clear();
        let newer = direct_frame(11);
        let anchor = direct
            .presented_anchor(&newer, geometry, egui::pos2(15.0, 15.0))
            .unwrap();
        assert_eq!((anchor.position.line, anchor.position.byte_column), (12, 1));
        assert_eq!(
            direct.wheel_window_at(&newer, geometry, egui::pos2(15.0, 15.0)),
            Some(1)
        );
        assert!(
            direct
                .presented_anchor(&newer, geometry, egui::pos2(105.0, 15.0))
                .is_none(),
            "split gutter is not text"
        );
        let mut changed = (*newer).clone();
        changed.windows[0].changedtick += 1;
        assert!(
            direct
                .wheel_window_at(&changed, geometry, egui::pos2(15.0, 15.0))
                .is_none(),
            "old-image wheel events cannot target an unseen buffer generation"
        );
        assert!(direct
            .presented_anchor(&changed, geometry, egui::pos2(15.0, 15.0))
            .is_none());
        assert!(direct.stale_presentation_at(&changed, geometry, egui::pos2(15.0, 15.0)));
        changed.windows[0].changedtick -= 1;
        changed.windows[0].native_pointer = false;
        assert!(
            !direct.stale_presentation_at(&changed, geometry, egui::pos2(15.0, 15.0)),
            "unchanged sources with mouse mappings keep their legacy wheel route"
        );
        assert!(direct
            .presented_anchor(
                &newer,
                Vim9RenderMetrics {
                    cell_width: 11.0,
                    ..geometry
                },
                egui::pos2(15.0, 15.0)
            )
            .is_none());
    }

    #[test]
    fn acknowledgement_between_input_and_paint_rebases_without_a_one_frame_jump() {
        let old = direct_frame(10);
        let geometry = Vim9RenderMetrics {
            origin_x: 0.0,
            origin_y: 0.0,
            cell_width: 8.0,
            line_height: 20.0,
        };
        let mut direct = direct_fixture(&old, geometry, 1.5);
        direct.scrolls.get_mut(&1).unwrap().submitted(7);
        let mut acknowledged = (*direct_frame(11)).clone();
        acknowledged.windows[0].scroll_ack = 7;
        direct.reconcile_with(&acknowledged, geometry, |_| true);
        assert_eq!(
            direct.offset(&acknowledged, 1, geometry),
            -10.0,
            "the renderer must apply the remaining half line in the first acknowledged frame"
        );
        assert_eq!(direct.scrolls[&1].request_id(), None);
        assert_eq!(
            direct
                .presented_anchor(&acknowledged, geometry, egui::pos2(9.0, 15.0))
                .unwrap()
                .position
                .line,
            12,
            "input still addresses the old submitted image until it is replaced"
        );
    }

    #[test]
    fn cancelled_native_drag_owns_its_release_without_replaying_a_target() {
        let mut direct = DirectInput {
            pointer: Some(NativePointer {
                gesture: 1,
                request: 7,
                target: Vim9BufferPosition {
                    window: 1,
                    buffer: 1,
                    changedtick: 3,
                    line: 10,
                    byte_column: 1,
                },
                metrics: Vim9RenderMetrics {
                    origin_x: 0.0,
                    origin_y: 0.0,
                    cell_width: 8.0,
                    line_height: 20.0,
                },
                cancelled: false,
                queued: None,
                released: false,
            }),
            ..Default::default()
        };
        direct.cancel();
        assert!(direct.finish_cancelled_pointer(true));
        assert!(direct.pointer.as_ref().unwrap().cancelled);
        assert!(direct.finish_cancelled_pointer(false));
        assert!(!direct.pointer_captured());
    }

    #[test]
    fn settling_an_unsupported_fractional_press_owns_its_physical_tail() {
        let frame = direct_frame(10);
        let geometry = Vim9RenderMetrics {
            origin_x: 0.0,
            origin_y: 0.0,
            cell_width: 8.0,
            line_height: 20.0,
        };
        let mut direct = direct_fixture(&frame, geometry, 0.3125);
        assert!(direct.presented_fraction_at(egui::pos2(9.0, 15.0)));
        assert!(!direct.presented_fraction_at(egui::pos2(105.0, 15.0)));
        direct.discard_press();
        assert!(direct.discarding_pointer());
        assert!(direct.finish_cancelled_pointer(true));
        assert!(direct.finish_cancelled_pointer(false));
        assert!(!direct.pointer_captured());
        assert!(direct.scrolls.is_empty());
        assert!(direct.presentation_retired);
        assert!(direct.presented_fraction_at(egui::pos2(9.0, 15.0)));
        direct.remember_presentation(frame, geometry, &egui::Context::default());
        assert!(!direct.presented_fraction_at(egui::pos2(9.0, 15.0)));
        assert!(!direct.presentation_retired);
    }

    #[test]
    fn replacing_a_fractional_editor_drops_new_press_ownership_but_keeps_captured_release() {
        let frame = direct_frame(10);
        let content = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(220.0, 120.0));
        let geometry = metrics(content, 10.0, 20.0);
        let point = egui::pos2(15.0, 15.0);
        let ctx = egui::Context::default();
        let draw = |input: &mut DirectInput| {
            let mut output = ctx.run_ui(Default::default(), |ui| {
                paint(
                    &ui.ctx().layer_painter(egui::LayerId::background()),
                    &frame,
                    content,
                    10.0,
                    20.0,
                    14.0,
                    true,
                    true,
                    &ScrollPresentation::default(),
                    0.0,
                    Some(input),
                );
                input.remember_presentation(Arc::clone(&frame), geometry, ui.ctx());
            });
            output.textures_delta.clear();
        };
        let mut previous = direct_fixture(&frame, geometry, 0.3125);
        let mut replacement = DirectInput::default();
        draw(&mut previous);
        previous.cancel();
        previous.retire_unpainted(&ctx);
        assert!(
            previous.presented_fraction_at(point),
            "switching owners cannot forget the old image before replacement paint"
        );
        draw(&mut replacement);
        previous.retire_unpainted(&ctx);
        replacement.retire_unpainted(&ctx);
        assert!(previous.presented.is_none());
        assert!(!previous.presented_fraction_at(point));
        assert!(!replacement.presented_fraction_at(point));
        assert!(
            !previous.finish_cancelled_pointer(true),
            "a hidden fractional editor must not swallow the new editor's ordinary press"
        );

        // A removed pane has the same expiration rule, but a press it already
        // captured must still consume its eventual physical release.
        let mut captured = direct_fixture(&frame, geometry, 0.3125);
        draw(&mut captured);
        captured.discard_press();
        draw(&mut replacement);
        captured.retire_unpainted(&ctx);
        assert!(captured.presented.is_none());
        assert!(captured.finish_cancelled_pointer(true));
        assert!(captured.finish_cancelled_pointer(false));
        assert!(!captured.pointer_captured());
    }

    #[test]
    fn disabling_smooth_motion_retires_the_fraction_and_presented_source() {
        let frame = direct_frame(10);
        let geometry = Vim9RenderMetrics {
            origin_x: 0.0,
            origin_y: 0.0,
            cell_width: 8.0,
            line_height: 20.0,
        };
        let mut direct = direct_fixture(&frame, geometry, 0.5);
        direct.set_smooth(false);
        assert_eq!(direct.offset(&frame, 1, geometry), 0.0);
        assert!(direct.presentation_retired);
        assert!(direct.presented.is_some());
        assert!(direct.scrolls.is_empty());
    }

    fn scroll_frame(revision: u64, top: u64) -> Arc<Vim9RenderFrame> {
        Arc::new(
            serde_json::from_value(serde_json::json!({
                "revision": revision, "supported": true, "rows": 10, "columns": 40,
                "window": 1, "buffer": 1, "mode": "n",
                "view": {"row": 0, "column": 0, "rows": 9, "columns": 19},
                "windows": [
                    {"window": 1, "buffer": 1, "topline": top,
                     "view": {"row": 0, "column": 0, "rows": 9, "columns": 19}},
                    {"window": 2, "buffer": 2, "topline": 1,
                     "view": {"row": 0, "column": 20, "rows": 9, "columns": 20}}
                ],
                "runs": [{"window": 1, "row": 0, "column": 0, "text": "text", "cells": 4}]
            }))
            .unwrap(),
        )
    }

    #[test]
    fn acknowledged_scroll_eases_only_owning_split_and_resets_on_direct_input() {
        let identity = Some(uuid::Uuid::new_v4());
        let mut presentation = ScrollPresentation::default();
        presentation.update(identity, Some(&scroll_frame(1, 1)), 20.0, 1.0, true, false);
        assert!(!presentation.active(1.0));
        presentation.update(identity, Some(&scroll_frame(2, 4)), 20.0, 1.02, true, false);
        assert_eq!(presentation.offset(1, 1.02), 60.0);
        assert_eq!(presentation.offset(2, 1.02), 0.0);
        assert!(
            presentation.windows.contains_key(&1),
            "old edge rows remain until settling"
        );
        assert!(presentation.offset(1, 1.06) > 0.0);
        assert!(presentation.offset(1, 1.06) < 60.0);
        assert_eq!(presentation.offset(1, 1.13), 0.0);

        presentation.update(identity, Some(&scroll_frame(3, 7)), 20.0, 1.14, true, false);
        assert!(presentation.active(1.14));
        presentation.update(identity, Some(&scroll_frame(3, 7)), 20.0, 1.15, true, true);
        assert!(
            !presentation.active(1.15),
            "clicks and keys use settled Vim cells"
        );
        presentation.update(identity, Some(&scroll_frame(4, 8)), 20.0, 1.16, true, false);
        assert!(
            !presentation.active(1.16),
            "delayed keyboard acknowledgement must not reanimate"
        );
    }

    #[test]
    fn scroll_never_crosses_editor_identity_resize_fallback_or_disabled_motion() {
        for action in 0..5 {
            let identity = Some(uuid::Uuid::new_v4());
            let mut presentation = ScrollPresentation::default();
            let next = scroll_frame(2, 4);
            presentation.update(identity, Some(&scroll_frame(1, 1)), 20.0, 1.0, true, false);
            presentation.update(identity, Some(&next), 20.0, 1.01, true, false);
            assert!(presentation.active(1.01));
            match action {
                0 => presentation.update(
                    Some(uuid::Uuid::new_v4()),
                    Some(&next),
                    20.0,
                    1.02,
                    true,
                    false,
                ),
                1 => presentation.update(identity, Some(&next), 21.0, 1.02, true, false),
                2 => presentation.update(identity, None, 20.0, 1.02, true, false),
                3 => presentation.update(identity, Some(&next), 20.0, 1.02, false, false),
                _ => {
                    let mut edited = (*next).clone();
                    edited.revision = 3;
                    edited.windows[0].changedtick += 1;
                    presentation.update(identity, Some(&Arc::new(edited)), 20.0, 1.02, true, false);
                }
            }
            assert!(!presentation.active(1.02), "reset case {action}");
        }
    }

    #[test]
    fn staggered_split_scrolls_keep_their_own_recent_edge_rows() {
        let identity = Some(uuid::Uuid::new_v4());
        let frame = |revision, left, right| {
            let mut frame = (*scroll_frame(revision, left)).clone();
            frame.windows[1].topline = right;
            Arc::new(frame)
        };
        let mut presentation = ScrollPresentation::default();
        presentation.update(identity, Some(&frame(1, 1, 1)), 20.0, 1.0, true, false);
        presentation.update(identity, Some(&frame(2, 1, 2)), 20.0, 1.01, true, false);
        presentation.update(identity, Some(&frame(3, 2, 2)), 20.0, 1.08, true, false);
        // The right animation finishes while the left is still settling.
        // Restarting the right split must retain its current top line, not
        // the global frame from before its already completed first scroll.
        presentation.update(identity, Some(&frame(4, 2, 3)), 20.0, 1.12, true, false);
        assert_eq!(presentation.windows[&2].sources.front().unwrap().1, 2);
        assert!(presentation.offset(1, 1.12) > 0.0);
        assert_eq!(presentation.offset(2, 1.12), 20.0);
    }

    #[test]
    fn sustained_scroll_rebases_edge_without_a_periodic_viewport_snap() {
        let identity = Some(uuid::Uuid::new_v4());
        let mut presentation = ScrollPresentation::default();
        presentation.update(identity, Some(&scroll_frame(1, 1)), 20.0, 1.0, true, false);
        for step in 1..=30 {
            let now = 1.0 + step as f64 * 0.016;
            let before = presentation.offset(1, now);
            let frame = scroll_frame(step + 1, step * 3 + 1);
            presentation.update(identity, Some(&frame), 20.0, now, true, false);
            assert!(presentation.active(now), "gesture reset at step {step}");
            let after = presentation.offset(1, now);
            assert!(
                (after - before - 60.0).abs() < 0.001,
                "step {step}: {before} -> {after}"
            );
            assert!(after < 180.0, "visible displacement fits the viewport");
            assert!(presentation.windows[&1].sources.len() <= 33);
        }
        assert!(
            presentation.windows[&1].sources.front().unwrap().1 > 1,
            "outgoing source follows a long gesture instead of keeping its first frame"
        );
    }

    #[test]
    fn precision_wheel_preserves_sub_line_motion_and_shortcuts() {
        let event = |unit, dy, modifiers| egui::Event::MouseWheel {
            unit,
            delta: egui::vec2(0.0, dy),
            modifiers,
            phase: egui::TouchPhase::Move,
        };
        let pixel = event(egui::MouseWheelUnit::Point, -8.0, egui::Modifiers::NONE);
        let mut accumulator = jterm_core::vim9::Vim9ScrollAccumulator::default();
        assert_eq!(
            accumulator.add(1, wheel_lines(&pixel, 20.0, 30).unwrap()),
            0
        );
        assert_eq!(
            accumulator.add(1, wheel_lines(&pixel, 20.0, 30).unwrap()),
            0
        );
        assert_eq!(
            accumulator.add(1, wheel_lines(&pixel, 20.0, 30).unwrap()),
            1
        );
        assert_eq!(
            wheel_lines(
                &event(egui::MouseWheelUnit::Line, -1.0, egui::Modifiers::NONE),
                20.0,
                30
            ),
            Some(3.0)
        );
        assert_eq!(
            wheel_lines(
                &event(egui::MouseWheelUnit::Page, 1.0, egui::Modifiers::NONE),
                20.0,
                30
            ),
            Some(-30.0)
        );
        assert!(wheel_lines(
            &event(egui::MouseWheelUnit::Line, -1.0, egui::Modifiers::CTRL),
            20.0,
            30
        )
        .is_none());
        assert!(wheel_lines(&pixel, f32::NAN, 30).is_none());
    }

    #[test]
    fn animated_paint_keeps_edge_background_clipped_and_ime_follows_caret() {
        let identity = Some(uuid::Uuid::new_v4());
        let previous = scroll_frame(1, 1);
        let mut current = (*scroll_frame(2, 4)).clone();
        current.cursor = jterm_core::vim9::Vim9RenderCursor {
            row: 1,
            column: 1,
            width: 1,
            visible: true,
            ..Default::default()
        };
        let current = Arc::new(current);
        let mut presentation = ScrollPresentation::default();
        presentation.update(identity, Some(&previous), 20.0, 1.0, true, false);
        presentation.update(identity, Some(&current), 20.0, 1.01, true, false);
        let now = 1.06;
        let offset = presentation.offset(1, now);
        let content = Rect::from_min_size(egui::pos2(10.0, 15.0), egui::vec2(400.0, 200.0));
        let caret = cursor_rect(&current, content, 10.0, 20.0, offset).unwrap();
        assert_eq!(caret.top(), content.top() + 20.0 + offset);
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            paint(
                ui.painter(),
                &current,
                content,
                10.0,
                20.0,
                14.0,
                true,
                true,
                &presentation,
                now,
                None,
            );
        });
        output.textures_delta.clear();
        let left = Rect::from_min_size(content.min, egui::vec2(190.0, 180.0));
        assert!(
            output
                .shapes
                .iter()
                .filter(|shape| shape.clip_rect == left)
                .count()
                >= 4
        );
        assert!(output.shapes.iter().any(|clipped| matches!(&clipped.shape,
            egui::Shape::Rect(shape) if shape.rect.top() < left.top() && clipped.clip_rect == left)),
            "retained old rows fill the exposed top edge");
    }

    #[test]
    fn undercurl_phase_is_continuous_at_high_dpi_style_boundaries() {
        let left = Rect::from_min_max(egui::pos2(10.0, 0.0), egui::pos2(23.25, 20.0));
        let right = Rect::from_min_max(egui::pos2(23.25, 0.0), egui::pos2(50.0, 20.0));
        let a = undercurl_points(left, 18.0, 1.0, 0.5);
        let b = undercurl_points(right, 18.0, 1.0, 0.5);
        assert_eq!(a.last(), b.first());
        assert!(a.windows(2).all(|pair| (pair[1].y - pair[0].y).abs() < 0.5));
        assert!(a.iter().all(|point| point.y >= 16.5 && point.y <= 18.0));
    }

    #[test]
    fn gui_rgb_is_exact_and_reverse_swaps_resolved_colors() {
        let normal = Vim9RenderStyle {
            foreground: Some(0x142536),
            background: Some(0x102030),
            special: Some(0x8a9bac),
            ..Default::default()
        };
        let mut style = Vim9RenderStyle {
            foreground: Some(0x19c3a7),
            special: Some(0xe83967),
            ..Default::default()
        };
        assert_eq!(
            colors(&style, &normal),
            (rgb(0x19c3a7), rgb(0x102030), rgb(0xe83967))
        );
        style.reverse = true;
        assert_eq!(
            colors(&style, &normal),
            (rgb(0x102030), rgb(0x19c3a7), rgb(0xe83967))
        );
        style.special = None;
        assert_eq!(
            colors(&style, &normal),
            (rgb(0x102030), rgb(0x19c3a7), rgb(0x8a9bac)),
            "unset underline/undercurl color inherits GUI Normal's special color"
        );
        assert_eq!(
            colors(
                &style,
                &Vim9RenderStyle {
                    special: None,
                    ..normal
                }
            )
            .2,
            rgb(0x102030),
            "without a special color, decoration follows the resolved foreground"
        );
    }

    #[test]
    fn legacy_and_direct_replace_modes_paint_the_same_underline_caret() {
        let mut frame = direct_frame(10);
        Arc::make_mut(&mut frame).mode = "R".into();
        let content = Rect::from_min_size(egui::pos2(7.25, 12.5), egui::vec2(220.0, 120.0));
        let cursor = cursor_rect(&frame, content, 10.0, 20.0, 0.0).unwrap();
        let expected =
            Rect::from_min_max(egui::pos2(cursor.left(), cursor.bottom() - 4.0), cursor.max);
        let direct = DirectInput::default();
        for direct_path in [false, true] {
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(Default::default(), |ui| {
                paint(
                    ui.painter(),
                    &frame,
                    content,
                    10.0,
                    20.0,
                    14.0,
                    true,
                    true,
                    &ScrollPresentation::default(),
                    0.0,
                    direct_path.then_some(&direct),
                );
            });
            output.textures_delta.clear();
            let carets: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Rect(shape) if shape.fill == rgb(0xddeeff) => Some(shape.rect),
                    _ => None,
                })
                .collect();
            assert_eq!(carets, vec![expected], "direct path: {direct_path}");
        }
    }

    #[test]
    fn buffer_ownership_excludes_gutters_and_command_line() {
        let frame: Vim9RenderFrame = serde_json::from_value(serde_json::json!({
            "supported": true, "rows": 25, "columns": 80,
            "view": {"row": 1, "column": 4, "rows": 22, "columns": 76}
        }))
        .unwrap();
        assert!(usable(&frame, 25, 80));
        assert!(!usable(&frame, 24, 80));
        assert!(covers(&frame, 1, 4));
        assert!(covers(&frame, 22, 79));
        assert!(!covers(&frame, 1, 3));
        assert!(!covers(&frame, 23, 4));
    }

    #[test]
    fn mixed_splits_paint_all_buffers_without_covering_separator_or_status_cells() {
        let frame: Vim9RenderFrame = serde_json::from_value(serde_json::json!({
            "supported": true, "rows": 10, "columns": 40,
            "default_style": {"foreground": 0xabcdef, "background": 0x102030},
            "view": {"row": 0, "column": 0, "rows": 5, "columns": 20},
            "windows": [
                {"window": 1, "view": {"row": 0, "column": 0, "rows": 5, "columns": 20}},
                {"window": 2, "view": {"row": 0, "column": 21, "rows": 5, "columns": 19}},
                {"window": 3, "view": {"row": 6, "column": 0, "rows": 3, "columns": 40}}
            ],
            "runs": [
                {"row": 1, "column": 1, "text": "first", "cells": 5},
                {"row": 1, "column": 22, "text": "second", "cells": 6},
                {"row": 7, "column": 1, "text": "third", "cells": 5}
            ]
        }))
        .unwrap();
        assert!(usable(&frame, 10, 40));
        assert!(covers(&frame, 1, 22), "inactive right split is native");
        assert!(covers(&frame, 7, 1), "bottom split is native");
        assert!(!covers(&frame, 1, 20), "vertical separator stays in Vim");
        assert!(!covers(&frame, 5, 1), "split status row stays in Vim");
        assert!(!covers(&frame, 9, 1), "command row stays in Vim");

        // Check the actual egui paint list, not merely the geometry helper.
        // Fractional metrics model a font size / HiDPI rounding boundary.
        let content = Rect::from_min_size(egui::pos2(13.25, 7.75), egui::vec2(400.0, 200.0));
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let painter = ui.ctx().layer_painter(egui::LayerId::background());
            paint(
                &painter,
                &frame,
                content,
                8.25,
                17.5,
                14.0,
                true,
                true,
                &ScrollPresentation::default(),
                0.0,
                None,
            );
        });
        output.textures_delta.clear();
        let expected: Vec<_> = frame
            .paint_views()
            .map(|v| {
                Rect::from_min_size(
                    content.min + egui::vec2(v.column as f32 * 8.25, v.row as f32 * 17.5),
                    egui::vec2(v.columns as f32 * 8.25, v.rows as f32 * 17.5),
                )
            })
            .collect();
        for view in expected {
            assert!(
                output.shapes.iter().any(|clipped| {
                    matches!(&clipped.shape, egui::Shape::Rect(shape)
                    if shape.rect == view && shape.fill == rgb(0x102030))
                }),
                "missing native background for {view:?}"
            );
        }
    }

    #[test]
    fn native_caret_and_ime_rect_share_fractional_split_geometry() {
        let frame: Vim9RenderFrame = serde_json::from_value(serde_json::json!({
            "supported": true, "rows": 8, "columns": 40, "mode": "i",
            "default_style": {"foreground": 0xa1b2c3},
            "view": {"row": 1, "column": 21, "rows": 5, "columns": 19},
            "windows": [
                {"window": 1, "view": {"row": 0, "column": 0, "rows": 6, "columns": 20}},
                {"window": 2, "view": {"row": 1, "column": 21, "rows": 5, "columns": 19}}
            ],
            "cursor": {"row": 2, "column": 25, "width": 2, "visible": true}
        }))
        .unwrap();
        let content = Rect::from_min_size(egui::pos2(11.5, 23.25), egui::vec2(400.0, 200.0));
        let ime = cursor_rect(&frame, content, 8.25, 17.5, 0.0).unwrap();
        assert_eq!(ime.min, content.min + egui::vec2(25.0 * 8.25, 2.0 * 17.5));
        assert_eq!(ime.size(), egui::vec2(16.5, 17.5));
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            paint(
                ui.painter(),
                &frame,
                content,
                8.25,
                17.5,
                14.0,
                true,
                true,
                &ScrollPresentation::default(),
                0.0,
                None,
            );
        });
        output.textures_delta.clear();
        assert!(
            output.shapes.iter().any(|clipped| {
                matches!(&clipped.shape, egui::Shape::Rect(shape)
                if shape.rect.min == ime.min
                    && shape.rect.size() == egui::vec2(2.0, ime.height())
                    && shape.fill == rgb(0xa1b2c3))
            }),
            "the native caret must use the same origin as the IME candidate anchor"
        );
        assert!(cursor_rect(&frame, content, f32::NAN, 17.5, 0.0).is_none());
        assert!(cursor_rect(&frame, content, 8.25, 0.0, 0.0).is_none());
    }
}
