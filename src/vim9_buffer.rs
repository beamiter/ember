//! The Vim buffer's native text surface.
//!
//! Its input is the authenticated Vim buffer protocol, never ANSI cells or a
//! terminal palette. Logical screen columns are retained for Vim hit testing;
//! egui owns glyph shaping, RGB backgrounds and all text decorations.

use egui::{self, Color32, FontId, Rect, Stroke};
use jterm_core::vim9::{
    Vim9PixelRect, Vim9RenderFrame, Vim9RenderMetrics, Vim9RenderRect, Vim9RenderStyle,
    Vim9ScrollAnimation,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// Acknowledged Vim frames are the source of truth. While a short scroll
/// settles, retain its old edge rows so no empty band flashes at the boundary.
/// State is bound to the editor's identity, never a renderer's reused pane.
#[derive(Default)]
pub(crate) struct ScrollPresentation {
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
        if direct_input {
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
            || frame.is_none();
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
    (fg, bg, style.special.map(rgb).unwrap_or(fg))
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
) {
    if !metrics(content, cell_width, line_height).is_valid()
        || !content.is_positive()
        || !font_size.is_finite()
        || font_size <= 0.0
    {
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
        let (fg, _, special) = colors(&run.style, &frame.default_style);
        let Some(run_rect) = geometry
            .cell_rect(run.row, run.column, run.cells)
            .map(gui_rect)
        else {
            continue;
        };
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
    if draw_cursor
        && frame.cursor.visible
        && logical_view.contains(frame.cursor.row, frame.cursor.column)
        && (!focused || cursor_on)
    {
        let Some(cursor) = geometry.cursor_rect(&frame.cursor).map(gui_rect) else {
            return;
        };
        if focused && (frame.mode.starts_with('i') || frame.mode.starts_with('R')) {
            let caret = Rect::from_min_size(cursor.min, egui::vec2(2.0, line_height));
            painter.rect_filled(caret, 0.0, normal_fg);
        } else {
            if focused {
                painter.rect_filled(cursor, 1.0, normal_fg.gamma_multiply(0.22));
            }
            painter.rect_stroke(
                cursor.shrink(0.5),
                1.0,
                Stroke::new(1.0, normal_fg),
                egui::StrokeKind::Inside,
            );
        }
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
            presentation.windows.get(&1).is_some(),
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
