//! The Vim buffer's native text surface.
//!
//! Its input is the authenticated Vim buffer protocol, never ANSI cells or a
//! terminal palette. Logical screen columns are retained for Vim hit testing;
//! egui owns glyph shaping, RGB backgrounds and all text decorations.

use eframe::egui::{self, Color32, FontId, Rect, Stroke};
use jterm_core::vim9::{Vim9RenderFrame, Vim9RenderStyle};

pub(crate) fn usable(frame: &Vim9RenderFrame, rows: usize, columns: usize) -> bool {
    frame.supported
        && frame.rows == rows
        && frame.columns == columns
        && frame.view.rows > 0
        && frame.view.columns > 0
        && frame.view.row.saturating_add(frame.view.rows) <= rows
        && frame.view.column.saturating_add(frame.view.columns) <= columns
}

pub(crate) fn covers(frame: &Vim9RenderFrame, row: usize, column: usize) -> bool {
    row >= frame.view.row
        && row < frame.view.row.saturating_add(frame.view.rows)
        && column >= frame.view.column
        && column < frame.view.column.saturating_add(frame.view.columns)
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

/// Paint only the buffer rectangle. Vim's command line and unsupported views
/// remain in the compatibility renderer outside this rectangle.
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
) {
    let view = Rect::from_min_size(
        content.min
            + egui::vec2(
                frame.view.column as f32 * cell_width,
                frame.view.row as f32 * line_height,
            ),
        egui::vec2(
            frame.view.columns as f32 * cell_width,
            frame.view.rows as f32 * line_height,
        ),
    )
    .intersect(content);
    let painter = painter.with_clip_rect(view);
    let (normal_fg, normal_bg, _) = colors(&frame.default_style, &frame.default_style);
    painter.rect_filled(view, 0.0, normal_bg);
    // Backgrounds precede every glyph so a later style run cannot erase the
    // preceding italic glyph's natural overhang.
    for run in &frame.runs {
        if covers(frame, run.row, run.column) && run.cells > 0 {
            let (_, bg, _) = colors(&run.style, &frame.default_style);
            painter.rect_filled(
                Rect::from_min_size(
                    content.min
                        + egui::vec2(run.column as f32 * cell_width, run.row as f32 * line_height),
                    egui::vec2(run.cells as f32 * cell_width, line_height),
                ),
                0.0,
                bg,
            );
        }
    }
    for run in &frame.runs {
        if !covers(frame, run.row, run.column) || run.cells == 0 {
            continue;
        }
        let (fg, _, special) = colors(&run.style, &frame.default_style);
        let run_rect = Rect::from_min_size(
            content.min + egui::vec2(run.column as f32 * cell_width, run.row as f32 * line_height),
            egui::vec2(run.cells as f32 * cell_width, line_height),
        );
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
        let thickness = (font_size / 14.0).clamp(1.0, 2.0);
        let underline_y = run_rect.bottom() - thickness * 1.5;
        if run.style.undercurl {
            let mut points = Vec::new();
            let step = (font_size / 5.0).max(2.0);
            let mut x = run_rect.left();
            let mut up = true;
            while x < run_rect.right() {
                points.push(egui::pos2(
                    x,
                    underline_y - if up { thickness * 1.5 } else { 0.0 },
                ));
                x += step;
                up = !up;
            }
            points.push(egui::pos2(run_rect.right(), underline_y));
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
    if frame.cursor.visible
        && covers(frame, frame.cursor.row, frame.cursor.column)
        && (!focused || cursor_on)
    {
        let cursor = Rect::from_min_size(
            content.min
                + egui::vec2(
                    frame.cursor.column as f32 * cell_width,
                    frame.cursor.row as f32 * line_height,
                ),
            egui::vec2(frame.cursor.width.max(1) as f32 * cell_width, line_height),
        );
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
}
