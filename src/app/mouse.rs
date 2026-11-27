use crate::terminal::{DisplayPoint, ProjectedViewport, PtyWindowPixels, TerminalState};
use eframe::egui;
use jterm_engine::mouse::MousePosition;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PixelViewport {
    generation: u64,
    size: PtyWindowPixels,
}

impl PixelViewport {
    fn for_terminal(terminal: &TerminalState) -> Option<Self> {
        let size = terminal.pty_window_pixels?;
        let generation = terminal.alternate_scroll_generation()?;
        (size.grid == terminal.get_dimensions()
            && size.pixels.0 != 0
            && size.pixels.1 != 0
            && terminal.projection_unavailable().is_none())
        .then_some(Self { generation, size })
    }

    pub(crate) fn position_at(
        self,
        pointer: egui::Pos2,
        rect: egui::Rect,
        cell_size: egui::Vec2,
        viewport: &ProjectedViewport,
    ) -> Option<MousePosition> {
        if !rect.is_finite()
            || !rect.contains(pointer)
            || !cell_size.is_finite()
            || cell_size.x <= 0.0
            || cell_size.y <= 0.0
        {
            return None;
        }
        // Keep the position within the displayed cell as its row maps back to
        // the live grid. Normalizing over the full measured area avoids
        // accumulating the rounding error of a physical cell size.
        let x = f64::from(pointer.x - rect.left()) / f64::from(cell_size.x);
        let y = f64::from(pointer.y - rect.top()) / f64::from(cell_size.y);
        let (screen_col, screen_row) = (x as usize, y as usize);
        let (row, col) = viewport.application_cell(DisplayPoint::new(screen_row, screen_col))?;
        let (cols, rows) = self.size.grid;
        if col >= cols || row >= rows {
            return None;
        }
        let physical = |cell: usize, fraction: f64, cells: usize, pixels: u16| {
            let pixel = (cell as f64 + fraction) * f64::from(pixels) / cells as f64;
            (pixel.floor() as u32).min(u32::from(pixels - 1))
        };
        Some(MousePosition::Pixel {
            x: physical(col, x.fract(), cols, self.size.pixels.0),
            y: physical(row, y.fract(), rows, self.size.pixels.1),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MouseReportSpace {
    Cells,
    Pixels(Option<PixelViewport>),
}

impl MouseReportSpace {
    pub(crate) fn for_terminal(terminal: &TerminalState) -> Self {
        if terminal.vt.protocol.modes().contains(&1016) {
            Self::Pixels(PixelViewport::for_terminal(terminal))
        } else {
            Self::Cells
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MouseMotion {
    space: MouseReportSpace,
    position: MousePosition,
    button: u8,
}

impl MouseMotion {
    pub(crate) fn new(space: MouseReportSpace, position: MousePosition, button: u8) -> Self {
        Self {
            space,
            position,
            button: if space == MouseReportSpace::Cells {
                0
            } else {
                button
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::HistoryProjection;

    #[test]
    fn pixel_mouse_preserves_distinct_offsets_within_one_cell() {
        let mut terminal = TerminalState::new(3, 2);
        terminal.enable_pixel_mouse();
        terminal.process_input(b"\x1b[?1002;1016h");
        terminal.install_pty_window_pixels(Some(PtyWindowPixels {
            grid: (3, 2),
            pixels: (49, 31),
        }));
        let space = MouseReportSpace::for_terminal(&terminal);
        let MouseReportSpace::Pixels(Some(pixels)) = space else {
            panic!("expected measured pixel coordinates");
        };
        let viewport = terminal.projected_viewport(HistoryProjection::identity(), false);
        let origin = egui::pos2(10.0, 20.0);
        let cell = egui::vec2(16.0, 18.0);
        let rect = egui::Rect::from_min_size(origin, egui::vec2(cell.x * 3.0, cell.y * 2.0));
        let position = |x, y| pixels.position_at(origin + egui::vec2(x, y), rect, cell, &viewport);
        let first = position(18.0, 27.0).unwrap();
        let next = position(24.0, 27.0).unwrap();
        assert_eq!(first, MousePosition::Pixel { x: 18, y: 23 });
        assert_eq!(next, MousePosition::Pixel { x: 24, y: 23 });
        assert_ne!(
            MouseMotion::new(space, first, 32),
            MouseMotion::new(space, next, 32)
        );
        assert!(position(-1.0, 27.0).is_none());
        assert!(position(48.0, 27.0).is_none());
    }

    #[test]
    fn pixel_mouse_epoch_retires_positions_on_same_grid_rescale_and_mode_aba() {
        let mut terminal = TerminalState::new(12, 8);
        terminal.enable_pixel_mouse();
        terminal.process_input(b"\x1b[?1002;1016h");
        assert_eq!(
            MouseReportSpace::for_terminal(&terminal),
            MouseReportSpace::Pixels(None)
        );
        let geometry = PtyWindowPixels {
            grid: (12, 8),
            pixels: (120, 144),
        };
        terminal.install_pty_window_pixels(Some(geometry));
        let initial = MouseReportSpace::for_terminal(&terminal);
        terminal.install_pty_window_pixels(Some(geometry));
        assert_eq!(MouseReportSpace::for_terminal(&terminal), initial);
        terminal.install_pty_window_pixels(Some(PtyWindowPixels {
            pixels: (240, 288),
            ..geometry
        }));
        terminal.install_pty_window_pixels(Some(geometry));
        let rescaled = MouseReportSpace::for_terminal(&terminal);
        assert_ne!(rescaled, initial);
        terminal.process_input(b"\x1b[?1006h");
        assert_eq!(
            MouseReportSpace::for_terminal(&terminal),
            MouseReportSpace::Cells
        );
        terminal.process_input(b"\x1b[?1016h");
        assert_ne!(MouseReportSpace::for_terminal(&terminal), rescaled);
        terminal.process_input(b"\x1bc");
        assert_eq!(
            MouseReportSpace::for_terminal(&terminal),
            MouseReportSpace::Cells
        );
        terminal.process_input(b"\x1b[?1002;1016h");
        assert!(matches!(
            MouseReportSpace::for_terminal(&terminal),
            MouseReportSpace::Pixels(Some(_))
        ));
    }
}
