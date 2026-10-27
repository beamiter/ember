//! Session-owned pixel motion for the projected Block/Chat document.
//!
//! The terminal still owns every cell. This state only chooses a continuous
//! viewport over that document; no animation manufactures output or changes
//! the PTY's size. Precision devices already supply their own momentum.
use jterm_core::transcript_canvas::CanvasMotion;

#[derive(Clone, Debug, Default)]
pub struct TranscriptCanvas {
    motion: CanvasMotion,
    initialized: bool,
    expected_offset: usize,
    document_rows: usize,
    projection_revision: u64,
    columns: usize,
    visible_rows: usize,
    line_height: f64,
    follows_live: bool,
    expected_follow: bool,
    last_time: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CanvasPresentation {
    pub offset_rows: usize,
    /// Positive value moves history upwards inside the viewport.
    pub translation: f32,
    pub margin: bool,
    pub follows_live: bool,
    pub animating: bool,
}

impl TranscriptCanvas {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn stop(&mut self) {
        self.motion.cancel();
    }

    pub fn pause_follow(&mut self) {
        self.motion.cancel();
        self.follows_live = false;
        self.expected_follow = false;
    }

    /// Admit raw pixels without multiplying touchpad momentum. Discrete wheel
    /// deltas are expressed in pixels by the host and use the shared spring.
    pub fn scroll(&mut self, pixels: f64, precision: bool) {
        if !self.initialized || !pixels.is_finite() || pixels == 0.0 {
            return;
        }
        if precision {
            self.motion.jump_to(self.motion.position() + pixels);
        } else {
            self.motion.scroll_by(pixels);
        }
        self.follows_live = self.motion.target() <= 0.0;
    }

    /// Rebase against the terminal's stable reading anchor before advancing.
    /// A streaming append follows gently only while the user owns the tail.
    #[allow(clippy::too_many_arguments)]
    pub fn present(
        &mut self,
        offset_rows: usize,
        document_rows: usize,
        projection_revision: u64,
        visible_rows: usize,
        columns: usize,
        follows_live: bool,
        line_height: f32,
        time: f64,
        reduced_motion: bool,
    ) -> CanvasPresentation {
        let height = f64::from(line_height.max(1.0));
        let max = document_rows.saturating_sub(visible_rows) as f64 * height;
        self.motion.set_reduced_motion(reduced_motion);
        let geometry_changed = self.columns != columns
            || self.visible_rows != visible_rows
            || self.line_height != height;
        if !self.initialized || geometry_changed {
            // The projection supplies a reflowed whole-row anchor. Preserve
            // the reader's position within that row across resize/font zoom,
            // but never carry an obsolete wheel destination into new geometry.
            // Ember projects ceil(position), so this remainder is non-positive.
            let remainder =
                if self.initialized && !self.follows_live && follows_live == self.expected_follow {
                    self.motion.position() / self.line_height - self.expected_offset as f64
                } else {
                    0.0
                };
            let position = if self.initialized && self.follows_live && follows_live {
                0.0
            } else {
                offset_rows as f64 + remainder
            };
            self.motion.set_bounds(0.0, max);
            self.motion.jump_to(position * height);
            self.follows_live = follows_live;
            self.initialized = true;
        } else if follows_live != self.expected_follow {
            // A keyboard action or explicit Return to live can change follow
            // ownership even when it names the same whole-row offset.
            self.motion.jump_to(offset_rows as f64 * height);
            self.follows_live = follows_live;
        } else if document_rows != self.document_rows
            || projection_revision != self.projection_revision
        {
            // Rebase before applying a smaller extent: clamping first would
            // move the reading anchor twice after collapse/history eviction.
            self.motion.set_bounds(
                0.0,
                (self
                    .document_rows
                    .max(document_rows)
                    .saturating_sub(visible_rows) as f64
                    * height)
                    .max(max),
            );
            if self.follows_live && follows_live {
                let appended = document_rows.saturating_sub(self.document_rows) as f64 * height;
                // Preserve the old writing position, then move towards the
                // newest output. Retargeting never restarts the spring clock.
                self.motion.shift(appended);
                // A large output burst is already in the document. Animate
                // only a bounded distance to its tail, not thousands of rows
                // of unread intermediate output in a fraction of a second.
                let limit = visible_rows.saturating_mul(2).max(1) as f64 * height;
                if self.motion.position() > limit {
                    self.motion.jump_to(limit);
                }
                self.motion.set_target(0.0);
            } else {
                let rebase = offset_rows as f64 - self.expected_offset as f64;
                self.motion.shift(rebase * height);
            }
        } else if offset_rows != self.expected_offset {
            // Search, keyboard navigation, scrollbar dragging, and return to
            // live own explicit destinations and cancel old wheel motion.
            self.motion.jump_to(offset_rows as f64 * height);
            self.follows_live = follows_live;
        }
        self.motion.set_bounds(0.0, max);
        let dt = self
            .last_time
            .filter(|previous| time.is_finite() && time >= *previous)
            .map_or(0.0, |previous| time - previous);
        self.last_time = time.is_finite().then_some(time);
        let position = self.motion.advance(dt).clamp(0.0, max);
        let rows = position / height;
        let rounded = rows.ceil() as usize;
        let translation = (rounded as f64 * height - position) as f32;
        let margin = translation > 0.001;
        self.expected_offset = rounded;
        self.expected_follow = self.follows_live;
        self.document_rows = document_rows;
        self.projection_revision = projection_revision;
        self.columns = columns;
        self.visible_rows = visible_rows;
        self.line_height = height;
        CanvasPresentation {
            offset_rows: rounded,
            translation: if margin { translation } else { 0.0 },
            margin,
            follows_live: self.follows_live,
            animating: self.motion.is_animating(),
        }
    }
}

/// One mapping is used by glyphs, card chrome and pointer hit testing. The
/// extra history row occupies a real document row; Chat's composer is moved
/// back by that one row so it remains fixed while history flows behind it.
#[derive(Clone, Copy, Debug, Default)]
pub struct CanvasGeometry {
    pub translation: f32,
    pub margin: bool,
    pub composer_start: Option<usize>,
}

impl CanvasGeometry {
    pub fn row_offset(self, row: usize, line_height: f32) -> f32 {
        if self.composer_start.is_some_and(|start| row >= start) {
            if self.margin {
                line_height
            } else {
                0.0
            }
        } else {
            self.translation
        }
    }

    pub fn inverse_y(self, y: f32, origin: f32, line_height: f32) -> f32 {
        let dock = self
            .composer_start
            .map(|start| origin + start as f32 * line_height - self.row_offset(start, line_height));
        y + if dock.is_some_and(|top| y >= top) {
            if self.margin {
                line_height
            } else {
                0.0
            }
        } else {
            self.translation
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(
        canvas: &mut TranscriptCanvas,
        offset: usize,
        rows: usize,
        follow: bool,
        t: f64,
    ) -> CanvasPresentation {
        canvas.present(offset, rows, rows as u64, 20, 80, follow, 20.0, t, false)
    }

    #[test]
    fn precision_pixels_remain_fractional_after_input_stops() {
        let mut canvas = TranscriptCanvas::default();
        present(&mut canvas, 0, 200, true, 0.0);
        canvas.scroll(3.25, true);
        let frame = present(&mut canvas, 0, 200, true, 0.016);
        assert_eq!(frame.offset_rows, 1);
        assert_eq!(frame.translation, 16.75);
        assert!(!frame.animating);
        assert!(!frame.follows_live);
        let idle = present(&mut canvas, 1, 200, false, 3.0);
        assert_eq!(idle.translation, frame.translation);
    }

    #[test]
    fn resize_and_font_zoom_preserve_fractional_reading_anchor() {
        let mut canvas = TranscriptCanvas::default();
        present(&mut canvas, 10, 200, false, 0.0);
        canvas.scroll(5.0, true);
        let before = present(&mut canvas, 10, 200, false, 0.016);
        assert_eq!(before.offset_rows, 11);
        let resized = canvas.present(16, 220, 2, 30, 100, false, 24.0, 0.032, false);
        assert_eq!(resized.offset_rows, 16);
        assert_eq!(resized.translation, 18.0);
        assert!(!resized.follows_live);
        assert!(!resized.animating);
    }

    #[test]
    fn resize_during_streaming_keeps_follow_at_the_live_edge() {
        let mut canvas = TranscriptCanvas::default();
        present(&mut canvas, 0, 200, true, 0.0);
        let moving = present(&mut canvas, 0, 205, true, 0.016);
        assert!(moving.animating);
        let resized = canvas.present(
            moving.offset_rows,
            205,
            2,
            30,
            100,
            true,
            24.0,
            0.032,
            false,
        );
        assert_eq!(resized.offset_rows, 0);
        assert_eq!(resized.translation, 0.0);
        assert!(resized.follows_live);
        assert!(!resized.animating);
    }

    #[test]
    fn resize_does_not_preserve_fraction_over_explicit_return_to_live() {
        let mut canvas = TranscriptCanvas::default();
        present(&mut canvas, 10, 200, false, 0.0);
        canvas.scroll(5.0, true);
        present(&mut canvas, 10, 200, false, 0.016);
        let live = canvas.present(0, 200, 2, 30, 100, true, 24.0, 0.032, false);
        assert_eq!(live.offset_rows, 0);
        assert_eq!(live.translation, 0.0);
        assert!(live.follows_live);
    }

    #[test]
    fn output_growth_keeps_reading_anchor_and_subpixel_remainder() {
        let mut canvas = TranscriptCanvas::default();
        present(&mut canvas, 10, 200, false, 0.0);
        canvas.scroll(4.5, true);
        let before = present(&mut canvas, 10, 200, false, 0.016);
        let after = present(&mut canvas, 16, 205, false, 0.032);
        assert_eq!(after.offset_rows, before.offset_rows + 5);
        assert_eq!(after.translation, before.translation);
        assert!(!after.animating);
    }

    #[test]
    fn streaming_tail_moves_continuously_and_then_sleeps() {
        let mut canvas = TranscriptCanvas::default();
        present(&mut canvas, 0, 200, true, 0.0);
        let mut frame = present(&mut canvas, 0, 201, true, 0.016);
        assert!(frame.animating);
        assert!(frame.offset_rows > 0);
        assert!(frame.translation > 0.0 && frame.translation < 20.0);
        for tick in 2..180 {
            frame = present(
                &mut canvas,
                frame.offset_rows,
                201,
                true,
                tick as f64 / 60.0,
            );
        }
        assert_eq!(frame.offset_rows, 0);
        assert!(!frame.animating);
        assert!(frame.follows_live);
    }

    #[test]
    fn large_streaming_bursts_bound_visual_catch_up_without_changing_extent() {
        let mut canvas = TranscriptCanvas::default();
        present(&mut canvas, 0, 200, true, 0.0);
        let mut frame = present(&mut canvas, 0, 10_200, true, 0.016);
        assert!(frame.animating);
        assert!(frame.follows_live);
        assert!(frame.offset_rows <= 40);
        assert_eq!(canvas.document_rows, 10_200);
        for tick in 2..180 {
            frame = present(
                &mut canvas,
                frame.offset_rows,
                10_200,
                true,
                tick as f64 / 60.0,
            );
        }
        assert_eq!(frame.offset_rows, 0);
        assert!(!frame.animating);
    }

    #[test]
    fn repeated_bursts_stay_bounded_but_never_pull_a_reader_to_live() {
        let mut canvas = TranscriptCanvas::default();
        present(&mut canvas, 0, 200, true, 0.0);
        let mut frame = present(&mut canvas, 0, 10_200, true, 0.016);
        for tick in 2..20 {
            frame = present(
                &mut canvas,
                frame.offset_rows,
                200 + tick * 10_000,
                true,
                tick as f64 / 60.0,
            );
            assert!(frame.offset_rows <= 40);
        }
        canvas.scroll(5.25, true);
        frame = present(&mut canvas, frame.offset_rows, 190_200, true, 0.34);
        assert!(!frame.follows_live);
        let held = frame;
        frame = present(&mut canvas, held.offset_rows + 10_000, 200_200, false, 0.36);
        assert_eq!(frame.offset_rows, held.offset_rows + 10_000);
        assert_eq!(frame.translation, held.translation);
        assert!(!frame.follows_live);
        assert!(!frame.animating);
    }

    #[test]
    fn large_burst_never_caps_an_old_history_reading_anchor() {
        let mut canvas = TranscriptCanvas::default();
        present(&mut canvas, 8_000, 10_200, false, 0.0);
        canvas.scroll(7.25, true);
        let before = present(&mut canvas, 8_000, 10_200, false, 0.016);
        let after = present(
            &mut canvas,
            before.offset_rows + 10_000,
            20_200,
            false,
            0.032,
        );
        assert_eq!(after.offset_rows, before.offset_rows + 10_000);
        assert_eq!(after.translation, before.translation);
        assert!(!after.follows_live);
        assert!(!after.animating);
    }

    #[test]
    fn reduced_motion_burst_goes_directly_to_live() {
        let mut canvas = TranscriptCanvas::default();
        present(&mut canvas, 0, 200, true, 0.0);
        let frame = canvas.present(0, 10_200, 2, 20, 80, true, 20.0, 0.016, true);
        assert_eq!(frame.offset_rows, 0);
        assert_eq!(frame.translation, 0.0);
        assert!(frame.follows_live);
        assert!(!frame.animating);
    }

    #[test]
    fn composer_and_history_share_an_invertible_geometry() {
        let geometry = CanvasGeometry {
            translation: 7.25,
            margin: true,
            composer_start: Some(18),
        };
        for row in [0, 5, 17, 18, 19] {
            let y = 10.0 + row as f32 * 20.0 + 5.0 - geometry.row_offset(row, 20.0);
            let restored = geometry.inverse_y(y, 10.0, 20.0);
            assert_eq!(restored, 10.0 + row as f32 * 20.0 + 5.0);
        }
        assert_eq!(18.0 * 20.0 - geometry.row_offset(18, 20.0), 17.0 * 20.0);
    }

    #[test]
    fn shrinking_history_rebases_before_clamping_the_old_position() {
        let mut canvas = TranscriptCanvas::default();
        present(&mut canvas, 80, 200, false, 0.0);
        canvas.scroll(3.5, true);
        let before = present(&mut canvas, 80, 200, false, 0.016);
        let after = present(&mut canvas, 11, 40, false, 0.032);
        assert_eq!(after.offset_rows, 11);
        assert_eq!(after.translation, before.translation);
    }

    #[test]
    fn fixed_capacity_history_replacement_keeps_fractional_anchor() {
        let mut canvas = TranscriptCanvas::default();
        canvas.present(10, 200, 1, 20, 80, false, 20.0, 0.0, false);
        canvas.scroll(7.5, true);
        let before = canvas.present(10, 200, 1, 20, 80, false, 20.0, 0.016, false);
        let after = canvas.present(9, 200, 2, 20, 80, false, 20.0, 0.032, false);
        assert_eq!(after.offset_rows, 9);
        assert_eq!(after.translation, before.translation);
    }

    #[test]
    fn suspended_tab_settles_pending_motion_without_replaying_it() {
        let mut canvas = TranscriptCanvas::default();
        present(&mut canvas, 0, 200, true, 0.0);
        canvas.scroll(90.0, false);
        let moving = present(&mut canvas, 0, 200, true, 0.016);
        assert!(moving.animating);
        let resumed = present(&mut canvas, moving.offset_rows, 200, false, 3.0);
        assert!(!resumed.animating);
        assert_eq!(resumed.offset_rows, 5);
        assert_eq!(resumed.translation, 10.0);
    }
}
