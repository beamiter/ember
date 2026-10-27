//! Native adapter for the shared reconstruction contract.
//!
//! This module owns chrome only. The live PTY, projection, IME and reading
//! anchors continue to belong to the existing terminal renderer and session.
use crate::reference_conversation_ui::{icon, icon_button};
use crate::{app::state::TerminalApp, config, sidebar, theme};
use egui::{Color32, FontId, RichText, Stroke};
use jterm_core::reference_shell::{self as shell, color};
use jterm_core::{reference_conversation as conversation, reference_icons::Icon};

pub(crate) fn rgb(value: [u8; 3]) -> Color32 {
    Color32::from_rgb(value[0], value[1], value[2])
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
    let mut mesh = egui::Mesh::default();
    for (pos, tint) in [
        (rect.left_top(), color::CANVAS),
        (rect.right_top(), color::CANVAS),
        (rect.left_bottom(), color::CANVAS_COOL),
        (rect.right_bottom(), color::CANVAS_COOL),
    ] {
        mesh.colored_vertex(pos, rgb(tint));
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    painter.add(egui::Shape::mesh(mesh));
}

fn header_action(ui: &mut egui::Ui, kind: Icon, label: &str) -> egui::Response {
    let response = ui
        .push_id(("reference-header-action", label), |ui| {
            ui.add_sized(
                [shell::HEADER_ACTION_SIZE, shell::HEADER_ACTION_SIZE],
                egui::Button::new("")
                    .fill(rgb(color::CONTROL_SURFACE))
                    .stroke(Stroke::new(1.0, rgb(color::CONTROL_BORDER)))
                    .corner_radius(6),
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
    style.visuals.panel_fill = rgb(color::NAVIGATION);
    style.visuals.window_fill = rgb(color::INSPECTOR_CARD);
    style.visuals.selection.bg_fill = rgb(color::NAVIGATION_SELECTED);
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, rgb(color::DARK_BORDER));
    style.visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    style.visuals.widgets.hovered.weak_bg_fill = rgb(color::NAVIGATION_SELECTED);
    style.spacing.item_spacing = egui::vec2(shell::METADATA_GAP, 4.0);
    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    style.spacing.interact_size.y = shell::CONTROL_HEIGHT;
    for font in style.text_styles.values_mut() {
        *font = FontId::proportional(10.0 * scale);
    }
}

fn nav_row(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let response = ui.add_sized(
        [ui.available_width(), shell::NAVIGATION_ROW_HEIGHT],
        egui::Button::new("")
            .left_text(label)
            .selected(selected)
            .corner_radius(7)
            .frame(selected),
    );
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
    }

    pub(crate) fn reference_layout(&self, ctx: &egui::Context) -> shell::Layout {
        let size = ctx.viewport_rect().size();
        shell::Layout::for_size(size.x, size.y)
    }

    /// The surrounding workspace is dark; both native presentations share
    /// the light document and white PTY-owned composer.
    pub(crate) fn reference_terminal_theme(&self) -> theme::Theme {
        let mut theme = self.current_theme.clone();
        {
            theme.terminal = theme::Theme::builtin_light().terminal;
            theme.terminal.background = color::CANVAS;
            theme.terminal.foreground = color::CANVAS_TEXT;
            theme.terminal.cursor = color::ACCENT;
            theme.tabbar.active_border = color::ACCENT;
        }
        theme
    }

    pub(crate) fn render_reference_topbar(&mut self, root: &mut egui::Ui) {
        let scale = self.config.effective_ui_font_size() / config::DEFAULT_FONT_SIZE;
        egui::Panel::top("reference-titlebar")
            .exact_size(shell::TITLEBAR_HEIGHT)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::NONE
                    .fill(rgb(color::TITLEBAR))
                    .inner_margin(7.0),
            )
            .show(root, |ui| {
                chrome_style(ui, scale);
                let blank = ui.max_rect();
                let drag = ui.interact(blank, ui.id().with("window-drag"), egui::Sense::drag());
                if drag.drag_started() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                ui.horizontal_centered(|ui| {
                    for (glyph, tip, command) in [
                        ("×", "Close window", egui::ViewportCommand::Close),
                        (
                            "−",
                            "Minimize window",
                            egui::ViewportCommand::Minimized(true),
                        ),
                    ] {
                        if ui
                            .add_sized(
                                [24.0, 24.0],
                                egui::Button::new(RichText::new(glyph).size(12.0))
                                    .fill(Color32::TRANSPARENT)
                                    .stroke(Stroke::NONE)
                                    .corner_radius(5),
                            )
                            .on_hover_text(tip)
                            .clicked()
                        {
                            ui.ctx().send_viewport_cmd(command);
                        }
                    }
                    if ui
                        .add_sized(
                            [24.0, 24.0],
                            egui::Button::new(RichText::new("□").size(12.0))
                                .fill(Color32::TRANSPARENT)
                                .stroke(Stroke::NONE)
                                .corner_radius(5),
                        )
                        .on_hover_text("Maximize or restore window")
                        .clicked()
                    {
                        let maximized =
                            ui.input(|input| input.viewport().maximized.unwrap_or(false));
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                    }
                    ui.add_space(8.0);
                    let (brand, _) = ui.allocate_exact_size(
                        egui::Vec2::splat(jterm_core::reference_identity::PRODUCT_ICON_SIZE),
                        egui::Sense::hover(),
                    );
                    product_identity(ui, brand, "ember");
                    ui.label(RichText::new("Ember").strong().size(12.0 * scale));
                    ui.label(
                        RichText::new(if self.config.chat_mode_enabled() {
                            "Chat"
                        } else {
                            "Block"
                        })
                        .size(9.0)
                        .color(rgb(color::CHROME_MUTED)),
                    );
                    let width = ui.available_width();
                    if width > 780.0 {
                        ui.add_space(18.0);
                        let path = self.reference_workspace_path();
                        ui.add_sized(
                            [160.0, 22.0],
                            egui::Label::new(
                                RichText::new(path.as_str()).color(rgb(color::CHROME_MUTED)),
                            )
                            .truncate(),
                        )
                        .on_hover_text(path);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, Icon::Settings, "Settings").clicked() {
                            self.config_panel.open(&self.config);
                        }
                        for (mode, label) in [
                            (config::TerminalMode::Chat, "Chat"),
                            (config::TerminalMode::Block, "Block"),
                        ] {
                            let selected = (mode == config::TerminalMode::Chat)
                                == self.config.chat_mode_enabled();
                            if ui
                                .add(egui::Button::new(label).selected(selected).corner_radius(7))
                                .on_hover_text("Change presentation of the same live shell")
                                .clicked()
                                && !selected
                            {
                                self.config.terminal_mode = Some(mode);
                                self.apply_runtime_config(ui.ctx());
                                self.force_resize_session = true;
                                self.return_focus_to_terminal(ui.ctx());
                                self.config_panel.sync_from_config(&self.config);
                                self.schedule_config_save();
                            }
                        }
                    });
                });
                if ui.max_rect().width() > 900.0 {
                    let width = shell::SEARCH_WIDTH;
                    let global_width = ui.ctx().viewport_rect().width();
                    let search = egui::Rect::from_min_size(
                        egui::pos2(
                            (global_width - width) * 0.5 + shell::SEARCH_CENTER_OFFSET,
                            shell::SEARCH_TOP,
                        ),
                        egui::vec2(width, shell::SEARCH_HEIGHT),
                    );
                    if ui
                        .put(
                            search,
                            egui::Button::new("")
                                .fill(rgb(color::INSPECTOR_CARD))
                                .corner_radius(6),
                        )
                        .on_hover_text("Open command launcher")
                        .clicked()
                    {
                        self.command_palette.open();
                    }
                    icon(
                        ui,
                        egui::Rect::from_min_size(
                            search.min + egui::vec2(9.0, 6.0),
                            egui::vec2(16.0, 16.0),
                        ),
                        Icon::Search,
                        rgb(color::CHROME_MUTED),
                    );
                    ui.painter().text(
                        search.left_center() + egui::vec2(34.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        "Search commands…",
                        egui::FontId::proportional(10.0),
                        rgb(color::CHROME_MUTED),
                    );
                    ui.painter().text(
                        search.right_center() - egui::vec2(10.0, 0.0),
                        egui::Align2::RIGHT_CENTER,
                        self.keybindings
                            .pretty_bindings_for("command_palette:toggle")
                            .first()
                            .map(String::as_str)
                            .unwrap_or("Unbound"),
                        egui::FontId::proportional(9.0),
                        rgb(color::CHROME_TEXT),
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
        egui::Panel::left("reference-rail")
            .exact_size(shell::RAIL_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::NONE.fill(rgb(color::RAIL)).inner_margin(10.0))
            .show(root, |ui| {
                chrome_style(ui, 1.0);
                ui.add_space(2.0);
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
                    let selected = kind == Icon::Grid;
                    let response = ui.add_sized(
                        [34.0, 34.0],
                        egui::Button::new("").selected(selected).corner_radius(8),
                    );
                    icon(
                        ui,
                        response.rect.shrink(8.0),
                        kind,
                        if selected {
                            rgb(color::CHROME_TEXT)
                        } else {
                            rgb(color::CHROME_MUTED)
                        },
                    );
                    if response.on_hover_text(hint).clicked() {
                        if key == "search" {
                            self.command_palette.open();
                        } else {
                            self.sidebar.note_files_user_intent();
                            self.sidebar.visible = true;
                            self.sidebar.view = view;
                        }
                    }
                    ui.add_space(7.0);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    let initial = std::env::var("USER")
                        .ok()
                        .and_then(|name| name.chars().next())
                        .unwrap_or('U')
                        .to_uppercase()
                        .to_string();
                    let profile = ui.add_sized(
                        [shell::PROFILE_DIAMETER, shell::PROFILE_DIAMETER],
                        egui::Button::new(initial)
                            .fill(rgb(color::PROFILE_BACKGROUND))
                            .stroke(Stroke::new(1.0, rgb(color::PROFILE_BORDER)))
                            .corner_radius(14),
                    );
                    profile.on_hover_text("Local profile");
                    if icon_button(ui, Icon::Settings, "Settings").clicked() {
                        self.config_panel.open(&self.config);
                    }
                });
            });
    }

    pub(crate) fn render_reference_navigation(&mut self, ui: &mut egui::Ui) -> bool {
        chrome_style(
            ui,
            self.config.effective_ui_font_size() / config::DEFAULT_FONT_SIZE,
        );
        ui.spacing_mut().item_spacing.y = shell::NAVIGATION_ROW_GAP;
        ui.label(
            RichText::new("WORKSPACE")
                .size(8.0)
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
        ui.add_space((shell::WORKSPACE_SELECTOR_TOP - ui.cursor().top()).max(0.0));
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
            egui::FontId::proportional(10.0),
            rgb(color::CHROME_TEXT),
        );
        ui.painter().text(
            selector.rect.right_center() - egui::vec2(32.0, 0.0),
            egui::Align2::CENTER_CENTER,
            "—",
            egui::FontId::proportional(9.0),
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
        ui.add_space((shell::NAVIGATION_ITEMS_TOP - ui.cursor().top()).max(0.0));
        let mut changed = false;
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
                egui::Rect::from_min_size(
                    response.rect.min + egui::vec2(9.0, 8.0),
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
                        self.sidebar.view = sidebar::SidebarView::Files;
                        changed = true;
                    }
                    "agent" => {
                        self.sidebar.view = sidebar::SidebarView::Tasks;
                        changed = true;
                    }
                    _ => {
                        self.sidebar.view = sidebar::SidebarView::Sessions;
                        changed = true;
                        self.return_focus_to_terminal(ui.ctx());
                    }
                }
            }
        }
        let target = shell::REFERENCE_RECENT_TOP;
        ui.add_space((target - ui.cursor().top()).max(0.0));
        let heading = ui.label(
            RichText::new("RECENT")
                .size(8.0)
                .color(rgb(color::CHROME_MUTED)),
        );
        ui.add_space((heading.rect.bottom() + shell::METADATA_GAP - ui.cursor().top()).max(0.0));
        changed
    }

    pub(crate) fn render_reference_fixture_recent(&mut self, ui: &mut egui::Ui) {
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
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), shell::RECENT_ROW_HEIGHT),
                        egui::Sense::hover(),
                    );
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
                    ui.painter().with_clip_rect(rect).text(
                        egui::pos2(left, rect.top() + 5.0),
                        egui::Align2::LEFT_TOP,
                        &entry.title,
                        egui::FontId::proportional(10.0),
                        rgb(color::CHROME_TEXT),
                    );
                    ui.painter().text(
                        egui::pos2(left, rect.top() + 19.0),
                        egui::Align2::LEFT_TOP,
                        &entry.detail,
                        egui::FontId::proportional(8.0),
                        rgb(color::CHROME_MUTED),
                    );
                    ui.painter().text(
                        egui::pos2(rect.right() - 7.0, rect.top() + 6.0),
                        egui::Align2::RIGHT_TOP,
                        &entry.time,
                        egui::FontId::proportional(8.0),
                        rgb(color::CHROME_MUTED),
                    );
                    response.on_hover_text(format!("{}\n{}", entry.title, entry.detail));
                }
            });
    }

    pub(crate) fn render_reference_spaces(&mut self, ui: &mut egui::Ui) {
        let status_top = ui.ctx().viewport_rect().bottom() - shell::NAVIGATION_STATUS_HEIGHT;
        let spaces_top = shell::REFERENCE_SPACES_TOP.min(status_top - 136.0);
        ui.add_space((spaces_top - ui.cursor().top()).max(0.0));
        let heading = ui.label(
            RichText::new("SPACES")
                .size(8.0)
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
            let badge = egui::Rect::from_center_size(
                egui::pos2(row.left() + 11.0, row.center().y),
                egui::vec2(22.0, 22.0),
            );
            ui.painter().rect_filled(
                badge,
                6,
                Color32::from_rgba_unmultiplied(tint[0], tint[1], tint[2], 32),
            );
            product_identity(ui, badge, name);
            ui.painter().text(
                egui::pos2(row.left() + 28.0, row.top() + 3.0),
                egui::Align2::LEFT_TOP,
                name,
                egui::FontId::proportional(10.0),
                rgb(color::CHROME_TEXT),
            );
            ui.painter().text(
                egui::pos2(row.left() + 28.0, row.top() + 17.0),
                egui::Align2::LEFT_TOP,
                detail,
                egui::FontId::proportional(8.0),
                rgb(color::CHROME_MUTED),
            );
            response.on_hover_text("Opening sibling applications is unavailable");
        }
        ui.add_space((status_top - ui.cursor().top()).max(0.0));
        let bounds = ui.available_rect_before_wrap();
        let text_y = ui.ctx().viewport_rect().bottom() - shell::NAVIGATION_STATUS_TEXT_BOTTOM;
        ui.painter().line_segment(
            [
                egui::pos2(bounds.left(), status_top),
                egui::pos2(bounds.right(), status_top),
            ],
            Stroke::new(1.0, rgb(color::DARK_BORDER)),
        );
        ui.painter().circle_filled(
            egui::pos2(bounds.left() + 3.0, text_y + 5.0),
            2.0,
            rgb(color::CHROME_MUTED),
        );
        ui.painter().text(
            egui::pos2(bounds.left() + 12.0, text_y),
            egui::Align2::LEFT_TOP,
            "Local session",
            egui::FontId::proportional(8.0),
            rgb(color::CHROME_MUTED),
        );
        ui.interact(
            egui::Rect::from_min_max(egui::pos2(bounds.left(), status_top), bounds.max),
            ui.id().with("reference-local-status"),
            egui::Sense::hover(),
        )
        .on_hover_text(format!(
            "{} sessions · local workspace",
            self.session_manager.len()
        ));
    }

    pub(crate) fn render_reference_files_navigation(&mut self, root: &mut egui::Ui) {
        if self.sidebar.view != sidebar::SidebarView::Files
            || !self.sidebar.visible
            || self.reference_layout(root.ctx()).navigation.is_none()
        {
            return;
        }
        egui::Panel::left("reference-files-navigation")
            .exact_size(shell::NAVIGATION_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::NONE
                    .fill(rgb(color::NAVIGATION))
                    .inner_margin(shell::NAVIGATION_PADDING),
            )
            .show(root, |ui| {
                self.render_reference_navigation(ui);
                let tabs: Vec<_> = (0..self.tabs.len())
                    .map(|i| (i, self.tab_title(i)))
                    .collect();
                for (index, title) in tabs {
                    if nav_row(ui, &title, self.tabs.active_index() == index).clicked() {
                        self.activate_tab(index);
                    }
                }
                self.render_reference_spaces(ui);
            });
    }

    pub(crate) fn render_reference_inspector(&mut self, root: &mut egui::Ui) {
        if self.reference_layout(root.ctx()).inspector.is_none()
            || self.sidebar.view == sidebar::SidebarView::Files
        {
            return;
        }
        egui::Panel::right("reference-inspector")
            .exact_size(shell::INSPECTOR_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::NONE
                    .fill(rgb(color::INSPECTOR))
                    .inner_margin(egui::Margin {
                        top: 11,
                        ..egui::Margin::same(shell::INSPECTOR_INSET as i8)
                    }),
            )
            .show(root, |ui| {
                chrome_style(
                    ui,
                    self.config.effective_ui_font_size() / config::DEFAULT_FONT_SIZE,
                );
                let key = ui.id().with("inspector-tab");
                let mut tab = ui
                    .ctx()
                    .data_mut(|data| data.get_temp::<usize>(key).unwrap_or(0));
                ui.horizontal(|ui| {
                    if ui
                        .add_sized(
                            [shell::INSPECTOR_TAB_WIDTH, 28.0],
                            egui::Button::selectable(tab == 0, "Agent"),
                        )
                        .clicked()
                    {
                        tab = 0;
                    }
                    if ui
                        .add_sized(
                            [shell::INSPECTOR_TAB_WIDTH, 28.0],
                            egui::Button::selectable(tab == 1, "Context"),
                        )
                        .clicked()
                    {
                        tab = 1;
                    }
                    if ui
                        .add_sized(
                            [shell::INSPECTOR_TAB_WIDTH, 28.0],
                            egui::Button::new("Files"),
                        )
                        .clicked()
                    {
                        self.sidebar.view = sidebar::SidebarView::Files;
                        self.sidebar.visible = true;
                    }
                });
                ui.ctx().data_mut(|data| data.insert_temp(key, tab));
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .id_salt("reference-inspector-scroll")
                    .show(ui, |ui| {
                        if tab == 1 {
                            self.render_reference_context(ui);
                        } else {
                            self.render_reference_agent(ui);
                        }
                    });
            });
    }

    fn render_reference_context(&mut self, ui: &mut egui::Ui) {
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
                    .size(8.0)
                    .color(rgb(color::CHROME_MUTED)),
            );
            ui.add(egui::Label::new(RichText::new(value.clone()).size(10.0)).wrap())
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
        crate::reference_conversation_ui::draw_inspector(ui, &model, fixture);
    }

    pub(crate) fn render_reference_canvas_header(&mut self, ui: &mut egui::Ui) {
        let title = if conversation::fixture_enabled() {
            conversation::acceptance_fixture().title
        } else {
            format!("Workspace / {}", self.tab_title(self.tabs.active_index()))
        };
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), shell::CANVAS_HEADER_HEIGHT),
            egui::Sense::hover(),
        );
        ui.painter().rect_filled(rect, 0, rgb(color::CANVAS));
        ui.painter().line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            Stroke::new(1.0, rgb(color::LIGHT_BORDER)),
        );
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(28.0, 7.0))),
            |ui| {
                *ui.visuals_mut() = egui::Visuals::light();
                ui.visuals_mut().override_text_color = Some(rgb(color::CANVAS_TEXT));
                ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                ui.visuals_mut().widgets.inactive.bg_fill = Color32::TRANSPARENT;
                ui.visuals_mut().widgets.inactive.bg_stroke =
                    Stroke::new(1.0, rgb(color::LIGHT_BORDER));
                let title_width = (ui.available_width() - 90.0).max(1.0);
                let origin = ui.max_rect().min;
                let display_title = if title.trim().is_empty() {
                    "Terminal session"
                } else {
                    title.as_str()
                };
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(egui::Rect::from_min_size(
                            origin,
                            egui::vec2(title_width, 36.0),
                        ))
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                    |ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.add(
                            egui::Label::new(
                                RichText::new(display_title)
                                    .size(shell::type_size::PAGE_TITLE)
                                    .strong(),
                            )
                            .truncate(),
                        )
                        .on_hover_text(display_title);
                        ui.label(
                            RichText::new(if conversation::fixture_enabled() {
                                conversation::FIXTURE_LABEL
                            } else if self.config.chat_mode_enabled() {
                                "Chat · live shell"
                            } else {
                                "Block · live terminal"
                            })
                            .size(9.0)
                            .color(rgb(color::CANVAS_MUTED)),
                        );
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
    }
}
