//! Per-pane status header shown above each terminal in a split layout.
//!
//! A single pane has no header: the window title and tab bar already name it,
//! and the row would only cost terminal lines. Once the layout holds more than
//! one pane the user needs to tell them apart at a glance, so each pane gets a
//! compact strip with its index, title, working directory, and the command it
//! is currently running.
//!
//! The header is also the drag handle for rearranging panes: pressing it and
//! releasing over another pane swaps the two sessions.

use crate::theme::{Theme, ThemeExt as _};
use egui::{Align2, Color32, FontId, Rect, Stroke, Vec2};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Height of the header strip. Deliberately smaller than a tab: it must cost
/// at most one terminal row at typical font sizes.
pub const PANE_HEADER_HEIGHT: f32 = 20.0;

/// Probing `/proc` for every pane on every frame would be pure waste at 60 fps
/// while telling the user nothing new. A command that finishes within this
/// window is invisible either way.
const STATUS_REFRESH_INTERVAL: Duration = Duration::from_millis(400);

/// Pointer travel required before a header press turns into a pane drag, so a
/// plain click still just focuses the pane.
pub const PANE_DRAG_THRESHOLD: f32 = 5.0;

/// What a pane header displays. Assembled once per refresh interval and cached
/// per session, since the working directory and foreground command are read
/// from `/proc`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneStatus {
    /// Session title: the user's custom name, else the working directory.
    pub title: String,
    /// Working directory with `$HOME` abbreviated to `~`.
    pub cwd: Option<String>,
    /// Foreground command, when the shell is not the one in the foreground.
    pub running_command: Option<String>,
}

/// Per-session status cache keyed by the stable session ID rather than the
/// session index: indices shift when a tab is closed or reordered, which would
/// otherwise show one pane's directory in another pane's header.
#[derive(Default)]
pub struct PaneStatusCache {
    entries: HashMap<String, (Instant, PaneStatus)>,
}

impl PaneStatusCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cached status for `session_id`, calling `probe` only when the entry is
    /// missing or older than [`STATUS_REFRESH_INTERVAL`].
    pub fn get(
        &mut self,
        session_id: &str,
        now: Instant,
        probe: impl FnOnce() -> PaneStatus,
    ) -> &PaneStatus {
        let stale = self
            .entries
            .get(session_id)
            .is_none_or(|(fetched, _)| now.duration_since(*fetched) >= STATUS_REFRESH_INTERVAL);
        if stale {
            self.entries
                .insert(session_id.to_string(), (now, probe()));
        }
        &self.entries[session_id].1
    }

    /// Drop entries for sessions that no longer exist, so a long-lived window
    /// that opens and closes many tabs does not accumulate them.
    pub fn retain_sessions(&mut self, live: &HashSet<String>) {
        self.entries.retain(|id, _| live.contains(id));
    }
}

/// Replace a leading `$HOME` with `~`. Returns the path unchanged when it is
/// outside the home directory or `$HOME` is unset.
pub fn abbreviate_home(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) => abbreviate_prefix(path, &home),
        Err(_) => path.to_string(),
    }
}

/// The substitution itself, with `home` supplied rather than read from the
/// environment so it is testable without mutating process-wide state.
fn abbreviate_prefix(path: &str, home: &str) -> String {
    if home.is_empty() || path == home {
        return if home.is_empty() {
            path.to_string()
        } else {
            "~".to_string()
        };
    }
    match path.strip_prefix(home) {
        // Only at a component boundary: `/home/user2` merely shares a prefix
        // with `/home/user` and is a different directory.
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// Last path component, used as the pane title when the directory is deep.
/// The root directory keeps its slash so it is not rendered as an empty title.
pub fn path_leaf(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    trimmed
        .rsplit('/')
        .next()
        .filter(|leaf| !leaf.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

/// Which pane the header belongs to and how it should be drawn this frame.
pub struct PaneHeaderVisual<'a> {
    /// 1-based pane number, matching the pane-navigation keybindings.
    pub index: usize,
    pub status: &'a PaneStatus,
    pub focused: bool,
    /// This pane is the source of an in-flight rearrange drag.
    pub drag_source: bool,
    /// Releasing the pointer now would swap this pane with the drag source.
    pub drop_target: bool,
}

/// Header rect and the terminal content rect below it, for a pane occupying
/// `pane_rect`. Panes too short to give up a row keep their full height and
/// render no header, so a heavily split layout never loses its last line.
pub fn split_header(pane_rect: Rect) -> (Option<Rect>, Rect) {
    if pane_rect.height() < PANE_HEADER_HEIGHT * 3.0 {
        return (None, pane_rect);
    }
    let header = Rect::from_min_size(
        pane_rect.min,
        Vec2::new(pane_rect.width(), PANE_HEADER_HEIGHT),
    );
    let content = Rect::from_min_max(
        egui::pos2(pane_rect.left(), pane_rect.top() + PANE_HEADER_HEIGHT),
        pane_rect.max,
    );
    (Some(header), content)
}

/// Compose the one-line summary drawn after the title: the working directory,
/// then the running command when there is one.
fn detail_text(status: &PaneStatus) -> String {
    let mut parts = Vec::new();
    if let Some(cwd) = &status.cwd {
        // The title is usually the directory leaf; showing the full path adds
        // information only when it has a parent.
        if cwd != &status.title {
            parts.push(cwd.clone());
        }
    }
    if let Some(command) = &status.running_command {
        parts.push(format!("▶ {command}"));
    }
    parts.join("  ·  ")
}

/// Blend `overlay` over `base` by `t` (0 → base, 1 → overlay).
fn mix(base: Color32, overlay: Color32, t: f32) -> Color32 {
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(
        lerp(base.r(), overlay.r()),
        lerp(base.g(), overlay.g()),
        lerp(base.b(), overlay.b()),
    )
}

/// Draw one pane header. Painting rather than laying out widgets keeps the
/// header outside egui's layout flow, which the terminal renderer owns.
pub fn draw_pane_header(
    painter: &egui::Painter,
    rect: Rect,
    theme: &Theme,
    visual: PaneHeaderVisual<'_>,
) {
    let accent = Theme::rgb_to_color32(theme.tabbar.active_border);
    let base_bg = Theme::rgb_to_color32(theme.tabbar.bg);
    let bg = if visual.drop_target {
        mix(base_bg, accent, 0.45)
    } else if visual.focused {
        mix(base_bg, accent, 0.18)
    } else {
        base_bg
    };
    let text_color = if visual.focused || visual.drop_target {
        Theme::rgb_to_color32(theme.tabbar.active_text)
    } else {
        Theme::rgb_to_color32(theme.tabbar.inactive_text)
    };
    let dim_color = Theme::rgb_to_color32(theme.ui.text_disabled);

    // A pane being dragged stays legible but recedes, so the drop target is
    // the visually dominant one.
    let opacity = if visual.drag_source { 0.5 } else { 1.0 };
    let fade = |color: Color32| color.gamma_multiply(opacity);

    painter.rect_filled(rect, egui::CornerRadius::ZERO, fade(bg));
    painter.hline(
        rect.left()..=rect.right(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, fade(Theme::rgb_to_color32(theme.ui.border))),
    );

    let font = FontId::proportional(11.0);
    let padding = 6.0;
    let mut cursor_x = rect.left() + padding;
    let center_y = rect.center().y;
    let text_right = rect.right() - padding;

    // Pane number chip: the same index the "focus pane N" bindings use.
    let index_text = visual.index.to_string();
    let index_galley = painter.layout_no_wrap(index_text, font.clone(), fade(accent));
    let chip_width = index_galley.size().x + 8.0;
    let chip = Rect::from_min_size(
        egui::pos2(cursor_x, center_y - PANE_HEADER_HEIGHT * 0.35),
        Vec2::new(chip_width, PANE_HEADER_HEIGHT * 0.7),
    );
    painter.rect_filled(
        chip,
        egui::CornerRadius::same(2),
        fade(accent.gamma_multiply(if visual.focused { 0.35 } else { 0.18 })),
    );
    painter.galley(
        egui::pos2(chip.center().x - index_galley.size().x / 2.0, center_y - index_galley.size().y / 2.0),
        index_galley,
        fade(accent),
    );
    cursor_x = chip.right() + padding;

    if cursor_x >= text_right {
        return;
    }

    // Title first, then as much of the detail line as fits. Both are clipped
    // to one row: a narrow pane must never push the header taller.
    let title_galley = clipped_galley(
        painter,
        &visual.status.title,
        font.clone(),
        fade(text_color),
        (text_right - cursor_x).min(rect.width() * 0.5),
    );
    let title_width = title_galley.size().x;
    painter.galley(
        egui::pos2(cursor_x, center_y - title_galley.size().y / 2.0),
        title_galley,
        fade(text_color),
    );
    cursor_x += title_width + padding;

    let detail = detail_text(visual.status);
    if !detail.is_empty() && cursor_x < text_right {
        let detail_color = if visual.status.running_command.is_some() {
            fade(mix(dim_color, accent, 0.5))
        } else {
            fade(dim_color)
        };
        let detail_galley =
            clipped_galley(painter, &detail, font, detail_color, text_right - cursor_x);
        painter.galley(
            egui::pos2(cursor_x, center_y - detail_galley.size().y / 2.0),
            detail_galley,
            detail_color,
        );
    }

    if visual.drop_target {
        painter.text(
            egui::pos2(text_right, center_y),
            Align2::RIGHT_CENTER,
            "⇄",
            FontId::proportional(12.0),
            accent,
        );
    }
}

/// Lay `text` out on a single row, ellipsizing rather than wrapping when it
/// does not fit `max_width`.
fn clipped_galley(
    painter: &egui::Painter,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::text::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width: max_width.max(0.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    painter.layout_job(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_leaf_handles_root_and_trailing_slashes() {
        assert_eq!(path_leaf("/"), "/");
        assert_eq!(path_leaf("/home/user/projects"), "projects");
        assert_eq!(path_leaf("/home/user/projects/"), "projects");
        assert_eq!(path_leaf("~"), "~");
    }

    #[test]
    fn abbreviate_home_only_replaces_a_component_boundary() {
        assert_eq!(abbreviate_prefix("/home/user", "/home/user"), "~");
        assert_eq!(abbreviate_prefix("/home/user/src", "/home/user"), "~/src");
        // A sibling directory that merely shares the prefix must stay intact.
        assert_eq!(
            abbreviate_prefix("/home/user2/src", "/home/user"),
            "/home/user2/src"
        );
        assert_eq!(abbreviate_prefix("/etc", "/home/user"), "/etc");
        assert_eq!(abbreviate_prefix("/etc", ""), "/etc");
    }

    #[test]
    fn detail_omits_a_cwd_that_merely_repeats_the_title() {
        let status = PaneStatus {
            title: "~/src".to_string(),
            cwd: Some("~/src".to_string()),
            running_command: None,
        };
        assert_eq!(detail_text(&status), "");

        let running = PaneStatus {
            running_command: Some("cargo".to_string()),
            ..status
        };
        assert_eq!(detail_text(&running), "▶ cargo");
    }

    #[test]
    fn short_panes_keep_their_full_height_and_get_no_header() {
        let tall = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(400.0, 300.0));
        let (header, content) = split_header(tall);
        assert!(header.is_some());
        assert_eq!(content.height(), 300.0 - PANE_HEADER_HEIGHT);

        let short = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(400.0, 30.0));
        let (header, content) = split_header(short);
        assert!(header.is_none());
        assert_eq!(content, short);
    }

    #[test]
    fn status_cache_refreshes_only_after_the_interval() {
        let mut cache = PaneStatusCache::new();
        let start = Instant::now();
        let first = cache
            .get("session-a", start, || PaneStatus {
                title: "first".to_string(),
                ..Default::default()
            })
            .clone();
        assert_eq!(first.title, "first");

        let cached = cache
            .get("session-a", start + Duration::from_millis(100), || PaneStatus {
                title: "second".to_string(),
                ..Default::default()
            })
            .clone();
        assert_eq!(cached.title, "first", "probe must not run before the interval");

        let refreshed = cache
            .get("session-a", start + STATUS_REFRESH_INTERVAL, || PaneStatus {
                title: "second".to_string(),
                ..Default::default()
            })
            .clone();
        assert_eq!(refreshed.title, "second");

        cache.retain_sessions(&HashSet::new());
        assert!(cache.entries.is_empty());
    }
}
