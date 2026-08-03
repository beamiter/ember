//! Egui renderer for the family-wide bottom status bar.
//!
//! What the bar says is owned by [`jterm_core::bottom_bar`]: the app gathers a
//! `Snapshot` from state it already tracks and `compose` turns it into ordered
//! segments. This module only draws the composed content, following the family
//! renderer contract so all four jterms look alike: a [`BAR_HEIGHT`] strip on
//! `theme.tabbar.bg` with a 1px `theme.ui.border` hairline on top, 11px chrome
//! text, one label per segment colored via `Tone::color`, the left group
//! packed from the left edge and the right group ending at the right edge.
//! Painted directly rather than laid out as widgets, like the pane header, so
//! it stays outside egui's layout flow.

use crate::theme::{Theme, ThemeExt as _};
use egui::{Color32, FontId, Sense, Stroke, Vec2};
use jterm_core::bottom_bar::{Content, Segment, BAR_HEIGHT};

/// Gap between adjacent segments, per the family renderer contract (~12px).
const SEGMENT_GAP: f32 = 12.0;

/// Horizontal inset at the window edges, matching the pane header's padding
/// scale rather than egui's default item spacing.
const EDGE_PADDING: f32 = 8.0;

/// Resolve a segment's tone against the theme, through the shared
/// `Tone::color` mapping so all four terminals agree on the palette.
fn segment_color(segment: &Segment, theme: &Theme) -> Color32 {
    Theme::rgb_to_color32(segment.tone.color(theme))
}

/// Draw the bar across the full width handed to `ui` (the enclosing bottom
/// panel spans the window). The colors follow the pane header's precedent:
/// opaque theme chrome over the transparent window clear color, with no extra
/// per-element opacity — the bar has no faded states.
pub fn draw(ui: &mut egui::Ui, theme: &Theme, content: &Content) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), BAR_HEIGHT), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        egui::CornerRadius::ZERO,
        Theme::rgb_to_color32(theme.tabbar.bg),
    );
    painter.hline(
        rect.left()..=rect.right(),
        rect.top() + 0.5,
        Stroke::new(1.0, Theme::rgb_to_color32(theme.ui.border)),
    );

    let font = FontId::proportional(11.0);
    let center_y = rect.center().y;

    // Right group first: it keeps its compose order reading left-to-right but
    // ends at the right edge, and its extent caps how far the left group may
    // run before eliding.
    let right_galleys: Vec<_> = content
        .right
        .iter()
        .map(|segment| {
            let color = segment_color(segment, theme);
            (
                painter.layout_no_wrap(segment.text.clone(), font.clone(), color),
                color,
            )
        })
        .collect();
    let right_width: f32 = right_galleys
        .iter()
        .map(|(galley, _)| galley.size().x)
        .sum::<f32>()
        + SEGMENT_GAP * right_galleys.len().saturating_sub(1) as f32;
    let right_start = (rect.right() - EDGE_PADDING - right_width).max(rect.left() + EDGE_PADDING);
    let mut cursor_x = right_start;
    for (galley, color) in right_galleys {
        let size = galley.size();
        painter.galley(egui::pos2(cursor_x, center_y - size.y / 2.0), galley, color);
        cursor_x += size.x + SEGMENT_GAP;
    }

    // Left group: packed from the left edge, the last visible segment elided
    // rather than running under the right group.
    let left_limit = if content.right.is_empty() {
        rect.right() - EDGE_PADDING
    } else {
        right_start - SEGMENT_GAP
    };
    let mut cursor_x = rect.left() + EDGE_PADDING;
    for segment in &content.left {
        let available = left_limit - cursor_x;
        if available <= 0.0 {
            break;
        }
        let color = segment_color(segment, theme);
        let galley = crate::pane_header::clipped_galley(
            painter,
            &segment.text,
            font.clone(),
            color,
            available,
        );
        let size = galley.size();
        painter.galley(egui::pos2(cursor_x, center_y - size.y / 2.0), galley, color);
        cursor_x += size.x + SEGMENT_GAP;
    }
}
