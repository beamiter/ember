// Rendering coordination module

use super::state::TerminalApp;
use crate::theme::ThemeExt as _;
use crate::{command_palette, config, config_panel, layout, search_replace_panel, theme};
use eframe::egui;

const MIN_FRAME_BUDGET: usize = 16 * 1024;
const MAX_FRAME_BUDGET: usize = 256 * 1024;
const TARGET_PARSE_TIME: std::time::Duration = std::time::Duration::from_millis(4);
const MIN_ADAPTIVE_SAMPLE_BYTES: usize = 4 * 1024;

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
        let pane_count = self.layout_manager.panes.len();
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
        let show_repo_strip = self.config.show_repo_strip;
        self.pane_status_cache
            .get(&session_id, now, || {
                let raw_cwd = reported_cwd.or_else(|| jterm_core::process::process_cwd(shell_pid));
                // The git probe rides the same sub-second cadence as the /proc
                // reads, and its own cache only runs git when the session is
                // new, changed directory, or finished a command.
                let git = show_repo_strip
                    .then(|| {
                        git_strip_cache.strip(&session_id, raw_cwd.as_deref(), |cwd| {
                            jterm_core::git_meta::read(std::path::Path::new(cwd))
                                .map(|meta| jterm_core::git_meta::format_strip(&meta))
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
                    .or_else(|| crate::session_manager::get_foreground_command(shell_pid));
                crate::pane_header::PaneStatus {
                    title,
                    cwd,
                    running_command,
                    git,
                }
            })
            .clone()
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
        let statuses: Vec<crate::pane_header::PaneStatus> = panes
            .iter()
            .map(|pane| self.pane_status(pane.session_idx, now))
            .collect();

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
                    .on_hover_text("Drag onto another pane to swap them");
                Some((pane.session_idx, response))
            })
            .collect();

        let pointer_pos = ctx.input(|input| input.pointer.latest_pos());

        if interaction_enabled {
            if self.pane_drag.is_none() {
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
                .and_then(|pos| self.layout_manager.session_at(pos))
                .filter(|target| *target != source)
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
            if let (Some(source), Some(target)) = (drag_source, drop_target) {
                if self.layout_manager.swap_sessions(source, target) {
                    self.sync_active_session_to_focused_pane();
                    self.schedule_session_save();
                    self.set_status("Swapped panes");
                    ctx.request_repaint();
                }
            }
            self.pane_drag = None;
        }
    }

    pub fn render_terminal_content(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let interaction_enabled = !self.terminal_input_blocked(ctx);
        if !interaction_enabled {
            self.dragging_divider = None;
        }
        // 终端显示区域
        self.renderer.sync_font_metrics(ctx);
        let available_rect = ui.available_rect_before_wrap();
        // Keep the focused pane's geometry current even in single-pane mode,
        // so split commands can validate the resulting child sizes before
        // creating another shell session.
        self.layout_manager.compute_pane_rects(available_rect);
        self.ensure_pane_renderer_capacity(ctx);
        let (cols, rows) = self.renderer.grid_dimensions(ui.available_size());
        crate::debug_log!("[RESIZE] grid_dimensions => {}x{}", cols, rows);

        // 单窗格才按整窗口尺寸 resize 活跃会话;多窗格时各窗格在下方
        // 各自按自己的 rect 尺寸 resize(否则活跃会话会被错误地撑成整窗口大小)。
        let multi_pane = self.layout_manager.panes().len() > 1;
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

        // 多窗格支持：如果有多于一个窗格，则进行分屏渲染
        if self.layout_manager.panes().len() > 1 {
            // 获取所有窗格信息
            let panes = self.layout_manager.panes().to_vec();
            let divider_rects = self.layout_manager.get_divider_rects();
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
                    let mut terminal_guard = session.terminal.lock();
                    if pane_cols != terminal_guard.grid.row_len()
                        || pane_rows != terminal_guard.grid.rows()
                    {
                        terminal_guard.on_resize(pane_cols, pane_rows);
                        let _ = session.shell.resize(pane_cols, pane_rows);
                    }
                    // per-pane 链接缓存:仅当 grid 或滚动变化时重建,避免每帧重做
                    // 链接检测(含逐行 String 分配)。失效条件与单窗格路径一致。
                    let grid_version = terminal_guard.get_grid_version();
                    let scroll_offset = terminal_guard.scroll_offset;
                    let renderer = &mut self.pane_renderers[pane_idx];
                    if grid_version != renderer.cached_links_grid_version
                        || scroll_offset != renderer.cached_links_scroll_offset
                        || terminal_ptr != renderer.cached_links_terminal_ptr
                    {
                        let visible_cells = terminal_guard.get_visible_cells();
                        let row_wrapped = terminal_guard.get_visible_row_wrapped();
                        renderer.cached_links = std::sync::Arc::new(
                            self.link_detector
                                .detect_links_in_visible_cells_with_wrapping(
                                    &visible_cells,
                                    &row_wrapped,
                                ),
                        );
                        renderer.cached_links_grid_version = grid_version;
                        renderer.cached_links_scroll_offset = scroll_offset;
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

                    // 在指定矩形内渲染（多窗格模式专用方法）
                    renderer.render_in_rect(
                        ui,
                        &mut terminal_guard,
                        interaction_enabled,
                        interaction_enabled && pane.focused,
                        pane_cursor_visible,
                        pane_search,
                        &links,
                        pane_hovered_link,
                        content_rect,
                    );
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
            let double_clicked_divider = interaction_enabled.then(|| {
                divider_interactions
                    .iter()
                    .find(|(_, response)| response.double_clicked_by(egui::PointerButton::Primary))
                    .map(|(divider, _)| divider.clone())
            });
            if let Some(divider) = double_clicked_divider.flatten() {
                if self.layout_manager.set_split_ratio(&divider.id, 0.5) {
                    self.schedule_session_save();
                }
                self.dragging_divider = None;
                self.set_status("Reset split to 50/50");
                ctx.request_repaint();
            } else if interaction_enabled && self.dragging_divider.is_none() {
                self.dragging_divider = divider_interactions
                    .iter()
                    .find(|(_, response)| response.is_pointer_button_down_on())
                    .map(|(divider, _)| divider.id.clone());
            }

            if interaction_enabled {
                if let Some(split_id) = self.dragging_divider.clone() {
                    // The layout resolves the divider's own node rectangle and
                    // snaps near even pair splits.
                    if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                        if self.layout_manager.drag_divider_to(&split_id, pos) {
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
            if interaction_enabled
                && self.dragging_divider.is_none()
                && ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary))
            {
                if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                    let on_divider = divider_rects
                        .iter()
                        .any(|divider| divider.rect.contains(pos));
                    if !on_divider && self.layout_manager.focus_pane_at(pos).is_some() {
                        self.sync_active_session_to_focused_pane();
                    }
                }
            }
        } else {
            // 单窗格渲染（原有逻辑）
            {
                let session = self.session_manager.get_active_session_mut();
                let terminal_ptr = std::sync::Arc::as_ptr(&session.terminal) as usize;
                let mut terminal_guard = session.terminal.lock();

                // 获取链接列表用于渲染（使用缓存）
                let grid_version = terminal_guard.get_grid_version();
                let scroll_offset = terminal_guard.scroll_offset;

                if grid_version != self.cached_links_grid_version
                    || scroll_offset != self.cached_links_scroll_offset
                    || terminal_ptr != self.cached_links_terminal_ptr
                {
                    let visible_cells = terminal_guard.get_visible_cells();
                    let row_wrapped = terminal_guard.get_visible_row_wrapped();
                    self.cached_links = self
                        .link_detector
                        .detect_links_in_visible_cells_with_wrapping(&visible_cells, &row_wrapped);
                    self.cached_links_grid_version = grid_version;
                    self.cached_links_scroll_offset = scroll_offset;
                    self.cached_links_terminal_ptr = terminal_ptr;
                }
                self.renderer.render(
                    ui,
                    &mut terminal_guard,
                    interaction_enabled,
                    self.cursor_visible,
                    &self.search_state,
                    &self.cached_links,
                    &self.hovered_link,
                );
            }
        }
    }

    #[allow(deprecated)]
    pub fn render_floating_panels(&mut self, ctx: &egui::Context) {
        const LIVE_SEARCH_REFRESH_INTERVAL: std::time::Duration =
            std::time::Duration::from_millis(300);
        let (search_needs_refresh, delayed_refresh) = if self.search_state.is_open {
            let session_idx = self.session_manager.active_index();
            let grid_version = {
                let session = self.session_manager.get_active_session_mut();
                session.terminal.lock().get_grid_version()
            };
            let session_changed = self.search_state.results_session_idx != Some(session_idx);
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
                || self.search_state.results_truncated
            {
                82.0
            } else {
                52.0
            };
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
                                    let keybinding_str = if pretty.is_empty() {
                                        "No binding".to_string()
                                    } else {
                                        pretty.join(" / ")
                                    };

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                egui::RichText::new(keybinding_str)
                                                    .size(10.0)
                                                    .color(egui::Color32::from_rgb(100, 150, 200)),
                                            );
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
                    // Apply all buffered edit values to config
                    self.config_panel.apply_to_config(&mut self.config);
                    // Update theme
                    if let Some(t) = theme::Theme::get_theme(&self.config.theme) {
                        self.current_theme = t.clone();
                    }
                    // Apply runtime changes (fonts, GPU, renderer)
                    self.apply_runtime_config(ctx);
                    // Save to file
                    match self.config.save() {
                        Ok(()) => {
                            self.config_last_mtime = config::Config::config_mtime();
                            self.set_status("Settings saved");
                        }
                        Err(error) => {
                            eprintln!("[Config] Failed to save: {}", error);
                            self.set_status_for(
                                format!("Settings are active but could not be saved: {error}"),
                                std::time::Duration::from_secs(6),
                            );
                        }
                    }
                }
                config_panel::ConfigAction::ResetToDefaults => {
                    self.config = config::Config::default();
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
                                let paste_result = {
                                    let session = self.session_manager.get_active_session_mut();
                                    crate::paste_text_into_session(
                                        session,
                                        result,
                                        self.config.paste_confirm,
                                        false,
                                        &mut self.pending_paste_confirm,
                                    )
                                };
                                match paste_result {
                                    Ok(true) if self.pending_paste_confirm.is_some() => {
                                        self.search_replace_panel.status =
                                            "Awaiting paste confirmation".to_string();
                                    }
                                    Ok(true) => {
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
            let cwd = {
                let session = self.session_manager.get_active_session_mut();
                let terminal = session.terminal.lock();
                terminal.current_working_dir.clone()
            };
            let shell = self
                .config
                .shell
                .clone()
                .unwrap_or_else(|| std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string()));
            self.agent_panel.drive(&self.config, cwd.as_deref(), &shell);
            let effects = self.agent_panel.show(ctx);
            for effect in effects {
                match effect {
                    crate::agent_panel::AgentEffect::RunCommand {
                        session_index,
                        command,
                    } => {
                        let mut bytes = command.into_bytes();
                        bytes.push(b'\r');
                        match self.session_manager.get_session_mut(session_index) {
                            Some(session) => {
                                if !session.queue_input(&bytes) {
                                    self.set_status("Agent command rejected: input queue is full");
                                }
                            }
                            None => self.set_status("Agent session's terminal no longer exists"),
                        }
                    }
                }
            }
        }
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

        let panel_bg = crate::theme::Theme::rgb_to_color32(self.current_theme.ui.panel_bg);
        let text_color = crate::theme::Theme::rgb_to_color32(self.current_theme.ui.text);
        let border = crate::theme::Theme::rgb_to_color32(self.current_theme.ui.border);

        let line_count = pending.text.lines().count();
        let byte_len = pending.text.len();
        // First few lines as a preview; truncate long single lines too.
        let preview: String = pending
            .text
            .lines()
            .take(8)
            .map(|l| {
                if l.len() > 200 {
                    let clipped: String = l.chars().take(200).collect();
                    format!("{}…", clipped)
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let truncated_preview = line_count > 8 || pending.text.len() != preview.len();

        let mut decision: Option<bool> = None;
        // 通过引用让 checkbox 在 self 上持久(对话框可能跨多帧)。
        let mut dont_ask_again = self.paste_dont_ask_again;
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
                ui.checkbox(&mut dont_ask_again, "不再询问(可在配置里重新开启)");
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("取消").clicked() {
                        decision = Some(false);
                    }
                    if ui.button("粘贴").clicked() {
                        decision = Some(true);
                    }
                });
                // Esc / Enter shortcuts.
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    decision = Some(false);
                }
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    decision = Some(true);
                }
            });
        if modal_response.should_close() {
            decision = Some(false);
        }

        self.paste_dont_ask_again = dont_ask_again;

        let Some(confirmed) = decision else {
            return;
        };
        // 用户做出选择后:若勾选"不再询问"则关掉确认对话框并落盘。
        // 取消粘贴时也尊重选择,符合"我不想再被打扰"的语义。
        if dont_ask_again && self.config.paste_confirm {
            self.config.paste_confirm = false;
            match self.config.save() {
                Ok(()) => {
                    self.config_last_mtime = config::Config::config_mtime();
                }
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
        let paste_bytes = crate::encode_terminal_paste(
            &pending.text,
            pending.bracketed,
            pending.submit_after_paste,
        );
        let write_result = {
            let session = self.session_manager.get_active_session_mut();
            session.shell.write(&paste_bytes)
        };
        if let Err(error) = write_result {
            let retryable = error.is_backpressure();
            self.set_status_for(
                format!("Paste failed: {error}"),
                std::time::Duration::from_secs(4),
            );
            if retryable {
                // The bounded writer guarantees Full enqueued zero
                // bytes, so reopening the confirmation is a safe exact retry.
                self.pending_paste_confirm = Some(pending);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
