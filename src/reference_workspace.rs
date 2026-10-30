//! Native adapter for the shared reconstruction contract.
//!
//! This module owns chrome only. The live PTY, projection, IME and reading
//! anchors continue to belong to the existing terminal renderer and session.
use crate::glass_shell::{self as shell, color, BoundedPanel as _};
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
                    shell::bounded_ui(ui, contents)
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

fn inspector_button(ui: &mut egui::Ui, enabled: bool, label: &str) -> egui::Response {
    let response = ui.add_enabled(enabled, egui::Button::new(label));
    remember_inspector_control(ui, &response);
    response
}

/// Inspector copies use exactly the same bounded, redacted text as the view.
/// A private tab must not reveal its title-derived directory through a tooltip
/// or clipboard action on a different chrome surface.
fn inspector_context_text(text: &str, private: bool, redact: bool, max_bytes: usize) -> String {
    if private {
        return "Hidden for private tab".into();
    }
    let text = if redact {
        jterm_core::redact::redact_secrets(text)
    } else {
        text.to_owned()
    };
    jterm_core::review_input::safe_multiline_display(&text, max_bytes)
}

fn command_outcome(record: &crate::terminal::CommandRecord) -> String {
    if record.state == crate::terminal::CommandState::Running {
        return "Running".into();
    }
    if !record.complete {
        return "At prompt · not completed".into();
    }
    let mut outcome = record
        .exit_code
        .map(|code| format!("Exit {code}"))
        .unwrap_or_else(|| "Finished · exit not reported".into());
    if let Some(duration) = record.duration_ms {
        outcome.push_str(&format!(" · {duration} ms"));
    }
    outcome
}

fn local_shell_owns_foreground(session: &crate::session::Session) -> bool {
    // A direct SSH command session can own its PTY under its own PID, but its
    // OSC directory describes the remote host. It has no local shell route.
    session.purpose == crate::session::SessionPurpose::Interactive
        && session.shell.shell_program().is_some()
        && session.shell_owns_foreground_pty()
}

/// A terminal report can outlive SSH and therefore name a remote namespace
/// even after the local shell regains its PTY. Only the shell process supplies
/// local authority, and the visible target must agree with that evidence.
fn verified_local_directory(
    reported: Option<&str>,
    process_cwd: Option<&str>,
) -> Option<std::path::PathBuf> {
    let local = crate::bottom_bar::local_files_path(process_cwd)?;
    if reported.is_some_and(|reported| std::path::Path::new(reported) != local.as_path()) {
        return None;
    }
    Some(local)
}

fn verified_session_directory(session: &crate::session::Session) -> Option<std::path::PathBuf> {
    if !local_shell_owns_foreground(session) {
        return None;
    }
    let process = jterm_core::process::process_cwd(session.get_shell_pid());
    let reported = session.terminal.lock().current_working_dir.clone();
    verified_local_directory(reported.as_deref(), process.as_deref())
}

fn workspace_identity(
    private: bool,
    reported_cwd: Option<&str>,
    process_cwd: Option<&str>,
    local_foreground: bool,
    session_label: &str,
    redact: bool,
) -> (String, String) {
    if private {
        return ("Private workspace".into(), "Private workspace".into());
    }
    let directory = if local_foreground {
        process_cwd
            .map(|cwd| (cwd, "Local shell directory"))
            .or_else(|| reported_cwd.map(|cwd| (cwd, "Shell-reported directory")))
    } else {
        // A remote/direct command's local client cwd is not its workspace.
        reported_cwd.map(|cwd| (cwd, "Shell-reported directory"))
    };
    if let Some((directory, source)) =
        directory.filter(|(cwd, _)| std::path::Path::new(cwd).is_absolute())
    {
        let name = std::path::Path::new(directory)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(directory);
        (
            jterm_core::review_input::safe_inline_display(
                &inspector_context_text(name, false, redact, 256),
                256,
            ),
            format!(
                "{source}: {}",
                inspector_context_text(directory, false, redact, 4096)
            ),
        )
    } else {
        let label = jterm_core::review_input::safe_inline_display(
            &inspector_context_text(session_label, false, redact, 256),
            256,
        );
        (
            label.clone(),
            format!("Active session: {label}\nDirectory not reported"),
        )
    }
}

/// Paint the optional identity only when its measured glyph bounds fit before
/// the actual right-hand controls. UI text and mode labels scale independently
/// of the fixed icon hit areas, so width heuristics are insufficient here.
fn paint_topbar_title(
    ui: &egui::Ui,
    titlebar_height: f32,
    scale: f32,
    right: f32,
) -> Option<egui::Rect> {
    let color = shell::rgba(shell::token::TEXT);
    let galley =
        ui.painter()
            .layout_no_wrap("Ember".into(), FontId::proportional(10.0 * scale), color);
    let rect = egui::Rect::from_min_size(
        egui::pos2(132.0, (titlebar_height - galley.size().y) * 0.5),
        galley.size(),
    );
    if rect.right() > right {
        return None;
    }
    ui.painter().galley(rect.min, galley, color);
    Some(rect)
}

fn header_action(ui: &mut egui::Ui, kind: Icon, label: &str) -> egui::Response {
    let response = ui
        .push_id(("reference-header-action", label), |ui| {
            let size = shell::HEADER_ACTION_SIZE;
            ui.spacing_mut().interact_size = egui::vec2(shell::HEADER_ACTION_SIZE, size);
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
                [shell::HEADER_ACTION_SIZE, size],
                egui::Button::new("").corner_radius(shell::CONTROL_RADIUS),
            )
        })
        .inner;
    icon(
        ui,
        egui::Rect::from_center_size(response.rect.center(), egui::Vec2::splat(14.0)),
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
    style.visuals = theme::workspace_visuals();
    style.visuals.override_text_color = Some(rgb(color::CHROME_TEXT));
    style.visuals.panel_fill = Color32::TRANSPARENT;
    style.visuals.disabled_alpha = 1.0;
    style.visuals.window_fill = rgb(color::INSPECTOR_CARD);
    style.visuals.selection.bg_fill = rgb(color::NAVIGATION_SELECTED);
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, rgb(color::DARK_BORDER));
    style.visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    style.visuals.widgets.hovered.weak_bg_fill = rgb(color::NAVIGATION_SELECTED);
    style.spacing.item_spacing = egui::vec2(shell::METADATA_GAP, 4.0);
    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    style.spacing.interact_size.y = 28.0;
    for font in style.text_styles.values_mut() {
        *font = FontId::proportional(10.0 * scale);
    }
}

fn nav_row(ui: &mut egui::Ui, label: &str, selected: bool, kind: Option<Icon>) -> egui::Response {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let line_height = ui
        .painter()
        .layout_no_wrap("Ag".into(), font.clone(), rgb(color::CHROME_TEXT))
        .size()
        .y;
    let height = shell::NAVIGATION_ROW_HEIGHT.max(line_height + 8.0);
    let response = ui
        .scope(|ui| {
            ui.spacing_mut().interact_size.y = height;
            ui.spacing_mut().button_padding = egui::Vec2::ZERO;
            ui.add_sized(
                [ui.available_width(), height],
                egui::Button::new(RichText::new("").size(12.0))
                    .selected(selected)
                    .corner_radius(7)
                    .frame(selected),
            )
        })
        .inner;
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, label)
    });
    let rect = response.rect;
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(
            rect.left() + if kind.is_some() { 34.0 } else { 10.0 },
            rect.top() + 4.0,
        ),
        egui::pos2(rect.right() - 10.0, rect.bottom() - 4.0),
    );
    let tint = rgb(if selected {
        color::CHROME_TEXT
    } else {
        color::CHROME_MUTED
    });
    let mut job =
        egui::text::LayoutJob::simple(label.to_owned(), font, tint, text_rect.width().max(0.0));
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    ui.painter()
        .with_clip_rect(text_rect.intersect(ui.clip_rect()))
        .galley(
            egui::pos2(text_rect.left(), rect.center().y - galley.size().y * 0.5),
            galley,
            tint,
        );
    if let Some(kind) = kind {
        icon(
            ui,
            egui::Rect::from_center_size(
                rect.left_center() + egui::vec2(16.5, 0.0),
                egui::vec2(13.0, 13.0),
            ),
            kind,
            tint,
        );
    }
    if selected {
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

    fn reference_workspace_identity(&self) -> (String, String) {
        let session = &self.session_manager.sessions()[self.session_manager.active_index()];
        let reported = session.terminal.lock().current_working_dir.clone();
        let local_foreground = local_shell_owns_foreground(session);
        let process = local_foreground
            .then(|| jterm_core::process::process_cwd(session.get_shell_pid()))
            .flatten();
        let label = session
            .metadata
            .custom_name
            .as_deref()
            .filter(|label| !label.is_empty())
            .unwrap_or(&session.metadata.name);
        workspace_identity(
            self.reference_private_title(),
            reported.as_deref(),
            process.as_deref(),
            local_foreground,
            label,
            self.config.ai_redact_secrets,
        )
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
        let mut layout =
            shell::Layout::with_sidebar_widths(size.x, size.y, self.reference_sidebar_widths());
        if !self.sidebar.visible {
            layout = layout.without_navigation();
        }
        if !self.reference_inspector.open {
            layout = layout.without_inspector();
        }
        layout.with_chrome_font_size(self.config.effective_ui_font_size())
    }

    fn reference_sidebar_widths(&self) -> shell::SidebarWidths {
        self.reference_inspector
            .sidebar_resize
            .preview_widths()
            .unwrap_or(shell::SidebarWidths {
                navigation: self.config.sidebar_width,
                inspector: self.config.inspector_width,
            })
    }

    /// Run before terminal pointer routing. Resize handles never request text
    /// focus, and own the complete press/drag/release even outside their gap.
    pub(crate) fn handle_reference_sidebar_resize(
        &mut self,
        ctx: &egui::Context,
        enabled: bool,
    ) -> bool {
        let layout = self.reference_layout(ctx);
        let drawer = self.reference_inspector_drawer_rect(ctx);
        let mut widths = self.reference_sidebar_widths();
        let outcome = self.reference_inspector.sidebar_resize.interact(
            ctx,
            layout,
            drawer,
            &mut widths,
            enabled,
        );
        if outcome.persist {
            // A hot reload may update the opposite preference during a long
            // pointer gesture. Commit only the field this grip actually edits.
            match outcome.edited_edge {
                Some(shell::SidebarEdge::Navigation) => {
                    self.config.sidebar_width = widths.navigation
                }
                Some(shell::SidebarEdge::Inspector) => {
                    self.config.inspector_width = widths.inspector
                }
                None => return outcome.pointer_claimed,
            }
            self.schedule_config_save();
        }
        outcome.pointer_claimed
    }

    pub(crate) fn reference_inspector_drawer_rect(
        &self,
        ctx: &egui::Context,
    ) -> Option<egui::Rect> {
        if !self.reference_inspector.open || !self.reference_inspector.narrow_drawer {
            return None;
        }
        let screen = ctx.viewport_rect();
        let layout = self.reference_layout(ctx);
        if layout.inspector.is_some() {
            return None;
        }
        let width = self.reference_sidebar_widths().inspector().min(
            (screen.width()
                - layout.rail.width
                - shell::token::OUTER_LEFT
                - shell::token::OUTER_RIGHT)
                .max(0.0),
        );
        let rect = egui::Rect::from_min_max(
            egui::pos2(
                screen.right() - shell::token::OUTER_RIGHT - width,
                screen.top() + layout.center.y,
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
                        .corner_radius(shell::CONTROL_RADIUS),
                )
            })
            .inner
            .on_hover_text(label);
        icon(
            ui,
            egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(14.0)),
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
                ReferenceInspectorRequest::RevealSessionDirectory {
                    session_id,
                    directory,
                } => {
                    let index = self.session_manager.index_of(&session_id);
                    let verified = index.and_then(|index| {
                        (index == self.session_manager.active_index()
                            && self
                                .tabs
                                .tab_of_session(index)
                                .is_some_and(|tab| !self.tabs.flags(tab).private_title))
                        .then(|| {
                            self.session_manager
                                .sessions()
                                .get(index)
                                .and_then(verified_session_directory)
                        })
                        .flatten()
                    });
                    if verified.as_ref() == Some(&directory) {
                        let previous = self.sidebar.view;
                        // Browse the already verified process path, rather
                        // than rereading an asynchronously changing OSC cache.
                        self.reveal_files_at_local_directory(directory);
                        if self.sidebar.view == sidebar::SidebarView::Files {
                            self.reference_inspector.selected_tab = ReferenceInspectorTab::Files;
                            self.reference_open_inspector(root.ctx());
                        }
                        self.reference_save_sidebar_view(previous);
                    } else {
                        self.set_status("The visible directory no longer matches the focused local shell. Use Files to choose a local or remote location.");
                    }
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
            .exact_size(layout.titlebar.height)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::NONE
                    .fill(shell::rgba(shell::token::TOP))
                    .inner_margin(0.0),
            )
            .show_bounded(root, |ui| {
                chrome_style(ui, scale);
                let bounds = ui.max_rect();
                // egui may route a button click and the background drag to
                // different IDs. Wait for real movement before asking the WM
                // to drag, so a control press cannot steal its own release.
                let drag = ui.interact(
                    bounds,
                    ui.id().with("window-drag"),
                    egui::Sense::click_and_drag(),
                );
                if drag.drag_started() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                for (i, glyph, tip) in [
                    (0, "×", "Close window"),
                    (1, "−", "Minimize window"),
                    (2, "□", "Maximize or restore window"),
                ] {
                    let r = egui::Rect::from_min_size(
                        egui::pos2(8.0 + i as f32 * 29.0, (layout.titlebar.height - 28.0) * 0.5),
                        egui::vec2(28.0, 28.0),
                    );
                    if ui
                        .put(
                            r,
                            egui::Button::new(RichText::new(glyph).size(16.0))
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
                let mode_width = shell::MODE_CONTROL_MIN_WIDTH * scale.max(1.0);
                let settings_left = bounds.right() - 8.0 - 32.0 - 6.0 - 32.0;
                let pair_left = settings_left - 8.0 - 2.0 * mode_width - 6.0;
                let mode_left = if pair_left >= 104.0 {
                    pair_left
                } else {
                    settings_left - 8.0 - 32.0
                };
                let identity_right = mode_left - 12.0;
                let brand = egui::Rect::from_min_size(
                    egui::pos2(104.0, (layout.titlebar.height - 20.0) * 0.5),
                    egui::vec2(20.0, 20.0),
                );
                if bounds.width() > 470.0 && brand.right() <= identity_right {
                    product_identity(ui, brand, "ember");
                }
                if bounds.width() > 700.0 {
                    let _ = paint_topbar_title(ui, layout.titlebar.height, scale, identity_right);
                }
                let button_h = shell::chrome_control_height(self.config.effective_ui_font_size());
                let control_top = (layout.titlebar.height - button_h) * 0.5;
                let toggle_w = 32.0;
                let toggle = egui::Rect::from_min_size(
                    egui::pos2(bounds.right() - 8.0 - toggle_w, control_top),
                    egui::vec2(toggle_w, button_h),
                );
                self.render_reference_inspector_toggle(ui, toggle);
                let settings = toggle.translate(egui::vec2(-toggle_w - 6.0, 0.0));
                let response = ui.put(
                    settings,
                    egui::Button::new("")
                        .fill(shell::rgba(shell::token::BUTTON))
                        .stroke(Stroke::new(1.0, shell::rgba(shell::token::BORDER)))
                        .corner_radius(shell::CONTROL_RADIUS),
                );
                icon(
                    ui,
                    egui::Rect::from_center_size(settings.center(), egui::Vec2::splat(14.0)),
                    Icon::Settings,
                    shell::rgba(shell::token::TEXT),
                );
                if response.on_hover_text("Settings").clicked() {
                    self.config_panel.open(&self.config);
                }
                let mode_padding = ui.spacing().button_padding;
                if bounds.width() <= 1000.0 {
                    ui.spacing_mut().button_padding.x = 6.0;
                }
                let mut requested_mode = None;
                if settings.left() - 8.0 - 2.0 * mode_width - 6.0 >= 104.0 {
                    for (i, mode, label) in [
                        (0, config::TerminalMode::Chat, "Chat"),
                        (1, config::TerminalMode::Block, "Block"),
                    ] {
                        let r = egui::Rect::from_min_size(
                            egui::pos2(
                                settings.left() - 8.0 - mode_width - i as f32 * (mode_width + 6.0),
                                control_top,
                            ),
                            egui::vec2(mode_width, button_h),
                        );
                        let selected =
                            (mode == config::TerminalMode::Chat) == self.config.chat_mode_enabled();
                        let response = ui.put(
                            r,
                            egui::Button::new(
                                RichText::new(label)
                                    .size(10.0 * scale)
                                    .color(shell::rgba(shell::token::TEXT)),
                            )
                            .truncate()
                            .fill(shell::rgba(if selected {
                                shell::token::ACCENT_TOP
                            } else {
                                shell::token::BUTTON
                            }))
                            .stroke(Stroke::new(1.0, shell::rgba(shell::token::BORDER)))
                            .corner_radius(shell::CONTROL_RADIUS),
                        );
                        if response.clicked() && !selected {
                            requested_mode = Some(mode);
                        }
                    }
                } else {
                    let bounds = egui::Rect::from_min_size(
                        egui::pos2(settings.left() - 8.0 - 32.0, control_top),
                        egui::vec2(32.0, button_h),
                    );
                    let mode = ui
                        .put(
                            bounds,
                            egui::Button::new("")
                                .fill(shell::rgba(shell::token::ACCENT_TOP))
                                .corner_radius(shell::CONTROL_RADIUS),
                        )
                        .on_hover_text("Terminal presentation: Chat or Block");
                    mode.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            true,
                            "Terminal presentation",
                        )
                    });
                    icon(
                        ui,
                        egui::Rect::from_center_size(mode.rect.center(), egui::Vec2::splat(18.0)),
                        Icon::Terminal,
                        shell::rgba(shell::token::TEXT),
                    );
                    let popup = egui::Popup::menu(&mode)
                        .style(crate::theme::configure_popup_style)
                        .show(|ui| {
                            for (value, label) in [
                                (config::TerminalMode::Chat, "Chat"),
                                (config::TerminalMode::Block, "Block"),
                            ] {
                                let selected = (value == config::TerminalMode::Chat)
                                    == self.config.chat_mode_enabled();
                                if ui.selectable_label(selected, label).clicked() && !selected {
                                    requested_mode = Some(value);
                                    ui.close();
                                }
                            }
                        })
                        .is_some();
                    crate::ui::remember_reference_chrome_scope(
                        ui.ctx(),
                        mode.id.with("mode-overflow"),
                        mode.rect,
                        popup,
                    );
                }
                if let Some(mode) = requested_mode {
                    self.config.terminal_mode = Some(mode);
                    self.apply_runtime_config(ui.ctx());
                    self.force_resize_session = true;
                    self.return_focus_to_terminal(ui.ctx());
                    self.config_panel.sync_from_config(&self.config);
                    self.schedule_config_save();
                }
                ui.spacing_mut().button_padding = mode_padding;
                let search_right = settings.left() - 8.0 - 2.0 * mode_width - 6.0 - 12.0;
                if bounds.width() >= 1250.0 && search_right > 600.0 {
                    let width = (bounds.width() * 0.3).min(360.0).min(search_right - 260.0);
                    let r = egui::Rect::from_min_size(
                        egui::pos2(
                            ((bounds.width() - width) * 0.5).min(search_right - width),
                            control_top,
                        ),
                        egui::vec2(width, button_h),
                    );
                    let shortcut = self
                        .keybindings
                        .pretty_bindings_for("command_palette:toggle")
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "Unbound".into());
                    if ui
                        .put(
                            r,
                            egui::Button::new("")
                                .fill(shell::rgba(shell::token::BUTTON))
                                .stroke(Stroke::new(1.0, shell::rgba(shell::token::BORDER)))
                                .corner_radius(shell::CONTROL_RADIUS),
                        )
                        .on_hover_text(format!(
                            "Search commands, files and tools\nOpen command launcher: {shortcut}"
                        ))
                        .clicked()
                    {
                        self.command_palette.open();
                    }
                    icon(
                        ui,
                        egui::Rect::from_center_size(
                            r.left_center() + egui::vec2(16.0, 0.0),
                            egui::vec2(16.0, 16.0),
                        ),
                        Icon::Search,
                        shell::rgba(shell::token::TEXT),
                    );
                    let shortcut_galley = (scale <= 1.0).then(|| {
                        ui.painter().layout_no_wrap(
                            shortcut,
                            FontId::proportional(9.0 * scale),
                            shell::rgba(shell::token::TEXT),
                        )
                    });
                    let caption_right =
                        shortcut_galley.as_ref().map_or(r.right() - 8.0, |galley| {
                            r.right() - 12.0 - galley.size().x - 12.0
                        });
                    let caption = egui::Rect::from_min_max(
                        egui::pos2(r.left() + 32.0, r.top() + 4.0),
                        egui::pos2(caption_right.max(r.left() + 32.0), r.bottom() - 4.0),
                    );
                    ui.painter().with_clip_rect(caption).text(
                        r.left_center() + egui::vec2(32.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        if scale > 1.0 {
                            "Search"
                        } else {
                            "Search commands, files, tools…"
                        },
                        FontId::proportional(10.0 * scale),
                        shell::rgba(shell::token::MUTED_TEXT),
                    );
                    if let Some(galley) = shortcut_galley {
                        let position = egui::pos2(
                            r.right() - 12.0 - galley.size().x,
                            r.center().y - galley.size().y * 0.5,
                        );
                        ui.painter().with_clip_rect(r.shrink(4.0)).galley(
                            position,
                            galley,
                            shell::rgba(shell::token::TEXT),
                        );
                    }
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
        egui::Panel::left("reference-rail")
            .exact_size(layout.rail.width + shell::token::OUTER_LEFT)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin {
                left: 10,
                right: 6,
                top: 6,
                bottom: 6,
            }))
            .show_bounded(root, |ui| {
                chrome_style(ui, scale);
                for (key, hint) in conversation::RAIL_NAVIGATION {
                    let hint = if key == "workspace" {
                        if self.sidebar.visible {
                            "Hide left sidebar"
                        } else {
                            "Show left sidebar"
                        }
                    } else {
                        hint
                    };
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
                    let size = shell::SELECTED_RAIL_SIZE;
                    let response = ui.add_sized(
                        [size, size],
                        egui::Button::new("")
                            .fill(if key == "workspace" && self.sidebar.visible {
                                shell::rgba(shell::token::SELECTED_NAV)
                            } else {
                                Color32::TRANSPARENT
                            })
                            .stroke(Stroke::NONE)
                            .corner_radius(shell::CONTROL_RADIUS),
                    );
                    icon(
                        ui,
                        egui::Rect::from_center_size(
                            response.rect.center(),
                            egui::Vec2::splat(14.0),
                        ),
                        kind,
                        shell::rgba(shell::token::TEXT),
                    );
                    if response.on_hover_text(hint).clicked() {
                        if key == "workspace" {
                            self.dispatch_command(
                                ui.ctx(),
                                crate::keybindings::Command::SidebarToggle,
                            );
                        } else if key == "search" {
                            self.command_palette.open();
                        } else {
                            self.reference_request_sidebar_view(view, ui.ctx());
                        }
                    }
                    ui.add_space(4.0);
                }
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
                .size(9.0 * scale)
                .color(rgb(color::CHROME_MUTED)),
        );
        let (workspace, workspace_detail) = self.reference_workspace_identity();
        ui.add_space(8.0);
        let selector = ui.add_sized(
            [ui.available_width(), shell::WORKSPACE_SELECTOR_HEIGHT],
            egui::Button::new("")
                .fill(rgb(color::INSPECTOR_CARD))
                .corner_radius(8),
        );
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
            selector.rect.max - egui::vec2(30.0, 0.0),
        );
        ui.painter().with_clip_rect(name_rect).text(
            name_rect.left_center(),
            egui::Align2::LEFT_CENTER,
            workspace,
            egui::FontId::proportional(10.0 * scale),
            rgb(color::CHROME_TEXT),
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
        let selector = selector.on_hover_text(format!(
            "Switch open workspace or browse Files\n{workspace_detail}"
        ));
        remember_inspector_control(ui, &selector);
        // Resolve the chosen stable session after the popup, so tab reorders
        // and background session exits cannot redirect a pending activation.
        let workspaces: Vec<_> = (0..self.tabs.len())
            .filter_map(|tab| {
                let index = self.tab_display_session(tab)?;
                let session = self.session_manager.sessions().get(index)?;
                let private = self.tabs.flags(tab).private_title;
                let cwd = (!private)
                    .then(|| session.terminal.lock().current_working_dir.clone())
                    .flatten();
                Some((
                    session.metadata.session_id.clone(),
                    self.tab_title(tab),
                    cwd,
                    tab == self.tabs.active_index(),
                ))
            })
            .collect();
        let mut activate = None;
        let popup_visible = egui::Popup::menu(&selector)
            .style(crate::theme::configure_popup_style)
            .show(|ui| {
                ui.set_max_width(360.0);
                ui.strong("Open workspaces");
                for (id, title, cwd, selected) in &workspaces {
                    let response = ui
                        .push_id(("workspace-session", id), |ui| {
                            ui.selectable_label(
                                *selected,
                                jterm_core::review_input::safe_inline_display(title, 256),
                            )
                        })
                        .inner;
                    let response = if let Some(cwd) = cwd {
                        response.on_hover_text(inspector_context_text(
                            cwd,
                            false,
                            self.config.ai_redact_secrets,
                            4096,
                        ))
                    } else {
                        response
                    };
                    remember_inspector_control(ui, &response);
                    if response.clicked() {
                        activate = Some(id.clone());
                        ui.close();
                    }
                }
                ui.separator();
                if inspector_button(ui, true, "New terminal session").clicked() {
                    self.new_tab();
                    ui.close();
                }
                if inspector_button(ui, true, "Open directory in Files…").clicked() {
                    self.sidebar.open_path_entry();
                    self.reference_request_sidebar_view(sidebar::SidebarView::Files, ui.ctx());
                    ui.close();
                }
            })
            .is_some();
        crate::ui::remember_reference_chrome_scope(
            ui.ctx(),
            selector.id.with("popup-scope"),
            selector.rect,
            popup_visible,
        );
        if let Some(index) = activate
            .as_deref()
            .and_then(|id| self.session_manager.index_of(id))
        {
            self.activate_session(index);
            self.return_focus_to_terminal(ui.ctx());
        }
        ui.add_space(8.0);
        let (task_label, task_attention) =
            crate::app::tasks::tasks_tab_label(self.task_manager.attention_counts());
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
            let attention = (key == "agent" && !conversation::fixture_enabled())
                .then_some(task_attention.as_deref())
                .flatten();
            let label = if attention.is_some() {
                format!("{label} · {task_label}")
            } else {
                label.to_owned()
            };
            let response = nav_row(ui, &label, selected, Some(Icon::from_key(key)));
            let response = if let Some(summary) = attention {
                response.on_hover_text(format!(
                    "{label}\n{summary}\nOpen the Agent Tasks dashboard"
                ))
            } else {
                response.on_hover_text(&label)
            };
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
            RichText::new("RECENT")
                .size(9.0 * scale)
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
                        FontId::proportional(9.0 * scale),
                        rgb(color::CHROME_MUTED),
                    );
                    let title_width =
                        (row_width - 28.0 - 7.0 - time.size().x - shell::METADATA_GAP).max(1.0);
                    let mut title_job = egui::text::LayoutJob::simple(
                        entry.title.clone(),
                        FontId::proportional(10.0 * scale),
                        rgb(color::CHROME_TEXT),
                        title_width,
                    );
                    title_job.wrap.max_rows = 1;
                    title_job.wrap.break_anywhere = true;
                    let title = ui.painter().layout_job(title_job);
                    let mut detail_job = egui::text::LayoutJob::simple(
                        entry.detail.clone(),
                        FontId::proportional(9.0 * scale),
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
        let row_height = shell::SPACES_ROW_HEIGHT.max(24.0 * scale + 4.0);
        let height = 16.0 * scale + shell::METADATA_GAP + 4.0 * row_height;
        // Fill the spare navigation space before Spaces, keeping recent tabs
        // near the top and product destinations above the session footer.
        ui.add_space((ui.clip_rect().bottom() - height - ui.cursor().top()).max(16.0));
        ui.label(
            RichText::new("SPACES")
                .size(8.0 * scale)
                .color(rgb(color::CHROME_MUTED)),
        );
        ui.spacing_mut().item_spacing.y = 0.0;
        for (name, detail) in conversation::SPACES {
            let (row, response) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), row_height),
                egui::Sense::hover(),
            );
            let badge = egui::Rect::from_center_size(
                egui::pos2(row.left() + 11.0, row.center().y),
                egui::Vec2::splat(jterm_core::reference_identity::SPACE_ICON_SIZE),
            );
            product_identity(ui, badge, name);
            let painter = ui.painter().with_clip_rect(row.intersect(ui.clip_rect()));
            painter.text(
                egui::pos2(row.left() + 30.0, row.center().y - 6.0 * scale),
                egui::Align2::LEFT_CENTER,
                name,
                FontId::proportional(10.0 * scale),
                rgb(color::CHROME_TEXT),
            );
            painter.text(
                egui::pos2(row.left() + 30.0, row.center().y + 6.0 * scale),
                egui::Align2::LEFT_CENTER,
                detail,
                FontId::proportional(8.0 * scale),
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
        let settings = egui::Rect::from_center_size(
            rect.right_center() - egui::vec2(14.0, 0.0),
            egui::vec2(28.0, 28.0),
        );
        if self.config.effective_ui_font_size() <= 24.0 {
            ui.painter().circle_filled(
                rect.left_center() + egui::vec2(8.0, 0.0),
                4.0,
                shell::rgba(shell::token::MUTED_TEXT),
            );
            let text = egui::Rect::from_min_max(
                rect.min + egui::vec2(26.0, 0.0),
                egui::pos2(
                    (settings.left() - 6.0).max(rect.left() + 26.0),
                    rect.bottom(),
                ),
            );
            ui.painter()
                .with_clip_rect(text.intersect(ui.clip_rect()))
                .text(
                    text.left_center(),
                    egui::Align2::LEFT_CENTER,
                    "Local session",
                    FontId::proportional(9.0 * scale),
                    shell::rgba(shell::token::MUTED_TEXT),
                );
        }
        let settings = settings.intersect(rect);
        let response = ui.interact(
            settings,
            egui::Id::new("reference-navigation-status-settings"),
            egui::Sense::click(),
        );
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Settings"));
        if response.hovered() || response.has_focus() {
            ui.painter().rect_filled(
                settings,
                shell::CONTROL_RADIUS,
                shell::rgba(shell::token::BUTTON_HOVER),
            );
        }
        if response.has_focus() {
            ui.painter().rect_stroke(
                settings,
                shell::CONTROL_RADIUS,
                Stroke::new(1.0, rgb(color::ACCENT)),
                egui::StrokeKind::Inside,
            );
        }
        icon(
            ui,
            egui::Rect::from_center_size(settings.center(), egui::Vec2::splat(14.0)),
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
                left: 10,
                right: 10,
                top: 10,
                bottom: shell::token::NAVIGATION_BOTTOM_GAP as i8,
            }))
            .show_bounded(root, |ui| {
                let bounds = ui.available_rect_before_wrap();
                let status = egui::Rect::from_min_max(
                    egui::pos2(
                        bounds.left(),
                        (bounds.bottom() - shell::NAVIGATION_STATUS_HEIGHT).max(bounds.top()),
                    ),
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
                                if nav_row(ui, &title, self.tabs.active_index() == index, None)
                                    .on_hover_text(&title)
                                    .clicked()
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
        let frame = egui::Frame::NONE.inner_margin(egui::Margin {
            left: 10 + shell::token::INSPECTOR_GAP as i8,
            right: 10 + shell::token::OUTER_RIGHT as i8,
            top: 10,
            bottom: shell::token::MAIN_BOTTOM_GAP as i8,
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
                    self.reference_layout(root.ctx())
                        .inspector
                        .map_or(shell::INSPECTOR_WIDTH, |rect| rect.width)
                        + shell::token::INSPECTOR_GAP
                        + shell::token::OUTER_RIGHT,
                )
                .resizable(false)
                .show_separator_line(false)
                .frame(frame)
                .show_bounded(root, |ui| self.render_reference_inspector_contents(ui));
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
                ui.horizontal_wrapped(|ui| {
                    ui.visuals_mut().selection.bg_fill = shell::rgba(shell::token::SELECTED_NAV);
                    ui.visuals_mut().selection.stroke =
                        Stroke::new(1.0, shell::rgba(shell::token::BORDER));
                    for (candidate, label) in [
                        (ReferenceInspectorTab::Agent, "Agent"),
                        (ReferenceInspectorTab::Context, "Context"),
                        (ReferenceInspectorTab::Files, "Files"),
                    ] {
                        let response = ui.add_sized(
                            [shell::INSPECTOR_TAB_WIDTH, 26.0],
                            egui::Button::selectable(tab == candidate, label)
                                .corner_radius(shell::CONTROL_RADIUS),
                        );
                        remember_inspector_control(ui, &response);
                        if response.clicked() {
                            self.reference_request_inspector_tab(candidate, ui.ctx());
                        }
                    }
                });
                ui.add_space(10.0);
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
        ui.label(
            RichText::new("Live facts from the focused terminal")
                .small()
                .weak(),
        );
        let private = self.reference_private_title();
        let redact = self.config.ai_redact_secrets;
        let session = &self.session_manager.sessions()[self.session_manager.active_index()];
        let id = session.metadata.session_id.clone();
        let pid = session.get_shell_pid();
        let shell = session
            .shell
            .shell_program()
            .unwrap_or("Not reported")
            .to_owned();
        let owns_foreground = local_shell_owns_foreground(session);
        let (reported_cwd, cols, rows, latest, retained) = {
            let terminal = session.terminal.lock();
            let latest = terminal
                .command_records()
                .iter()
                .rev()
                .find(|record| record.start_mark_seen || record.complete);
            (
                terminal.current_working_dir.clone(),
                terminal.grid.row_len(),
                terminal.grid.rows(),
                latest.map(|record| {
                    (
                        inspector_context_text(
                            record
                                .command
                                .as_deref()
                                .unwrap_or("Command text not reported"),
                            private,
                            redact,
                            4096,
                        ),
                        command_outcome(record),
                        record.command_exact,
                    )
                }),
                latest.and_then(|record| terminal.command_output_text(&record.id, 8192)),
            )
        };
        let local_process_cwd = (session.purpose == crate::session::SessionPurpose::Interactive
            && session.shell.shell_program().is_some())
        .then(|| jterm_core::process::process_cwd(pid))
        .flatten();
        let (cwd, cwd_source) = if reported_cwd.is_some() {
            (reported_cwd, "Shell report")
        } else if local_process_cwd.is_some() {
            (local_process_cwd.clone(), "Local process")
        } else {
            // An SSH client's local cwd does not describe its remote shell.
            (None, "Not reported")
        };
        let browse_directory = (!private && owns_foreground)
            .then(|| verified_local_directory(cwd.as_deref(), local_process_cwd.as_deref()))
            .flatten();
        let displayed_cwd = inspector_context_text(
            cwd.as_deref().unwrap_or("Not reported by shell or process"),
            private,
            redact,
            4096,
        );
        let mode = if self.config.chat_mode_enabled() {
            "Chat"
        } else {
            "Block"
        };
        let fields = [
            ("PRESENTATION", mode.to_owned()),
            ("DIRECTORY", displayed_cwd.clone()),
            ("DIRECTORY SOURCE", cwd_source.into()),
            (
                "SHELL",
                inspector_context_text(&shell, private, redact, 512),
            ),
            ("SESSION", id.clone()),
            ("PROCESS PID", pid.to_string()),
            ("GRID", format!("{cols} columns × {rows} rows")),
            (
                "INPUT OWNER",
                if owns_foreground {
                    "Local shell"
                } else {
                    "Foreground program"
                }
                .into(),
            ),
        ];
        ui.add_space(12.0);
        for (label, value) in &fields {
            ui.label(
                RichText::new(*label)
                    .size(9.0 * scale)
                    .color(rgb(color::CHROME_MUTED)),
            );
            ui.add(egui::Label::new(RichText::new(value).size(9.0 * scale)).wrap())
                .on_hover_text(value);
            ui.add_space(12.0);
        }
        ui.horizontal_wrapped(|ui| {
            if inspector_button(ui, !private && cwd.is_some(), "Copy directory").clicked() {
                ui.ctx().copy_text(displayed_cwd.clone());
            }
            if inspector_button(ui, browse_directory.is_some(), "Browse directory")
                .on_disabled_hover_text("Available when this directory matches the local shell process at its prompt. Use Files to choose another local or remote location.")
                .clicked()
            {
                if let Some(directory) = browse_directory {
                    self.reference_inspector.pending = Some(ReferenceInspectorRequest::RevealSessionDirectory {
                        session_id: id.clone(), directory,
                    });
                    self.return_focus_to_terminal(ui.ctx());
                    ui.ctx().request_repaint();
                }
            }
            if inspector_button(ui, true, "Copy session details").clicked() {
                ui.ctx().copy_text(fields.iter().map(|(key, value)| format!("{key}: {value}")).collect::<Vec<_>>().join("\n"));
            }
        });
        ui.separator();
        ui.strong("Latest command");
        if let Some((command, outcome, exact)) = latest {
            ui.add(egui::Label::new(RichText::new(&command).monospace()).wrap());
            ui.label(&outcome);
            if !exact && !private {
                ui.label(
                    RichText::new("Command text reconstructed from the terminal")
                        .small()
                        .weak(),
                );
            }
            let output = retained
                .as_ref()
                .map(|output| inspector_context_text(&output.text, private, redact, 8192));
            if let Some(output) = output.as_deref().filter(|text| !text.trim().is_empty()) {
                egui::ScrollArea::vertical()
                    .id_salt(("reference-context-output", &id))
                    .max_height(180.0)
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(output).monospace()).wrap());
                    });
                if retained.as_ref().is_some_and(|output| output.truncated) {
                    ui.label(
                        RichText::new("Output preview is truncated to 8 KiB")
                            .small()
                            .weak(),
                    );
                }
            } else {
                ui.label(
                    RichText::new("No retained output for this command")
                        .small()
                        .weak(),
                );
            }
            ui.horizontal_wrapped(|ui| {
                if inspector_button(ui, !private, "Copy command").clicked() {
                    ui.ctx().copy_text(command);
                }
                if inspector_button(
                    ui,
                    !private && output.as_ref().is_some_and(|text| !text.is_empty()),
                    "Copy output preview",
                )
                .clicked()
                {
                    ui.ctx().copy_text(output.unwrap_or_default());
                }
            });
        } else {
            ui.label("No command lifecycle has been reported yet. Run a command in a shell with OSC 133 integration to see its exit status, duration and captured output.");
        }
        ui.separator();
        ui.strong("AI context settings");
        ui.label(format!(
            "AI features: {} · command-context sharing: {} · secret redaction: {}",
            if self.config.ai_enabled { "on" } else { "off" },
            if self.config.ai_share_command_context {
                "on"
            } else {
                "off"
            },
            if redact { "on" } else { "off" }
        ));
        if inspector_button(ui, true, "Configure AI and privacy…").clicked() {
            self.config_panel.open_ai(&self.config);
        }
    }

    fn render_reference_agent(&mut self, ui: &mut egui::Ui) {
        let fixture = conversation::fixture_enabled();
        let scale = self.config.effective_ui_font_size() / config::DEFAULT_FONT_SIZE;
        if fixture {
            crate::reference_conversation_ui::draw_inspector(
                ui,
                &conversation::acceptance_fixture().inspector,
                true,
                scale,
            );
            return;
        }
        let enabled = self.config.experimental_task_sidebar;
        let private = self.reference_private_title();
        let redact = self.config.ai_redact_secrets;
        ui.horizontal_wrapped(|ui| {
            if inspector_button(ui, enabled, "New task")
                .on_disabled_hover_text("Enable the Tasks dashboard in Settings → AI")
                .clicked()
            {
                self.open_task_composer();
                self.reference_request_sidebar_view(sidebar::SidebarView::Tasks, ui.ctx());
            }
            if inspector_button(ui, enabled, "Task dashboard")
                .on_disabled_hover_text("Enable the Tasks dashboard in Settings → AI")
                .clicked()
            {
                self.reference_request_sidebar_view(sidebar::SidebarView::Tasks, ui.ctx());
            }
            if inspector_button(ui, true, "Conversation library").clicked() {
                self.open_agent_sessions();
            }
        });
        if !enabled {
            ui.label(
                RichText::new("Enable Agent Tasks in Settings to start a task.")
                    .small()
                    .weak(),
            );
            if inspector_button(ui, true, "Configure Agent features…").clicked() {
                self.config_panel.open_ai(&self.config);
            }
        }
        let tasks: Vec<_> = self
            .task_manager
            .tasks()
            .iter()
            .filter(|task| task.status != crate::agent::TaskStatus::Archived)
            .map(|task| {
                (
                    task.id,
                    inspector_context_text(&task.title, private, redact, 256),
                )
            })
            .collect();
        let selected = conversation::observed_inspector_task(
            self.task_manager.tasks(),
            self.task_sidebar.selected,
        )
        .map(|task| task.id);
        self.task_sidebar.selected = selected;
        if !tasks.is_empty() {
            let selected_title = selected
                .and_then(|id| tasks.iter().find(|(candidate, _)| *candidate == id))
                .map(|(_, title)| title.as_str())
                .unwrap_or("Choose task");
            let response = egui::ComboBox::from_id_salt("reference-agent-task-selector")
                .popup_style(crate::theme::configure_popup_style.into())
                .width(ui.available_width())
                .selected_text(selected_title)
                .show_ui(ui, |ui| {
                    for (id, title) in &tasks {
                        let response =
                            ui.selectable_value(&mut self.task_sidebar.selected, Some(*id), title);
                        remember_inspector_control(ui, &response);
                    }
                });
            remember_inspector_control(ui, &response.response);
        }
        let selected = self.task_sidebar.selected;
        if let Some(id) = selected {
            let task = self.task_manager.get(id);
            let terminal = task
                .and_then(|task| task.terminal_session_id.as_deref())
                .is_some_and(|session| self.session_manager.index_of(session).is_some());
            let validation = task
                .and_then(|task| task.validation.terminal_session_id.as_deref())
                .is_some_and(|session| self.session_manager.index_of(session).is_some());
            ui.push_id(("reference-agent-task-actions", id), |ui| {
                ui.horizontal_wrapped(|ui| {
                    if inspector_button(ui, terminal, "Agent terminal")
                        .on_disabled_hover_text("No live Agent terminal is attached to this task")
                        .clicked()
                    {
                        self.task_sidebar.pending_action =
                            Some(crate::app::tasks::TaskSidebarAction::FocusTerminal(id));
                    }
                    if inspector_button(ui, validation, "Validation output")
                        .on_disabled_hover_text("This task has no retained validation terminal")
                        .clicked()
                    {
                        self.task_sidebar.pending_action =
                            Some(crate::app::tasks::TaskSidebarAction::FocusValidation(id));
                    }
                    if inspector_button(ui, true, "Review diff").clicked() {
                        self.task_sidebar.pending_action =
                            Some(crate::app::tasks::TaskSidebarAction::ReviewDiff(id));
                    }
                });
            });
            ui.label(
                RichText::new(
                    "Start, approvals, replies and validation are available in the task dashboard.",
                )
                .small()
                .weak(),
            );
        } else {
            ui.add_space(8.0);
            ui.strong("No visible Agent tasks");
            ui.label(RichText::new("Start or select a task").small().weak());
            if inspector_button(ui, true, "Open terminal assistant").clicked() {
                let session_id = self.session_manager.sessions()
                    [self.session_manager.active_index()]
                .metadata
                .session_id
                .clone();
                self.agent_panel.open(&self.config, session_id);
            }
        }
        ui.add_space(12.0);
        let mut model =
            conversation::observed_task_inspector(self.task_manager.tasks(), selected, |id| {
                let task = self.task_manager.get(id);
                crate::agent::DashboardActivity {
                    has_agent_terminal: task
                        .and_then(|task| task.terminal_session_id.as_deref())
                        .is_some_and(|session| self.session_manager.index_of(session).is_some()),
                    has_validation_terminal: task
                        .and_then(|task| task.validation.terminal_session_id.as_deref())
                        .is_some_and(|session| self.session_manager.index_of(session).is_some()),
                    has_active_agent_stream: self.task_manager.has_active_agent_event_stream(id),
                    native_preparing: self.agent_runtime.has_preparing(id),
                }
            });
        if let Some(session) = selected
            .and_then(|id| self.task_manager.get(id))
            .and_then(|task| task.terminal_session_id.as_deref())
            .and_then(|id| self.session_manager.index_of(id))
            .and_then(|index| self.session_manager.sessions().get(index))
        {
            let terminal = session.terminal.lock();
            if let Some(output) = terminal
                .command_records()
                .iter()
                .rev()
                .find(|record| record.start_mark_seen || record.complete)
                .and_then(|record| terminal.command_output_text(&record.id, 8192))
                .filter(|output| !output.text.trim().is_empty())
            {
                model.output.push_str("\n\nAgent terminal preview\n");
                model.output.push_str(&output.text);
                if output.truncated {
                    model.output.push_str("\n[Output preview truncated]");
                }
            }
        }
        model.title = inspector_context_text(&model.title, private, redact, 256);
        model.output = inspector_context_text(&model.output, private, redact, 8192);
        for (label, value) in &mut model.metadata {
            let sensitive = matches!(label.as_str(), "Workspace" | "Branch");
            *value = inspector_context_text(value, private && sensitive, redact, 4096);
        }
        if let Some(step) = &mut model.current_step {
            step.title = inspector_context_text(&step.title, false, redact, 512);
            step.detail = inspector_context_text(&step.detail, private, redact, 4096);
        }
        for step in &mut model.queue {
            step.title = inspector_context_text(&step.title, private, redact, 512);
            step.detail = inspector_context_text(&step.detail, false, redact, 4096);
        }
        crate::reference_conversation_ui::draw_inspector(ui, &model, false, scale);
    }

    pub(crate) fn render_reference_canvas_header(&mut self, ui: &mut egui::Ui) {
        let scale = self.config.effective_ui_font_size() / config::DEFAULT_FONT_SIZE;
        let title = if conversation::fixture_enabled() {
            conversation::acceptance_fixture().title
        } else {
            self.tab_title(self.tabs.active_index())
        };
        let title = if title.starts_with("Workspace / ") {
            title
        } else {
            format!("Workspace / {title}")
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
        shell::bounded_ui_at(ui, rect.shrink2(egui::vec2(28.0, 8.0)), |ui| {
            ui.visuals_mut().override_text_color = Some(rgb(color::CANVAS_TEXT));
            let origin = ui.max_rect().min;
            let title_width = (ui.available_width()
                - 3.0 * shell::HEADER_ACTION_SIZE
                - 2.0 * shell::HEADER_ACTION_GAP
                - 8.0)
                .max(0.0);
            let title_rect =
                egui::Rect::from_min_size(origin, egui::vec2(title_width, 18.0 * scale));
            let mut heading = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(title_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            heading
                .add(
                    egui::Label::new(
                        RichText::new(&title)
                            .size(shell::type_size::PAGE_TITLE * scale)
                            .strong(),
                    )
                    .truncate()
                    .halign(egui::Align::Min),
                )
                .on_hover_text(&title);
            let subtitle = egui::Rect::from_min_size(
                origin + egui::vec2(0.0, 18.0 * scale),
                egui::vec2(title_width, 13.0 * scale),
            );
            let mut detail = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(subtitle)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            detail.add(
                egui::Label::new(
                    RichText::new(if self.config.chat_mode_enabled() {
                        "Chat session · Local"
                    } else {
                        "Block session · Local"
                    })
                    .size(9.0 * scale)
                    .color(rgb(color::CHROME_MUTED)),
                )
                .truncate()
                .halign(egui::Align::Min),
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
                                    ui.menu_button("Export session…", |ui| {
                                        ui.set_max_width(300.0);
                                        ui.label("Save retained completed command blocks to a private local export file.");
                                        let reason = if self.reference_private_title() {
                                            Some("Reveal this private tab before exporting its terminal content")
                                        } else if !self.config.command_cards_enabled() {
                                            Some("Use Block or Chat presentation to export command blocks")
                                        } else if self.pending_session_export.is_some() {
                                            Some("A session export is already in progress")
                                        } else {
                                            let session = &self.session_manager.sessions()[self.session_manager.active_index()];
                                            let terminal = session.terminal.lock();
                                            if terminal.is_alt_buffer_active() {
                                                Some("Return to the shell before exporting command blocks")
                                            } else if !terminal.command_records().iter().any(|record| record.complete) {
                                                Some("No completed command blocks have been retained in this session")
                                            } else {
                                                None
                                            }
                                        };
                                        if let Some(reason) = reason {
                                            ui.label(RichText::new(reason).small().weak());
                                        }
                                        for (label, format) in [
                                            ("Export Markdown", crate::block_export::SessionExportFormat::Markdown),
                                            ("Export JSON", crate::block_export::SessionExportFormat::Json),
                                        ] {
                                            if inspector_button(ui, reason.is_none(), label).clicked() {
                                                self.block_export_session(format);
                                                ui.close();
                                            }
                                        }
                                    });
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
        });
        // The header was already allocated at its shared fixed height. The
        // overlay's smaller controls must not pull the parent cursor back to
        // their measured minimum and change terminal rows/composer budgeting.
        ui.advance_cursor_after_rect(rect);
    }
}

#[cfg(test)]
mod reference_panel_tests {
    use super::{
        command_outcome, inspector_context_text, verified_local_directory, workspace_identity,
    };
    use crate::terminal::TerminalState;

    #[test]
    fn topbar_identity_uses_measured_glyph_bounds_before_mode_controls() {
        for font in [14.0, 32.0, 72.0] {
            for width in [854.0, 855.0, 1000.0, 1356.0] {
                let ctx = egui::Context::default();
                let scale = font / crate::config::DEFAULT_FONT_SIZE;
                let layout =
                    crate::glass_shell::Layout::for_size(width, 850.0).with_chrome_font_size(font);
                let settings_left = width - 8.0 - 32.0 - 6.0 - 32.0;
                let mode_width = crate::glass_shell::MODE_CONTROL_MIN_WIDTH * scale.max(1.0);
                let pair_left = settings_left - 8.0 - 2.0 * mode_width - 6.0;
                let mode_left = if pair_left >= 104.0 {
                    pair_left
                } else {
                    settings_left - 8.0 - 32.0
                };
                let mut painted = None;
                let output = ctx.run_ui(Default::default(), |ui| {
                    painted = super::paint_topbar_title(
                        ui,
                        layout.titlebar.height,
                        scale,
                        mode_left - 12.0,
                    );
                });
                output.drop_without_applying_deltas();
                if let Some(rect) = painted {
                    assert!(rect.right() <= mode_left - 12.0);
                }
                if font == 72.0 && pair_left >= 104.0 && width <= 1000.0 {
                    assert!(
                        painted.is_none(),
                        "large identity text cannot paint beneath the mode controls"
                    );
                }
                if font == 14.0 || width == 1356.0 || pair_left < 104.0 {
                    assert!(
                        painted.is_some(),
                        "the full identity remains visible when it fits"
                    );
                }
                let icon =
                    egui::Rect::from_min_size(egui::pos2(104.0, 0.0), egui::vec2(20.0, 20.0));
                if icon.right() <= mode_left - 12.0 {
                    assert!(!icon
                        .intersect(egui::Rect::from_min_size(
                            egui::pos2(mode_left, 0.0),
                            egui::vec2(32.0, 32.0)
                        ))
                        .is_positive());
                }
            }
        }
    }

    #[test]
    fn private_context_hides_directory_command_and_output_before_copy() {
        for source in ["/home/private/project", "echo secret", "sensitive output"] {
            assert_eq!(
                inspector_context_text(source, true, false, 128),
                "Hidden for private tab"
            );
        }
    }

    #[test]
    fn context_browse_requires_the_visible_directory_in_the_local_process_namespace() {
        use std::path::PathBuf;
        assert_eq!(
            verified_local_directory(Some("/remote/project"), Some("/local/project")),
            None
        );
        assert_eq!(
            verified_local_directory(Some("/remote/project"), None),
            None
        );
        assert_eq!(verified_local_directory(None, Some("relative")), None);
        assert_eq!(
            verified_local_directory(None, Some("/local/project")),
            Some(PathBuf::from("/local/project"))
        );
        assert_eq!(
            verified_local_directory(Some("/local/project"), Some("/local/project")),
            Some(PathBuf::from("/local/project"))
        );
    }

    #[test]
    fn workspace_caption_uses_active_directory_and_keeps_remote_client_cwd_out() {
        let (caption, tooltip) = workspace_identity(
            false,
            Some("/projects/alpha"),
            Some("/projects/beta"),
            true,
            "Session 2",
            true,
        );
        assert_eq!(caption, "beta");
        assert_eq!(tooltip, "Local shell directory: /projects/beta");
        let (caption, tooltip) = workspace_identity(
            false,
            Some("/remote/beta"),
            Some("/local/alpha"),
            false,
            "SSH session",
            true,
        );
        assert_eq!(caption, "beta");
        assert_eq!(tooltip, "Shell-reported directory: /remote/beta");
        let (caption, tooltip) = workspace_identity(
            false,
            None,
            Some("/local/alpha"),
            false,
            "SSH session",
            true,
        );
        assert_eq!(caption, "SSH session");
        assert!(!tooltip.contains("alpha"));
        assert!(tooltip.contains("Directory not reported"));
        assert_eq!(
            workspace_identity(
                true,
                Some("/remote/beta"),
                Some("/local/alpha"),
                false,
                "secret session",
                true,
            ),
            ("Private workspace".into(), "Private workspace".into())
        );
    }

    #[test]
    fn context_preview_redacts_secrets_and_bounds_untrusted_text() {
        let secret = "AKIAIOSFODNN7EXAMPLE";
        let source = format!("token {secret}\nstatus\u{202e}{}\u{1b}", "界".repeat(100));
        let preview = inspector_context_text(&source, false, true, 128);
        assert!(!preview.contains(secret));
        assert!(!preview.contains('\u{202e}'));
        assert!(!preview.contains('\u{1b}'));
        assert!(preview.contains('\n'));
        assert!(preview.len() <= 128);
        assert!(inspector_context_text(secret, false, false, 128).contains(secret));
    }

    #[test]
    fn context_command_outcome_uses_reported_failure_and_duration() {
        let mut terminal = TerminalState::new(40, 5);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B;jsh_id=ctx-7\x07false\r\n\x1b]133;C;jsh_id=ctx-7;cmdline_url=false\x07");
        assert_eq!(
            command_outcome(terminal.command_records().back().unwrap()),
            "Running"
        );
        terminal.process_input(b"\x1b]133;D;7;jsh_id=ctx-7;duration_ms=42\x07");
        let mut record = terminal.command_records().back().unwrap().clone();
        assert_eq!(command_outcome(&record), "Exit 7 · 42 ms");
        record.exit_code = None;
        record.duration_ms = None;
        assert_eq!(command_outcome(&record), "Finished · exit not reported");
    }
}
