// Tab management module

use super::state::TerminalApp;
use crate::theme::ThemeExt as _;
use eframe::egui;

impl TerminalApp {
    /// 关闭指定会话,并同步修正分屏窗格保存的 session_idx,避免删除后索引错位。
    /// 返回是否真的关闭了会话。
    pub fn close_session_synced(&mut self, index: usize) -> bool {
        let removed_session_id = self
            .session_manager
            .sessions()
            .get(index)
            .map(|session| session.metadata.session_id.clone());
        if !self.session_manager.close_session(index) {
            return false;
        }
        if self
            .pending_paste_confirm
            .as_ref()
            .map(|pending| &pending.session_id)
            == removed_session_id.as_ref()
        {
            self.pending_paste_confirm = None;
            self.paste_dont_ask_again = false;
        }
        if self
            .terminal_mouse_capture
            .as_ref()
            .map(|capture| &capture.session_id)
            == removed_session_id.as_ref()
        {
            self.terminal_mouse_capture = None;
            self.last_terminal_mouse_motion = None;
        }
        let fallback = self.session_manager.active_index();
        self.layout_manager.on_session_removed(index, fallback);
        self.layout_manager.show_session(fallback);
        self.force_resize_session = true;
        if self.search_state.is_open {
            self.refresh_search_matches();
        }
        true
    }

    /// 重排 tab 并同步分屏窗格保存的会话索引。
    pub fn reorder_sessions_synced(&mut self, from_idx: usize, to_idx: usize) {
        if from_idx == to_idx
            || from_idx >= self.session_manager.len()
            || to_idx >= self.session_manager.len()
        {
            return;
        }
        self.session_manager.reorder_sessions(from_idx, to_idx);
        self.layout_manager.on_session_reordered(from_idx, to_idx);
    }

    /// 会话标题:用户双击重命名设置的 custom_name 优先;否则用 shell 当前工作
    /// 目录(对 HOME 做 ~ 缩写);最后回退到会话名。
    pub fn session_cwd_title(session: &crate::session::Session) -> String {
        if let Some(ref custom) = session.metadata.custom_name {
            if !custom.is_empty() {
                return custom.clone();
            }
        }
        let pid = session.get_shell_pid();
        crate::session_manager::get_process_cwd(pid)
            .map(|cwd| {
                if let Ok(home) = std::env::var("HOME") {
                    if cwd == home {
                        "~".to_string()
                    } else if let Some(rest) = cwd.strip_prefix(&home) {
                        format!("~{}", rest)
                    } else {
                        cwd
                    }
                } else {
                    cwd
                }
            })
            .unwrap_or_else(|| session.metadata.name.clone())
    }

    /// 在侧边栏内以垂直列表渲染会话标签(Sidebar tab 模式)。
    /// 与顶部 tab bar 行为对齐:支持按住 5px 阈值后竖向拖拽重排,松开时插入到目标行位置。
    pub fn render_sidebar_sessions(&mut self, ui: &mut egui::Ui) {
        let visible_sessions: Vec<usize> = self
            .layout_manager
            .panes()
            .iter()
            .map(|pane| pane.session_idx)
            .collect();
        self.session_manager.refresh_unseen_flags(&visible_sessions);
        let active = self.session_manager.active_index();
        let infos: Vec<(usize, String, bool)> = self
            .session_manager
            .sessions()
            .iter()
            .enumerate()
            .map(|(i, s)| (i, Self::session_cwd_title(s), s.metadata.unseen_output))
            .collect();
        let multi = infos.len() > 1;

        let mut switch_to: Option<usize> = None;
        let mut close_idx: Option<usize> = None;
        let mut new_session = false;
        let mut reorder: Option<(usize, usize)> = None;
        let mut begin_rename: Option<usize> = None;
        // 提交/取消重命名需要在循环外处理,这里只收集事件,避免与 self 借用冲突。
        let mut commit_rename: Option<(usize, String)> = None;
        let mut cancel_rename = false;

        // 拖拽阈值与顶部 tab bar 保持一致(5px),用 y 轴判断
        let ctx = ui.ctx().clone();
        let pointer_pos = ctx.input(|i| i.pointer.latest_pos());
        let is_actually_dragging = match (self.dragging_tab, self.drag_start_pos, pointer_pos) {
            (Some(_), Some(start_y), Some(p)) => (p.y - start_y).abs() > 5.0,
            _ => false,
        };

        // 收集本帧每行的矩形,渲染后用于命中检测/插入指示线
        let mut row_rects: Vec<(usize, egui::Rect)> = Vec::with_capacity(infos.len());

        egui::ScrollArea::vertical()
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let row_h = ui.spacing().interact_size.y;
                for (i, title, unseen) in &infos {
                    let is_active = *i == active;
                    let is_dragging_this = self.dragging_tab == Some(*i);
                    let is_renaming_this =
                        self.renaming_tab.as_ref().map(|(idx, _)| *idx) == Some(*i);
                    let row_rect = egui::Rect::from_min_size(
                        ui.cursor().min,
                        egui::vec2(ui.available_width(), row_h),
                    );
                    let row_hovered = pointer_pos.map(|p| row_rect.contains(p)).unwrap_or(false);
                    let row_resp = ui.horizontal(|ui| {
                        ui.set_min_height(row_h);
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            if multi {
                                if row_hovered {
                                    let close_resp = ui
                                        .add_sized([row_h, row_h], egui::Button::new("✕").small())
                                        .on_hover_text("关闭会话");
                                    if close_resp.clicked() {
                                        close_idx = Some(*i);
                                    }
                                } else {
                                    ui.add_space(row_h);
                                }
                            }
                            if is_renaming_this {
                                // 重命名输入框:取出当前 buf,绘制 TextEdit,事件落入 commit/cancel
                                let mut buf = self
                                    .renaming_tab
                                    .as_ref()
                                    .map(|(_, b)| b.clone())
                                    .unwrap_or_default();
                                let edit = egui::TextEdit::singleline(&mut buf)
                                    .desired_width(ui.available_width())
                                    .hint_text("(空=清除自定义名)");
                                let r = ui.add_sized([ui.available_width(), row_h], edit);
                                r.request_focus();
                                // 同步回 self
                                if let Some((_, ref mut existing)) = self.renaming_tab {
                                    *existing = buf.clone();
                                }
                                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                                let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                                let lost_focus = r.lost_focus() && !enter && !esc;
                                if enter {
                                    commit_rename = Some((*i, buf));
                                } else if esc || lost_focus {
                                    cancel_rename = true;
                                }
                            } else {
                                // 后台 tab 有未查看输出时用圆点提醒。
                                let marker = if !is_active && *unseen { "• " } else { "  " };
                                // 拖拽中的源 tab 略微淡化
                                let dim = is_dragging_this && is_actually_dragging;
                                let btn = egui::Button::selectable(
                                    is_active,
                                    egui::RichText::new(format!("{}{}", marker, title)).color(
                                        if dim {
                                            ui.visuals().weak_text_color()
                                        } else if is_active {
                                            ui.visuals().strong_text_color()
                                        } else {
                                            ui.visuals().text_color()
                                        },
                                    ),
                                )
                                .sense(egui::Sense::click_and_drag());
                                let resp = ui
                                    .add_sized([ui.available_width(), row_h], btn)
                                    .on_hover_text("双击重命名");

                                // 拖拽开始:仅在按下且尚未跟踪时记录起点
                                if resp.drag_started() {
                                    self.dragging_tab = Some(*i);
                                    self.drag_start_pos =
                                        resp.interact_pointer_pos().or(pointer_pos).map(|p| p.y);
                                }
                                if resp.double_clicked() {
                                    begin_rename = Some(*i);
                                } else if resp.clicked() && !is_actually_dragging {
                                    switch_to = Some(*i);
                                }
                            }
                        });
                    });
                    row_rects.push((*i, row_resp.response.rect));
                }
            });

        // 拖拽结束:松开鼠标
        let any_released = ctx.input(|i| i.pointer.any_released());
        if any_released {
            if is_actually_dragging {
                if let (Some(from_idx), Some(p)) = (self.dragging_tab, pointer_pos) {
                    // 找到光标所在行;否则若超出列表,夹到最后一行
                    let mut target_idx = from_idx;
                    let mut matched = false;
                    for (idx, rect) in &row_rects {
                        if p.y >= rect.top() && p.y < rect.bottom() {
                            target_idx = *idx;
                            matched = true;
                            break;
                        }
                    }
                    if !matched {
                        if let Some((idx, rect)) = row_rects.last() {
                            if p.y >= rect.bottom() {
                                target_idx = *idx;
                            }
                        }
                        if let Some((idx, rect)) = row_rects.first() {
                            if p.y < rect.top() {
                                target_idx = *idx;
                            }
                        }
                    }
                    if target_idx != from_idx {
                        reorder = Some((from_idx, target_idx));
                    }
                }
            }
            self.dragging_tab = None;
            self.drag_start_pos = None;
        }

        // 拖拽过程中绘制插入指示线
        if is_actually_dragging {
            if let (Some(from_idx), Some(p)) = (self.dragging_tab, pointer_pos) {
                let accent =
                    crate::theme::Theme::rgb_to_color32(self.renderer.theme.tabbar.active_border);
                let painter = ui.painter();
                let mut drawn = false;
                for (idx, rect) in &row_rects {
                    if p.y >= rect.top() && p.y < rect.bottom() {
                        // 按光标在行内的上/下半决定插入到该行上沿还是下沿
                        let line_y = if p.y - rect.center().y < 0.0 {
                            rect.top()
                        } else {
                            rect.bottom()
                        };
                        let _ = idx;
                        painter.hline(
                            rect.left()..=rect.right(),
                            line_y,
                            egui::Stroke::new(2.0, accent),
                        );
                        drawn = true;
                        break;
                    }
                }
                if !drawn {
                    if let Some((_, rect)) = row_rects.last() {
                        if p.y >= rect.bottom() {
                            painter.hline(
                                rect.left()..=rect.right(),
                                rect.bottom(),
                                egui::Stroke::new(2.0, accent),
                            );
                        }
                    }
                }
                let _ = from_idx;
                // 拖拽中持续重绘
                ctx.request_repaint();
            }
        }

        ui.add_space(4.0);
        if ui.button("＋ New session").clicked() {
            new_session = true;
        }

        if let Some((from_idx, to_idx)) = reorder {
            // 重排后索引会漂移,正在编辑的重命名失效,避免提交到错的会话
            self.renaming_tab = None;
            self.reorder_sessions_synced(from_idx, to_idx);
            self.schedule_session_save();
        }
        if let Some(i) = switch_to {
            self.activate_session(i);
        }
        if let Some(i) = close_idx {
            if self.session_manager.len() > 1 {
                self.renaming_tab = None;
                self.close_session_synced(i);
                self.schedule_session_save();
            }
        }
        if new_session {
            let idx = self.create_session_with_current_config(None, None);
            self.activate_session(idx);
            self.schedule_session_save();
        }
        if let Some(i) = begin_rename {
            let initial = self
                .session_manager
                .sessions()
                .get(i)
                .map(|s| {
                    s.metadata
                        .custom_name
                        .clone()
                        .unwrap_or_else(|| Self::session_cwd_title(s))
                })
                .unwrap_or_default();
            self.renaming_tab = Some((i, initial));
        }
        if let Some((i, new_name)) = commit_rename {
            self.apply_rename(i, new_name);
        } else if cancel_rename {
            self.renaming_tab = None;
        }
    }

    /// 应用 tab 重命名:trim 后写入 custom_name(空串等同清除自定义名,回退到 CWD 标题)。
    /// 触发持久化,确保下次启动保留用户标签。
    pub fn apply_rename(&mut self, i: usize, raw: String) {
        let raw_trimmed_len = raw.trim().len();
        let trimmed = crate::session_persistence::bounded_session_name(&raw);
        let was_truncated = trimmed.len() < raw_trimmed_len;
        if let Some(s) = self.session_manager.get_session_mut(i) {
            s.metadata.custom_name = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.clone())
            };
            // name 字段同时同步,既影响 fallback 也用于持久化里的 name 字段。
            if !trimmed.is_empty() {
                s.metadata.name = trimmed;
            }
        }
        self.renaming_tab = None;
        self.schedule_session_save();
        if was_truncated {
            self.set_status("Tab name was shortened to the 256-byte persistence limit");
        }
    }

    /// 渲染会话标签栏。返回 true 表示请求关闭窗口，render_ui 应据此提前返回。
    /// Sidebar tab 模式下的精简顶部栏：仅含侧边栏 toggle(☰)，用于预留顶部空间，
    /// 避免用浮动按钮直接覆盖终端内容造成 UI 干扰。
    pub fn render_sidebar_mode_top_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let tab_height = 30.0;
        let tab_rect = egui::Rect::from_min_size(
            ui.cursor().left_top(),
            egui::vec2(ui.available_width(), tab_height),
        );

        let tb = self.renderer.theme.tabbar.clone();
        let tb_bg = crate::theme::Theme::rgb_to_color32(tb.bg);
        let tb_inactive_text = crate::theme::Theme::rgb_to_color32(tb.inactive_text);
        let tb_active_text = crate::theme::Theme::rgb_to_color32(tb.active_text);
        let tab_hover_fill = egui::Color32::from_white_alpha(18);

        let painter = ui.painter();
        let tab_alpha = (self.renderer.opacity * 255.0) as u8;
        painter.rect_filled(
            tab_rect,
            0.0,
            egui::Color32::from_rgba_unmultiplied(tb_bg.r(), tb_bg.g(), tb_bg.b(), tab_alpha),
        );

        let hover_pos = ctx.input(|i| i.pointer.hover_pos());
        let mouse_released = ctx.input(|i| i.pointer.any_released());

        let toggle_btn_rect = egui::Rect::from_min_size(
            egui::pos2(tab_rect.left() + 5.0, tab_rect.top() + 5.0),
            egui::vec2(26.0, tab_height - 10.0),
        );
        let btn_hovered = hover_pos
            .map(|p| toggle_btn_rect.contains(p))
            .unwrap_or(false);
        let btn_t = ctx.animate_bool_with_time(
            egui::Id::new("sidebar_mode_toggle_btn_hover"),
            btn_hovered,
            0.12,
        );
        painter.rect_filled(toggle_btn_rect, 6.0, tab_hover_fill.gamma_multiply(btn_t));
        painter.text(
            toggle_btn_rect.center(),
            egui::Align2::CENTER_CENTER,
            "☰",
            egui::FontId::proportional(15.0),
            if btn_hovered {
                tb_active_text
            } else {
                tb_inactive_text
            },
        );
        if btn_hovered {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            if mouse_released {
                self.sidebar.visible = !self.sidebar.visible;
                if self.sidebar.visible {
                    self.sidebar.refresh();
                }
            }
        }

        // ⬒ 标签栏位置切换（当前为侧边栏模式 → 点击移回顶部）
        let pos_btn_rect = egui::Rect::from_min_size(
            egui::pos2(tab_rect.left() + 5.0 + 28.0, tab_rect.top() + 5.0),
            egui::vec2(26.0, tab_height - 10.0),
        );
        let pos_hovered = hover_pos.map(|p| pos_btn_rect.contains(p)).unwrap_or(false);
        let pos_t = ctx.animate_bool_with_time(
            egui::Id::new("sidebar_mode_tabpos_btn_hover"),
            pos_hovered,
            0.12,
        );
        painter.rect_filled(pos_btn_rect, 6.0, tab_hover_fill.gamma_multiply(pos_t));
        painter.text(
            pos_btn_rect.center(),
            egui::Align2::CENTER_CENTER,
            "⬒",
            egui::FontId::proportional(14.0),
            if pos_hovered {
                tb_active_text
            } else {
                tb_inactive_text
            },
        );
        if pos_hovered {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            if mouse_released {
                self.toggle_tab_bar_position();
            }
        }

        // 关闭窗口按钮（最右侧，与顶部 tab 模式保持一致）
        let close_win_size = 25.0;
        let close_win_rect = egui::Rect::from_min_size(
            egui::pos2(
                tab_rect.right() - close_win_size - 5.0,
                tab_rect.top() + 5.0,
            ),
            egui::vec2(close_win_size, tab_height - 10.0),
        );
        let close_win_hovered = hover_pos
            .map(|p| close_win_rect.contains(p))
            .unwrap_or(false);
        let close_win_bg = if close_win_hovered {
            egui::Color32::from_rgb(200, 60, 55)
        } else {
            egui::Color32::TRANSPARENT
        };
        painter.rect_filled(close_win_rect, 6.0, close_win_bg);

        let cw_cross = 5.0;
        let cw_center = close_win_rect.center();
        let cw_x_color = if close_win_hovered {
            egui::Color32::WHITE
        } else {
            tb_inactive_text
        };
        painter.line_segment(
            [
                egui::pos2(cw_center.x - cw_cross, cw_center.y - cw_cross),
                egui::pos2(cw_center.x + cw_cross, cw_center.y + cw_cross),
            ],
            egui::Stroke::new(1.5, cw_x_color),
        );
        painter.line_segment(
            [
                egui::pos2(cw_center.x + cw_cross, cw_center.y - cw_cross),
                egui::pos2(cw_center.x - cw_cross, cw_center.y + cw_cross),
            ],
            egui::Stroke::new(1.5, cw_x_color),
        );
        if close_win_hovered {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            if mouse_released {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        ui.allocate_exact_size(
            egui::vec2(ui.available_width(), tab_height),
            egui::Sense::hover(),
        );
    }

    pub fn render_tab_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
        let visible_sessions: Vec<usize> = self
            .layout_manager
            .panes()
            .iter()
            .map(|pane| pane.session_idx)
            .collect();
        self.session_manager.refresh_unseen_flags(&visible_sessions);
        let tab_height = 30.0;
        let close_btn_size = 14.0;
        let tab_rect = egui::Rect::from_min_size(
            ui.cursor().left_top(),
            egui::vec2(ui.available_width(), tab_height),
        );

        let painter = ui.painter();

        // 主题色（一次性取出，避免后续借用冲突）
        let tb = self.renderer.theme.tabbar.clone();
        let tb_bg = crate::theme::Theme::rgb_to_color32(tb.bg);
        let tb_accent = crate::theme::Theme::rgb_to_color32(tb.active_border);
        let tb_inactive_text = crate::theme::Theme::rgb_to_color32(tb.inactive_text);
        let tb_active_text = crate::theme::Theme::rgb_to_color32(tb.active_text);
        let tb_close_bg = crate::theme::Theme::rgb_to_color32(tb.close_btn_bg);
        let tb_close_hover = crate::theme::Theme::rgb_to_color32(tb.close_btn_hover);
        // 在栏背景上叠加的「悬停/活跃」填充：以中性白做低透明 tint，跨深浅主题都协调
        let tab_hover_fill = egui::Color32::from_white_alpha(18);
        let tab_active_fill = tb_bg.lerp_to_gamma(tb_accent, 0.16);

        // 背景
        let tab_alpha = (self.renderer.opacity * 255.0) as u8;
        painter.rect_filled(
            tab_rect,
            0.0,
            egui::Color32::from_rgba_unmultiplied(tb_bg.r(), tb_bg.g(), tb_bg.b(), tab_alpha),
        );

        // === Tab 布局常量 ===
        let tab_padding = 20.0 + close_btn_size + 4.0; // 文本左右 padding + 关闭按钮
        let min_tab_width: f32 = 60.0;
        let max_tab_width: f32 = 200.0;
        let active_tab_extra: f32 = 60.0;
        let active_min_width: f32 = min_tab_width * 2.0; // 当前 session 最小宽度，更突出
        let tab_spacing: f32 = 1.0;
        let base_left_margin: f32 = 5.0;
        // 顶栏左侧始终预留两个控件位：☰ 侧边栏开关 + ⬓ 标签栏位置切换。
        // 标签从这两个控件之后开始排布，避免遮挡。
        let ctrl_btn_w: f32 = 28.0;
        let left_margin: f32 = base_left_margin + ctrl_btn_w * 2.0 + 4.0;
        let reserved_right: f32 = 80.0; // "+"按钮 + 关闭窗口按钮 + margin

        let active_idx_for_layout = self.session_manager.active_index();

        // 文本测量闭包
        let measure = |text: &str| -> f32 {
            painter
                .layout_no_wrap(
                    text.to_string(),
                    egui::FontId::monospace(12.0),
                    egui::Color32::WHITE,
                )
                .rect
                .width()
        };

        // 路径缩略闭包：将 CWD 路径缩略到 max_text_w 像素以内
        let abbreviate_path = |title: &str, max_text_w: f32| -> String {
            if measure(title) <= max_text_w {
                return title.to_string();
            }
            let (prefix, path_part) = if let Some(rest) = title.strip_prefix("~/") {
                ("~/", rest)
            } else if let Some(rest) = title.strip_prefix('/') {
                ("/", rest)
            } else {
                ("", title)
            };
            let parts: Vec<&str> = path_part.split('/').collect();
            if parts.len() <= 1 {
                let ellipsis = "...";
                let mut truncated = String::new();
                for ch in title.chars() {
                    let test = format!("{}{}{}", truncated, ch, ellipsis);
                    if measure(&test) > max_text_w {
                        break;
                    }
                    truncated.push(ch);
                }
                return format!("{}{}", truncated, ellipsis);
            }
            let last = parts[parts.len() - 1];
            let abbreviated_middle: Vec<String> = parts[..parts.len() - 1]
                .iter()
                .map(|p| p.chars().next().map(|c| c.to_string()).unwrap_or_default())
                .collect();
            let short_path = format!("{}{}/{}", prefix, abbreviated_middle.join("/"), last);
            if measure(&short_path) <= max_text_w {
                return short_path;
            }
            let short_prefix = format!("{}{}/", prefix, abbreviated_middle.join("/"));
            let ellipsis = "...";
            let mut truncated = short_prefix.clone();
            for ch in last.chars() {
                let test = format!("{}{}{}", truncated, ch, ellipsis);
                if measure(&test) > max_text_w {
                    break;
                }
                truncated.push(ch);
            }
            format!("{}{}", truncated, ellipsis)
        };

        // === 第一阶段：收集原始路径 + 为每个 tab 生成 display_text ===
        // 活跃 tab 允许更大的文本宽度
        let active_max_text = max_tab_width + active_tab_extra - tab_padding;
        let inactive_max_text = max_tab_width - tab_padding;

        let tab_unseen: Vec<bool> = self
            .session_manager
            .sessions()
            .iter()
            .map(|s| s.metadata.unseen_output)
            .collect();

        let tab_infos: Vec<(usize, String, f32)> = self
            .session_manager
            .sessions()
            .iter()
            .enumerate()
            .map(|(idx, session)| {
                let tab_title = Self::session_cwd_title(session);

                let max_text_w = if idx == active_idx_for_layout {
                    active_max_text
                } else {
                    inactive_max_text
                };
                let display_text = abbreviate_path(&tab_title, max_text_w);
                let ideal_width = if idx == active_idx_for_layout {
                    (measure(&display_text) + tab_padding).max(active_min_width)
                } else {
                    (measure(&display_text) + tab_padding).clamp(min_tab_width, max_tab_width)
                };
                (idx, display_text, ideal_width)
            })
            .collect();

        // === 第二阶段：布局分配 —— 计算每个 tab 的最终宽度 ===
        let available_width = tab_rect.width() - left_margin - reserved_right;
        let n = tab_infos.len();
        let total_spacing = if n > 1 {
            (n - 1) as f32 * tab_spacing
        } else {
            0.0
        };

        let tab_widths: Vec<f32> = {
            let total_ideal: f32 = tab_infos.iter().map(|(_, _, w)| w).sum::<f32>() + total_spacing;

            if total_ideal <= available_width {
                // 空间足够，各自用理想宽度
                tab_infos.iter().map(|(_, _, w)| *w).collect()
            } else {
                // 空间不足：先保障活跃 tab，压缩非活跃 tab
                let active_w = tab_infos
                    .iter()
                    .find(|(idx, _, _)| *idx == active_idx_for_layout)
                    .map(|(_, _, w)| *w)
                    .unwrap_or(min_tab_width);
                let remaining = (available_width - active_w - total_spacing).max(0.0);
                let inactive_count = n.saturating_sub(1);

                if inactive_count == 0 {
                    // 只有一个 tab
                    vec![available_width.max(min_tab_width)]
                } else {
                    let per_inactive = (remaining / inactive_count as f32).max(min_tab_width);
                    tab_infos
                        .iter()
                        .map(|(idx, _, w)| {
                            if *idx == active_idx_for_layout {
                                // 活跃 tab 也不能超过可用空间
                                active_w.min(available_width - total_spacing)
                            } else {
                                (*w).min(per_inactive).max(min_tab_width)
                            }
                        })
                        .collect()
                }
            }
        };

        // === 第三阶段：滚动偏移 —— 保证活跃 tab 可见 ===
        {
            let total_width: f32 = tab_widths.iter().sum::<f32>() + total_spacing;
            let max_scroll = (total_width - available_width).max(0.0);

            if total_width <= available_width {
                self.tab_scroll_offset = 0.0;
            } else {
                // 计算活跃 tab 的位置
                let mut active_left: f32 = 0.0;
                for (i, tw) in tab_widths.iter().enumerate() {
                    if i == active_idx_for_layout {
                        break;
                    }
                    active_left += tw + tab_spacing;
                }
                let active_right = active_left
                    + tab_widths
                        .get(active_idx_for_layout)
                        .copied()
                        .unwrap_or(0.0);

                // 如果活跃 tab 左边超出可视区
                if active_left < self.tab_scroll_offset {
                    self.tab_scroll_offset = active_left;
                }
                // 如果活跃 tab 右边超出可视区
                if active_right > self.tab_scroll_offset + available_width {
                    self.tab_scroll_offset = active_right - available_width;
                }
                self.tab_scroll_offset = self.tab_scroll_offset.clamp(0.0, max_scroll);
            }
        }

        // 检测悬停位置（在绘制之前）
        let hover_pos = ctx.input(|i| i.pointer.hover_pos());
        self.hovered_tab_index = None;

        // 更新当前鼠标x位置（用于拖拽动画）
        if let Some(pos) = hover_pos {
            self.current_mouse_x = pos.x;
        }

        // 检测鼠标释放（点击完成或拖拽结束）
        let mouse_released = ctx.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
        let mouse_pressed = ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
        // 双击 tab 进入重命名:此处只检测,具体哪一个 tab 在循环里命中后处理。
        let mouse_double_clicked = ctx.input(|i| {
            i.pointer
                .button_double_clicked(egui::PointerButton::Primary)
        });
        let mut begin_rename_idx: Option<usize> = None;
        let mut renaming_rect: Option<egui::Rect> = None;

        // === 顶栏左侧控件：☰ 侧边栏开关 + ⬓ 标签栏位置切换（始终显示）===
        {
            // ☰ 侧边栏开关
            let sb_btn_rect = egui::Rect::from_min_size(
                egui::pos2(tab_rect.left() + base_left_margin, tab_rect.top() + 5.0),
                egui::vec2(ctrl_btn_w - 4.0, tab_height - 10.0),
            );
            let sb_hovered = hover_pos.map(|p| sb_btn_rect.contains(p)).unwrap_or(false);
            let sb_t = ctx.animate_bool_with_time(
                egui::Id::new("sidebar_toggle_btn_hover"),
                sb_hovered,
                0.12,
            );
            painter.rect_filled(sb_btn_rect, 6.0, tab_hover_fill.gamma_multiply(sb_t));
            painter.text(
                sb_btn_rect.center(),
                egui::Align2::CENTER_CENTER,
                "☰",
                egui::FontId::proportional(15.0),
                if self.sidebar.visible || sb_hovered {
                    tb_active_text
                } else {
                    tb_inactive_text
                },
            );

            // ⬓ 标签栏位置切换（当前为顶部模式 → 点击移入侧边栏）
            let pos_btn_rect = egui::Rect::from_min_size(
                egui::pos2(
                    tab_rect.left() + base_left_margin + ctrl_btn_w,
                    tab_rect.top() + 5.0,
                ),
                egui::vec2(ctrl_btn_w - 4.0, tab_height - 10.0),
            );
            let pos_hovered = hover_pos.map(|p| pos_btn_rect.contains(p)).unwrap_or(false);
            let pos_t = ctx.animate_bool_with_time(
                egui::Id::new("tabpos_toggle_btn_hover"),
                pos_hovered,
                0.12,
            );
            painter.rect_filled(pos_btn_rect, 6.0, tab_hover_fill.gamma_multiply(pos_t));
            painter.text(
                pos_btn_rect.center(),
                egui::Align2::CENTER_CENTER,
                "⬓",
                egui::FontId::proportional(14.0),
                if pos_hovered {
                    tb_active_text
                } else {
                    tb_inactive_text
                },
            );

            if sb_hovered {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                if mouse_released {
                    self.sidebar.visible = !self.sidebar.visible;
                    if self.sidebar.visible {
                        self.sidebar.refresh();
                    }
                }
            }
            if pos_hovered {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                if mouse_released {
                    self.toggle_tab_bar_position();
                }
            }
        }

        // 检查是否发生了实际的拖拽（超过阈值距离）
        let is_actually_dragging =
            if let (Some(_), Some(start_x)) = (self.dragging_tab, self.drag_start_pos) {
                if let Some(current_pos) = ctx.input(|i| i.pointer.latest_pos()) {
                    let distance = (current_pos.x - start_x).abs();
                    distance > 5.0 // 5px拖拽阈值
                } else {
                    false
                }
            } else {
                false
            };

        // === 交互辅助：用 tab_widths 计算 tab 位置的宏 ===
        // scroll_base: 绝对坐标 x 基准（减去滚动偏移）
        let scroll_base = tab_rect.left() + left_margin - self.tab_scroll_offset;

        // 处理拖拽结束或点击
        if mouse_released {
            if is_actually_dragging {
                // 实际拖拽结束 - 计算drop目标并执行重排
                if let Some(from_idx) = self.dragging_tab {
                    if let Some(hover_pos) = hover_pos {
                        if tab_rect.contains(hover_pos) {
                            let mut x_off = scroll_base;
                            let mut target_idx = from_idx;

                            for (i, &tw) in tab_widths.iter().enumerate() {
                                if hover_pos.x >= x_off && hover_pos.x < x_off + tw {
                                    target_idx = i;
                                    break;
                                }
                                x_off += tw + tab_spacing;
                            }

                            // 执行重排
                            if target_idx != from_idx {
                                self.reorder_sessions_synced(from_idx, target_idx);
                            }
                        }
                    }
                }
                self.dragging_tab = None;
                self.drag_start_pos = None;
            } else {
                // 简单点击（没有发生实际拖拽）
                if let Some(click_pos) = hover_pos.or_else(|| ctx.input(|i| i.pointer.latest_pos()))
                {
                    if tab_rect.contains(click_pos) {
                        let mut x_off = scroll_base;
                        for (i, &tw) in tab_widths.iter().enumerate() {
                            let tab_rect_item = egui::Rect::from_min_size(
                                egui::pos2(x_off, tab_rect.top() + 5.0),
                                egui::vec2(tw, tab_height - 10.0),
                            );

                            let close_btn_rect = egui::Rect::from_min_size(
                                egui::pos2(
                                    tab_rect_item.right() - close_btn_size - 3.0,
                                    tab_rect_item.center().y - close_btn_size / 2.0,
                                ),
                                egui::vec2(close_btn_size, close_btn_size),
                            );

                            if close_btn_rect.contains(click_pos) {
                                if self.session_manager.len() > 1 {
                                    self.close_session_synced(i);
                                    self.schedule_session_save();
                                } else {
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                    return true;
                                }
                                self.dragging_tab = None;
                                self.drag_start_pos = None;
                                break;
                            } else if tab_rect_item.contains(click_pos) {
                                self.activate_session(i);
                                self.dragging_tab = None;
                                self.drag_start_pos = None;
                                break;
                            }

                            x_off += tw + tab_spacing;
                        }
                    }
                }
                // 清除拖拽状态（即使没有找到点击的tab）
                if self.dragging_tab.is_some() {
                    self.dragging_tab = None;
                    self.drag_start_pos = None;
                }
            }
        }

        // 检测拖拽开始（鼠标按下且移动）
        if mouse_pressed {
            if let Some(press_pos) = ctx.input(|i| i.pointer.press_origin()) {
                if self.dragging_tab.is_none() {
                    let mut x_off = scroll_base;
                    for (i, &tw) in tab_widths.iter().enumerate() {
                        let tab_rect_item = egui::Rect::from_min_size(
                            egui::pos2(x_off, tab_rect.top() + 5.0),
                            egui::vec2(tw, tab_height - 10.0),
                        );

                        let close_btn_rect = egui::Rect::from_min_size(
                            egui::pos2(
                                tab_rect_item.right() - close_btn_size - 3.0,
                                tab_rect_item.center().y - close_btn_size / 2.0,
                            ),
                            egui::vec2(close_btn_size, close_btn_size),
                        );

                        if tab_rect_item.contains(press_pos) && !close_btn_rect.contains(press_pos)
                        {
                            self.dragging_tab = Some(i);
                            self.drag_start_pos = Some(press_pos.x);
                            break;
                        }

                        x_off += tw + tab_spacing;
                    }
                }
            }
        }

        // 计算拖拽过程中的动画效果
        let mut drag_target_idx: Option<usize> = None;
        if is_actually_dragging {
            if let Some(hover_pos) = hover_pos {
                if let Some(_from_idx) = self.dragging_tab {
                    let mut x_off = scroll_base;
                    for (i, &tw) in tab_widths.iter().enumerate() {
                        if hover_pos.x >= x_off && hover_pos.x < x_off + tw {
                            drag_target_idx = Some(i);
                            break;
                        }
                        x_off += tw + tab_spacing;
                    }
                }
            }
            // 请求持续重绘以显示动画
            ctx.request_repaint();
        }

        // === 渲染 Tab 栏（使用 clip rect 裁剪溢出内容）===
        let tab_clip_rect = egui::Rect::from_min_max(
            egui::pos2(tab_rect.left() + left_margin, tab_rect.top()),
            egui::pos2(tab_rect.right() - reserved_right, tab_rect.bottom()),
        );
        let clipped_painter = painter.with_clip_rect(tab_clip_rect);

        let mut x_offset = scroll_base;
        let active_idx = self.session_manager.active_index();
        // 活跃指示条目标位置（非拖拽时用于滑动动画）
        let mut active_indicator_target: Option<(f32, f32, f32)> = None;

        // 绘制每个标签
        for (i, (_, display_text, _)) in tab_infos.iter().enumerate() {
            let tab_width = tab_widths[i];
            let mut tab_rect_item = egui::Rect::from_min_size(
                egui::pos2(x_offset, tab_rect.top() + 5.0),
                egui::vec2(tab_width, tab_height - 10.0),
            );

            let is_active = i == active_idx;
            let is_dragging = self.dragging_tab == Some(i);
            let is_drag_target = drag_target_idx == Some(i);

            // 计算拖拽过程中的位移：被拖拽 tab 跟随鼠标；其余 tab 缓动让位
            let push_id = egui::Id::new(("tab_push", i));
            if is_actually_dragging && is_dragging {
                // 被拖拽的Tab跟随鼠标移动（即时，无缓动）
                if let Some(start_x) = self.drag_start_pos {
                    let offset = self.current_mouse_x - start_x;
                    tab_rect_item = tab_rect_item.translate(egui::vec2(offset, 0.0));
                }
                ctx.animate_value_with_time(push_id, 0.0, 0.0); // 重置让位动画
            } else {
                // 计算让位目标偏移，再缓动到该位置
                let mut push_target = 0.0;
                if is_actually_dragging {
                    if let Some(from_idx) = self.dragging_tab {
                        let drag_to_left = is_drag_target
                            && drag_target_idx.map(|t| t < from_idx).unwrap_or(false);
                        let drag_to_right = is_drag_target
                            && drag_target_idx.map(|t| t > from_idx).unwrap_or(false);
                        if drag_to_left && i > from_idx {
                            push_target = tab_width + tab_spacing;
                        } else if drag_to_right && i < from_idx {
                            push_target = -(tab_width + tab_spacing);
                        }
                    }
                }
                let push = ctx.animate_value_with_time(push_id, push_target, 0.12);
                if push.abs() > 0.1 {
                    tab_rect_item = tab_rect_item.translate(egui::vec2(push, 0.0));
                }
            }

            // 检测悬停
            let is_hovered = if let Some(hover_pos) = hover_pos {
                tab_rect_item.contains(hover_pos) && tab_clip_rect.contains(hover_pos)
            } else {
                false
            };

            if is_hovered && !is_actually_dragging {
                self.hovered_tab_index = Some(i);
            }

            // 背景色：圆角 pill 风格，hover 强度做淡入淡出
            let tab_rounding = 6.0;
            let hover_t = ctx.animate_bool_with_time(
                egui::Id::new(("tab_hover", i)),
                (is_hovered || is_dragging) && !is_actually_dragging,
                0.12,
            );
            let bg_color = if is_active {
                // 活跃 tab：基础填充，hover 时再微亮
                tab_active_fill.lerp_to_gamma(tab_active_fill.blend(tab_hover_fill), hover_t)
            } else {
                tab_hover_fill.gamma_multiply(hover_t)
            };

            // 绘制Tab背景
            if is_dragging && is_actually_dragging {
                let drag_bg = if is_active {
                    tab_active_fill.gamma_multiply(0.55)
                } else {
                    tab_hover_fill.gamma_multiply(0.55)
                };
                clipped_painter.rect_filled(tab_rect_item, tab_rounding, drag_bg);
            } else {
                clipped_painter.rect_filled(tab_rect_item, tab_rounding, bg_color);

                // Active Tab 底部圆角 accent 指示条
                if is_active {
                    if is_actually_dragging {
                        // 拖拽中：指示条立即跟随，不做滑动
                        let indicator = egui::Rect::from_min_max(
                            egui::pos2(tab_rect_item.left() + 6.0, tab_rect_item.bottom() - 3.0),
                            egui::pos2(tab_rect_item.right() - 6.0, tab_rect_item.bottom() - 1.0),
                        );
                        clipped_painter.rect_filled(indicator, 1.5, tb_accent);
                    } else {
                        // 记录目标，循环结束后用动画绘制滑动指示条
                        active_indicator_target = Some((
                            tab_rect_item.left(),
                            tab_rect_item.right(),
                            tab_rect_item.bottom(),
                        ));
                    }
                }

                // 拖拽过程中，在目标Tab位置显示插入指示线
                if is_drag_target && is_actually_dragging {
                    let insert_line_x = if self.current_mouse_x - tab_rect_item.center().x < 0.0 {
                        tab_rect_item.left()
                    } else {
                        tab_rect_item.right()
                    };
                    clipped_painter.vline(
                        insert_line_x,
                        tab_rect_item.top()..=tab_rect_item.bottom(),
                        egui::Stroke::new(2.0, tb_accent),
                    );
                }
            }

            // 双击检测:落在本 tab 矩形且可见 -> 进入重命名
            let is_renaming_this = self.renaming_tab.as_ref().map(|(idx, _)| *idx) == Some(i);
            if is_renaming_this {
                renaming_rect = Some(tab_rect_item);
            }
            if mouse_double_clicked && !is_actually_dragging {
                if let Some(p) = hover_pos {
                    if tab_rect_item.contains(p) && tab_clip_rect.contains(p) {
                        begin_rename_idx = Some(i);
                    }
                }
            }

            // 后台 tab 有未查看输出:在标题左侧画 accent 小圆点提示。
            // 活跃 tab 已是聚焦态,无需额外提示。
            let has_unseen = !is_active && tab_unseen.get(i).copied().unwrap_or(false);
            let text_left_x = if has_unseen {
                let dot_center = egui::pos2(tab_rect_item.left() + 8.0, tab_rect_item.center().y);
                clipped_painter.circle_filled(dot_center, 2.5, tb_accent);
                tab_rect_item.left() + 16.0
            } else {
                tab_rect_item.left() + 10.0
            };

            // 重命名中:跳过文本绘制,留给后续 TextEdit 覆盖,避免文字重叠
            if !is_renaming_this {
                // 绘制文本（使用 tab 内部 clip 防止文本溢出 tab 边界）
                let text_clip = egui::Rect::from_min_max(
                    tab_rect_item.left_top(),
                    egui::pos2(
                        tab_rect_item.right() - close_btn_size - 6.0,
                        tab_rect_item.bottom(),
                    ),
                );
                let text_painter = painter.with_clip_rect(text_clip.intersect(tab_clip_rect));
                text_painter.text(
                    egui::pos2(text_left_x, tab_rect_item.center().y),
                    egui::Align2::LEFT_CENTER,
                    display_text,
                    egui::FontId::monospace(12.0),
                    if is_active {
                        tb_active_text
                    } else {
                        tb_inactive_text
                    },
                );
            }

            // 绘制关闭按钮（仅在悬停Tab时显示）
            let close_btn_rect = egui::Rect::from_min_size(
                egui::pos2(
                    tab_rect_item.right() - close_btn_size - 3.0,
                    tab_rect_item.center().y - close_btn_size / 2.0,
                ),
                egui::vec2(close_btn_size, close_btn_size),
            );

            if is_hovered && !is_dragging {
                let close_btn_hovered = if let Some(hover_pos) = hover_pos {
                    close_btn_rect.contains(hover_pos)
                } else {
                    false
                };

                if close_btn_hovered {
                    clipped_painter.circle_filled(
                        close_btn_rect.center(),
                        close_btn_size / 2.0 + 2.0,
                        tb_close_bg,
                    );
                }

                let close_x_color = if close_btn_hovered {
                    tb_close_hover
                } else {
                    tb_inactive_text
                };

                let cross_offset = close_btn_size / 3.0;
                clipped_painter.line_segment(
                    [
                        egui::pos2(
                            close_btn_rect.center().x - cross_offset,
                            close_btn_rect.center().y - cross_offset,
                        ),
                        egui::pos2(
                            close_btn_rect.center().x + cross_offset,
                            close_btn_rect.center().y + cross_offset,
                        ),
                    ],
                    egui::Stroke::new(1.5, close_x_color),
                );
                clipped_painter.line_segment(
                    [
                        egui::pos2(
                            close_btn_rect.center().x + cross_offset,
                            close_btn_rect.center().y - cross_offset,
                        ),
                        egui::pos2(
                            close_btn_rect.center().x - cross_offset,
                            close_btn_rect.center().y + cross_offset,
                        ),
                    ],
                    egui::Stroke::new(1.5, close_x_color),
                );
            }

            x_offset += tab_width + tab_spacing;
        }

        // 活跃指示条滑动动画（非拖拽时）
        if let Some((target_left, target_right, bottom)) = active_indicator_target {
            let anim_left =
                ctx.animate_value_with_time(egui::Id::new("tab_indicator_left"), target_left, 0.15);
            let anim_right = ctx.animate_value_with_time(
                egui::Id::new("tab_indicator_right"),
                target_right,
                0.15,
            );
            let indicator = egui::Rect::from_min_max(
                egui::pos2(anim_left + 6.0, bottom - 3.0),
                egui::pos2(anim_right - 6.0, bottom - 1.0),
            );
            clipped_painter.rect_filled(indicator, 1.5, tb_accent);
        }

        // "+" 按钮 - 新建会话（紧跟最后一个 Tab，但不超过 clip 区域）
        let plus_btn_x = x_offset
            .max(tab_rect.left() + left_margin)
            .min(tab_clip_rect.right());
        let plus_btn_rect = egui::Rect::from_min_size(
            egui::pos2(plus_btn_x + 4.0, tab_rect.top() + 5.0),
            egui::vec2(25.0, tab_height - 10.0),
        );

        // 检测"+"按钮悬停
        let plus_btn_hovered = if let Some(hover_pos) = hover_pos {
            plus_btn_rect.contains(hover_pos)
        } else {
            false
        };

        let plus_btn_color = if plus_btn_hovered {
            tab_hover_fill
        } else {
            egui::Color32::TRANSPARENT
        };

        painter.rect_filled(plus_btn_rect, 6.0, plus_btn_color);

        let plus_text_color = if plus_btn_hovered {
            tb_active_text
        } else {
            tb_inactive_text
        };

        painter.text(
            plus_btn_rect.center(),
            egui::Align2::CENTER_CENTER,
            "+",
            egui::FontId::monospace(14.0),
            plus_text_color,
        );

        // 检测 "+" 按钮点击（在鼠标释放时）
        if mouse_released {
            if let Some(click_pos) = ctx.input(|i| i.pointer.latest_pos()) {
                if plus_btn_rect.contains(click_pos) {
                    let new_idx = self.create_session_with_current_config(None, None);
                    self.activate_session(new_idx);
                    self.schedule_session_save();
                }
            }
        }

        // 关闭窗口按钮（最右侧）
        let close_win_size = 25.0;
        let close_win_rect = egui::Rect::from_min_size(
            egui::pos2(
                tab_rect.right() - close_win_size - 5.0,
                tab_rect.top() + 5.0,
            ),
            egui::vec2(close_win_size, tab_height - 10.0),
        );

        let close_win_hovered = if let Some(hover_pos) = hover_pos {
            close_win_rect.contains(hover_pos)
        } else {
            false
        };

        let close_win_bg = if close_win_hovered {
            egui::Color32::from_rgb(200, 60, 55)
        } else {
            egui::Color32::TRANSPARENT
        };

        painter.rect_filled(close_win_rect, 6.0, close_win_bg);

        // 绘制 X 符号
        let cw_cross = 5.0;
        let cw_center = close_win_rect.center();
        let cw_x_color = if close_win_hovered {
            egui::Color32::WHITE
        } else {
            tb_inactive_text
        };
        painter.line_segment(
            [
                egui::pos2(cw_center.x - cw_cross, cw_center.y - cw_cross),
                egui::pos2(cw_center.x + cw_cross, cw_center.y + cw_cross),
            ],
            egui::Stroke::new(1.5, cw_x_color),
        );
        painter.line_segment(
            [
                egui::pos2(cw_center.x + cw_cross, cw_center.y - cw_cross),
                egui::pos2(cw_center.x - cw_cross, cw_center.y + cw_cross),
            ],
            egui::Stroke::new(1.5, cw_x_color),
        );

        // 检测关闭窗口按钮点击
        if mouse_released {
            if let Some(click_pos) = ctx.input(|i| i.pointer.latest_pos()) {
                if close_win_rect.contains(click_pos) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

        // 向下移动光标
        ui.allocate_exact_size(
            egui::vec2(ui.available_width(), tab_height),
            egui::Sense::hover(),
        );

        // 进入重命名:用 begin_rename_idx 标记的会话当前标题做初值。
        if let Some(i) = begin_rename_idx {
            let initial = self
                .session_manager
                .sessions()
                .get(i)
                .map(|s| {
                    s.metadata
                        .custom_name
                        .clone()
                        .unwrap_or_else(|| Self::session_cwd_title(s))
                })
                .unwrap_or_default();
            self.renaming_tab = Some((i, initial));
        }

        // 渲染重命名输入框:Area 覆盖在 tab 矩形上方,foreground 层级保证可见。
        // commit(Enter)写入 custom_name + 持久化;cancel(Esc/失焦)放弃。
        if let (Some((idx, _)), Some(rect)) = (self.renaming_tab.clone(), renaming_rect) {
            let mut buf = self
                .renaming_tab
                .as_ref()
                .map(|(_, b)| b.clone())
                .unwrap_or_default();
            let mut do_commit = false;
            let mut do_cancel = false;
            egui::Area::new(egui::Id::new(("tab_rename_overlay", idx)))
                .order(egui::Order::Foreground)
                .fixed_pos(rect.left_top())
                .show(ctx, |ui| {
                    ui.allocate_ui_with_layout(
                        rect.size(),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            let r = ui.add_sized(
                                rect.size(),
                                egui::TextEdit::singleline(&mut buf)
                                    .desired_width(rect.width() - 8.0)
                                    .font(egui::FontId::monospace(12.0))
                                    .hint_text("空=清除自定义名"),
                            );
                            r.request_focus();
                            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                            let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                            if enter {
                                do_commit = true;
                            } else if esc || (r.lost_focus() && !enter) {
                                do_cancel = true;
                            }
                        },
                    );
                });
            if do_commit {
                self.apply_rename(idx, buf);
            } else if do_cancel {
                self.renaming_tab = None;
            } else if let Some((_, ref mut existing)) = self.renaming_tab {
                *existing = buf;
            }
        }

        false
    }
}
