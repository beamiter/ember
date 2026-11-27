//! Native read-only viewers of an existing owner's last published terminal frame.
//! No Session, ShellSession, input handle or resize capability crosses this boundary.
use crate::terminal::{DynamicColorPalette, ProjectedViewport, TerminalState};
use crate::theme::ThemeExt as _;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct Frame {
    viewport: ProjectedViewport,
    canvas: crate::transcript_canvas::CanvasGeometry,
    theme: crate::theme::Theme,
    palette: DynamicColorPalette,
    foreground: Option<(u8, u8, u8)>,
    background: Option<(u8, u8, u8)>,
    font_size: f32,
    cell: egui::Vec2,
}
impl Frame {
    pub(crate) fn capture(
        viewport: ProjectedViewport,
        renderer: &crate::ui::TerminalRenderer,
        terminal: &TerminalState,
    ) -> Arc<Self> {
        Arc::new(Self {
            viewport,
            canvas: renderer.canvas_geometry,
            theme: renderer.theme.clone(),
            palette: *terminal.colors.palette_overrides(),
            foreground: terminal.colors.foreground_override(),
            background: terminal.colors.background_override(),
            font_size: renderer.font_size,
            cell: egui::vec2(renderer.char_width, renderer.line_height),
        })
    }
    pub(crate) fn paint(&self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(
            rect,
            0.0,
            self.background
                .map(|(r, g, b)| egui::Color32::from_rgb(r, g, b))
                .unwrap_or_else(|| self.theme.terminal_background()),
        );
        if !self.viewport.is_available() {
            ui.label("Owner projection is unavailable");
            return;
        }
        for (row, cells) in self.viewport.cells().iter().enumerate() {
            let y =
                rect.top() + row as f32 * self.cell.y - self.canvas.row_offset(row, self.cell.y);
            if y + self.cell.y < rect.top() || y > rect.bottom() {
                continue;
            }
            for (col, c) in cells.iter().enumerate() {
                if c.flags.wide_continuation() {
                    continue;
                }
                let x = rect.left() + col as f32 * self.cell.x;
                if x > rect.right() {
                    break;
                }
                let mut fg = crate::color::resolve_fg_with_palette(
                    c.foreground,
                    &self.theme,
                    Some(&self.palette),
                    self.foreground,
                    c.flags.bold(),
                    c.flags.dim(),
                );
                let mut bg = crate::color::resolve_bg_with_palette(
                    c.background,
                    &self.theme,
                    Some(&self.palette),
                    self.background,
                );
                if c.flags.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let width = self.cell.x * if c.flags.wide() { 2.0 } else { 1.0 };
                let cell =
                    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(width, self.cell.y));
                painter.rect_filled(cell, 0.0, bg);
                if c.flags.concealed() {
                    continue;
                }
                if c.character != '\0' && c.character != ' ' {
                    painter.text(
                        cell.center(),
                        egui::Align2::CENTER_CENTER,
                        c.character,
                        egui::FontId::monospace(self.font_size),
                        fg,
                    );
                }
                if c.flags.underline() != crate::terminal::UnderlineStyle::None {
                    painter.line_segment(
                        [
                            cell.left_bottom() - egui::vec2(0.0, 1.0),
                            cell.right_bottom() - egui::vec2(0.0, 1.0),
                        ],
                        egui::Stroke::new(1.0, fg),
                    );
                }
                if c.flags.strikethrough() {
                    painter.line_segment(
                        [cell.left_center(), cell.right_center()],
                        egui::Stroke::new(1.0, fg),
                    );
                }
                if c.flags.overline() {
                    let inset = egui::vec2(0.0, 1.0);
                    painter.line_segment(
                        [cell.left_top() + inset, cell.right_top() + inset],
                        egui::Stroke::new(1.0, fg),
                    );
                }
            }
        }
    }
}
