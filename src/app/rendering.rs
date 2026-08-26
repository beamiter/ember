// Rendering coordination module

use super::state::TerminalApp;
use crate::theme::ThemeExt as _;
use crate::{command_palette, config, config_panel, layout, search_replace_panel, theme};
use eframe::egui;

const MIN_FRAME_BUDGET: usize = 16 * 1024;
const MAX_FRAME_BUDGET: usize = 256 * 1024;
const TARGET_PARSE_TIME: std::time::Duration = std::time::Duration::from_millis(4);
const MIN_ADAPTIVE_SAMPLE_BYTES: usize = 4 * 1024;

fn prune_permanently_unavailable_collapses(
    policy: &mut crate::terminal::ProjectionPolicy,
    terminal: &crate::terminal::TerminalState,
    checked: &mut Option<(u64, u64)>,
) {
    let finished_revision = terminal.finished_output_revision();
    let source = (policy.revision(), finished_revision);
    if !collapse_availability_check_needed(*checked, policy.revision(), finished_revision) {
        return;
    }
    if policy.is_identity() {
        *checked = (finished_revision != 0).then_some(source);
        return;
    }
    let stale: smallvec::SmallVec<[u64; 4]> = policy
        .collapsed_zone_ids()
        .filter(|zone_id| terminal.finished_output_range(*zone_id).is_none())
        .collect();
    for zone_id in stale {
        policy.expand(zone_id);
    }
    let finished_revision = terminal.finished_output_revision();
    *checked = (finished_revision != 0).then_some((policy.revision(), finished_revision));
}

fn collapse_availability_check_needed(
    cached: Option<(u64, u64)>,
    policy_revision: u64,
    finished_revision: u64,
) -> bool {
    finished_revision == 0 || cached != Some((policy_revision, finished_revision))
}

fn paste_confirmation_decision(
    armed: bool,
    requested: Option<bool>,
    modal_should_close: bool,
) -> Option<bool> {
    armed.then(|| requested.or_else(|| modal_should_close.then_some(false)))?
}

fn agent_input_route_is_clean(direct_input_blocked: bool, pending_input: bool) -> bool {
    !direct_input_blocked && !pending_input
}

fn terminal_frame_interaction_enabled(
    terminal_input_blocked: bool,
    frame_pointer_input_blocked: bool,
) -> bool {
    !terminal_input_blocked && !frame_pointer_input_blocked
}

fn block_search_record_is_bookmarked(
    record_id: &str,
    live_record_sequences: &std::collections::HashMap<String, u64>,
    bookmarked_sequences: &std::collections::HashSet<u64>,
) -> bool {
    live_record_sequences
        .get(record_id)
        .is_some_and(|sequence| bookmarked_sequences.contains(sequence))
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct BlockSearchRowWidgetIdentity<'a> {
    session_id: &'a str,
    record_version: crate::block_search::BlockSearchRecordVersion,
    record_id: &'a str,
    line_no: Option<usize>,
    is_output_line: bool,
}

fn block_search_row_widget_identity<'a>(
    session_id: &'a str,
    record_version: crate::block_search::BlockSearchRecordVersion,
    hit: &'a crate::block_mode::BlockSearchHit,
) -> BlockSearchRowWidgetIdentity<'a> {
    BlockSearchRowWidgetIdentity {
        session_id,
        record_version,
        record_id: &hit.record_id,
        line_no: hit.line_no,
        is_output_line: hit.is_output_line,
    }
}

/// Adjust the next PTY parsing budget from measured parser work, not from the
/// interval between UI frames. The latter includes time spent completely idle
/// and used to collapse the budget after a cursor-blink repaint.
///
/// Samples are accepted only while output is backlogged. A short final chunk
/// is dominated by fixed per-frame work and does not describe parser
/// throughput. Each update is smoothed and rate-limited so one unusually
/// expensive escape sequence cannot make the controller oscillate.
pub(crate) fn adapt_frame_budget(
    current: usize,
    processed_bytes: usize,
    parse_time: std::time::Duration,
    output_backlogged: bool,
) -> usize {
    let current = current.clamp(MIN_FRAME_BUDGET, MAX_FRAME_BUDGET);
    if !output_backlogged || processed_bytes < MIN_ADAPTIVE_SAMPLE_BYTES || parse_time.is_zero() {
        return current;
    }

    let estimated = (processed_bytes as u128)
        .saturating_mul(TARGET_PARSE_TIME.as_nanos())
        .checked_div(parse_time.as_nanos())
        .unwrap_or(MAX_FRAME_BUDGET as u128)
        .min(usize::MAX as u128) as usize;
    let desired = estimated.clamp(MIN_FRAME_BUDGET, MAX_FRAME_BUDGET);

    // Limit one observation to ±25%, then move one quarter of the way toward
    // it. This behaves as a small EWMA without storing a second floating-point
    // state value in TerminalApp.
    let lower = current.saturating_mul(3) / 4;
    let upper = current.saturating_mul(5) / 4;
    let limited = desired.clamp(lower, upper);
    ((current.saturating_mul(3) + limited) / 4).clamp(MIN_FRAME_BUDGET, MAX_FRAME_BUDGET)
}

impl TerminalApp {
    /// Keep exactly one renderer per pane while split mode is active. This
    /// removes the old four-pane ceiling and also releases texture caches when
    /// panes are closed. Zoom keeps the underlying split renderers warm.
    fn ensure_pane_renderer_capacity(&mut self, ctx: &egui::Context) {
        let pane_count = self.layout().panes.len();
        let required = if pane_count > 1 { pane_count } else { 0 };
        self.pane_renderers.truncate(required);
        while self.pane_renderers.len() < required {
            let mut renderer = crate::ui::TerminalRenderer::new(
                self.renderer.font_size,
                self.renderer.padding,
                self.renderer.line_spacing,
                self.renderer.scrollbar_visibility.clone(),
                self.renderer.theme.clone(),
            );
            renderer.opacity = self.renderer.opacity;
            renderer.font_ligatures = self.renderer.font_ligatures;
            renderer.click_moves_cursor = self.renderer.click_moves_cursor;
            renderer.block_mode = self.renderer.block_mode;
            renderer.block_compact = self.renderer.block_compact;
            renderer.gpu_rendering = self.renderer.gpu_rendering;
            renderer.wgpu_render_state = self.renderer.wgpu_render_state.clone();
            renderer.sync_font_metrics(ctx);
            self.pane_renderers.push(renderer);
        }
    }

    /// Assemble one pane's header line.
    ///
    /// The working directory and the foreground command are read from `/proc`,
    /// so the result goes through a per-session cache that refreshes a few
    /// times a second instead of on every frame.
    fn pane_status(
        &mut self,
        session_idx: usize,
        now: std::time::Instant,
    ) -> crate::pane_header::PaneStatus {
        let Some(session) = self.session_manager.sessions().get(session_idx) else {
            return crate::pane_header::PaneStatus::default();
        };
        let session_id = session.metadata.session_id.clone();
        let custom_name = session
            .metadata
            .custom_name
            .clone()
            .filter(|name| !name.is_empty());
        let fallback_name = session.metadata.name.clone();
        let shell_pid = session.get_shell_pid();
        // OSC 7 outranks /proc: under ssh or tmux the local shell's own cwd
        // does not describe where the user actually is.
        let (reported_cwd, reported_command) = {
            let terminal = session.terminal.lock();
            (
                terminal.current_working_dir.clone(),
                terminal.running_command().map(str::to_string),
            )
        };

        let git_strip_cache = &mut self.git_strip_cache;
        // The pane headers and the bottom bar share one probe; skipping it
        // entirely needs both consumers switched off.
        let probe_git = self.config.show_repo_strip || self.config.bottom_bar;
        self.pane_status_cache
            .get(&session_id, now, || {
                let raw_cwd = reported_cwd.or_else(|| jterm_core::process::process_cwd(shell_pid));
                // The git probe rides the same sub-second cadence as the /proc
                // reads, and its own cache only runs git when the session is
                // new, changed directory, or finished a command.
                let git = probe_git
                    .then(|| {
                        git_strip_cache.meta(&session_id, raw_cwd.as_deref(), |cwd| {
                            jterm_core::git_meta::read(std::path::Path::new(cwd))
                        })
                    })
                    .flatten();
                let cwd = raw_cwd.map(|cwd| crate::pane_header::abbreviate_home(&cwd));
                let title = custom_name
                    .or_else(|| cwd.as_deref().map(crate::pane_header::path_leaf))
                    .unwrap_or(fallback_name);
                // Shells without OSC 133 integration report no command; the
                // PTY's foreground process group still names one.
                let running_command = reported_command
                    .or_else(|| crate::session_manager::get_foreground_command(shell_pid))
                    .map(|command| crate::review_text::visible_bounded(&command, 512));
                crate::pane_header::PaneStatus {
                    title,
                    cwd,
                    running_command,
                    git,
                }
            })
            .clone()
    }

    /// Draw the family-wide bottom status bar across the full window width.
    ///
    /// Declared before the sidebar (like the top bar) so egui hands it the
    /// entire bottom edge; the CentralPanel then re-grids the terminal from
    /// whatever height remains. Content is composed by
    /// `jterm_core::bottom_bar` from the focused session's state, so the bar
    /// reads the same in every jterm.
    pub(crate) fn render_bottom_bar(&mut self, root_ui: &mut egui::Ui) {
        if !self.config.bottom_bar {
            return;
        }
        let active_idx = self.session_manager.active_index();
        let status = self.pane_status(active_idx, std::time::Instant::now());

        // The last *complete* record carries the exit/duration to show. Only
        // a tail record past its C mark counts as running: a Prompt/Editing
        // record is merely the shell waiting at an idle prompt, and treating
        // it as running would pin an ellipsis to the bar forever.
        let (cols, rows, last_exit, last_duration_ms, tail_running) =
            match self.session_manager.sessions().get(active_idx) {
                Some(session) => {
                    let terminal = session.terminal.lock();
                    let records = terminal.command_records();
                    let last = records.iter().rev().find(|record| record.complete);
                    (
                        terminal.grid.row_len() as u16,
                        terminal.grid.rows() as u16,
                        last.and_then(|record| record.exit_code),
                        last.and_then(|record| record.duration_ms),
                        records.back().is_some_and(|record| {
                            record.state == crate::terminal::CommandState::Running
                        }),
                    )
                }
                None => (0, 0, None, None, false),
            };

        let snapshot = jterm_core::bottom_bar::Snapshot {
            // PaneStatus.cwd is already `~`-abbreviated, so compose needs no
            // home directory to collapse it again.
            cwd: status.cwd.as_deref().map(std::path::Path::new),
            home: None,
            git: status.git.as_ref(),
            running: status.running_command.is_some() || tail_running,
            last_exit,
            last_duration_ms,
            cols,
            rows,
            tab_index: self.tabs.active_index(),
            tab_count: self.tabs.len(),
        };
        let content = jterm_core::bottom_bar::compose(&snapshot);

        egui::Panel::bottom("bottom_bar")
            .exact_size(jterm_core::bottom_bar::BAR_HEIGHT)
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show(root_ui, |ui| {
                crate::bottom_bar::draw(ui, &self.current_theme, &content);
            });
    }

    /// Draw the per-pane header strips and run the drag-to-rearrange gesture.
    ///
    /// Pressing a header focuses its pane through the ordinary click-to-focus
    /// path; dragging it onto another pane swaps the two sessions. Only the
    /// contents move — the split geometry the user arranged stays put.
    fn render_pane_headers(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        panes: &[layout::Pane],
        pane_chrome: &[(Option<egui::Rect>, egui::Rect)],
        interaction_enabled: bool,
    ) {
        // Keep the status cache from growing with a long-lived window's tab churn.
        let live_session_ids: std::collections::HashSet<String> = self
            .session_manager
            .sessions()
            .iter()
            .map(|session| session.metadata.session_id.clone())
            .collect();
        self.pane_status_cache.retain_sessions(&live_session_ids);
        self.git_strip_cache.retain_sessions(&live_session_ids);

        if !interaction_enabled {
            self.pane_drag = None;
        }

        let now = std::time::Instant::now();
        let mut statuses: Vec<crate::pane_header::PaneStatus> = panes
            .iter()
            .map(|pane| self.pane_status(pane.session_idx, now))
            .collect();
        // The status may carry git metadata probed for the bottom bar; the
        // headers' repo strip stays gated by its own toggle.
        if !self.config.show_repo_strip {
            for status in &mut statuses {
                status.git = None;
            }
        }

        let handles: Vec<(usize, egui::Response)> = panes
            .iter()
            .enumerate()
            .filter_map(|(pane_idx, pane)| {
                let header_rect = pane_chrome[pane_idx].0?;
                let response = ui
                    .interact(
                        header_rect,
                        ui.id().with(("pane-header", pane.session_idx)),
                        egui::Sense::click_and_drag(),
                    )
                    .on_hover_text(
                        "Drag onto another pane to swap · drag to the tab bar to make a tab",
                    );
                Some((pane.session_idx, response))
            })
            .collect();

        let pointer_pos = ctx.input(|input| {
            super::tabs::workspace_drag_pointer_pos(
                input.pointer.interact_pos(),
                input.pointer.hover_pos(),
            )
        });

        if interaction_enabled {
            if self.pane_drag.is_none() && self.dragging_tab_session_id.is_none() {
                if let Some((session_idx, origin)) = handles.iter().find_map(|(idx, response)| {
                    response
                        .drag_started()
                        .then(|| response.interact_pointer_pos().map(|pos| (*idx, pos)))
                        .flatten()
                }) {
                    // Anchor the drag to the session's stable ID: a background
                    // shell can exit mid-drag, shifting every later index.
                    if let Some(session) = self.session_manager.sessions().get(session_idx) {
                        self.pane_drag = Some(super::state::PaneDrag {
                            session_id: session.metadata.session_id.clone(),
                            origin,
                            active: false,
                        });
                    }
                }
            }
            if let (Some(drag), Some(pos)) = (self.pane_drag.as_mut(), pointer_pos) {
                if !drag.active
                    && (pos - drag.origin).length() > crate::pane_header::PANE_DRAG_THRESHOLD
                {
                    drag.active = true;
                }
            }
        }

        let drag_source = self
            .pane_drag
            .as_ref()
            .filter(|drag| drag.active)
            .and_then(|drag| self.session_manager.index_of(&drag.session_id))
            .filter(|session_idx| panes.iter().any(|pane| pane.session_idx == *session_idx));
        let drop_target = drag_source.and_then(|source| {
            pointer_pos
                .and_then(|pos| self.layout().session_at(pos))
                .filter(|target| *target != source)
        });
        let tab_bar_drop_target = drag_source.filter(|source| {
            self.tabs
                .tab_of_session(*source)
                .is_some_and(|tab_idx| self.tabs.sessions_in(tab_idx).len() > 1)
                && pointer_pos.is_some_and(|pos| {
                    self.tab_bar_drop_rects
                        .iter()
                        .any(|rect| rect.contains(pos))
                })
        });

        if drag_source.is_some() {
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        }

        let painter = ui.painter();
        let accent = crate::theme::Theme::rgb_to_color32(self.current_theme.tabbar.active_border);
        for (pane_idx, pane) in panes.iter().enumerate() {
            let is_target = drop_target == Some(pane.session_idx);
            if is_target {
                // Tint the whole pane, not just its strip: the strip is only a
                // few pixels tall and the pointer is usually far from it.
                painter.rect_filled(
                    pane.rect,
                    egui::CornerRadius::ZERO,
                    accent.gamma_multiply(0.15),
                );
                painter.rect_stroke(
                    pane.rect.shrink(1.0),
                    egui::CornerRadius::ZERO,
                    egui::Stroke::new(2.0, accent),
                    egui::StrokeKind::Inside,
                );
            }
            let Some(header_rect) = pane_chrome[pane_idx].0 else {
                continue;
            };
            crate::pane_header::draw_pane_header(
                painter,
                header_rect,
                &self.current_theme,
                crate::pane_header::PaneHeaderVisual {
                    index: pane_idx + 1,
                    status: &statuses[pane_idx],
                    focused: pane.focused,
                    drag_source: drag_source == Some(pane.session_idx),
                    drop_target: is_target,
                },
            );
        }

        // Resolve the gesture only after painting, so the swap's new geometry
        // is drawn by the next frame rather than half-applied to this one.
        if self.pane_drag.is_some() && ctx.input(|input| input.pointer.any_released()) {
            if let Some(source) = tab_bar_drop_target {
                if self.tabs.promote_split_pane_to_tab(source) {
                    self.renaming_tab = None;
                    self.sync_active_session_to_focused_pane();
                    self.force_resize_session = true;
                    self.schedule_session_save();
                    self.set_status("Moved pane to a new tab");
                    ctx.request_repaint();
                }
            } else if let (Some(source), Some(target)) = (drag_source, drop_target) {
                if self.layout_mut().swap_sessions(source, target) {
                    self.sync_active_session_to_focused_pane();
                    self.schedule_session_save();
                    self.set_status("Swapped panes");
                    ctx.request_repaint();
                }
            }
            self.pane_drag = None;
        }
    }

    /// Paint and consume the tab-to-pane half of workspace drag/drop. The
    /// source is re-resolved from its stable session ID on every frame; target
    /// indices come from the current active layout, so background session exits
    /// cannot redirect a drop to a different PTY.
    fn render_tab_to_pane_drop_zones(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        pane_targets: &[(usize, egui::Rect)],
        interaction_enabled: bool,
    ) {
        if self.dragging_tab_session_id.is_none() {
            return;
        }

        let released = ctx.input(|input| input.pointer.any_released());
        let active = interaction_enabled && self.tab_drag_is_active(ctx);
        let source = active
            .then(|| self.resolved_dragging_tab())
            .flatten()
            .filter(|(source_tab_idx, source_session_idx)| {
                self.tabs.sessions_in(*source_tab_idx) == vec![*source_session_idx]
                    && *source_tab_idx != self.tabs.active_index()
            });
        let pointer_pos = ctx.input(|input| {
            super::tabs::workspace_drag_pointer_pos(
                input.pointer.interact_pos(),
                input.pointer.hover_pos(),
            )
        });
        let hovered_target = source.and_then(|_| {
            pointer_pos.and_then(|pos| {
                pane_targets
                    .iter()
                    .find(|(_, rect)| rect.contains(pos))
                    .copied()
            })
        });
        let minimum_pane_size = self.renderer.minimum_split_pane_size();
        let selected_drop = hovered_target.and_then(|(target_session_idx, target_rect)| {
            let direction = pointer_pos.and_then(|pos| layout::pane_drop_zone(target_rect, pos))?;
            self.layout()
                .can_split_session_pane(
                    target_session_idx,
                    direction.horizontal(),
                    minimum_pane_size,
                )
                .then_some((target_session_idx, direction))
        });

        if let Some((target_session_idx, target_rect)) = hovered_target {
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            let accent =
                crate::theme::Theme::rgb_to_color32(self.current_theme.tabbar.active_border);
            let painter = ui.painter();
            for (direction, arrow) in [
                (layout::PaneDropDirection::Left, "←"),
                (layout::PaneDropDirection::Right, "→"),
                (layout::PaneDropDirection::Top, "↑"),
                (layout::PaneDropDirection::Bottom, "↓"),
            ] {
                let valid = self.layout().can_split_session_pane(
                    target_session_idx,
                    direction.horizontal(),
                    minimum_pane_size,
                );
                let Some(zone) = layout::pane_drop_zone_rect(target_rect, direction) else {
                    continue;
                };
                let selected = selected_drop == Some((target_session_idx, direction));
                painter.rect_filled(
                    zone.shrink(2.0),
                    egui::CornerRadius::same(4),
                    accent.gamma_multiply(if selected {
                        0.32
                    } else if valid {
                        0.10
                    } else {
                        0.03
                    }),
                );
                painter.rect_stroke(
                    zone.shrink(2.0),
                    egui::CornerRadius::same(4),
                    egui::Stroke::new(
                        if selected { 2.0 } else { 1.0 },
                        accent.gamma_multiply(if valid { 0.9 } else { 0.25 }),
                    ),
                    egui::StrokeKind::Inside,
                );
                painter.text(
                    zone.center(),
                    egui::Align2::CENTER_CENTER,
                    arrow,
                    egui::FontId::proportional(18.0),
                    accent.gamma_multiply(if valid { 1.0 } else { 0.3 }),
                );
            }
            ctx.request_repaint();
        }

        // CentralPanel is rendered after both tab bars, so it is the final
        // consumer for a tab release outside the reorder strips. Success or
        // failure, release is one-shot and an invalid/self drop is a no-op.
        if released {
            let mut moved = false;
            if let (Some((_, source_session_idx)), Some((target_session_idx, direction))) =
                (source, selected_drop)
            {
                if self.tabs.move_single_pane_tab_to_split(
                    source_session_idx,
                    target_session_idx,
                    direction,
                ) {
                    self.renaming_tab = None;
                    self.sync_active_session_to_focused_pane();
                    self.force_resize_session = true;
                    self.schedule_session_save();
                    self.set_status("Moved tab into a split pane");
                    ctx.request_repaint();
                    moved = true;
                }
            }
            if moved {
                self.finish_workspace_drag();
            } else {
                self.clear_workspace_drag();
            }
        }
    }

    pub fn render_terminal_content(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        frame_pointer_input_blocked: bool,
    ) {
        let interaction_enabled = terminal_frame_interaction_enabled(
            self.terminal_input_blocked(ctx),
            frame_pointer_input_blocked,
        );
        let workspace_drag_active = self.tab_drag_is_active(ctx) || self.pane_drag.is_some();
        let terminal_interaction_enabled = interaction_enabled && !workspace_drag_active;
        if !terminal_interaction_enabled {
            self.dragging_divider = None;
        }
        // 终端显示区域
        self.renderer.sync_font_metrics(ctx);
        let available_rect = ui.available_rect_before_wrap();
        // Keep the focused pane's geometry current even in single-pane mode,
        // so split commands can validate the resulting child sizes before
        // creating another shell session.
        self.layout_mut().compute_pane_rects(available_rect);
        let pane_drop_targets: Vec<(usize, egui::Rect)> = {
            let panes = self.layout().panes();
            let multi_pane = panes.len() > 1;
            panes
                .iter()
                .map(|pane| {
                    let content_rect = if multi_pane {
                        crate::pane_header::split_header(pane.rect).1
                    } else {
                        pane.rect
                    };
                    (pane.session_idx, content_rect)
                })
                .collect()
        };
        self.ensure_pane_renderer_capacity(ctx);
        let (cols, rows) = self.renderer.grid_dimensions(ui.available_size());
        crate::debug_log!("[RESIZE] grid_dimensions => {}x{}", cols, rows);

        // 单窗格才按整窗口尺寸 resize 活跃会话;多窗格时各窗格在下方
        // 各自按自己的 rect 尺寸 resize(否则活跃会话会被错误地撑成整窗口大小)。
        let multi_pane = self.layout().panes().len() > 1;
        if !multi_pane && (cols != self.cols || rows != self.rows || self.force_resize_session) {
            let session = self.session_manager.get_active_session_mut();
            let _ = session.shell.resize(cols, rows);
            let mut terminal = session.terminal.lock();
            terminal.on_resize(cols, rows);
            self.cols = cols;
            self.rows = rows;
            if self.force_resize_session {
                // Session 切换时重置 renderer 的 IME 状态缓存
                // 这样下一帧会重新发送 IMEAllowed(true)，确保 IME 不会丢失
                self.renderer.reset_ime_state();
            }
            self.force_resize_session = false;
        }

        // Block-mode gutter clicks reported by whichever renderer drew the
        // clicked pane this frame, applied after every pane has rendered.
        let mut pending_block_click: Option<(String, crate::block_mode::BlockClick)> = None;
        let mut pending_block_menu: Option<(String, crate::block_mode::BlockMenuRequest)> = None;

        // 多窗格支持：如果有多于一个窗格，则进行分屏渲染
        if self.layout().panes().len() > 1 {
            // 获取所有窗格信息
            let panes = self.layout().panes().to_vec();
            let divider_rects = self.layout().get_divider_rects();
            let inactive_search = crate::search::SearchState::default();

            // 每个窗格顶部让出一条状态栏；终端内容渲染在它下方的矩形里，
            // 于是 shell 的 grid 尺寸、鼠标坐标映射、链接命中都基于同一个
            // content rect,不会被标题栏挤偏一行。
            let pane_chrome: Vec<(Option<egui::Rect>, egui::Rect)> = panes
                .iter()
                .map(|pane| crate::pane_header::split_header(pane.rect))
                .collect();

            // 为每个窗格渲染
            for (pane_idx, pane) in panes.iter().enumerate() {
                if pane_idx >= self.pane_renderers.len() {
                    break;
                }

                let content_rect = pane_chrome[pane_idx].1;
                let session_idx = pane.session_idx;
                // 按本窗格 rect 的尺寸 resize 该窗格会话的 shell + 终端 grid,
                // 否则窗格内的 shell 仍以为自己拥有整窗口宽高,导致换行/清屏错乱。
                let (pane_cols, pane_rows) =
                    self.pane_renderers[pane_idx].grid_dimensions(content_rect.size());
                if let Some(session) = self.session_manager.get_session_mut(session_idx) {
                    let terminal_ptr = std::sync::Arc::as_ptr(&session.terminal) as usize;
                    let terminal_arc = std::sync::Arc::clone(&session.terminal);
                    let mut terminal_guard = terminal_arc.lock();
                    if pane_cols != terminal_guard.grid.row_len()
                        || pane_rows != terminal_guard.grid.rows()
                    {
                        terminal_guard.on_resize(pane_cols, pane_rows);
                        let _ = session.shell.resize(pane_cols, pane_rows);
                    }
                    prune_permanently_unavailable_collapses(
                        &mut session.projection_policy,
                        &terminal_guard,
                        &mut session.collapse_availability_cache,
                    );
                    let projection_policy = &session.projection_policy;
                    let projection_view_state = &mut session.projection_view_state;
                    // per-pane 链接缓存:仅当 grid 或滚动变化时重建,避免每帧重做
                    // 链接检测(含逐行 String 分配)。失效条件与单窗格路径一致。
                    let renderer = &mut self.pane_renderers[pane_idx];
                    let viewport = renderer.projected_viewport_with_state(
                        &mut terminal_guard,
                        projection_policy,
                        projection_view_state,
                    );
                    renderer.set_projection_frame(
                        &terminal_guard,
                        viewport.clone(),
                        projection_policy,
                    );
                    let projection_key = viewport.key();
                    if renderer.cached_links_projection_key != Some(projection_key)
                        || terminal_ptr != renderer.cached_links_terminal_ptr
                    {
                        renderer.cached_links = std::sync::Arc::new(
                            self.link_detector
                                .detect_links_in_visible_cells_with_wrapping(
                                    viewport.cells(),
                                    viewport.row_wrapped(),
                                ),
                        );
                        renderer.cached_links_projection_key = Some(projection_key);
                        renderer.cached_links_terminal_ptr = terminal_ptr;
                    }
                    // O(1) clone Arc,规避 &mut renderer 与 &renderer.cached_links 借用冲突。
                    let links = renderer.cached_links.clone();
                    let pane_cursor_visible = terminal_guard.is_cursor_visible()
                        && (!pane.focused || self.cursor_visible);
                    let pane_search = if pane.focused {
                        &self.search_state
                    } else {
                        &inactive_search
                    };
                    let pane_hovered_link = if pane.focused {
                        &self.hovered_link
                    } else {
                        &None
                    };

                    // 本 pane 的会话若持有 block 选中,让 renderer 高亮它。
                    let pane_block_selection = self
                        .block_selection
                        .as_ref()
                        .filter(|selection| selection.session_id == session.metadata.session_id);
                    renderer.set_block_selection(pane_block_selection);
                    renderer.set_block_bookmarks(
                        self.block_bookmarks.get(&session.metadata.session_id),
                    );

                    // 在指定矩形内渲染（多窗格模式专用方法）
                    renderer.render_in_rect(
                        ui,
                        &mut terminal_guard,
                        terminal_interaction_enabled,
                        terminal_interaction_enabled && pane.focused,
                        pane_cursor_visible,
                        pane_search,
                        &links,
                        pane_hovered_link,
                        content_rect,
                    );
                    if let Some(request) = renderer.take_projected_scroll_request() {
                        match request {
                            crate::ui::ProjectedScrollRequest::SetOffset(offset) => {
                                projection_view_state.set_offset(offset, &viewport);
                            }
                            crate::ui::ProjectedScrollRequest::Delta(lines) => {
                                projection_view_state.scroll(lines, &viewport);
                            }
                        }
                    }

                    if let Some(click) = renderer.block_click.take() {
                        pending_block_click = Some((session.metadata.session_id.clone(), click));
                    }
                    if let Some(action) = renderer.block_menu_action.take() {
                        pending_block_menu = Some((session.metadata.session_id.clone(), action));
                    }
                }
            }

            // 窗格标题栏。注册在分隔线之前:两者的命中区在窗格顶角重叠,
            // 后注册的分隔线在那里胜出,拖动边界不会被标题栏抢走。
            self.render_pane_headers(ui, ctx, &panes, &pane_chrome, interaction_enabled);

            // 用主题强调色标出当前输入 pane。边框画在终端内容之后，确保
            // GPU/Glow 两条渲染路径下都不会被背景覆盖。
            let painter = ui.painter();
            if let Some(focused_pane) = panes.iter().find(|pane| pane.focused) {
                painter.rect_stroke(
                    focused_pane.rect.shrink(1.0),
                    egui::CornerRadius::ZERO,
                    egui::Stroke::new(
                        1.5,
                        crate::theme::Theme::rgb_to_color32(
                            self.current_theme.tabbar.active_border,
                        ),
                    ),
                    egui::StrokeKind::Inside,
                );
            }

            // 给分隔线注册真正的交互控件。它们晚于 terminal response 注册，
            // 因此双击/拖动不会穿透到相邻终端触发选词或鼠标协议。
            let divider_interactions: Vec<(layout::SplitDivider, egui::Response)> = divider_rects
                .iter()
                .map(|divider| {
                    let cursor = match divider.axis {
                        layout::SplitAxis::Vertical => egui::CursorIcon::ResizeHorizontal,
                        layout::SplitAxis::Horizontal => egui::CursorIcon::ResizeVertical,
                    };
                    let response = ui
                        .interact(
                            divider.rect,
                            ui.id().with(("terminal-split-divider", &divider.id)),
                            egui::Sense::click_and_drag(),
                        )
                        .on_hover_cursor(cursor)
                        .on_hover_text("Drag to resize · double-click to reset");
                    (divider.clone(), response)
                })
                .collect();
            let hovered_divider = divider_interactions
                .iter()
                .find(|(_, response)| response.hovered())
                .map(|(divider, _)| divider.clone());
            let active_divider = self.dragging_divider.as_ref().and_then(|split_id| {
                divider_rects
                    .iter()
                    .find(|divider| &divider.id == split_id)
                    .cloned()
            });
            if let Some(divider) = active_divider.or_else(|| hovered_divider.clone()) {
                ctx.set_cursor_icon(match divider.axis {
                    layout::SplitAxis::Vertical => egui::CursorIcon::ResizeHorizontal,
                    layout::SplitAxis::Horizontal => egui::CursorIcon::ResizeVertical,
                });
            }

            // 命中区域为 10px，但只画细线；hover/drag 时加粗并使用强调色。
            for divider in &divider_rects {
                let highlighted = self.dragging_divider.as_ref() == Some(&divider.id)
                    || hovered_divider
                        .as_ref()
                        .is_some_and(|hovered| hovered.id == divider.id);
                let divider_color = if highlighted {
                    crate::theme::Theme::rgb_to_color32(self.current_theme.tabbar.active_border)
                } else {
                    crate::theme::Theme::rgb_to_color32(self.current_theme.ui.border)
                };
                let stroke = egui::Stroke::new(if highlighted { 2.0 } else { 1.0 }, divider_color);
                let center = divider.rect.center();
                match divider.axis {
                    layout::SplitAxis::Vertical => {
                        painter.vline(
                            center.x,
                            divider.container_rect.top()..=divider.container_rect.bottom(),
                            stroke,
                        );
                    }
                    layout::SplitAxis::Horizontal => {
                        painter.hline(
                            divider.container_rect.left()..=divider.container_rect.right(),
                            center.y,
                            stroke,
                        );
                    }
                }
            }

            // 双击恢复 50/50；普通按下则锁定最深层分隔线，拖出命中区域后
            // 仍继续调整同一个 split。
            let double_clicked_divider = terminal_interaction_enabled.then(|| {
                divider_interactions
                    .iter()
                    .find(|(_, response)| response.double_clicked_by(egui::PointerButton::Primary))
                    .map(|(divider, _)| divider.clone())
            });
            if let Some(divider) = double_clicked_divider.flatten() {
                if self.layout_mut().set_split_ratio(&divider.id, 0.5) {
                    self.schedule_session_save();
                }
                self.dragging_divider = None;
                self.set_status("Reset split to 50/50");
                ctx.request_repaint();
            } else if terminal_interaction_enabled && self.dragging_divider.is_none() {
                self.dragging_divider = divider_interactions
                    .iter()
                    .find(|(_, response)| response.is_pointer_button_down_on())
                    .map(|(divider, _)| divider.id.clone());
            }

            if terminal_interaction_enabled {
                if let Some(split_id) = self.dragging_divider.clone() {
                    // The layout resolves the divider's own node rectangle and
                    // snaps near even pair splits.
                    if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                        if self.layout_mut().drag_divider_to(&split_id, pos) {
                            self.schedule_session_save();
                        }
                    }
                }
                if ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary)) {
                    self.dragging_divider = None;
                }
            }

            // 点击某个窗格 → 切换输入焦点到该窗格(忽略落在分隔线上的点击,
            // 那是用于拖拽调整比例的)。
            if terminal_interaction_enabled
                && self.dragging_divider.is_none()
                && ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary))
            {
                if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                    let on_divider = divider_rects
                        .iter()
                        .any(|divider| divider.rect.contains(pos));
                    if !on_divider && self.layout_mut().focus_pane_at(pos).is_some() {
                        self.sync_active_session_to_focused_pane();
                    }
                }
            }
        } else {
            // 单窗格渲染（原有逻辑）
            {
                let session = self.session_manager.get_active_session_mut();
                let session_id = session.metadata.session_id.clone();
                let block_selection = self
                    .block_selection
                    .as_ref()
                    .filter(|selection| selection.session_id == session_id);
                self.renderer.set_block_selection(block_selection);
                self.renderer
                    .set_block_bookmarks(self.block_bookmarks.get(&session_id));
                let terminal_ptr = std::sync::Arc::as_ptr(&session.terminal) as usize;
                let terminal_arc = std::sync::Arc::clone(&session.terminal);
                let mut terminal_guard = terminal_arc.lock();
                prune_permanently_unavailable_collapses(
                    &mut session.projection_policy,
                    &terminal_guard,
                    &mut session.collapse_availability_cache,
                );
                let projection_policy = &session.projection_policy;
                let projection_view_state = &mut session.projection_view_state;

                // 获取链接列表用于渲染（使用缓存）
                let viewport = self.renderer.projected_viewport_with_state(
                    &mut terminal_guard,
                    projection_policy,
                    projection_view_state,
                );
                self.renderer.set_projection_frame(
                    &terminal_guard,
                    viewport.clone(),
                    projection_policy,
                );
                let projection_key = viewport.key();

                if self.cached_links_projection_key != Some(projection_key)
                    || terminal_ptr != self.cached_links_terminal_ptr
                {
                    self.cached_links = self
                        .link_detector
                        .detect_links_in_visible_cells_with_wrapping(
                            viewport.cells(),
                            viewport.row_wrapped(),
                        );
                    self.cached_links_projection_key = Some(projection_key);
                    self.cached_links_terminal_ptr = terminal_ptr;
                }
                self.renderer.render(
                    ui,
                    &mut terminal_guard,
                    terminal_interaction_enabled,
                    self.cursor_visible,
                    &self.search_state,
                    &self.cached_links,
                    &self.hovered_link,
                );
                if let Some(request) = self.renderer.take_projected_scroll_request() {
                    match request {
                        crate::ui::ProjectedScrollRequest::SetOffset(offset) => {
                            projection_view_state.set_offset(offset, &viewport);
                        }
                        crate::ui::ProjectedScrollRequest::Delta(lines) => {
                            projection_view_state.scroll(lines, &viewport);
                        }
                    }
                }
                drop(terminal_guard);
                if let Some(click) = self.renderer.block_click.take() {
                    pending_block_click = Some((session_id.clone(), click));
                }
                if let Some(action) = self.renderer.block_menu_action.take() {
                    pending_block_menu = Some((session_id, action));
                }
            }
        }

        self.render_tab_to_pane_drop_zones(ui, ctx, &pane_drop_targets, interaction_enabled);

        if let Some((session_id, click)) = pending_block_click {
            match click {
                crate::block_mode::BlockClick::Select { record_id, gesture } => {
                    self.apply_block_pointer_selection(&session_id, &record_id, gesture);
                }
                crate::block_mode::BlockClick::Clear => {
                    // Full-duplex sync: deselecting either view also drops
                    // the Commands-sidebar row highlight it mirrored.
                    self.clear_block_selection();
                }
            }
        }
        if let Some((session_id, request)) = pending_block_menu {
            self.execute_block_menu_action(&session_id, request);
        }
    }

    #[allow(deprecated)]
    pub fn render_floating_panels(&mut self, ctx: &egui::Context) {
        const LIVE_SEARCH_REFRESH_INTERVAL: std::time::Duration =
            std::time::Duration::from_millis(300);
        if self.search_state.is_open && self.search_state.projection_message.is_some() {
            let (session_id, policy_revision) = {
                let session = self.session_manager.get_active_session_mut();
                (
                    session.metadata.session_id.clone(),
                    session.projection_policy.revision(),
                )
            };
            if !self
                .search_state
                .projection_diagnostic_is_current(&session_id, policy_revision)
            {
                self.reveal_current_search_match();
            }
        }
        let (search_needs_refresh, delayed_refresh) = if self.search_state.is_open {
            let session_idx = self.session_manager.active_index();
            let (grid_version, session_id) = {
                let session = self.session_manager.get_active_session_mut();
                (
                    session.terminal.lock().get_grid_version(),
                    session.metadata.session_id.clone(),
                )
            };
            let session_changed = self.search_state.results_session_idx != Some(session_idx)
                || self.search_state.results_session_id.as_deref() != Some(session_id.as_str());
            let grid_changed = self.search_state.results_grid_version != Some(grid_version);
            let elapsed = self
                .search_state
                .results_refreshed_at
                .map(|refreshed| refreshed.elapsed())
                .unwrap_or(LIVE_SEARCH_REFRESH_INTERVAL);
            (
                session_changed || (grid_changed && elapsed >= LIVE_SEARCH_REFRESH_INTERVAL),
                grid_changed
                    .then_some(LIVE_SEARCH_REFRESH_INTERVAL.saturating_sub(elapsed))
                    .filter(|remaining| !remaining.is_zero()),
            )
        } else {
            (false, None)
        };
        if search_needs_refresh {
            self.refresh_search_matches();
        } else if let Some(delay) = delayed_refresh {
            ctx.request_repaint_after(delay);
        }

        // 搜索面板 UI（浮动窗口，右上角）
        if self.search_state.is_open {
            let screen_rect = ctx.viewport_rect();
            let search_width = (screen_rect.width() - 24.0).clamp(300.0, 520.0);
            let search_height = if self.search_state.error_message.is_some()
                || self.search_state.projection_message.is_some()
                || self.search_state.results_truncated
            {
                82.0
            } else {
                52.0
            };
            let mut reveal_hidden_match = false;
            egui::Window::new("Search")
                .title_bar(false)
                .resizable(false)
                .default_pos(egui::pos2(
                    (screen_rect.right() - search_width - 12.0).max(screen_rect.left() + 12.0),
                    screen_rect.top() + 48.0,
                ))
                .default_size([search_width, search_height])
                .fixed_size([search_width, search_height])
                .frame(egui::Frame {
                    fill: crate::theme::Theme::rgb_to_color32(self.current_theme.search.bg),
                    stroke: egui::Stroke::new(
                        1.0,
                        crate::theme::Theme::rgb_to_color32(self.current_theme.search.border),
                    ),
                    corner_radius: egui::CornerRadius::same(8),
                    inner_margin: egui::Margin::same(6),
                    ..Default::default()
                })
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        // 搜索输入框
                        ui.label("Search:");
                        let search_response = ui.text_edit_singleline(&mut self.search_state.query);

                        // 自动 focus 搜索框
                        if self.search_state.search_focused {
                            ui.memory_mut(|mem| mem.request_focus(search_response.id));
                            self.search_state.search_focused = false;
                        }

                        // Aa / .* 切换按钮:用 selectable_label 表达 on/off 状态。
                        // 切换后需要立刻按新选项重新搜索,否则用户看不到效果。
                        let case_btn = ui
                            .selectable_label(self.search_state.case_sensitive, "Aa")
                            .on_hover_text("区分大小写 (Match Case)");
                        if case_btn.clicked() {
                            self.search_state.case_sensitive = !self.search_state.case_sensitive;
                        }
                        let regex_btn = ui
                            .selectable_label(self.search_state.use_regex, ".*")
                            .on_hover_text("正则表达式 (Regex)");
                        if regex_btn.clicked() {
                            self.search_state.use_regex = !self.search_state.use_regex;
                        }

                        if search_response.changed() || case_btn.clicked() || regex_btn.clicked() {
                            self.refresh_search_matches();
                        }

                        // 显示匹配计数
                        if !self.search_state.matches.is_empty() {
                            ui.label(format!(
                                "{}/{}{}",
                                self.search_state.current_match_index + 1,
                                self.search_state.matches.len(),
                                if self.search_state.results_truncated {
                                    "+"
                                } else {
                                    ""
                                }
                            ));
                        } else if !self.search_state.query.is_empty() {
                            ui.label("No matches");
                        }

                        // 上一个/下一个 按钮
                        if ui
                            .button("↑")
                            .on_hover_text("Previous match (Shift+Enter)")
                            .clicked()
                        {
                            self.select_prev_search_match();
                            self.search_state.search_focused = true;
                        }
                        if ui.button("↓").on_hover_text("Next match (Enter)").clicked() {
                            self.select_next_search_match();
                            self.search_state.search_focused = true;
                        }

                        // 关闭按钮
                        if ui.button("✕").on_hover_text("Close search (Esc)").clicked() {
                            self.search_state.close();
                            self.save_ui_history();
                        }
                    });

                    // 显示错误信息（如正则表达式错误）
                    if let Some(error) = &self.search_state.error_message {
                        ui.label(egui::RichText::new(error).color(egui::Color32::RED));
                    } else if let Some(message) = &self.search_state.projection_message {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(message).color(egui::Color32::YELLOW));
                            if self.search_state.hidden_projection_zone.is_some()
                                && ui.button("Reveal match").clicked()
                            {
                                reveal_hidden_match = true;
                            }
                        });
                    } else if self.search_state.results_truncated {
                        ui.label(
                            egui::RichText::new(format!(
                                "Showing the first {} matches",
                                crate::search::MAX_SEARCH_MATCHES
                            ))
                            .color(egui::Color32::YELLOW),
                        );
                    }
                });
            if reveal_hidden_match {
                self.reveal_hidden_search_match();
                self.search_state.search_focused = true;
            }
        }

        // 命令调色板 UI（中央弹窗）
        let mut clicked_palette_command = None;
        let mut hovered_palette_index = None;
        if self.command_palette.is_open {
            let screen_rect = ctx.viewport_rect();
            let palette_width = (screen_rect.width() - 32.0).clamp(360.0, 720.0);
            let palette_height = (screen_rect.height() - 96.0).clamp(300.0, 520.0);
            let palette_pos = egui::pos2(
                screen_rect.center().x - palette_width / 2.0,
                screen_rect.top() + (screen_rect.height() * 0.12).max(24.0),
            );

            egui::Window::new("Command Palette")
                .title_bar(false)
                .resizable(false)
                .movable(false)
                .default_pos(palette_pos)
                .default_size([palette_width, palette_height])
                .fixed_size([palette_width, palette_height])
                .frame(egui::Frame {
                    fill: crate::theme::Theme::rgb_to_color32(self.current_theme.ui.panel_bg),
                    stroke: egui::Stroke::new(
                        1.0,
                        crate::theme::Theme::rgb_to_color32(self.current_theme.ui.border),
                    ),
                    corner_radius: egui::CornerRadius::same(10),
                    inner_margin: egui::Margin::same(8),
                    ..Default::default()
                })
                .show(ctx, |ui| {
                    // 搜索输入框
                    ui.horizontal(|ui| {
                        ui.label("🔍");
                        let search_response =
                            ui.text_edit_singleline(&mut self.command_palette.search_query);
                        if search_response.changed() {
                            self.command_palette.update_search_results();
                        }
                        if self.command_palette.needs_focus {
                            search_response.request_focus();
                            self.command_palette.needs_focus = false;
                        }
                        if search_response.has_focus()
                            && self.command_palette.search_query.is_empty()
                        {
                            ui.label("Search commands...");
                        }
                    });

                    ui.separator();

                    // 命令列表
                    // Own a snapshot so pointer actions can be applied after the
                    // window closure without borrowing command_palette twice.
                    let results = self.command_palette.get_results().to_vec();
                    let selected_index = self.command_palette.selected_index;

                    egui::ScrollArea::vertical()
                        .max_height(palette_height - 100.0)
                        .show(ui, |ui| {
                            for (idx, (cmd_info, _score)) in results.iter().enumerate() {
                                let is_selected = idx == selected_index;

                                let bg_color = if is_selected {
                                    crate::theme::Theme::rgb_to_color32(
                                        self.current_theme.tabbar.active_border,
                                    )
                                    .gamma_multiply(0.18)
                                } else {
                                    egui::Color32::TRANSPARENT
                                };

                                let item_response = ui.horizontal(|ui| {
                                    let item_rect = ui.available_rect_before_wrap();
                                    ui.painter().rect_filled(item_rect, 2.0, bg_color);

                                    // 分类标签
                                    let category_color = match cmd_info.category {
                                        command_palette::CommandCategory::Session => {
                                            egui::Color32::from_rgb(100, 150, 255)
                                        }
                                        command_palette::CommandCategory::Edit => {
                                            egui::Color32::from_rgb(100, 200, 100)
                                        }
                                        command_palette::CommandCategory::Search => {
                                            egui::Color32::from_rgb(255, 200, 100)
                                        }
                                        command_palette::CommandCategory::Terminal => {
                                            egui::Color32::from_rgb(150, 150, 255)
                                        }
                                        command_palette::CommandCategory::Window => {
                                            egui::Color32::from_rgb(200, 100, 200)
                                        }
                                        command_palette::CommandCategory::Config => {
                                            egui::Color32::from_rgb(200, 180, 100)
                                        }
                                    };

                                    ui.colored_label(
                                        category_color,
                                        format!("[{}]", cmd_info.category),
                                    );

                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new(&cmd_info.name).strong());
                                        ui.label(
                                            egui::RichText::new(&cmd_info.description)
                                                .size(10.0)
                                                .color(ui.visuals().weak_text_color()),
                                        );
                                    });

                                    // 快捷键显示 — 走 pretty_bindings_for 统一美化:
                                    // 之前直接展示原始小写 "ctrl+shift+f",这里改为 "Ctrl+Shift+F",
                                    // 与帮助面板保持一致。
                                    let pretty = self
                                        .keybindings
                                        .pretty_bindings_for(&cmd_info.command.to_string());
                                    // 未绑定的命令显示它的 `id`,而不是无用的
                                    // "No binding":这正是用户要写进
                                    // keybindings.toml 的那个字符串。用等宽
                                    // 弱色渲染,免得 id 被误读成一个键位。
                                    let bound = !pretty.is_empty();
                                    let keybinding_str = if bound {
                                        pretty.join(" / ")
                                    } else {
                                        cmd_info.command.to_string()
                                    };

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let text =
                                                egui::RichText::new(keybinding_str).size(10.0);
                                            ui.label(if bound {
                                                text.color(egui::Color32::from_rgb(100, 150, 200))
                                            } else {
                                                text.monospace()
                                                    .color(ui.visuals().weak_text_color())
                                            });
                                        },
                                    );
                                });

                                // Auto-scroll to keep selected item visible
                                if is_selected {
                                    item_response
                                        .response
                                        .scroll_to_me(Some(egui::Align::Center));
                                }

                                let click_response = ui
                                    .interact(
                                        item_response.response.rect,
                                        item_response.response.id.with("palette_click"),
                                        egui::Sense::click(),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                if click_response.hovered() {
                                    hovered_palette_index = Some(idx);
                                }
                                if click_response.clicked() {
                                    clicked_palette_command = Some(cmd_info.command.clone());
                                }

                                ui.separator();
                            }

                            // 如果没有结果
                            if results.is_empty() {
                                ui.label(
                                    egui::RichText::new("No commands found")
                                        .color(ui.visuals().weak_text_color()),
                                );
                            }
                        });

                    // 底部提示
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("↑↓ Navigate  Enter Execute  Esc Cancel")
                                .size(10.0)
                                .color(ui.visuals().weak_text_color()),
                        );
                    });
                });
        }

        if let Some(index) = hovered_palette_index {
            self.command_palette.selected_index = index;
        }
        if let Some(command) = clicked_palette_command {
            self.dispatch_palette_command(ctx, command);
        }

        // 跨块搜索选择器(block:search):与命令面板同款的中央浮层。
        let mut clicked_hit_index = None;
        let mut clicked_bookmark_target = None;
        let mut hovered_hit_index = None;
        let mut focused_hit_index = None;
        let block_search_pointer_moved =
            ctx.input(|input| input.pointer.delta() != egui::Vec2::ZERO);
        if self.block_search.is_open {
            // Hits always describe the active session, finalized-record
            // version, query and filter. Query edits only rescan the cache;
            // pane changes and new/evicted completed blocks rebuild it.
            // This is cheap when current: it compares the stable finalized-
            // record version and query, then returns without touching output
            // text. A completed block (including same-length deque rotation)
            // rebuilds the bounded index before any old hit can be accepted.
            self.refresh_block_search_hits();

            let active_index = self.session_manager.active_index();
            let picker_session_id = self.block_search.session_id.clone();
            let (pane_has_prompt_marks, live_record_sequences) = self
                .session_manager
                .sessions()
                .get(active_index)
                .map(|session| {
                    let terminal = session.terminal.lock();
                    let sequences = if picker_session_id.as_deref()
                        == Some(session.metadata.session_id.as_str())
                    {
                        terminal
                            .command_records()
                            .iter()
                            .filter(|record| record.complete)
                            .map(|record| (record.id.clone(), record.sequence))
                            .collect::<std::collections::HashMap<_, _>>()
                    } else {
                        std::collections::HashMap::new()
                    };
                    (terminal.has_prompt_marks(), sequences)
                })
                .unwrap_or_default();
            let bookmarked_sequences = picker_session_id
                .as_deref()
                .and_then(|session_id| self.block_bookmarks.get(session_id))
                .cloned()
                .unwrap_or_default();
            let has_live_bookmarks = live_record_sequences
                .values()
                .any(|sequence| bookmarked_sequences.contains(sequence));
            let pane_has_completed_blocks = self
                .block_search
                .record_version
                .is_some_and(|version| version.len > 0);

            let screen_rect = ctx.viewport_rect();
            let picker_width = (screen_rect.width() - 32.0).clamp(360.0, 720.0);
            let picker_height = (screen_rect.height() - 96.0).clamp(300.0, 520.0);
            let picker_pos = egui::pos2(
                screen_rect.center().x - picker_width / 2.0,
                screen_rect.top() + (screen_rect.height() * 0.12).max(24.0),
            );
            let mut intent_control_focused = false;

            egui::Window::new("Block Search")
                .title_bar(false)
                .resizable(false)
                .movable(false)
                .default_pos(picker_pos)
                .default_size([picker_width, picker_height])
                .fixed_size([picker_width, picker_height])
                .frame(egui::Frame {
                    fill: crate::theme::Theme::rgb_to_color32(self.current_theme.ui.panel_bg),
                    stroke: egui::Stroke::new(
                        1.0,
                        crate::theme::Theme::rgb_to_color32(self.current_theme.ui.border),
                    ),
                    corner_radius: egui::CornerRadius::same(10),
                    inner_margin: egui::Margin::same(8),
                    ..Default::default()
                })
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("🔍");
                        let search_response = ui.text_edit_singleline(&mut self.block_search.query);
                        if search_response.changed() {
                            self.block_search.query =
                                crate::block_mode::bounded_block_search_query(std::mem::take(
                                    &mut self.block_search.query,
                                ));
                            self.refresh_block_search_hits();
                        }
                        if self.block_search.needs_focus {
                            search_response.request_focus();
                            self.block_search.needs_focus = false;
                        }
                        if search_response.has_focus() && self.block_search.query.is_empty() {
                            ui.label("Search block commands and output...");
                        }
                    });

                    // Keep the query usable at the picker's 360 px minimum.
                    // The compact matching/actions row fits below it instead
                    // of squeezing the text editor after Refresh was added.
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Match").small());
                        let case_button = ui
                            .selectable_label(self.block_search.case_sensitive, "Aa")
                            .on_hover_text("Match case");
                        let regex_button = ui
                            .selectable_label(self.block_search.regex, ".*")
                            .on_hover_text("Regular expression");
                        let whole_word_button = ui
                            .selectable_label(self.block_search.whole_word, "W")
                            .on_hover_text("Match whole words");
                        case_button.widget_info(|| {
                            egui::WidgetInfo::selected(
                                egui::WidgetType::Button,
                                true,
                                self.block_search.case_sensitive,
                                "Match case (Ctrl+I)",
                            )
                        });
                        regex_button.widget_info(|| {
                            egui::WidgetInfo::selected(
                                egui::WidgetType::Button,
                                true,
                                self.block_search.regex,
                                "Regular expression (Ctrl+R)",
                            )
                        });
                        whole_word_button.widget_info(|| {
                            egui::WidgetInfo::selected(
                                egui::WidgetType::Button,
                                true,
                                self.block_search.whole_word,
                                "Match whole words (Ctrl+W)",
                            )
                        });
                        intent_control_focused |= case_button.has_focus()
                            || regex_button.has_focus()
                            || whole_word_button.has_focus();
                        if case_button.clicked() {
                            self.block_search.case_sensitive = !self.block_search.case_sensitive;
                            self.block_search.computed_query = None;
                            self.block_search.needs_focus = true;
                            self.refresh_block_search_hits();
                        }
                        if regex_button.clicked() {
                            self.block_search.regex = !self.block_search.regex;
                            self.block_search.computed_query = None;
                            self.block_search.needs_focus = true;
                            self.refresh_block_search_hits();
                        }
                        if whole_word_button.clicked() {
                            self.block_search.whole_word = !self.block_search.whole_word;
                            self.block_search.computed_query = None;
                            self.block_search.needs_focus = true;
                            self.refresh_block_search_hits();
                        }
                        let refresh_button = ui
                            .button("Refresh")
                            .on_hover_text("Refresh block search results (F5)");
                        refresh_button.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                true,
                                "Refresh block search results (F5)",
                            )
                        });
                        intent_control_focused |= refresh_button.has_focus();
                        if refresh_button.clicked() {
                            self.block_search_manual_refresh();
                        }
                        let reset_button = ui
                            .button("Reset")
                            .on_hover_text("Reset query, matching options, scope, and filters");
                        reset_button.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                true,
                                "Reset block search intent (Ctrl+Shift+U)",
                            )
                        });
                        intent_control_focused |= reset_button.has_focus();
                        if reset_button.clicked() {
                            self.block_search.reset_intent();
                            self.refresh_block_search_hits();
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Scope").small());
                        for (label, scope) in [
                            ("All", crate::block_mode::BlockSearchScope::All),
                            ("Cmd", crate::block_mode::BlockSearchScope::Command),
                            ("Out", crate::block_mode::BlockSearchScope::Output),
                        ] {
                            let scope_button =
                                ui.selectable_label(self.block_search.scope == scope, label);
                            scope_button.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::WidgetType::Button,
                                    true,
                                    self.block_search.scope == scope,
                                    format!("Search scope: {label}"),
                                )
                            });
                            intent_control_focused |= scope_button.has_focus();
                            if scope_button.clicked() {
                                self.block_search.scope = scope;
                                self.block_search.computed_query = None;
                                self.block_search.needs_focus = true;
                                self.refresh_block_search_hits();
                            }
                        }
                    });

                    ui.horizontal_wrapped(|ui| {
                        for (label, filter) in [
                            ("All", crate::block_search::BlockSearchFilter::All),
                            ("Failed", crate::block_search::BlockSearchFilter::Failed),
                            ("Slow", crate::block_search::BlockSearchFilter::Slow),
                            (
                                "Bookmarked",
                                crate::block_search::BlockSearchFilter::Bookmarked,
                            ),
                            (
                                "Background",
                                crate::block_search::BlockSearchFilter::Background,
                            ),
                        ] {
                            let filter_button =
                                ui.selectable_label(self.block_search.filter == filter, label);
                            filter_button.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::WidgetType::Button,
                                    true,
                                    self.block_search.filter == filter,
                                    format!("Block filter: {label}"),
                                )
                            });
                            intent_control_focused |= filter_button.has_focus();
                            if filter_button.clicked() {
                                self.block_search.filter = filter;
                                self.block_search.computed_query = None;
                                self.block_search.needs_focus = true;
                                self.refresh_block_search_hits();
                            }
                        }
                    });

                    ui.separator();
                    let query_error = self.block_search.query_error.clone();
                    if let Some(error) = &query_error {
                        ui.label(egui::RichText::new(error).small().color(egui::Color32::RED));
                    } else {
                        ui.label(
                            egui::RichText::new(self.block_search.count_label())
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                    }

                    // Give ScrollArea the complete row count while it builds
                    // widgets only for the visible range. Pre-slicing around
                    // the keyboard highlight made the scrollbar describe just
                    // that slice, so pointer users could not jump to later
                    // results in a broad query.
                    const BLOCK_SEARCH_ROW_HEIGHT: f32 = 40.0;
                    let hit_count = if query_error.is_none() {
                        self.block_search.hits.len()
                    } else {
                        0
                    };
                    let selected_index = self.block_search.selected_index;
                    let query_is_empty = self.block_search.query.trim().is_empty();
                    let list_height = picker_height - 148.0;
                    let scroll_to_selected =
                        std::mem::take(&mut self.block_search.scroll_to_selected);

                    if hit_count > 0 {
                        let mut scroll = egui::ScrollArea::vertical().max_height(list_height);
                        // Pointer movement wins if both devices act in one
                        // frame. A stationary cursor cannot cancel keyboard
                        // traversal simply because recentering moved a row
                        // underneath it.
                        if scroll_to_selected && !block_search_pointer_moved {
                            let stride =
                                BLOCK_SEARCH_ROW_HEIGHT + ui.spacing().item_spacing.y;
                            scroll = scroll.vertical_scroll_offset(
                                crate::block_mode::block_search_centered_scroll_offset(
                                    hit_count,
                                    selected_index,
                                    stride,
                                    list_height,
                                ),
                            );
                        }
                        scroll.show_rows(
                            ui,
                            BLOCK_SEARCH_ROW_HEIGHT,
                            hit_count,
                            |ui, row_range| {
                                for idx in row_range {
                                    let Some(hit) = self.block_search.hits.get(idx) else {
                                        continue;
                                    };
                                    let is_selected = idx == selected_index;
                                    let width = ui.available_width();
                                    let (rect, _) = ui.allocate_exact_size(
                                        egui::vec2(width, BLOCK_SEARCH_ROW_HEIGHT),
                                        egui::Sense::hover(),
                                    );
                                    let bookmark_width = 30.0;
                                    let row_rect = egui::Rect::from_min_max(
                                        rect.min,
                                        egui::pos2(rect.right() - bookmark_width, rect.bottom()),
                                    );
                                    let bookmark_rect = egui::Rect::from_min_max(
                                        egui::pos2(row_rect.right(), rect.top()),
                                        rect.max,
                                    );
                                    let record_version = self
                                        .block_search
                                        .record_version
                                        .unwrap_or_default();
                                    let stable_row_identity = block_search_row_widget_identity(
                                        picker_session_id.as_deref().unwrap_or_default(),
                                        record_version,
                                        hit,
                                    );
                                    let response = ui
                                        .push_id(("block-search-result", stable_row_identity), |ui| {
                                            ui.put(
                                                row_rect,
                                                egui::Button::new("").frame(false),
                                            )
                                        })
                                        .inner;
                                    if is_selected {
                                        ui.painter().rect_filled(
                                            rect,
                                            2.0,
                                            crate::theme::Theme::rgb_to_color32(
                                                self.current_theme.tabbar.active_border,
                                            )
                                            .gamma_multiply(0.18),
                                        );
                                    }

                                    let content_rect = row_rect.shrink2(egui::vec2(4.0, 2.0));
                                    ui.scope_builder(
                                        egui::UiBuilder::new()
                                            .max_rect(content_rect)
                                            .layout(egui::Layout::left_to_right(
                                                egui::Align::Center,
                                            )),
                                        |ui| {
                                            let (marker, marker_color) = match hit.line_no {
                                                Some(line_no) => (
                                                    format!("L{line_no}"),
                                                    egui::Color32::from_rgb(255, 200, 100),
                                                ),
                                                None => (
                                                    "cmd".to_string(),
                                                    egui::Color32::from_rgb(150, 150, 255),
                                                ),
                                            };
                                            ui.colored_label(marker_color, marker);

                                            let text_width = ui.available_width();
                                            ui.vertical(|ui| {
                                                ui.add_sized(
                                                    [text_width, 18.0],
                                                    egui::Label::new(
                                                        egui::RichText::new(&hit.line_text)
                                                            .monospace()
                                                            .strong(),
                                                    )
                                                    .truncate(),
                                                );
                                                let context = if hit.is_output_line {
                                                    format!(
                                                        "{} · L{}",
                                                        if hit.command_preview.is_empty() {
                                                            "(no command)"
                                                        } else {
                                                            hit.command_preview.as_str()
                                                        },
                                                        hit.line_no.unwrap_or(0)
                                                    )
                                                } else {
                                                    "command".to_string()
                                                };
                                                let weak = ui.visuals().weak_text_color();
                                                ui.add_sized(
                                                    [text_width, 14.0],
                                                    egui::Label::new(
                                                        egui::RichText::new(context)
                                                            .monospace()
                                                            .size(10.0)
                                                            .color(weak),
                                                    )
                                                    .truncate(),
                                                );
                                            });
                                        },
                                    );
                                    ui.painter().line_segment(
                                        [rect.left_bottom(), rect.right_bottom()],
                                        ui.visuals().widgets.noninteractive.bg_stroke,
                                    );

                                    let bookmarked = block_search_record_is_bookmarked(
                                        &hit.record_id,
                                        &live_record_sequences,
                                        &bookmarked_sequences,
                                    );
                                    let bookmark_label = if bookmarked {
                                        "Remove bookmark from this block"
                                    } else {
                                        "Bookmark this block for this running session"
                                    };
                                    let bookmark_response = ui
                                        .push_id(
                                            ("block-search-bookmark", stable_row_identity),
                                            |ui| {
                                                ui.put(
                                                    bookmark_rect
                                                        .shrink2(egui::vec2(2.0, 4.0)),
                                                    egui::Button::new(if bookmarked {
                                                        "★"
                                                    } else {
                                                        "☆"
                                                    })
                                                    .selected(bookmarked)
                                                    .frame(false),
                                                )
                                            },
                                        )
                                        .inner
                                        .on_hover_text(bookmark_label);
                                    bookmark_response.widget_info(|| {
                                        egui::WidgetInfo::selected(
                                            egui::WidgetType::Button,
                                            true,
                                            bookmarked,
                                            bookmark_label,
                                        )
                                    });
                                    intent_control_focused |= bookmark_response.has_focus();
                                    if bookmark_response.clicked() {
                                        clicked_bookmark_target =
                                            Some(self.block_search.bookmark_target(idx));
                                    }

                                    let response = response
                                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                                    let row_accessible_label = if hit.is_output_line {
                                        format!(
                                            "Result {} of {}; {}; output line {} for {}",
                                            idx + 1,
                                            hit_count,
                                            hit.line_text,
                                            hit.line_no.unwrap_or(0),
                                            if hit.command_preview.is_empty() {
                                                "a commandless block"
                                            } else {
                                                hit.command_preview.as_str()
                                            }
                                        )
                                    } else {
                                        format!(
                                            "Result {} of {}; {}; command",
                                            idx + 1,
                                            hit_count,
                                            hit.line_text
                                        )
                                    };
                                    response.widget_info(|| {
                                        egui::WidgetInfo::selected(
                                            egui::WidgetType::Button,
                                            true,
                                            is_selected,
                                            &row_accessible_label,
                                        )
                                    });
                                    if response.hovered() {
                                        hovered_hit_index = Some(idx);
                                    }
                                    if response.has_focus() {
                                        focused_hit_index = Some(idx);
                                    }
                                    if response.clicked() {
                                        clicked_hit_index = Some(idx);
                                    }
                                }
                            },
                        );
                    } else if query_error.is_none() {
                        egui::ScrollArea::vertical()
                            .max_height(list_height)
                            .show(ui, |ui| {
                                let mut empty_message = if !pane_has_prompt_marks {
                                    "This pane has no command blocks: the shell is not reporting commands (OSC 133). Run “Install or update jsh” from the command palette.".to_string()
                                } else if !pane_has_completed_blocks {
                                    "This pane has no completed command blocks yet".to_string()
                                } else if self.block_search.filter
                                    == crate::block_search::BlockSearchFilter::Bookmarked
                                {
                                    crate::block_search::bookmarked_empty_message(
                                        has_live_bookmarks,
                                        query_is_empty,
                                    )
                                    .to_string()
                                } else if query_is_empty {
                                    if self.block_search.filter
                                        == crate::block_search::BlockSearchFilter::All
                                    {
                                        "Type to search every command block in this session"
                                            .to_string()
                                    } else {
                                        "No matching blocks".to_string()
                                    }
                                } else {
                                    "No matches".to_string()
                                };
                                if self.block_search.older_not_indexed {
                                    empty_message.push_str(" · older blocks not indexed");
                                }
                                ui.label(
                                    egui::RichText::new(empty_message)
                                        .color(ui.visuals().weak_text_color()),
                                );
                            });
                    }

                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new(
                                "F5 Refresh  ↑↓ Navigate  Enter Jump  Shift+Enter Jump & Next  Ctrl+Shift+B Bookmark  Ctrl+U Clear  Ctrl+Shift+U Reset  Esc Close",
                            )
                                .size(10.0)
                                .color(ui.visuals().weak_text_color()),
                        );
                    });
                });
            self.block_search.intent_control_focused = intent_control_focused;
        }

        if let Some(index) = focused_hit_index {
            self.block_search.selected_index = index;
        }
        if let Some(index) = hovered_hit_index {
            self.block_search
                .select_hovered(index, block_search_pointer_moved);
        }
        if let Some(target) = clicked_bookmark_target {
            if self.block_search_toggle_bookmark(target) {
                ctx.request_repaint();
            }
        } else if let Some(index) = clicked_hit_index {
            self.block_search.selected_index = index;
            self.block_search_confirm();
        }

        // 远程主机选择器（浮动窗口）
        if let Some(index) =
            self.remote_picker
                .show(ctx, &self.config.remote_hosts, &self.current_theme)
        {
            self.connect_remote_host(index);
        }

        // 文件树文件操作对话框（新建/重命名/删除确认，浮动窗口）
        self.render_sidebar_fs_dialogs(ctx);

        // 帮助面板 UI（浮动窗口）
        let mut help_open = self.help_panel.is_open;
        self.help_panel.show(
            ctx,
            &mut help_open,
            &self.command_palette,
            &self.keybindings,
            &self.current_theme,
        );
        self.help_panel.is_open = help_open;

        // 配置面板 UI（浮动窗口）
        let config_actions = self.config_panel.show(ctx, &self.current_theme);
        for action in config_actions {
            match action {
                config_panel::ConfigAction::CustomThemeApplied(theme) => {
                    self.current_theme = *theme.clone();
                    self.apply_runtime_config(ctx);
                }
                config_panel::ConfigAction::SaveRequested => {
                    let bottom_bar_was = self.config.bottom_bar;
                    let keep_tasks_visible = self.agent_runtime.has_any_activity();
                    // Apply all buffered edit values to config
                    self.config_panel.apply_to_config(&mut self.config);
                    if keep_tasks_visible && !self.config.experimental_task_sidebar {
                        self.config.experimental_task_sidebar = true;
                        self.set_status_for(
                            "Tasks remains enabled while native work is active; turn it off after cleanup",
                            std::time::Duration::from_secs(5),
                        );
                    }
                    // The bottom bar takes/returns a strip of window height;
                    // re-grid the PTY at once instead of waiting for the next
                    // natural resize.
                    if self.config.bottom_bar != bottom_bar_was {
                        self.force_resize_session = true;
                    }
                    // Update theme
                    if let Some(t) = theme::Theme::get_theme(&self.config.theme) {
                        self.current_theme = t.clone();
                    }
                    // Apply runtime changes (fonts, GPU, renderer)
                    self.apply_runtime_config(ctx);
                    // Save to file
                    match self.config.save() {
                        Ok(()) => {
                            let (invalid, inactive) = crate::config::remote_host_problem_counts(
                                &self.config.remote_hosts,
                            );
                            if invalid > 0 || inactive > 0 {
                                let mut details = Vec::new();
                                if invalid > 0 {
                                    details.push(format!(
                                        "{invalid} active remote draft(s) are invalid and cannot run"
                                    ));
                                }
                                if inactive > 0 {
                                    details.push(format!(
                                        "{inactive} remote draft(s) beyond the {}-host limit remain retained",
                                        crate::config::MAX_REMOTE_HOSTS
                                    ));
                                }
                                self.set_status(format!("Settings saved; {}", details.join("; ")));
                            } else {
                                self.set_status("Settings saved");
                            }
                        }
                        Err(error) => {
                            eprintln!("[Config] Failed to save: {}", error);
                            self.set_status_for(
                                format!("Settings are active but could not be saved: {error}"),
                                std::time::Duration::from_secs(6),
                            );
                        }
                    }
                    self.config_panel.sync_from_config(&self.config);
                }
                config_panel::ConfigAction::ResetToDefaults => {
                    let bottom_bar_was = self.config.bottom_bar;
                    let keep_tasks_visible = self.agent_runtime.has_any_activity();
                    // Replacing the whole struct (never field-by-field) is what
                    // makes Reset the escape hatch out of `Config::load_error`:
                    // an explicit reset is the one time overwriting a broken
                    // config file is what the user asked for.
                    self.config = config::Config::default();
                    if keep_tasks_visible {
                        self.config.experimental_task_sidebar = true;
                    }
                    if self.config.bottom_bar != bottom_bar_was {
                        self.force_resize_session = true;
                    }
                    self.current_theme =
                        theme::Theme::get_theme(&self.config.theme).unwrap_or_default();
                    self.apply_runtime_config(ctx);
                    self.config_panel.sync_from_config(&self.config);
                    self.config_panel.edit_debug_overlay = self.debug_panel.is_open;
                    self.schedule_config_save();
                }
                config_panel::ConfigAction::DebugPanelToggled(open) => {
                    self.debug_panel.is_open = open;
                }
            }
        }

        // Find & Replace 面板（对当前选中文本操作）
        if let Some(sr_action) = self.search_replace_panel.show(ctx, &self.current_theme) {
            // 先读取选中文本并释放终端锁，再 mutate panel/clipboard/PTY
            let selection = {
                let session = self.session_manager.get_active_session_mut();
                let terminal = session.terminal.lock();
                terminal.copy_selection()
            };
            match selection {
                Some(text) => {
                    if let Some(result) = self.search_replace_panel.apply(&text) {
                        match sr_action {
                            search_replace_panel::SearchReplaceAction::ReplaceToClipboard => {
                                if let Some(clipboard) = &self.clipboard {
                                    if let Err(e) = clipboard.copy(&result) {
                                        log::warn!("{}", e);
                                    }
                                }
                            }
                            search_replace_panel::SearchReplaceAction::TypeIntoTerminal => {
                                let active_session_id = self
                                    .session_manager
                                    .sessions()
                                    .get(self.session_manager.active_index())
                                    .map(|session| session.metadata.session_id.clone());
                                let direct_input_blocked =
                                    active_session_id.as_deref().is_none_or(|session_id| {
                                        self.direct_input_is_blocked_for_session(session_id)
                                    });
                                let paste_result = {
                                    let session = self.session_manager.get_active_session_mut();
                                    crate::paste_text_into_session(
                                        session,
                                        result,
                                        self.config.paste_confirm,
                                        crate::PasteOrigin::PromptInsert,
                                        false,
                                        direct_input_blocked,
                                        &mut self.pending_paste_confirm,
                                    )
                                };
                                match paste_result {
                                    Ok(true) if self.pending_paste_confirm.is_some() => {
                                        self.search_replace_panel.status =
                                            "Awaiting paste confirmation".to_string();
                                    }
                                    Ok(true) => {
                                        if let Some(session_id) = active_session_id {
                                            self.clear_block_selection_for_session(&session_id);
                                        }
                                        self.search_replace_panel.status =
                                            "Typed into terminal".to_string();
                                    }
                                    Ok(false) => {
                                        self.search_replace_panel.status =
                                            "Nothing to type".to_string();
                                    }
                                    Err(error) => {
                                        self.search_replace_panel.status =
                                            format!("Terminal paste failed: {error}");
                                    }
                                }
                            }
                        }
                    }
                }
                None => {
                    self.search_replace_panel.status = "No selection".to_string();
                }
            }
        }

        // Render after every surface that can create a pending paste. This
        // gives the modal top focus in the same frame as Find & Replace's
        // "Type into terminal" action, so Enter/Escape cannot return to the
        // panel or leak into the PTY underneath.
        self.show_paste_confirm_dialog(ctx);

        // 状态 toast(右下角)——把分散在 input/main/window 里写入 status_message
        // 的反馈集中显示;过期由 current_status_for_display 内部判定后清理。
        self.render_status_toast(ctx);

        // Debug overlay panel — only gather stats (and lock the terminal) when open.
        if self.debug_panel.is_open {
            let session = self.session_manager.get_active_session_mut();
            let terminal = session.terminal.lock();
            let grid_cols = terminal.grid.cols();
            let grid_rows = terminal.grid.rows();
            let scrollback_used = terminal.scrollback.len();
            let kitty_images_count = terminal.kitty_graphics.image_count();
            let kitty_memory_mb = terminal.kitty_graphics.image_memory_mb();
            let scrollback_max = terminal.max_scrollback();
            drop(terminal);
            let pending_output_bytes = session.pending_output.len();
            let session_count = self.session_manager.len();
            let texture_cache_size = self.renderer.texture_cache_len();
            let frame_budget_kb = self.adaptive_frame_budget / 1024;
            self.debug_panel.show(
                ctx,
                grid_cols,
                grid_rows,
                session_count,
                scrollback_used,
                scrollback_max,
                kitty_images_count,
                kitty_memory_mb,
                pending_output_bytes,
                texture_cache_size,
                frame_budget_kb,
            );
        }

        // AI agent panel: advance the session (harvest model replies, start
        // the next request), render, then apply approved-command effects.
        if self.agent_panel.is_open {
            // A task remains bound to its source terminal even if the user
            // inspects another tab while the model is working. Feeding the
            // active tab's cwd here would silently splice unrelated workspace
            // context into the next Agent turn.
            let bound_session_id = self.agent_panel.bound_session_id().map(str::to_owned);
            let bound_session = bound_session_id.as_deref().and_then(|session_id| {
                self.session_manager
                    .sessions()
                    .iter()
                    .find(|session| session.metadata.session_id == session_id)
            });
            let (cwd, trusted_local_cwd) = bound_session.map_or((None, None), |session| {
                let reported_cwd = session.terminal.lock().current_working_dir.clone();
                let process_cwd = jterm_core::process::process_cwd(session.get_shell_pid());
                let cwd = reported_cwd.or_else(|| process_cwd.clone());
                let trusted_local_cwd = process_cwd.filter(|local| cwd.as_deref() == Some(local));
                (cwd, trusted_local_cwd)
            });
            let shell = self
                .config
                .shell
                .clone()
                .unwrap_or_else(|| std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string()));
            if bound_session_id.is_some() && bound_session.is_none() {
                self.agent_panel.binding_lost();
            } else {
                self.agent_panel.drive(
                    &self.config,
                    cwd.as_deref(),
                    trusted_local_cwd.as_deref(),
                    &shell,
                );
            }
            let effects = self.agent_panel.show(ctx);
            for effect in effects {
                match effect {
                    crate::agent_panel::AgentEffect::RunCommand {
                        session_id,
                        command,
                        required_cwd,
                        epoch,
                        generation,
                    } => {
                        if !self.agent_panel.claim_run_effect(
                            &session_id,
                            &command,
                            epoch,
                            generation,
                        ) {
                            log::warn!(
                                "agent: dropped a stale run effect for terminal session {session_id}"
                            );
                            continue;
                        }
                        match self.session_manager.index_of(&session_id) {
                            Some(session_index) => {
                                let direct_input_blocked =
                                    self.direct_input_is_blocked_for_session(&session_id);
                                let Some(session) =
                                    self.session_manager.get_session_mut(session_index)
                                else {
                                    self.agent_panel.execution_start_failed(
                                        generation,
                                        "Agent session's terminal no longer exists",
                                    );
                                    continue;
                                };
                                if let Some(required_cwd) = required_cwd.as_deref() {
                                    let reported_cwd =
                                        session.terminal.lock().current_working_dir.clone();
                                    let process_cwd =
                                        jterm_core::process::process_cwd(session.get_shell_pid());
                                    let matches = crate::app::commands::verified_local_command_cwd(
                                        required_cwd,
                                        reported_cwd.as_deref(),
                                        process_cwd.as_deref(),
                                    );
                                    if !matches {
                                        self.agent_panel.execution_start_failed(
                                            generation,
                                            "Agent command was not started: the recorded cwd is not independently verified by the local shell process",
                                        );
                                        self.set_status(
                                            "Agent command was not started: return a local shell to the recorded working directory",
                                        );
                                        continue;
                                    }
                                }
                                if !agent_input_route_is_clean(
                                    direct_input_blocked,
                                    !session.pending_input.is_empty(),
                                ) {
                                    self.agent_panel.execution_start_failed(
                                        generation,
                                        "Agent command was not started: older terminal input is still pending",
                                    );
                                    self.set_status(
                                        "Agent command was not started: older terminal input is still pending",
                                    );
                                    continue;
                                }
                                if !session.shell_owns_foreground_pty() {
                                    self.agent_panel.execution_start_failed(
                                    generation,
                                    "Agent command was not started: the interactive shell does not own the foreground PTY",
                                );
                                    continue;
                                }
                                let bracketed = {
                                    let mut terminal = session.terminal.lock();
                                    if let Err(error) =
                                        terminal.arm_agent_execution(generation, &command)
                                    {
                                        drop(terminal);
                                        self.agent_panel.execution_start_failed(
                                            generation,
                                            format!("Agent command was not started: {error}"),
                                        );
                                        continue;
                                    }
                                    terminal.is_bracketed_paste_enabled()
                                };
                                let bytes = crate::encode_submitted_command(&command, bracketed);
                                if !session.queue_agent_input(&bytes) {
                                    session.terminal.lock().disarm_agent_execution(generation);
                                    self.agent_panel.execution_start_failed(
                                        generation,
                                        "Agent command rejected: input queue is full",
                                    );
                                    self.set_status("Agent command rejected: input queue is full");
                                } else {
                                    self.clear_block_selection_for_session(&session_id);
                                }
                            }
                            None => {
                                self.agent_panel.execution_start_failed(
                                    generation,
                                    "Agent session's terminal no longer exists",
                                );
                                self.set_status("Agent session's terminal no longer exists");
                            }
                        }
                    }
                    crate::agent_panel::AgentEffect::ReviewDiff {
                        session_id,
                        recorded_cwd,
                        epoch,
                    } => {
                        if !self.agent_panel.claim_context_effect(&session_id, epoch) {
                            log::warn!(
                                "agent: dropped a stale diff effect for terminal session {session_id}"
                            );
                            continue;
                        }
                        let trusted_cwd = self
                            .session_manager
                            .sessions()
                            .iter()
                            .find(|session| session.metadata.session_id == session_id)
                            .and_then(|session| {
                                jterm_core::process::process_cwd(session.get_shell_pid())
                            });
                        let Some(trusted_cwd) = trusted_cwd else {
                            self.set_status_for(
                                "Native diff is unavailable because the local source process cwd could not be verified",
                                std::time::Duration::from_secs(6),
                            );
                            continue;
                        };
                        let trusted_path = std::path::Path::new(&trusted_cwd);
                        let recorded_matches = recorded_cwd
                            .as_deref()
                            .is_some_and(|recorded| std::path::Path::new(recorded) == trusted_path);
                        if !trusted_path.is_absolute() || !recorded_matches {
                            self.set_status_for(
                                "Native diff requires the recorded command cwd to match the verified local shell cwd",
                                std::time::Duration::from_secs(6),
                            );
                            continue;
                        }
                        if let Err(error) = self.agent_diff.request(trusted_path.to_path_buf()) {
                            self.set_status_for(
                                format!("Could not open Agent diff: {error}"),
                                std::time::Duration::from_secs(5),
                            );
                        }
                    }
                }
            }
        }
        self.agent_diff.show(ctx);
    }

    /// 状态消息 toast。固定锚在屏幕右下角,过期后下一帧自动消失。
    /// 之前 status_message 被多处写入却没有渲染端,所有反馈都被悄悄丢弃。
    fn render_status_toast(&mut self, ctx: &egui::Context) {
        let Some(message) = self.current_status_for_display().map(|s| s.to_string()) else {
            return;
        };
        // 临近过期时淡出,避免突兀消失。
        let fade_alpha: f32 = if let Some(deadline) = self.status_expires_at {
            let remaining = deadline
                .saturating_duration_since(std::time::Instant::now())
                .as_secs_f32();
            // 最后 350ms 做线性淡出
            (remaining / 0.35).clamp(0.0, 1.0)
        } else {
            1.0
        };
        if fade_alpha <= 0.0 {
            return;
        }

        let panel_bg = crate::theme::Theme::rgb_to_color32(self.current_theme.ui.panel_bg);
        let border = crate::theme::Theme::rgb_to_color32(self.current_theme.ui.border);
        let text_color = crate::theme::Theme::rgb_to_color32(self.current_theme.ui.text);
        let alpha = (fade_alpha * 230.0) as u8;
        let bg =
            egui::Color32::from_rgba_unmultiplied(panel_bg.r(), panel_bg.g(), panel_bg.b(), alpha);

        egui::Area::new(egui::Id::new("status_toast"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .order(egui::Order::Tooltip)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame {
                    fill: bg,
                    stroke: egui::Stroke::new(1.0, border),
                    corner_radius: egui::CornerRadius::same(8),
                    inner_margin: egui::Margin::symmetric(12, 8),
                    ..Default::default()
                }
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(message)
                            .color(text_color.gamma_multiply(fade_alpha))
                            .size(12.0),
                    );
                });
            });

        // 还在显示期间持续重绘,保证淡出/到期清理及时生效。
        ctx.request_repaint();
    }

    fn show_paste_confirm_dialog(&mut self, ctx: &egui::Context) {
        // Snapshot what we need from the pending paste and decide outside of
        // the dialog closure so we can mutably touch session_manager / shell
        // without holding any borrow on self.pending_paste_confirm.
        let Some(pending) = self.pending_paste_confirm.as_ref() else {
            return;
        };
        let decision_armed = pending.decision_armed;

        let panel_bg = crate::theme::Theme::rgb_to_color32(self.current_theme.ui.panel_bg);
        let text_color = crate::theme::Theme::rgb_to_color32(self.current_theme.ui.text);
        let border = crate::theme::Theme::rgb_to_color32(self.current_theme.ui.border);

        let line_count = pending.text.lines().count();
        let byte_len = pending.text.len();
        // 剪贴板里嵌有 ESC[200~/ESC[201~ 只可能是括号粘贴注入尝试:编码器已经
        // 剔除,但用户有权知道自己复制到了什么。
        let had_embedded_marker = pending.risk.had_embedded_paste_marker;
        let had_visual_spoofing = pending.had_visual_spoofing;
        // First few lines as a preview; truncate long single lines too.
        let mut clipped_line = false;
        let preview: String = pending
            .text
            .lines()
            .take(8)
            .map(|l| {
                // The preview itself is part of the approval boundary: never
                // let bidi/default-ignorable scalars disguise what will be
                // delivered after confirmation.
                let visible = crate::review_text::visible_bounded(l, 8 * 1024);
                if visible.chars().count() > 200 {
                    clipped_line = true;
                    let clipped: String = visible.chars().take(200).collect();
                    format!("{}…", clipped)
                } else {
                    visible
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let truncated_preview = line_count > 8 || clipped_line;

        let mut decision: Option<bool> = None;
        // 通过引用让 checkbox 在 self 上持久(对话框可能跨多帧)。
        let mut dont_ask_again = if had_visual_spoofing {
            false
        } else {
            self.paste_dont_ask_again
        };
        // Some(true) = paste, Some(false) = cancel.
        let modal_response = egui::Modal::new(egui::Id::new("paste_confirmation_modal"))
            .frame(egui::Frame {
                fill: panel_bg,
                stroke: egui::Stroke::new(1.0, border),
                corner_radius: egui::CornerRadius::same(10),
                inner_margin: egui::Margin::same(14),
                ..Default::default()
            })
            .show(ctx, |ui| {
                ui.set_max_width(640.0);
                ui.heading("⚠ 确认粘贴");
                ui.label(
                    egui::RichText::new(format!(
                        "粘贴包含 {} 行 / {} 字节,执行前请确认内容:",
                        line_count, byte_len
                    ))
                    .color(text_color),
                );
                if had_embedded_marker {
                    ui.label(
                        egui::RichText::new(
                            "⚠ 剪贴板内嵌括号粘贴结束序列(ESC[201~),已剔除;\
                             这通常意味着有人想让剩余内容被 shell 直接执行。",
                        )
                        .color(text_color),
                    );
                }
                if had_visual_spoofing {
                    ui.label(
                        egui::RichText::new(
                            "⚠ 剪贴板包含不可见、双向或非标准空白字符；预览已将其转义。\
                             此类粘贴始终需要逐次确认。",
                        )
                        .color(text_color),
                    );
                }
                ui.add_space(6.0);
                egui::Frame::group(ui.style())
                    .stroke(egui::Stroke::new(1.0, border))
                    .show(ui, |ui| {
                        ui.set_min_width(600.0);
                        egui::ScrollArea::vertical()
                            .max_height(220.0)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&preview).monospace().color(text_color),
                                    )
                                    .wrap(),
                                );
                                if truncated_preview {
                                    ui.label(
                                        egui::RichText::new("…(预览已截断)").color(text_color),
                                    );
                                }
                            });
                    });
                ui.add_space(8.0);
                if !had_visual_spoofing {
                    ui.add_enabled(
                        decision_armed,
                        egui::Checkbox::new(&mut dont_ask_again, "不再询问(可在配置里重新开启)"),
                    );
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(decision_armed, egui::Button::new("取消"))
                        .clicked()
                    {
                        decision = Some(false);
                    }
                    if ui
                        .add_enabled(decision_armed, egui::Button::new("粘贴"))
                        .clicked()
                    {
                        decision = Some(true);
                    }
                });
                // Esc / Enter shortcuts.
                if decision_armed && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    decision = Some(false);
                }
                if decision_armed && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    decision = Some(true);
                }
            });
        decision =
            paste_confirmation_decision(decision_armed, decision, modal_response.should_close());

        self.paste_dont_ask_again = dont_ask_again;

        if !decision_armed {
            if let Some(pending) = self.pending_paste_confirm.as_mut() {
                pending.decision_armed = true;
            }
            ctx.request_repaint();
            return;
        }

        let Some(confirmed) = decision else {
            return;
        };
        // 用户做出选择后:若勾选"不再询问"则关掉确认对话框并落盘。
        // 取消粘贴时也尊重选择,符合"我不想再被打扰"的语义。
        if dont_ask_again && self.config.paste_confirm {
            self.config.paste_confirm = false;
            match self.config.save() {
                Ok(()) => {}
                Err(error) => {
                    eprintln!(
                        "[Config] failed to save paste_confirm preference: {}",
                        error
                    );
                    self.set_status_for(
                        format!("Paste preference changed for this run but was not saved: {error}"),
                        std::time::Duration::from_secs(6),
                    );
                }
            }
        }
        self.paste_dont_ask_again = false;
        let pending = self.pending_paste_confirm.take().expect("pending was Some");
        if !confirmed {
            return;
        }
        // 只在仍是同一个 tab 时投递,避免误粘到刚切换过去的会话。
        if self
            .session_manager
            .get_active_session_mut()
            .metadata
            .session_id
            != pending.session_id
        {
            return;
        }
        // Encoded here rather than when the dialog opened: the shell may have
        // entered or left bracketed-paste mode while the modal was up, and the
        // framing has to match the mode that is live at delivery time.
        let direct_input_blocked = self.direct_input_is_blocked_for_session(&pending.session_id);
        let write_result = {
            let session = self.session_manager.get_active_session_mut();
            crate::write_paste_to_session(
                session,
                &pending.text,
                pending.submit_after_paste,
                direct_input_blocked,
            )
        };
        match write_result {
            Ok(true) => self.clear_block_selection_for_session(&pending.session_id),
            Ok(false) => {}
            Err(error) => {
                let retryable = error.is_retryable();
                self.set_status_for(
                    format!("Paste failed: {error}"),
                    std::time::Duration::from_secs(4),
                );
                if retryable {
                    // Busy/Full admitted zero bytes, so reopening with the
                    // normalized source is a safe delivery-time-mode retry.
                    self.pending_paste_confirm = Some(pending);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_search_hits_share_one_sequence_bookmark_state() {
        let live = std::collections::HashMap::from([
            ("same-record".to_string(), 42),
            ("other-record".to_string(), 43),
        ]);
        let bookmarks = std::collections::HashSet::from([42]);
        // Command and output hits carry the same record id; every virtual row
        // therefore reflects the same sequence truth without per-row state.
        assert!(block_search_record_is_bookmarked(
            "same-record",
            &live,
            &bookmarks
        ));
        assert!(block_search_record_is_bookmarked(
            "same-record",
            &live,
            &bookmarks
        ));
        assert!(!block_search_record_is_bookmarked(
            "other-record",
            &live,
            &bookmarks
        ));
    }

    #[test]
    fn virtual_row_widget_identity_follows_hit_not_visual_index() {
        let hit = |record_id: &str, line_no: Option<usize>| crate::block_mode::BlockSearchHit {
            record_id: record_id.to_string(),
            is_output_line: line_no.is_some(),
            line_no,
            match_span: None,
            line_text: String::new(),
            command_preview: String::new(),
        };
        let version = crate::block_search::BlockSearchRecordVersion {
            len: 2,
            oldest_sequence: Some(7),
            newest_sequence: Some(8),
        };
        let first = hit("record-a", Some(2));
        let second = hit("record-b", None);
        let before_reorder = block_search_row_widget_identity("pane", version, &first);
        let reordered_hits = [second, first.clone()];
        assert_eq!(
            before_reorder,
            block_search_row_widget_identity("pane", version, &reordered_hits[1]),
            "moving the same hit to another visual index keeps pointer ownership"
        );
        assert_ne!(
            before_reorder,
            block_search_row_widget_identity("pane", version, &reordered_hits[0])
        );
        assert_ne!(
            before_reorder,
            block_search_row_widget_identity(
                "pane",
                crate::block_search::BlockSearchRecordVersion {
                    newest_sequence: Some(9),
                    ..version
                },
                &first,
            ),
            "a retained-record generation change cancels an in-flight click"
        );
    }

    #[test]
    fn idle_and_short_tail_samples_do_not_change_the_budget() {
        let budget = 64 * 1024;
        assert_eq!(
            adapt_frame_budget(budget, 0, std::time::Duration::from_secs(1), false),
            budget
        );
        assert_eq!(
            adapt_frame_budget(
                budget,
                MIN_ADAPTIVE_SAMPLE_BYTES - 1,
                std::time::Duration::from_millis(20),
                true,
            ),
            budget
        );
        assert_eq!(
            adapt_frame_budget(budget, budget, std::time::Duration::from_millis(20), false,),
            budget
        );
    }

    #[test]
    fn saturated_fast_and_slow_samples_move_in_the_expected_direction() {
        let budget = 64 * 1024;
        let faster = adapt_frame_budget(budget, budget, std::time::Duration::from_millis(1), true);
        let slower = adapt_frame_budget(budget, budget, std::time::Duration::from_millis(16), true);
        assert!(faster > budget);
        assert!(slower < budget);
        assert!(faster <= budget * 5 / 4);
        assert!(slower >= budget * 3 / 4);
    }

    #[test]
    fn adaptive_budget_always_respects_hard_bounds() {
        assert_eq!(
            adapt_frame_budget(1, usize::MAX, std::time::Duration::from_nanos(1), true,),
            MIN_FRAME_BUDGET * 17 / 16
        );

        let mut high = MAX_FRAME_BUDGET;
        for _ in 0..8 {
            high = adapt_frame_budget(high, usize::MAX, std::time::Duration::from_nanos(1), true);
        }
        assert_eq!(high, MAX_FRAME_BUDGET);

        let mut low = MIN_FRAME_BUDGET;
        for _ in 0..8 {
            low = adapt_frame_budget(
                low,
                MIN_ADAPTIVE_SAMPLE_BYTES,
                std::time::Duration::from_secs(1),
                true,
            );
        }
        assert_eq!(low, MIN_FRAME_BUDGET);
    }

    #[test]
    fn risky_paste_opening_batch_cannot_confirm_or_cancel_its_first_render() {
        assert_eq!(paste_confirmation_decision(false, Some(true), false), None);
        assert_eq!(paste_confirmation_decision(false, Some(false), true), None);
        assert_eq!(
            paste_confirmation_decision(true, Some(true), false),
            Some(true)
        );
        assert_eq!(paste_confirmation_decision(true, None, true), Some(false));
    }

    #[test]
    fn agent_command_rejects_barriers_and_pending_input_before_arming() {
        assert!(agent_input_route_is_clean(false, false));
        assert!(!agent_input_route_is_clean(true, false));
        assert!(!agent_input_route_is_clean(false, true));
        assert!(!agent_input_route_is_clean(true, true));
    }

    #[test]
    fn accepted_paste_frame_disables_all_terminal_render_interaction() {
        assert!(terminal_frame_interaction_enabled(false, false));
        assert!(!terminal_frame_interaction_enabled(false, true));
        assert!(!terminal_frame_interaction_enabled(true, false));
        assert!(!terminal_frame_interaction_enabled(true, true));
    }

    #[test]
    fn permanently_evicted_collapse_requests_return_to_the_identity_fast_path() {
        let mut terminal = crate::terminal::TerminalState::new(12, 6);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;C;id=fold\x07OUT\r\n\x1b]133;D;0;id=fold\x07",
        );
        let zone_id = terminal.command_records().back().unwrap().sequence;
        assert!(terminal.finished_output_range(zone_id).is_some());
        let mut policy = crate::terminal::ProjectionPolicy::new();
        assert!(policy.collapse(zone_id));

        // A real resize deliberately invalidates exact raw output ownership.
        // Keeping the request would make every identity frame resolve a stale
        // policy forever even though no menu target can restore it.
        terminal.on_resize(13, 6);
        let mut checked = None;
        prune_permanently_unavailable_collapses(&mut policy, &terminal, &mut checked);
        assert!(policy.is_identity());
        assert_eq!(
            checked,
            Some((policy.revision(), terminal.finished_output_revision()))
        );
        assert!(!collapse_availability_check_needed(
            checked,
            policy.revision(),
            terminal.finished_output_revision(),
        ));
        assert!(collapse_availability_check_needed(
            checked,
            policy.revision().saturating_add(1),
            terminal.finished_output_revision(),
        ));
        assert!(collapse_availability_check_needed(
            checked,
            policy.revision(),
            0
        ));
    }
}
