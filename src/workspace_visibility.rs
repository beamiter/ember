//! Navigation-only memory; no conversation/runtime handles are touched.
#[derive(Default, Debug)]
pub(crate) struct ScrollBookmark {
    offset: f32,
    restore: bool,
}
impl ScrollBookmark {
    pub fn toggled(&mut self, visible: bool) {
        self.restore = visible;
    }
    pub fn restore_offset(&mut self, visible: bool) -> Option<f32> {
        if visible && std::mem::take(&mut self.restore) {
            Some(self.offset)
        } else {
            None
        }
    }
    pub fn record(&mut self, rendered_expanded: bool, now_expanded: bool, offset: f32) {
        if rendered_expanded && now_expanded && offset.is_finite() {
            self.offset = offset.max(0.0);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collapse_does_not_replace_the_scroll_bookmark_with_a_clamped_view() {
        let mut state = ScrollBookmark::default();
        state.record(true, true, 240.0);
        state.toggled(false);
        state.record(false, false, 0.0);
        assert_eq!(state.restore_offset(false), None);
        state.toggled(true);
        state.record(false, true, 0.0); // a delayed collapsed-frame callback
        assert_eq!(state.restore_offset(true), Some(240.0));
        assert_eq!(state.restore_offset(true), None);
    }
}
