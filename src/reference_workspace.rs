//! Native adapter for the shared reconstruction contract.
//!
//! This module owns chrome only. The live PTY, projection, IME and reading
//! anchors continue to belong to the existing terminal renderer and session.
use crate::glass_shell::{self as shell, color};
use crate::reference_conversation_ui::icon;
use crate::{
    app::state::{ReferenceInspectorRequest, ReferenceInspectorTab, TerminalApp},
    config, sidebar, theme,
};
use egui::{Color32, FontId, RichText, Stroke};
use jterm_core::{reference_conversation as conversation, reference_icons::Icon};

pub(crate) fn rgb(value: [u8; 3]) -> Color32 {
    shell::paint_color(value)
}

/// Reuse the shipped native identities, decoded once per egui context.
fn product_identity(ui: &egui::Ui, rect: egui::Rect, product: &str) {
    let key = egui::Id::new(("reference-product-identity", product));
    let cached = ui
        .ctx()
        .data(|data| data.get_temp::<egui::TextureHandle>(key));
    let texture = cached.or_else(|| {
        let png = jterm_core::reference_identity::png(product)?;
        let pixels = image::load_from_memory(png).ok()?.into_rgba8();
        let size = [pixels.width() as usize, pixels.height() as usize];
        let image = egui::ColorImage::from_rgba_unmultiplied(size, pixels.as_raw());
        let texture = ui
            .ctx()
            .load_texture(product, image, egui::TextureOptions::LINEAR);
        ui.ctx()
            .data_mut(|data| data.insert_temp(key, texture.clone()));
        Some(texture)
    });
    if let Some(texture) = texture {
        ui.painter().image(
            texture.id(),
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}

/// A small native mesh keeps the light canvas continuous without adding layers
/// over text or moving any terminal cells.
pub(crate) fn paint_canvas_background(painter: &egui::Painter, rect: egui::Rect) {
    shell::paint_panel(painter, rect, shell::token::CENTER);
}

/// A compact inspector occupies a bounded overlay surface, never another
/// terminal/session or a second copy of its widgets.
pub(crate) fn show_inspector_drawer<R>(
    root: &mut egui::Ui,
    rect: egui::Rect,
    id: &'static str,
    frame: egui::Frame,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let ctx = root.ctx().clone();
    crate::ui::remember_reference_chrome_scope(
        &ctx,
        egui::Id::new(id).with("input-scope"),
        rect,
        false,
    );
    egui::Area::new(egui::Id::new(id))
        // This is modeless chrome: existing dialogs and popup menus retain
        // their normal ability to appear in front of it.
        .order(egui::Order::Middle)
        .fixed_pos(rect.min)
        .movable(false)
        .constrain_to(ctx.viewport_rect())
        .show(&ctx, |ui| {
            ui.set_min_size(rect.size());
            ui.set_max_size(rect.size());
            ui.set_clip_rect(rect.intersect(ui.clip_rect()));
            shell::paint_panel(ui.painter(), rect, shell::token::INSPECTOR);
            frame
                .show(ui, |ui| {
                    ui.set_min_height(ui.available_height());
                    contents(ui)
                })
                .inner
        })
}

pub(crate) fn remember_inspector_control(ui: &egui::Ui, response: &egui::Response) {
    crate::ui::remember_reference_chrome_scope(
        ui.ctx(),
        response.id,
        response.rect,
        response.clicked(),
    );
    if response.has_focus() {
        ui.ctx().data_mut(|data| {
            data.insert_temp(egui::Id::new("block-action-focus"), response.id);
        });
    }
}

fn header_action(ui: &mut egui::Ui, kind: Icon, label: &str) -> egui::Response {
    let response = ui
        .push_id(("reference-header-action", label), |ui| {
            ui.spacing_mut().interact_size = egui::Vec2::splat(shell::HEADER_ACTION_SIZE);
            ui.spacing_mut().button_padding = egui::Vec2::ZERO;
            let visuals = ui.visuals_mut();
            visuals.widgets.inactive.bg_fill = shell::rgba(shell::token::BUTTON);
            visuals.widgets.inactive.weak_bg_fill = shell::rgba(shell::token::BUTTON);
            visuals.widgets.inactive.bg_stroke =
                Stroke::new(1.0, shell::rgba(shell::token::BORDER));
            visuals.widgets.hovered.bg_fill = shell::rgba(shell::token::BUTTON_HOVER);
            visuals.widgets.hovered.weak_bg_fill = shell::rgba(shell::token::BUTTON_HOVER);
            visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, shell::rgba(shell::token::BORDER));
            visuals.widgets.active.bg_fill = shell::rgba(shell::token::ACCENT_TOP);
            visuals.widgets.active.weak_bg_fill = shell::rgba(shell::token::ACCENT_TOP);
            visuals.widgets.active.bg_stroke = Stroke::new(1.0, rgb(color::ACCENT));
            ui.add_sized(
                [shell::HEADER_ACTION_SIZE, shell::HEADER_ACTION_SIZE],
                egui::Button::new("").corner_radius(shell::CONTROL_RADIUS),
            )
        })
        .inner;
    icon(
        ui,
        response.rect.shrink(4.0),
        kind,
        rgb(color::CANVAS_MUTED),
    );
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect,
            shell::CONTROL_RADIUS,
            Stroke::new(1.0, rgb(color::ACCENT)),
            egui::StrokeKind::Inside,
        );
        let id = response.id;
        ui.ctx()
            .data_mut(|data| data.insert_temp(egui::Id::new("block-action-focus"), id));
    }
    response.on_hover_text(label)
}

pub(crate) fn chrome_style(ui: &mut egui::Ui, scale: f32) {
    let style = ui.style_mut();
    style.visuals = egui::Visuals::dark();
    style.visuals.override_text_color = Some(rgb(color::CHROME_TEXT));
    style.visuals.panel_fill = Color32::TRANSPARENT;
    style.visuals.disabled_alpha = 1.0;
    style.visuals.window_fill = rgb(color::INSPECTOR_CARD);
    style.visuals.selection.bg_fill = rgb(color::NAVIGATION_SELECTED);
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, rgb(color::DARK_BORDER));
    style.visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    style.visuals.widgets.hovered.weak_bg_fill = rgb(color::NAVIGATION_SELECTED);
    style.spacing.item_spacing = egui::vec2(shell::METADATA_GAP, 4.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.spacing.interact_size.y = 28.0;
    for font in style.text_styles.values_mut() {
        *font = FontId::proportional(16.0 * scale);
    }
}

fn nav_row(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let response = ui
        .scope(|ui| {
            // Only navigation rows use this compact height; composer/tool
            // controls retain their shared 28px minimum hit area.
            ui.spacing_mut().interact_size.y = shell::NAVIGATION_ROW_HEIGHT;
            ui.add_sized(
                [ui.available_width(), shell::NAVIGATION_ROW_HEIGHT],
                egui::Button::new("")
                    .left_text(label)
                    .selected(selected)
                    .corner_radius(7)
                    .frame(selected),
            )
        })
        .inner;
    if selected {
        let rect = response.rect;
        ui.painter().rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(rect.right() - 2.0, rect.top() + 7.0),
                egui::vec2(2.0, rect.height() - 14.0),
            ),
            1,
            rgb(color::ACCENT),
        );
    }
    response
}

impl TerminalApp {
    fn reference_private_title(&self) -> bool {
        self.tabs.flags(self.tabs.active_index()).private_title
    }

    fn reference_workspace_path(&self) -> String {
        if self.reference_private_title() {
            "Private workspace".to_owned()
        } else {
            self.sidebar.current_dir.display().to_string()
        }
    }

    pub(crate) fn observe_reference_shell(&self, ctx: &egui::Context) {
        let size = ctx.viewport_rect().size();
        let state = format!(
            "presentation={} logical={}x{} dpr={}",
            if self.config.chat_mode_enabled() {
                "Chat"
            } else {
                "Block"
            },
            size.x,
            size.y,
            ctx.pixels_per_point()
        );
        let key = egui::Id::new("reference-shell-observation");
        let changed = ctx.data_mut(|data| {
            if data.get_temp::<String>(key).as_ref() == Some(&state) {
                false
            } else {
                data.insert_temp(key, state.clone());
                true
            }
        });
        if changed {
            log::info!(target: "ember::reference_workspace", "REFERENCE_SHELL_STATE {state}");
        }
        static TRACE_INSPECTOR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *TRACE_INSPECTOR
            .get_or_init(|| std::env::var_os("JTERM_REFERENCE_INSPECTOR_TRACE").is_some())
        {
            // Opt-in chrome metadata, not native widget geometry or PTY text.
            // Pixel captures independently verify actual allocation/placement.
            let layout = self.reference_layout(ctx);
            let observation = format!(
                "shown={} drawer={} tab={:?} center_x={} center_width={} composer_width={}",
                self.reference_inspector_visible(ctx),
                self.reference_inspector_drawer_rect(ctx).is_some(),
                self.reference_selected_inspector_tab(),
                layout.center.x,
                layout.center.width,
                layout.composer.width,
            );
            let key = egui::Id::new("reference-inspector-observation");
            let changed = ctx.data_mut(|data| {
                if data.get_temp::<String>(key).as_ref() == Some(&observation) {
                    false
                } else {
                    data.insert_temp(key, observation.clone());
                    true
                }
            });
            if changed {
                log::info!(target: "ember::reference_workspace", "REFERENCE_INSPECTOR_STATE {observation}");
            }
        }
    }

    pub(crate) fn reference_layout(&self, ctx: &egui::Context) -> shell::Layout {
        let size = ctx.viewport_rect().size();
        let layout = shell::Layout::for_size(size.x, size.y);
        if self.reference_inspector.open {
            layout
        } else {
            layout.without_inspector()
        }
    }

    pub(crate) fn reference_inspector_drawer_rect(
        &self,
        ctx: &egui::Context,
    ) -> Option<egui::Rect> {
        if !self.reference_inspector.open || !self.reference_inspector.narrow_drawer {
            return None;
        }
        let screen = ctx.viewport_rect();
        let layout = shell::Layout::for_size(screen.width(), screen.height());
        if layout.inspector.is_some() {
            return None;
        }
        let width = shell::INSPECTOR_WIDTH.min((screen.width() - layout.rail.width).max(0.0));
        let rect = egui::Rect::from_min_max(
            egui::pos2(
                screen.right() - shell::token::OUTER_RIGHT - width,
                screen.top() + shell::token::MAIN_TOP,
            ),
            egui::pos2(
                screen.right() - shell::token::OUTER_RIGHT,
                screen.bottom() - shell::token::SHORT_BOTTOM_GAP,
            ),
        );
        rect.is_positive().then_some(rect)
    }

    pub(crate) fn reference_inspector_visible(&self, ctx: &egui::Context) -> bool {
        self.reference_layout(ctx).inspector.is_some()
            || self.reference_inspector_drawer_rect(ctx).is_some()
    }

    pub(crate) fn reference_open_inspector(&mut self, ctx: &egui::Context) {
        self.reference_inspector.open = true;
        let screen = ctx.viewport_rect();
        if shell::Layout::for_size(screen.width(), screen.height())
            .inspector
            .is_none()
        {
            self.reference_inspector.narrow_drawer = true;
        }
    }

    pub(crate) fn reference_selected_inspector_tab(&self) -> ReferenceInspectorTab {
        if self.sidebar.view == sidebar::SidebarView::Files {
            ReferenceInspectorTab::Files
        } else if self.reference_inspector.selected_tab == ReferenceInspectorTab::Files {
            ReferenceInspectorTab::Agent
        } else {
            self.reference_inspector.selected_tab
        }
    }

    pub(crate) fn reference_select_inspector_tab(&mut self, tab: ReferenceInspectorTab) {
        let previous_view = self.sidebar.view;
        self.reference_inspector.selected_tab = tab;
        match tab {
            ReferenceInspectorTab::Files => {
                self.sidebar.view = sidebar::SidebarView::Files;
                self.sidebar.visible = true;
            }
            ReferenceInspectorTab::Agent if self.sidebar.view == sidebar::SidebarView::Files => {
                self.sidebar.view = sidebar::SidebarView::Tasks;
            }
            ReferenceInspectorTab::Context if self.sidebar.view == sidebar::SidebarView::Files => {
                self.sidebar.view = sidebar::SidebarView::Sessions;
            }
            _ => {}
        }
        self.reference_save_sidebar_view(previous_view);
    }

    fn reference_save_sidebar_view(&mut self, previous_view: sidebar::SidebarView) {
        if self.sidebar.view != previous_view {
            self.config.sidebar_view = self.sidebar.view;
            self.schedule_config_save();
            if self.sidebar.view == sidebar::SidebarView::Files {
                if let Some(error) = self.sidebar.refresh() {
                    self.set_status_for(
                        format!("Files refresh failed: {error}"),
                        std::time::Duration::from_secs(5),
                    );
                }
            }
        }
    }

    pub(crate) fn reference_request_inspector_tab(
        &mut self,
        tab: ReferenceInspectorTab,
        ctx: &egui::Context,
    ) {
        // A Files/Agent transition must not allocate both right panels in the
        // click frame. Keep the existing owner until the next frame begins.
        self.reference_inspector.pending = Some(ReferenceInspectorRequest::Tab(tab));
        self.return_focus_to_terminal(ctx);
        ctx.request_repaint();
    }

    fn reference_request_sidebar_view(&mut self, view: sidebar::SidebarView, ctx: &egui::Context) {
        self.sidebar.note_files_user_intent();
        self.reference_inspector.pending = Some(ReferenceInspectorRequest::Navigation(view));
        self.return_focus_to_terminal(ctx);
        ctx.request_repaint();
    }

    fn render_reference_inspector_toggle(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let shown = self.reference_inspector_visible(ui.ctx());
        let label = if shown {
            "Hide right sidebar"
        } else {
            "Show right sidebar"
        };
        let response = ui
            .push_id("reference-inspector-toggle", |ui| {
                ui.put(
                    rect,
                    egui::Button::new("")
                        .selected(shown)
                        .fill(if shown {
                            rgb(color::NAVIGATION_SELECTED)
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(Stroke::new(1.0, rgb(color::DARK_BORDER)))
                        .corner_radius(12),
                )
            })
            .inner
            .on_hover_text(label);
        icon(
            ui,
            rect.shrink2(egui::vec2(15.0, 10.0)),
            Icon::PanelRight,
            rgb(if shown {
                color::CHROME_TEXT
            } else {
                color::CHROME_MUTED
            }),
        );
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Button, true, shown, label)
        });
        remember_inspector_control(ui, &response);
        if response.clicked() {
            if shown {
                self.reference_inspector.open = false;
                self.reference_inspector.narrow_drawer = false;
            } else {
                self.reference_open_inspector(ui.ctx());
            }
            // No session/terminal/input reset: the normal resize path detects
            // the changed center width and retains its own reading anchor.
            self.return_focus_to_terminal(ui.ctx());
            ui.ctx().request_repaint();
        }
    }

    /// The surrounding workspace is dark; both native presentations share
    /// the light document and white PTY-owned composer.
    pub(crate) fn reference_terminal_theme(&self) -> theme::Theme {
        let mut theme = self.current_theme.clone();
        theme.terminal.background = [4, 10, 21];
        theme.terminal.foreground = [239, 245, 255];
        theme.terminal.cursor = color::ACCENT;
        theme.tabbar.active_border = color::ACCENT;
        theme
    }

    pub(crate) fn render_reference_topbar(&mut self, root: &mut egui::Ui) {
        if let Some(request) = self.reference_inspector.pending.take() {
            match request {
                ReferenceInspectorRequest::Tab(tab) => {
                    self.reference_select_inspector_tab(tab);
                    self.reference_open_inspector(root.ctx());
                }
                ReferenceInspectorRequest::Navigation(view) => {
                    let previous = self.sidebar.view;
                    self.sidebar.visible = true;
                    self.sidebar.view = view;
                    if view == sidebar::SidebarView::Files {
                        self.reference_inspector.selected_tab = ReferenceInspectorTab::Files;
                        self.reference_open_inspector(root.ctx());
                    } else if self.reference_inspector.selected_tab == ReferenceInspectorTab::Files
                    {
                        self.reference_inspector.selected_tab = ReferenceInspectorTab::Agent;
                    }
                    self.reference_save_sidebar_view(previous);
                }
            }
        }
        let scale = self.config.effective_ui_font_size() / config::DEFAULT_FONT_SIZE;
        let layout = self.reference_layout(root.ctx());
        for (rect, fill) in [
            (Some(layout.rail), shell::token::RAIL),
            (layout.navigation, shell::token::NAVIGATION),
            (Some(layout.center), shell::token::CENTER),
            (layout.inspector, shell::token::INSPECTOR),
        ] {
            if let Some(r) = rect {
                shell::paint_panel(
                    root.painter(),
                    egui::Rect::from_min_size(egui::pos2(r.x, r.y), egui::vec2(r.width, r.height)),
                    fill,
                );
            }
        }
        egui::Panel::top("reference-titlebar")
            .exact_size(shell::TITLEBAR_HEIGHT)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::NONE
                    .fill(shell::rgba(shell::token::TOP))
                    .inner_margin(0.0),
            )
            .show(root, |ui| {
                chrome_style(ui, scale);
                let bounds = ui.max_rect();
                let drag = ui.interact(bounds, ui.id().with("window-drag"), egui::Sense::drag());
                if drag.drag_started() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                for (i, glyph, tip) in [
                    (0, "×", "Close window"),
                    (1, "−", "Minimize window"),
                    (2, "□", "Maximize or restore window"),
                ] {
                    let r = egui::Rect::from_min_size(
                        egui::pos2(14.0 + i as f32 * 31.0, 19.0),
                        egui::vec2(28.0, 30.0),
                    );
                    if ui
                        .put(
                            r,
                            egui::Button::new(RichText::new(glyph).size(19.0 * scale))
                                .fill(Color32::TRANSPARENT)
                                .stroke(Stroke::NONE),
                        )
                        .on_hover_text(tip)
                        .clicked()
                    {
                        match i {
                            0 => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close),
                            1 => ui
                                .ctx()
                                .send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                            _ => {
                                let v = ui.input(|i| i.viewport().maximized.unwrap_or(false));
                                ui.ctx()
                                    .send_viewport_cmd(egui::ViewportCommand::Maximized(!v));
                            }
                        }
                    }
                }
                let brand =
                    egui::Rect::from_min_size(egui::pos2(128.0, 19.0), egui::vec2(33.0, 33.0));
                if bounds.width() > 470.0 {
                    product_identity(ui, brand, "ember");
                }
                if bounds.width() > 700.0 {
                    ui.painter().text(
                        egui::pos2(176.0, 35.0),
                        egui::Align2::LEFT_CENTER,
                        "Ember",
                        FontId::proportional(24.0 * scale),
                        shell::rgba(shell::token::TEXT),
                    );
                    ui.painter().text(
                        egui::pos2(262.0, 38.0),
                        egui::Align2::LEFT_CENTER,
                        if self.config.chat_mode_enabled() {
                            "Chat"
                        } else {
                            "Block"
                        },
                        FontId::proportional(14.0 * scale),
                        shell::rgba(shell::token::MUTED_TEXT),
                    );
                }
                let button_h = 42.0;
                let toggle_w = if bounds.width() > 1000.0 { 54.0 } else { 42.0 };
                let toggle = egui::Rect::from_min_size(
                    egui::pos2(bounds.right() - 12.0 - toggle_w, 16.0),
                    egui::vec2(toggle_w, button_h),
                );
                self.render_reference_inspector_toggle(ui, toggle);
                let settings = toggle.translate(egui::vec2(-toggle_w - 10.0, 0.0));
                let response = ui.put(
                    settings,
                    egui::Button::new("")
                        .fill(shell::rgba(shell::token::BUTTON))
                        .stroke(Stroke::new(1.0, shell::rgba(shell::token::BORDER)))
                        .corner_radius(12),
                );
                icon(
                    ui,
                    settings.shrink2(egui::vec2(15.0, 10.0)),
                    Icon::Settings,
                    shell::rgba(shell::token::TEXT),
                );
                if response.on_hover_text("Settings").clicked() {
                    self.config_panel.open(&self.config);
                }
                let mode_width = if bounds.width() > 1000.0 { 90.0 } else { 64.0 };
                let mode_padding = ui.spacing().button_padding;
                if bounds.width() <= 1000.0 {
                    ui.spacing_mut().button_padding.x = 6.0;
                }
                for (i, mode, label) in [
                    (0, config::TerminalMode::Chat, "Chat"),
                    (1, config::TerminalMode::Block, "Block"),
                ] {
                    let r = egui::Rect::from_min_size(
                        egui::pos2(
                            settings.left() - 18.0 - mode_width - i as f32 * (mode_width + 10.0),
                            16.0,
                        ),
                        egui::vec2(mode_width, button_h),
                    );
                    let selected =
                        (mode == config::TerminalMode::Chat) == self.config.chat_mode_enabled();
                    let response = ui.put(
                        r,
                        egui::Button::new(
                            RichText::new(label)
                                .size(16.0 * scale)
                                .color(shell::rgba(shell::token::TEXT)),
                        )
                        .truncate()
                        .fill(shell::rgba(if selected {
                            shell::token::ACCENT_TOP
                        } else {
                            shell::token::BUTTON
                        }))
                        .stroke(Stroke::new(1.0, shell::rgba(shell::token::BORDER)))
                        .corner_radius(12),
                    );
                    if response.clicked() && !selected {
                        self.config.terminal_mode = Some(mode);
                        self.apply_runtime_config(ui.ctx());
                        self.force_resize_session = true;
                        self.return_focus_to_terminal(ui.ctx());
                        self.config_panel.sync_from_config(&self.config);
                        self.schedule_config_save();
                    }
                }
                ui.spacing_mut().button_padding = mode_padding;
                if bounds.width() >= 1250.0 {
                    let width = (bounds.width() * 0.337).min(543.0);
                    let r = egui::Rect::from_min_size(
                        egui::pos2((bounds.width() - width) * 0.5, 16.0),
                        egui::vec2(width, 42.0),
                    );
                    if ui
                        .put(
                            r,
                            egui::Button::new("")
                                .fill(shell::rgba(shell::token::BUTTON))
                                .stroke(Stroke::new(1.0, shell::rgba(shell::token::BORDER)))
                                .corner_radius(12),
                        )
                        .on_hover_text("Open command launcher")
                        .clicked()
                    {
                        self.command_palette.open();
                    }
                    icon(
                        ui,
                        egui::Rect::from_center_size(
                            r.left_center() + egui::vec2(24.0, 0.0),
                            egui::vec2(20.0, 20.0),
                        ),
                        Icon::Search,
                        shell::rgba(shell::token::TEXT),
                    );
                    ui.painter().text(
                        r.left_center() + egui::vec2(50.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        "Search commands, files, tools…",
                        FontId::proportional(14.0 * scale),
                        shell::rgba(shell::token::MUTED_TEXT),
                    );
                    ui.painter().text(
                        r.right_center() - egui::vec2(12.0, 0.0),
                        egui::Align2::RIGHT_CENTER,
                        self.keybindings
                            .pretty_bindings_for("command_palette:toggle")
                            .first()
                            .map(String::as_str)
                            .unwrap_or("Unbound"),
                        FontId::proportional(13.0 * scale),
                        shell::rgba(shell::token::TEXT),
                    );
                }
            });
    }

    pub(crate) fn render_reference_optional_controls(&mut self, root: &mut egui::Ui) {
        let tabs = root.ctx().data_mut(|data| {
            data.get_temp::<bool>(egui::Id::new("reference:tabs"))
                .unwrap_or(false)
        });
        let status = root.ctx().data_mut(|data| {
            data.get_temp::<bool>(egui::Id::new("reference:status"))
                .unwrap_or(false)
        });
        let shell_updates = root.ctx().data_mut(|data| {
            data.get_temp::<bool>(egui::Id::new("reference:shell-updates"))
                .unwrap_or(false)
        });
        if tabs {
            egui::Panel::top("reference-session-controls").show(root, |ui| {
                let ctx = ui.ctx().clone();
                if self.show_top_tab_bar() {
                    self.render_tab_bar(ui, &ctx);
                } else {
                    self.render_sidebar_mode_top_bar(ui, &ctx);
                }
            });
        }
        if status {
            self.render_bottom_bar(root);
        }
        if shell_updates && self.render_jsh_notice(root) {
            self.install_or_update_jsh();
        }
    }

    pub(crate) fn render_reference_rail(&mut self, root: &mut egui::Ui) {
        let scale = self.config.effective_ui_font_size() / config::DEFAULT_FONT_SIZE;
        let layout = self.reference_layout(root.ctx());
        let short = root.ctx().viewport_rect().height() < shell::token::SHORT_HEIGHT;
        egui::Panel::left("reference-rail")
            .exact_size(layout.rail.width + shell::token::OUTER_LEFT)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin {
                left: 17,
                right: 12,
                top: 8,
                bottom: if short { 24 } else { 50 },
            }))
            .show(root, |ui| {
                chrome_style(ui, scale);
                for (key, hint) in conversation::RAIL_NAVIGATION {
                    let kind = if key == "workspace" {
                        Icon::Grid
                    } else {
                        Icon::from_key(key)
                    };
                    let view = match key {
                        "folder" => sidebar::SidebarView::Files,
                        "agent" => sidebar::SidebarView::Tasks,
                        _ => sidebar::SidebarView::Sessions,
                    };
                    let size = (layout.rail.width - 24.0).min(49.0);
                    let response = ui.add_sized(
                        [size, size],
                        egui::Button::new("")
                            .fill(if key == "workspace" {
                                shell::rgba(shell::token::ACCENT_TOP)
                            } else {
                                Color32::TRANSPARENT
                            })
                            .stroke(Stroke::NONE)
                            .corner_radius(12),
                    );
                    icon(
                        ui,
                        egui::Rect::from_center_size(
                            response.rect.center(),
                            egui::Vec2::splat(if size < 40.0 { 20.0 } else { 24.0 }),
                        ),
                        kind,
                        shell::rgba(shell::token::TEXT),
                    );
                    if response.on_hover_text(hint).clicked() {
                        if key == "search" {
                            self.command_palette.open();
                        } else {
                            self.reference_request_sidebar_view(view, ui.ctx());
                        }
                    }
                    ui.add_space(8.0);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    let initial = std::env::var("USER")
                        .ok()
                        .and_then(|s| s.chars().next())
                        .unwrap_or('U')
                        .to_uppercase()
                        .to_string();
                    ui.add_sized(
                        [48.0, 48.0],
                        egui::Button::new(RichText::new(initial).size(20.0 * scale))
                            .fill(shell::rgba(shell::token::BUTTON))
                            .stroke(Stroke::new(1.0, shell::rgba(shell::token::BORDER)))
                            .corner_radius(24),
                    )
                    .on_hover_text("Local profile");
                });
            });
    }

    pub(crate) fn render_reference_navigation(&mut self, ui: &mut egui::Ui) -> bool {
        let scale = self.config.effective_ui_font_size() / config::DEFAULT_FONT_SIZE;
        chrome_style(
            ui,
            self.config.effective_ui_font_size() / config::DEFAULT_FONT_SIZE,
        );
        ui.spacing_mut().item_spacing.y = shell::NAVIGATION_ROW_GAP;
        ui.label(
            RichText::new("WORKSPACE")
                .size(13.0 * scale)
                .color(rgb(color::CHROME_MUTED)),
        );
        let full_path = self.reference_workspace_path();
        let workspace = if self.reference_private_title() {
            full_path.clone()
        } else {
            self.sidebar
                .current_dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| full_path.clone())
        };
        ui.add_space(8.0);
        let selector = ui
            .add_enabled_ui(false, |ui| {
                ui.add_sized(
                    [ui.available_width(), shell::WORKSPACE_SELECTOR_HEIGHT],
                    egui::Button::new("")
                        .fill(rgb(color::INSPECTOR_CARD))
                        .corner_radius(8),
                )
            })
            .inner;
        icon(
            ui,
            egui::Rect::from_center_size(
                selector.rect.left_center() + egui::vec2(17.0, 0.0),
                egui::vec2(16.0, 16.0),
            ),
            Icon::Files,
            rgb(color::CHROME_MUTED),
        );
        let name_rect = egui::Rect::from_min_max(
            selector.rect.min + egui::vec2(32.0, 0.0),
            selector.rect.max - egui::vec2(48.0, 0.0),
        );
        ui.painter().with_clip_rect(name_rect).text(
            name_rect.left_center(),
            egui::Align2::LEFT_CENTER,
            workspace,
            egui::FontId::proportional(16.0 * scale),
            rgb(color::CHROME_TEXT),
        );
        ui.painter().text(
            selector.rect.right_center() - egui::vec2(32.0, 0.0),
            egui::Align2::CENTER_CENTER,
            "—",
            egui::FontId::proportional(9.0 * scale),
            rgb(color::CHROME_MUTED),
        );
        icon(
            ui,
            egui::Rect::from_center_size(
                selector.rect.right_center() - egui::vec2(12.0, 0.0),
                egui::vec2(12.0, 12.0),
            ),
            Icon::ChevronDown,
            rgb(color::CHROME_MUTED),
        );
        selector.on_disabled_hover_text(format!(
            "Workspace switching is unavailable\n{full_path}\nBranch not reported"
        ));
        ui.add_space(8.0);
        for (key, label) in conversation::NAVIGATION {
            let label = if key == "conversation" {
                if self.config.chat_mode_enabled() {
                    "Chat"
                } else {
                    "Block"
                }
            } else {
                label
            };
            let selected = match key {
                "conversation" => self.sidebar.view == sidebar::SidebarView::Sessions,
                "folder" => self.sidebar.view == sidebar::SidebarView::Files,
                "agent" => self.sidebar.view == sidebar::SidebarView::Tasks,
                _ => false,
            };
            let response = nav_row(ui, &format!("          {label}"), selected);
            icon(
                ui,
                egui::Rect::from_center_size(
                    response.rect.left_center() + egui::vec2(16.5, 0.0),
                    egui::vec2(15.0, 15.0),
                ),
                Icon::from_key(key),
                if selected {
                    rgb(color::CHROME_TEXT)
                } else {
                    rgb(color::CHROME_MUTED)
                },
            );
            if response.clicked() {
                match key {
                    "settings" => self.config_panel.open(&self.config),
                    "folder" => {
                        self.reference_request_sidebar_view(sidebar::SidebarView::Files, ui.ctx());
                    }
                    "agent" => {
                        self.reference_request_sidebar_view(sidebar::SidebarView::Tasks, ui.ctx());
                    }
                    _ => {
                        self.reference_request_sidebar_view(
                            sidebar::SidebarView::Sessions,
                            ui.ctx(),
                        );
                    }
                }
            }
        }
        ui.add_space(18.0);
        let heading = ui.label(
            RichText::new("TABS")
                .size(13.0 * scale)
                .color(rgb(color::CHROME_MUTED)),
        );
        ui.add_space((heading.rect.bottom() + shell::METADATA_GAP - ui.cursor().top()).max(0.0));
        false // Navigation ownership changes are applied before next frame's panels.
    }

    pub(crate) fn render_reference_fixture_recent(&mut self, ui: &mut egui::Ui) {
        let scale = self.config.effective_ui_font_size() / config::DEFAULT_FONT_SIZE;
        let max_height = (ui.ctx().viewport_rect().bottom()
            - shell::NAVIGATION_STATUS_HEIGHT
            - 136.0
            - ui.cursor().top()
            - 8.0)
            .max(shell::RECENT_ROW_HEIGHT);
        egui::ScrollArea::vertical()
            .id_salt("reference-recent-fixture")
            .max_height(max_height)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for entry in conversation::fixture_recent_entries() {
                    let row_width = ui.available_width();
                    let time = ui.painter().layout_no_wrap(
                        entry.time.clone(),
                        FontId::proportional(12.0 * scale),
                        rgb(color::CHROME_MUTED),
                    );
                    let title_width =
                        (row_width - 28.0 - 7.0 - time.size().x - shell::METADATA_GAP).max(1.0);
                    let mut title_job = egui::text::LayoutJob::simple(
                        entry.title.clone(),
                        FontId::proportional(16.0 * scale),
                        rgb(color::CHROME_TEXT),
                        title_width,
                    );
                    title_job.wrap.max_rows = 1;
                    title_job.wrap.break_anywhere = true;
                    let title = ui.painter().layout_job(title_job);
                    let mut detail_job = egui::text::LayoutJob::simple(
                        entry.detail.clone(),
                        FontId::proportional(12.0 * scale),
                        rgb(color::CHROME_MUTED),
                        (row_width - 35.0).max(1.0),
                    );
                    detail_job.wrap.max_rows = 1;
                    detail_job.wrap.break_anywhere = true;
                    let detail = ui.painter().layout_job(detail_job);
                    let gap = 2.0 * scale;
                    let group_height = title.size().y + gap + detail.size().y;
                    let height = if scale <= 1.0 {
                        shell::RECENT_ROW_HEIGHT
                    } else {
                        shell::RECENT_ROW_HEIGHT.max(group_height + 6.0)
                    };
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(row_width, height), egui::Sense::hover());
                    if entry.selected {
                        ui.painter()
                            .rect_filled(rect, 6, rgb(color::NAVIGATION_SELECTED));
                    }
                    icon(
                        ui,
                        egui::Rect::from_center_size(
                            egui::pos2(rect.left() + 13.0, rect.center().y),
                            egui::vec2(14.0, 14.0),
                        ),
                        Icon::from_key(entry.icon_key),
                        rgb(color::CHROME_MUTED),
                    );
                    let left = rect.left() + 28.0;
                    let title_top = rect.center().y - group_height * 0.5;
                    let detail_top = title_top + title.size().y + gap;
                    let time_top = title_top + (title.size().y - time.size().y) * 0.5;
                    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
                    painter.galley(egui::pos2(left, title_top), title, rgb(color::CHROME_TEXT));
                    painter.galley(
                        egui::pos2(left, detail_top),
                        detail,
                        rgb(color::CHROME_MUTED),
                    );
                    painter.galley(
                        egui::pos2(rect.right() - 7.0 - time.size().x, time_top),
                        time,
                        rgb(color::CHROME_MUTED),
                    );
                    response.on_hover_text(format!("{}\n{}", entry.title, entry.detail));
                }
            });
    }

    pub(crate) fn render_reference_spaces(&mut self, ui: &mut egui::Ui) {
        let scale = self.config.effective_ui_font_size() / config::DEFAULT_FONT_SIZE;
        ui.add_space(20.0);
        let heading = ui.label(
            RichText::new("SPACES")
                .size(13.0 * scale)
                .color(rgb(color::CHROME_MUTED)),
        );
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.add_space((heading.rect.bottom() + shell::METADATA_GAP - ui.cursor().top()).max(0.0));
        for ((name, detail), tint) in conversation::SPACES.into_iter().zip([
            [129, 150, 246],
            [231, 155, 86],
            [237, 132, 114],
            [120, 188, 235],
        ]) {
            let (row, response) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), shell::SPACES_ROW_HEIGHT),
                egui::Sense::hover(),
            );
            if name.eq_ignore_ascii_case("Ember") {
                ui.painter()
                    .rect_filled(row, 8, shell::rgba(shell::token::BUTTON));
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        row.min + egui::vec2(-5.0, 4.0),
                        egui::vec2(4.0, row.height() - 8.0),
                    ),
                    2,
                    Color32::from_rgb(255, 157, 66),
                );
            }
            let badge = egui::Rect::from_center_size(
                egui::pos2(row.left() + 16.0, row.center().y),
                egui::vec2(32.0, 32.0),
            );
            ui.painter().rect_filled(
                badge,
                6,
                Color32::from_rgba_unmultiplied(tint[0], tint[1], tint[2], 32),
            );
            product_identity(ui, badge, name);
            let name_galley = ui.painter().layout_no_wrap(
                name.into(),
                FontId::proportional(16.0 * scale),
                rgb(color::CHROME_TEXT),
            );
            let detail_left = row.left() + 46.0 + name_galley.size().x + shell::METADATA_GAP;
            let mut detail_job = egui::text::LayoutJob::simple(
                detail.into(),
                FontId::proportional(12.0 * scale),
                rgb(color::CHROME_MUTED),
                (row.right() - detail_left).max(1.0),
            );
            detail_job.wrap.max_rows = 1;
            detail_job.wrap.break_anywhere = true;
            let detail_galley = ui.painter().layout_job(detail_job);
            let painter = ui.painter().with_clip_rect(row.intersect(ui.clip_rect()));
            painter.galley(
                egui::pos2(
                    row.left() + 46.0,
                    row.center().y - name_galley.size().y * 0.5,
                ),
                name_galley,
                rgb(color::CHROME_TEXT),
            );
            painter.galley(
                egui::pos2(detail_left, row.center().y - detail_galley.size().y * 0.5),
                detail_galley,
                rgb(color::CHROME_MUTED),
            );
            response.on_hover_text(format!(
                "{name} · {detail}\nOpening sibling applications is unavailable"
            ));
        }
    }

    pub(crate) fn render_reference_status(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let scale = self.config.effective_ui_font_size() / config::DEFAULT_FONT_SIZE;
        ui.painter().hline(
            rect.x_range(),
            rect.top(),
            Stroke::new(1.0, shell::rgba(shell::token::BORDER)),
        );
        ui.painter().circle_filled(
            rect.left_center() + egui::vec2(8.0, 0.0),
            4.0,
            shell::rgba(shell::token::MUTED_TEXT),
        );
        ui.painter().text(
            rect.left_center() + egui::vec2(26.0, 0.0),
            egui::Align2::LEFT_CENTER,
            "Local session",
            FontId::proportional(13.0 * scale),
            shell::rgba(shell::token::MUTED_TEXT),
        );
        let settings = egui::Rect::from_center_size(
            rect.right_center() - egui::vec2(12.0, 0.0),
            egui::vec2(24.0, 28.0),
        );
        let response = ui.put(
            settings,
            egui::Button::new("")
                .fill(Color32::TRANSPARENT)
                .stroke(Stroke::NONE),
        );
        icon(
            ui,
            settings.shrink(3.0),
            Icon::Settings,
            shell::rgba(shell::token::TEXT),
        );
        if response.on_hover_text("Settings").clicked() {
            self.config_panel.open(&self.config);
        }
    }

    pub(crate) fn render_reference_files_navigation(&mut self, root: &mut egui::Ui) {
        if self.sidebar.view != sidebar::SidebarView::Files
            || !self.sidebar.visible
            || self.reference_layout(root.ctx()).navigation.is_none()
        {
            return;
        }
        egui::Panel::left("reference-files-navigation")
            .exact_size(
                self.reference_layout(root.ctx())
                    .navigation
                    .map_or(0.0, |r| r.width),
            )
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin {
                left: 17,
                right: 16,
                top: 17,
                bottom: if root.ctx().viewport_rect().height() < shell::token::SHORT_HEIGHT {
                    12
                } else {
                    30
                },
            }))
            .show(root, |ui| {
                let bounds = ui.available_rect_before_wrap();
                let status = egui::Rect::from_min_max(
                    egui::pos2(bounds.left(), (bounds.bottom() - 60.0).max(bounds.top())),
                    bounds.max,
                );
                let body =
                    egui::Rect::from_min_max(bounds.min, egui::pos2(bounds.right(), status.top()));
                ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("glass-files-navigation-body")
                        .min_scrolled_height(0.0)
                        .max_height(body.height())
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            self.render_reference_navigation(ui);
                            let tabs: Vec<_> = (0..self.tabs.len())
                                .map(|i| (i, self.tab_title(i)))
                                .collect();
                            for (index, title) in tabs {
                                if nav_row(ui, &title, self.tabs.active_index() == index).clicked()
                                {
                                    self.activate_tab(index);
                                }
                            }
                            self.render_reference_spaces(ui);
                        });
                });
                self.render_reference_status(ui, status);
            });
    }

    pub(crate) fn render_reference_inspector(&mut self, root: &mut egui::Ui) {
        if !self.reference_inspector_visible(root.ctx())
            || self.sidebar.view == sidebar::SidebarView::Files
        {
            return;
        }
        let short = root.ctx().viewport_rect().height() < shell::token::SHORT_HEIGHT;
        let frame = egui::Frame::NONE.inner_margin(egui::Margin {
            left: 20,
            right: 18,
            top: 15,
            bottom: if short { 12 } else { 46 },
        });
        if let Some(rect) = self.reference_inspector_drawer_rect(root.ctx()) {
            let response =
                show_inspector_drawer(root, rect, "reference-inspector-drawer", frame, |ui| {
                    self.render_reference_inspector_contents(ui);
                });
            self.font_zoom.register(
                response.response.rect,
                response.response.layer_id,
                crate::font_zoom::FontGroup::Ui,
            );
            self.font_zoom
                .identify_inspector_drawer(response.response.layer_id);
        } else {
            egui::Panel::right("reference-inspector")
                .exact_size(
                    shell::INSPECTOR_WIDTH
                        + shell::token::INSPECTOR_GAP
                        + shell::token::OUTER_RIGHT,
                )
                .resizable(false)
                .show_separator_line(false)
                .frame(frame)
                .show(root, |ui| self.render_reference_inspector_contents(ui));
        }
    }

    fn render_reference_inspector_contents(&mut self, ui: &mut egui::Ui) {
        // Explicit global Ui identity preserves the same tab widgets and
        // per-tab scroll state when a dock becomes a compact drawer.
        ui.scope_builder(
            egui::UiBuilder::new().id(egui::Id::new("reference-inspector-body")),
            |ui| {
                chrome_style(
                    ui,
                    self.config.effective_ui_font_size() / config::DEFAULT_FONT_SIZE,
                );
                let tab = self.reference_selected_inspector_tab();
                ui.horizontal(|ui| {
                    ui.visuals_mut().selection.bg_fill = shell::rgba(shell::token::ACCENT_TOP);
                    ui.visuals_mut().selection.stroke =
                        Stroke::new(1.0, shell::rgba(shell::token::BORDER));
                    for (candidate, label) in [
                        (ReferenceInspectorTab::Agent, "Agent"),
                        (ReferenceInspectorTab::Context, "Context"),
                        (ReferenceInspectorTab::Files, "Files"),
                    ] {
                        let response = ui.add_sized(
                            [shell::INSPECTOR_TAB_WIDTH, 42.0],
                            egui::Button::selectable(tab == candidate, label)
                                .corner_radius(shell::CONTROL_RADIUS),
                        );
                        remember_inspector_control(ui, &response);
                        if response.clicked() {
                            self.reference_request_inspector_tab(candidate, ui.ctx());
                        }
                    }
                });
                ui.add_space(16.0);
                egui::ScrollArea::vertical()
                    .id_salt(("reference-inspector-scroll", tab))
                    .show(ui, |ui| {
                        if tab == ReferenceInspectorTab::Context {
                            self.render_reference_context(ui);
                        } else {
                            self.render_reference_agent(ui);
                        }
                    });
            },
        );
    }

    fn render_reference_context(&mut self, ui: &mut egui::Ui) {
        let scale = self.config.effective_ui_font_size() / config::DEFAULT_FONT_SIZE;
        ui.heading("Session context");
        ui.add_space(12.0);
        let session = self.session_manager.get_active_session_mut();
        let id = session.metadata.session_id.clone();
        let pid = session.get_shell_pid();
        let terminal = session.terminal.lock();
        let cwd = terminal
            .current_working_dir
            .clone()
            .unwrap_or_else(|| "Not reported by shell".into());
        let mode = if self.config.chat_mode_enabled() {
            "Chat"
        } else {
            "Block"
        };
        for (label, value) in [
            ("PRESENTATION", mode.to_owned()),
            ("DIRECTORY", cwd),
            ("SESSION", id),
            ("SHELL PID", pid.to_string()),
            (
                "GRID",
                format!("{} × {}", terminal.grid.row_len(), terminal.grid.rows()),
            ),
        ] {
            ui.label(
                RichText::new(label)
                    .size(13.0 * scale)
                    .color(rgb(color::CHROME_MUTED)),
            );
            ui.add(egui::Label::new(RichText::new(value.clone()).size(10.0 * scale)).wrap())
                .on_hover_text(value);
            ui.add_space(12.0);
        }
    }

    fn render_reference_agent(&mut self, ui: &mut egui::Ui) {
        let fixture = conversation::fixture_enabled();
        let mut model = if fixture {
            conversation::acceptance_fixture().inspector
        } else {
            conversation::Inspector::empty()
        };
        if !fixture {
            let selected = self
                .task_sidebar
                .selected
                .and_then(|id| self.task_manager.get(id))
                .or_else(|| self.task_manager.tasks().last());
            let (_, attention) =
                crate::app::tasks::tasks_tab_label(self.task_manager.attention_counts());
            if let Some(task) = selected {
                model.title = task.title.clone();
                model.subtitle = format!(
                    "{} · {}",
                    task.provider.display_name(),
                    attention.unwrap_or_else(|| task.status.label().to_owned())
                );
                model.badge = task.status.label().to_owned();
                model.metadata = vec![
                    ("Model".into(), "Not reported".into()),
                    (
                        "Workspace".into(),
                        if self.reference_private_title() {
                            "Private workspace".into()
                        } else {
                            task.repo_root
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "Not reported".into())
                        },
                    ),
                    ("Branch".into(), task.branch.clone()),
                    ("Environment".into(), "Local worktree".into()),
                    (
                        "Started".into(),
                        crate::block_mode::format_local_time_of_day(
                            task.created_at_ms / 1000,
                            crate::block_mode::local_utc_offset_secs(task.created_at_ms / 1000),
                        ),
                    ),
                ];
                model.current_step = task
                    .status_detail
                    .as_ref()
                    .map(|detail| conversation::Step {
                        title: task.title.clone(),
                        detail: detail.clone(),
                        elapsed: None,
                        state: if task.status.is_running() {
                            conversation::StepState::Running
                        } else {
                            conversation::StepState::Queued
                        },
                    });
                if let Some(session) = task
                    .terminal_session_id
                    .as_deref()
                    .and_then(|id| self.session_manager.index_of(id))
                    .and_then(|index| self.session_manager.sessions().get(index))
                {
                    let terminal = session.terminal.lock();
                    model.output = terminal
                        .command_records()
                        .back()
                        .and_then(|record| terminal.command_output_text(&record.id, 8192))
                        .map(|output| output.text)
                        .unwrap_or_else(|| "No retained command output".into());
                }
            }
            model.queue = self
                .task_manager
                .tasks()
                .iter()
                .filter(|task| task.status != crate::agent::TaskStatus::Archived)
                .map(|task| conversation::Step {
                    title: task.title.clone(),
                    detail: task.status.label().into(),
                    elapsed: None,
                    state: if task.status == crate::agent::TaskStatus::Completed {
                        conversation::StepState::Complete
                    } else if task.status.is_running() {
                        conversation::StepState::Running
                    } else {
                        conversation::StepState::Queued
                    },
                })
                .collect();
        }
        crate::reference_conversation_ui::draw_inspector(
            ui,
            &model,
            fixture,
            self.config.effective_ui_font_size() / config::DEFAULT_FONT_SIZE,
        );
    }

    pub(crate) fn render_reference_canvas_header(&mut self, ui: &mut egui::Ui) {
        let scale = self.config.effective_ui_font_size() / config::DEFAULT_FONT_SIZE;
        let title = if conversation::fixture_enabled() {
            conversation::acceptance_fixture().title
        } else {
            format!("Workspace / {}", self.tab_title(self.tabs.active_index()))
        };
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(
                ui.available_width(),
                self.reference_layout(ui.ctx()).header.height,
            ),
            egui::Sense::hover(),
        );

        ui.painter().line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            Stroke::new(1.0, rgb(color::LIGHT_BORDER)),
        );
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(
                24.0,
                if rect.height() < 88.0 { 12.0 } else { 19.0 },
            ))),
            |ui| {
                *ui.visuals_mut() = egui::Visuals::dark();
                ui.visuals_mut().override_text_color = Some(rgb(color::CANVAS_TEXT));
                ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                ui.visuals_mut().widgets.inactive.bg_fill = Color32::TRANSPARENT;
                ui.visuals_mut().widgets.inactive.bg_stroke =
                    Stroke::new(1.0, rgb(color::LIGHT_BORDER));
                let icon_space = if rect.width() > 500.0 { 68.0 } else { 0.0 };
                if icon_space > 0.0 {
                    let tile = egui::Rect::from_min_size(
                        rect.min + egui::vec2(18.0, 19.0),
                        egui::vec2(54.0, 54.0),
                    );
                    ui.painter()
                        .rect_filled(tile, 12, shell::rgba(shell::token::BUTTON));
                    product_identity(ui, tile.shrink(8.0), "ember");
                }
                let title_width = (ui.available_width() - 160.0 - icon_space).max(1.0);
                let origin = ui.max_rect().min + egui::vec2(icon_space, 0.0);
                let subtitle = if conversation::fixture_enabled() {
                    conversation::FIXTURE_LABEL
                } else if self.config.chat_mode_enabled() {
                    "Chat · live shell"
                } else {
                    "Block · live terminal"
                };
                let title_height = ui
                    .painter()
                    .layout_no_wrap(
                        "Ag".into(),
                        FontId::proportional(shell::type_size::PAGE_TITLE * scale),
                        rgb(color::CANVAS_TEXT),
                    )
                    .size()
                    .y;
                let subtitle_height = ui
                    .painter()
                    .layout_no_wrap(
                        subtitle.into(),
                        FontId::proportional(14.0 * scale),
                        rgb(color::CANVAS_MUTED),
                    )
                    .size()
                    .y;
                let text_gap = 5.0 * scale;
                let group_height = title_height + text_gap + subtitle_height;
                let text_origin = egui::pos2(
                    origin.x,
                    rect.top() + ((rect.height() - group_height) * 0.5).max(4.0),
                );
                let display_title = if title.trim().is_empty() {
                    "Terminal session"
                } else {
                    title.as_str()
                };
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(egui::Rect::from_min_size(
                            text_origin,
                            egui::vec2(title_width, group_height),
                        ))
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                    |ui| {
                        ui.spacing_mut().item_spacing.y = text_gap;
                        ui.add(
                            egui::Label::new(
                                RichText::new(display_title)
                                    .size(shell::type_size::PAGE_TITLE * scale)
                                    .strong(),
                            )
                            .truncate(),
                        )
                        .on_hover_text(display_title);
                        ui.add(
                            egui::Label::new(
                                RichText::new(subtitle)
                                    .size(14.0 * scale)
                                    .color(rgb(color::CANVAS_MUTED)),
                            )
                            .truncate(),
                        )
                        .on_hover_text(subtitle);
                    },
                );
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(egui::Rect::from_min_max(
                        egui::pos2(
                            ui.max_rect().right()
                                - 3.0 * shell::HEADER_ACTION_SIZE
                                - 2.0 * shell::HEADER_ACTION_GAP,
                            origin.y,
                        ),
                        ui.max_rect().max,
                    )),
                    |ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = shell::HEADER_ACTION_GAP;
                            let group = ui.max_rect();
                            let more = header_action(ui, Icon::More, "Session tools");
                            let visible = egui::Popup::menu(&more)
                                .show(|ui| {
                                    if ui.button("Command launcher").clicked() {
                                        self.command_palette.open();
                                        ui.close();
                                    }
                                    if ui
                                        .button(if self.block_bar_user_hidden {
                                            "Show block controls"
                                        } else {
                                            "Hide block controls"
                                        })
                                        .clicked()
                                    {
                                        self.block_bar_user_hidden = !self.block_bar_user_hidden;
                                        ui.close();
                                    }
                                    for (key, label) in [
                                        ("reference:tabs", "Session tab strip"),
                                        ("reference:status", "Session status bar"),
                                        ("reference:shell-updates", "Shell updates"),
                                    ] {
                                        let id = egui::Id::new(key);
                                        let mut value = ui.ctx().data_mut(|data| {
                                            data.get_temp::<bool>(id).unwrap_or(false)
                                        });
                                        if ui.checkbox(&mut value, label).changed() {
                                            ui.ctx().data_mut(|data| data.insert_temp(id, value));
                                        }
                                    }
                                    ui.add_enabled(false, egui::Button::new("Share · unavailable"));
                                })
                                .is_some();
                            if header_action(ui, Icon::Search, "Search terminal").clicked() {
                                self.search_state.open();
                            }
                            if header_action(ui, Icon::Plus, "New terminal session").clicked() {
                                self.new_tab();
                            }
                            crate::ui::remember_reference_chrome_scope(
                                ui.ctx(),
                                ui.id().with("reference-header-keyboard-scope"),
                                group,
                                visible,
                            );
                        });
                    },
                );
            },
        );
        // The header was already allocated at its shared fixed height. The
        // overlay's smaller controls must not pull the parent cursor back to
        // their measured minimum and change terminal rows/composer budgeting.
        ui.advance_cursor_after_rect(rect);
    }
}
