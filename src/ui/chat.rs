use super::*;

/// A command bubble and a result surface are separate geometry, even though
/// their bytes still belong to one semantic lifecycle and one persistent PTY.
#[derive(Clone, Copy)]
struct ChatCard {
    rect: egui::Rect,
    command: bool,
    composer: bool,
    top_closed: bool,
    bottom_closed: bool,
}

/// Finished neutral output belongs to the shared canvas rather than another
/// outlined bubble. Keep unresolved/failed, selected and live surfaces visible.
fn paint_reference_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    kind: jterm_core::reference_icons::Icon,
    tint: Color32,
) {
    let scale = rect.width().min(rect.height()) / 24.0;
    let stroke = egui::Stroke::new(1.5 * scale, tint);
    for path in kind.paths() {
        let points: Vec<_> = path
            .iter()
            .map(|&(x, y)| rect.min + egui::vec2(x, y) * scale)
            .collect();
        painter.add(egui::Shape::line(points.clone(), stroke));
        if let (Some(first), Some(last)) = (points.first(), points.last()) {
            painter.circle_filled(*first, stroke.width * 0.5, tint);
            painter.circle_filled(*last, stroke.width * 0.5, tint);
        }
    }
}

/// Shared native tool slots for editable prompts and non-editable foreground
/// chrome. This factory creates no text editor and never writes terminal bytes.
fn reference_chrome_width_scale(ui: &Ui) -> f32 {
    // The context owns UI typography; a transcript's local Main font cannot
    // change chrome slots or the native input/PTY row budget.
    (egui::TextStyle::Body.resolve(&ui.ctx().global_style()).size
        / crate::config::DEFAULT_FONT_SIZE)
        .max(1.0)
}

fn draw_reference_composer_tools(ui: &mut Ui, rect: egui::Rect, interactive: bool, owner: usize) {
    use jterm_core::{
        reference_icons::Icon,
        reference_shell::{self as shell, color},
    };
    let rgb = |[r, g, b]: [u8; 3]| Color32::from_rgb(r, g, b);
    let popup_color = |[r, g, b, a]: [u8; 4]| Color32::from_rgba_unmultiplied(r, g, b, a);
    let width_scale = reference_chrome_width_scale(ui);
    let composer_width = rect.width()
        + shell::COMPOSER_CONTROL_LEFT
        + shell::COMPOSER_CONTROL_RIGHT
        + shell::COMPOSER_SEND_WIDTH
        + shell::METADATA_GAP;
    let normalized_width = composer_width / width_scale;
    ui.scope_builder(
        egui::UiBuilder::new()
            .id_salt(("reference-composer-tools", owner))
            .max_rect(rect),
        |ui| {
            ui.set_clip_rect(rect.intersect(ui.clip_rect()));
            ui.spacing_mut().interact_size.y = shell::CONTROL_HEIGHT;
            *ui.visuals_mut() = egui::Visuals::light();
            ui.visuals_mut().disabled_alpha = 1.0;
            let widgets = &mut ui.visuals_mut().widgets;
            for visuals in [&mut widgets.inactive, &mut widgets.noninteractive] {
                visuals.bg_fill = rgb(color::CONTROL_SURFACE);
                visuals.weak_bg_fill = rgb(color::CONTROL_SURFACE);
            }
            for visuals in [&mut widgets.hovered, &mut widgets.active] {
                visuals.bg_fill = rgb(color::ACCENT_PALE);
                visuals.weak_bg_fill = rgb(color::ACCENT_PALE);
            }
            let model_width = if normalized_width >= shell::COMPOSER_MODEL_MIN_AVAILABLE {
                shell::COMPOSER_MODEL_MIN_WIDTH * width_scale
            } else {
                0.0
            };
            let tools_right = rect.right()
                - model_width
                - if model_width > 0.0 {
                    shell::METADATA_GAP
                } else {
                    0.0
                };
            let mut x = rect.left();
            for (kind, label, width, available) in [
                (Icon::Plus, "", shell::COMPOSER_PLUS_WIDTH, false),
                (Icon::Attach, "Attach", shell::COMPOSER_ATTACH_WIDTH, false),
                (Icon::Image, "Image", shell::COMPOSER_IMAGE_WIDTH, false),
                (Icon::Code, "Code", shell::COMPOSER_CODE_WIDTH, false),
                (
                    Icon::Terminal,
                    "Terminal",
                    shell::COMPOSER_TERMINAL_WIDTH,
                    true,
                ),
                (Icon::Grid, "Tools", shell::COMPOSER_TOOLS_WIDTH, true),
            ] {
                if matches!(label, "Attach" | "Image" | "Code")
                    && normalized_width < shell::COMPOSER_SECONDARY_MIN_WIDTH
                {
                    continue;
                }
                if label == "Terminal" && normalized_width < shell::COMPOSER_TERMINAL_MIN_AVAILABLE
                {
                    continue;
                }
                let width = width * width_scale;
                if x + width > tools_right {
                    continue;
                }
                let bounds = egui::Rect::from_min_size(
                    egui::pos2(x, rect.top()),
                    egui::vec2(width, shell::CONTROL_HEIGHT),
                );
                let enabled = interactive && available;
                let response = ui
                    .push_id(("reference-footer-tool", label), |ui| {
                        ui.add_enabled_ui(enabled, |ui| {
                            ui.put(
                                bounds,
                                egui::Button::new(
                                    egui::RichText::new("").size(shell::type_size::CONTROL),
                                )
                                .stroke(egui::Stroke::new(1.0, rgb(color::CONTROL_BORDER)))
                                .corner_radius(7),
                            )
                        })
                        .inner
                    })
                    .inner;
                let tint = rgb(if enabled {
                    color::CANVAS_TEXT
                } else {
                    color::DISABLED_TEXT
                });
                if enabled && response.has_focus() {
                    ui.painter().rect_stroke(
                        bounds,
                        7,
                        egui::Stroke::new(1.0, rgb(color::ACCENT)),
                        egui::StrokeKind::Inside,
                    );
                }
                paint_reference_icon(
                    ui.painter(),
                    egui::Rect::from_center_size(
                        egui::pos2(
                            if label.is_empty() {
                                bounds.center().x
                            } else {
                                x + 12.0
                            },
                            bounds.center().y,
                        ),
                        egui::vec2(12.0, 12.0),
                    ),
                    kind,
                    tint,
                );
                if !label.is_empty() {
                    ui.painter().text(
                        egui::pos2(x + 22.0, bounds.center().y),
                        egui::Align2::LEFT_CENTER,
                        label,
                        FontId::proportional(shell::type_size::CONTROL),
                        tint,
                    );
                }
                if label == "Tools" {
                    paint_reference_icon(
                        ui.painter(),
                        egui::Rect::from_center_size(
                            egui::pos2(bounds.right() - 12.0, bounds.center().y),
                            egui::vec2(12.0, 12.0),
                        ),
                        Icon::ChevronDown,
                        tint,
                    );
                }
                response
                    .clone()
                    .on_hover_text(CHAT_FOOTER_HINT)
                    .on_disabled_hover_text(if interactive {
                        "This attachment action is unavailable"
                    } else {
                        "Input belongs to the foreground program; message tools are unavailable"
                    });
                if label == "Tools" && interactive {
                    let popup_id = egui::Popup::default_response_id(&response);
                    let focused = response.has_focus();
                    ui.ctx().data_mut(|data| {
                        data.insert_temp(egui::Id::new("block-action-popup"), popup_id);
                        if focused {
                            data.insert_temp(egui::Id::new("block-action-focus"), response.id);
                        }
                    });
                    let visible = egui::Popup::menu(&response)
                        .width(jterm_core::reference_popup::WIDTH)
                        .frame(
                            egui::Frame::NONE
                                .fill(popup_color(jterm_core::reference_popup::SURFACE))
                                .stroke(egui::Stroke::new(
                                    1.0,
                                    popup_color(jterm_core::reference_popup::BORDER),
                                ))
                                .corner_radius(jterm_core::reference_popup::RADIUS as u8)
                                .inner_margin(jterm_core::reference_popup::PADDING),
                        )
                        .show(|ui| {
                            use jterm_core::reference_popup::{self as popup, ToolCommandId};
                            *ui.visuals_mut() = egui::Visuals::dark();
                            ui.visuals_mut().disabled_alpha = 1.0;
                            ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                            ui.set_width(popup::WIDTH - 2.0 * popup::PADDING - 2.0);
                            for command in popup::COMMANDS {
                                if command.separator_before {
                                    let (line, _) = ui.allocate_exact_size(
                                        egui::vec2(ui.available_width(), 9.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().hline(
                                        line.x_range(),
                                        line.center().y,
                                        egui::Stroke::new(1.0, popup_color(popup::BORDER)),
                                    );
                                }
                                let item = ui
                                    .push_id(command.title, |ui| {
                                        ui.add_enabled_ui(!command.placeholder, |ui| {
                                            ui.add_sized(
                                                [ui.available_width(), popup::ROW_HEIGHT],
                                                egui::Button::new("")
                                                    .fill(Color32::TRANSPARENT)
                                                    .stroke(egui::Stroke::NONE)
                                                    .corner_radius(6),
                                            )
                                        })
                                        .inner
                                    })
                                    .inner;
                                item.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::Button,
                                        !command.placeholder,
                                        command.title,
                                    )
                                });
                                if item.hovered() || item.has_focus() {
                                    ui.painter().rect_filled(
                                        item.rect,
                                        6,
                                        popup_color(popup::SELECTED),
                                    );
                                }
                                let tint = if command.placeholder {
                                    popup_color(popup::MUTED)
                                } else {
                                    popup_color(popup::FOREGROUND)
                                };
                                paint_reference_icon(
                                    ui.painter(),
                                    egui::Rect::from_center_size(
                                        item.rect.left_center() + egui::vec2(12.0, 0.0),
                                        egui::vec2(popup::ICON_SIZE, popup::ICON_SIZE),
                                    ),
                                    Icon::from_key(command.icon_key),
                                    tint,
                                );
                                ui.painter().text(
                                    item.rect.left_center() + egui::vec2(28.0, 0.0),
                                    egui::Align2::LEFT_CENTER,
                                    command.title,
                                    FontId::proportional(10.0),
                                    tint,
                                );
                                ui.painter().text(
                                    item.rect.right_center() - egui::vec2(7.0, 0.0),
                                    egui::Align2::RIGHT_CENTER,
                                    command.description,
                                    FontId::proportional(9.0),
                                    popup_color(popup::MUTED),
                                );
                                if item.clicked() {
                                    let intent = match command.id {
                                        ToolCommandId::Terminal => "terminal",
                                        ToolCommandId::CommandPalette => "launcher",
                                        ToolCommandId::SearchCanvas => "search",
                                        ToolCommandId::Attach
                                        | ToolCommandId::Image
                                        | ToolCommandId::Code => continue,
                                    };
                                    ui.ctx().data_mut(|data| {
                                        data.insert_temp(
                                            egui::Id::new("reference-tool-request"),
                                            intent.to_owned(),
                                        )
                                    });
                                    ui.close();
                                }
                            }
                        })
                        .is_some();
                    remember_reference_chrome_scope(
                        ui.ctx(),
                        ui.id().with("reference-tools-keyboard-scope"),
                        response.rect,
                        visible,
                    );
                }
                x += width + shell::METADATA_GAP;
            }
            if model_width > 0.0 {
                let bounds = egui::Rect::from_min_max(
                    egui::pos2(rect.right() - model_width, rect.top()),
                    rect.max,
                );
                ui.push_id("reference-model-unavailable", |ui| {
                    ui.add_enabled_ui(false, |ui| {
                        ui.put(
                            bounds,
                            egui::Button::new(
                                egui::RichText::new("").size(shell::type_size::CONTROL),
                            )
                            .fill(rgb(color::CONTROL_SURFACE))
                            .stroke(egui::Stroke::new(1.0, rgb(color::CONTROL_BORDER)))
                            .corner_radius(7),
                        )
                    })
                    .inner
                })
                .inner
                .on_disabled_hover_text("No model is attached to this terminal input owner");
                ui.painter().with_clip_rect(bounds).text(
                    bounds.left_center() + egui::vec2(8.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    shell::MODEL_UNAVAILABLE_LABEL,
                    FontId::proportional(shell::type_size::CONTROL),
                    rgb(color::DISABLED_TEXT),
                );
                paint_reference_icon(
                    ui.painter(),
                    egui::Rect::from_center_size(
                        bounds.right_center() - egui::vec2(12.0, 0.0),
                        egui::vec2(12.0, 12.0),
                    ),
                    Icon::ChevronDown,
                    rgb(color::DISABLED_TEXT),
                );
            }
        },
    );
}

fn chat_output_is_quiet(
    command: bool,
    composer: bool,
    selected: bool,
    active: bool,
    successful: bool,
) -> bool {
    !command && !composer && !selected && !active && successful
}

const CHAT_FOOTER_HINT: &str = "Shell  ·  Tab to complete  ·  Ctrl+C to interrupt";

/// Observe only the visible hint, never the Run control or terminal input.
/// A clipped keyboard hint must remain discoverable in narrow panes.
fn draw_chat_footer_hint(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: egui::Id,
    font_size: f32,
    color: egui::Color32,
) {
    let clip = rect.intersect(ui.clip_rect());
    if !clip.is_positive() {
        return;
    }
    ui.painter().with_clip_rect(clip).text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        CHAT_FOOTER_HINT,
        egui::FontId::proportional(font_size),
        color,
    );
    ui.interact(clip, id, egui::Sense::hover())
        .on_hover_text(CHAT_FOOTER_HINT);
}

// Paint-only contract shared with the native GTK Chat surfaces. Never inset
// text or alter projected rows to make a capsule fit. Both paint passes use it.
fn chat_card_rounding(card: ChatCard) -> egui::CornerRadius {
    let radius: f32 = if card.composer {
        jterm_core::reference_shell::COMPOSER_RADIUS
    } else {
        jterm_core::reference_shell::CARD_RADIUS
    };
    let limit = (card.rect.width().min(card.rect.height()) * 0.5).max(0.0);
    let rounded = radius.min(limit) as u8;
    let tail = 5.0_f32.min(limit) as u8;
    egui::CornerRadius {
        nw: if !card.top_closed {
            0
        } else if !card.command && !card.composer {
            tail
        } else {
            rounded
        },
        ne: if card.top_closed { rounded } else { 0 },
        sw: if card.bottom_closed { rounded } else { 0 },
        se: if !card.bottom_closed {
            0
        } else if card.command {
            tail
        } else {
            rounded
        },
    }
}

// The dock can be adjacent on screen while skipping history document rows.
// Only a contiguous following history row proves the command's real bottom.
fn chat_history_edge_closed(
    end: usize,
    history_end: usize,
    previous: Option<usize>,
    next: Option<usize>,
) -> bool {
    end > 0
        && end < history_end
        && previous
            .zip(next)
            .is_some_and(|(a, b)| a.checked_add(1) == Some(b))
}

fn row_rect(content: egui::Rect, rows: std::ops::Range<usize>, line_height: f32) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(
            content.left(),
            content.top() + rows.start as f32 * line_height,
        ),
        egui::pos2(
            content.right(),
            content.top() + rows.end as f32 * line_height,
        ),
    )
    .intersect(content)
}

impl TerminalRenderer {
    fn canvas_chat_row_rect(
        &self,
        content: egui::Rect,
        rows: std::ops::Range<usize>,
    ) -> egui::Rect {
        let row = rows.start;
        let extended = egui::Rect::from_min_max(
            content.min,
            content.max
                + egui::vec2(
                    0.0,
                    if self.canvas_geometry.margin {
                        self.line_height
                    } else {
                        0.0
                    },
                ),
        );
        let rect = row_rect(extended, rows, self.line_height).translate(egui::vec2(
            0.0,
            -self.canvas_geometry.row_offset(row, self.line_height),
        ));
        let mut clip = content;
        if let Some(start) = self.canvas_geometry.composer_start {
            let top = content.top() + start as f32 * self.line_height
                - self.canvas_geometry.row_offset(start, self.line_height);
            if row < start {
                clip.max.y = clip.max.y.min(top);
            } else {
                clip.min.y = clip.min.y.max(top);
            }
        }
        rect.intersect(clip)
    }

    pub(crate) fn shared_composer_rect(
        &self,
        content: egui::Rect,
        rows: std::ops::Range<usize>,
    ) -> egui::Rect {
        let native = self.canvas_chat_row_rect(content, rows);
        // Standalone/native renderer users have the same dock contract even
        // when no application panel has supplied the optional outer bounds.
        let outer = self.workspace_outer_rect.unwrap_or_else(|| {
            egui::Rect::from_min_max(
                content.min - egui::vec2(self.padding + self.block_gutter_width(), self.padding),
                content.max
                    + egui::vec2(
                        self.padding + Self::SCROLLBAR_WIDTH + Self::SCROLLBAR_GAP,
                        self.padding,
                    ),
            )
        });
        let top = (outer.bottom() - jterm_core::reference_shell::COMPOSER_HEIGHT)
            .min(native.top() - 6.0)
            .max(outer.top());
        egui::Rect::from_min_max(egui::pos2(outer.left(), top), outer.max)
    }

    fn composer_footer_rect(
        &self,
        content: egui::Rect,
        rows: std::ops::Range<usize>,
    ) -> egui::Rect {
        let first_footer = rows
            .end
            .saturating_sub((28.0 / self.line_height).ceil() as usize)
            .max(rows.start);
        let native_footer = self.canvas_chat_row_rect(content, first_footer..rows.end);
        let frame = self.shared_composer_rect(content, rows);
        egui::Rect::from_min_max(
            egui::pos2(
                frame.left() + jterm_core::reference_shell::COMPOSER_CONTROL_LEFT,
                (frame.bottom() - 36.0).max(native_footer.top()),
            ),
            frame.max
                - egui::vec2(
                    jterm_core::reference_shell::COMPOSER_CONTROL_RIGHT,
                    jterm_core::reference_shell::COMPOSER_CONTROL_BOTTOM,
                ),
        )
    }

    fn chat_cards(
        &self,
        terminal: &TerminalState,
        viewport: &ProjectedViewport,
        entry: &BlockChromeEntry,
        content: egui::Rect,
    ) -> Vec<ChatCard> {
        let mut cards = Vec::with_capacity(2);
        let Some(record) = terminal.command_records().get(entry.span.record_index) else {
            return cards;
        };
        if entry.live {
            if let Some(rows) = viewport.chat_composer_rows() {
                cards.push(ChatCard {
                    rect: self.shared_composer_rect(content, rows),
                    command: false,
                    composer: true,
                    top_closed: true,
                    bottom_closed: true,
                });
                return cards;
            }
        }
        let history_end = viewport
            .chat_composer_rows()
            .map_or(viewport.rows(), |rows| rows.start);
        let command = viewport
            .chat_command_spans()
            .iter()
            .find(|command| command.sequence == record.sequence);
        let mut result_start =
            entry.span.first_row + usize::from(entry.metadata_row == Some(entry.span.first_row));
        if let Some(command) = command.filter(|_| self.chat_command_bubbles) {
            let mut rect = self.canvas_chat_row_rect(content, command.rows.clone());
            rect.min.x = (content.left() + command.column as f32 * self.char_width
                - self.char_width * 0.8)
                .max(content.left());
            // Padding is drawn in proven synthetic/blank space, never by
            // translating the terminal text independently of its origins.
            rect.min.y = (rect.top() - self.line_height * 0.12).max(content.top());
            rect.max.y = (rect.bottom() + self.line_height * 0.12).min(content.bottom());
            cards.push(ChatCard {
                rect,
                command: true,
                composer: false,
                top_closed: entry.span.starts_in_viewport
                    && command.rows.start as f32 * self.line_height
                        >= self.canvas_geometry.translation,
                // A clipped command must not acquire a tail at the dock boundary.
                bottom_closed: chat_history_edge_closed(
                    command.rows.end,
                    history_end,
                    command
                        .rows
                        .end
                        .checked_sub(1)
                        .and_then(|row| viewport.view_document_row(row)),
                    viewport.view_document_row(command.rows.end),
                ),
            });
            result_start = command.rows.end;
            if matches!(
                viewport.row_kind(result_start),
                Some(crate::terminal::ProjectedRowKind::BlockSpacer { .. })
            ) {
                result_start += 1;
            }
        }
        if result_start <= entry.span.last_row {
            cards.push(ChatCard {
                rect: self.canvas_chat_row_rect(content, result_start..entry.span.last_row + 1),
                command: false,
                composer: false,
                top_closed: (command.is_some() || entry.span.starts_in_viewport)
                    && result_start as f32 * self.line_height >= self.canvas_geometry.translation,
                // Be conservative where the dock skips offscreen history.
                bottom_closed: entry.span.ends_in_viewport && entry.span.last_row + 1 < history_end,
            });
        }
        for card in &mut cards {
            if !card.command {
                card.rect.min.x -= self.char_width.min(8.0);
            }
        }
        cards
    }

    pub(super) fn chat_preedit_clip(
        &self,
        terminal: &TerminalState,
        viewport: &ProjectedViewport,
        entries: &[BlockChromeEntry],
        content: egui::Rect,
        visible: egui::Rect,
        caret: egui::Rect,
    ) -> Option<egui::Rect> {
        if let Some(rows) = viewport.chat_composer_rows() {
            let frame = self.shared_composer_rect(content, rows.clone());
            if frame.contains(caret.center()) {
                let mut clip = frame.intersect(visible);
                clip.max.y = clip
                    .max
                    .y
                    .min(self.composer_footer_rect(content, rows).top());
                return (clip.is_positive() && clip.intersects(caret)).then_some(clip);
            }
        }
        let mut has_cards = false;
        for entry in entries {
            for card in self.chat_cards(terminal, viewport, entry, content) {
                has_cards = true;
                if card.command || !card.rect.contains(caret.center()) {
                    continue;
                }
                let mut clip = card.rect.intersect(visible);
                if card.composer {
                    if let Some(rows) = viewport.chat_composer_rows() {
                        clip.max.y = clip
                            .max
                            .y
                            .min(self.composer_footer_rect(content, rows).top());
                    }
                }
                return (clip.is_positive() && clip.intersects(caret)).then_some(clip);
            }
        }
        // A native surface without shell markers still owns its actual caret.
        // A known command/header card is never a substitute for an input body.
        (!has_cards && visible.intersects(caret)).then_some(visible)
    }

    fn chat_card_fill(&self, entry: &BlockChromeEntry, card: ChatCard) -> Color32 {
        if card.composer {
            let [r, g, b] = jterm_core::reference_shell::color::COMPOSER;
            return Color32::from_rgba_unmultiplied(
                r,
                g,
                b,
                (255.0 * self.opacity.clamp(0.0, 1.0)) as u8,
            );
        }
        if !card.command && !card.composer && !entry.selected && !entry.active {
            let [r, g, b] = jterm_core::reference_shell::color::TERMINAL;
            return Color32::from_rgba_unmultiplied(
                r,
                g,
                b,
                (255.0 * self.opacity.clamp(0.0, 1.0)) as u8,
            );
        }
        if chat_output_is_quiet(
            card.command,
            card.composer,
            entry.selected,
            entry.active,
            matches!(entry.outcome, crate::block_mode::BlockOutcome::Success),
        ) {
            return Color32::TRANSPARENT;
        }
        let color = if card.command {
            self.block_accent_color()
        } else {
            self.theme.terminal_foreground()
        };
        let alpha = if card.command {
            62
        } else if card.composer {
            15
        } else if entry.selected {
            24
        } else {
            11
        };
        Color32::from_rgba_unmultiplied(
            color.r(),
            color.g(),
            color.b(),
            (alpha as f32 * self.opacity.clamp(0.0, 1.0)) as u8,
        )
    }

    pub(super) fn draw_chat_backgrounds(
        &self,
        painter: &egui::Painter,
        terminal: &TerminalState,
        viewport: &ProjectedViewport,
        entries: &[BlockChromeEntry],
        content: egui::Rect,
        base_bg: Color32,
    ) -> Vec<Option<Color32>> {
        let mut backdrops = vec![None; viewport.rows()];
        for entry in entries {
            for card in self.chat_cards(terminal, viewport, entry, content) {
                if !card.rect.is_positive() {
                    continue;
                }
                let radius = chat_card_rounding(card);
                let fill = self.chat_card_fill(entry, card);
                painter.rect_filled(card.rect, radius, fill);
                let top = ((card.rect.top() - content.top()) / self.line_height)
                    .ceil()
                    .max(0.0) as usize;
                let bottom = ((card.rect.bottom() - content.top()) / self.line_height)
                    .ceil()
                    .max(0.0) as usize;
                for row in backdrops.iter_mut().take(bottom).skip(top) {
                    *row = Some(composite_over_opaque(base_bg, fill));
                }
            }
        }
        backdrops
    }

    pub(super) fn draw_chat_foreground(
        &self,
        painter: &egui::Painter,
        terminal: &TerminalState,
        viewport: &ProjectedViewport,
        entries: &[BlockChromeEntry],
        content: egui::Rect,
    ) {
        for entry in entries {
            for card in self.chat_cards(terminal, viewport, entry, content) {
                if !card.rect.is_positive() {
                    continue;
                }
                if chat_output_is_quiet(
                    card.command,
                    card.composer,
                    entry.selected,
                    entry.active,
                    matches!(entry.outcome, crate::block_mode::BlockOutcome::Success),
                ) {
                    continue;
                }
                let color = if card.composer || entry.active {
                    self.block_accent_color()
                } else {
                    self.theme.terminal_foreground()
                };
                let alpha = if card.composer {
                    95
                } else if entry.selected {
                    100
                } else {
                    25
                };
                let radius = chat_card_rounding(card);
                painter.rect_stroke(
                    card.rect,
                    radius,
                    egui::Stroke::new(
                        1.0,
                        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha),
                    ),
                    egui::StrokeKind::Inside,
                );
            }
            let Some(record) = terminal.command_records().get(entry.span.record_index) else {
                continue;
            };
            let Some(command) = viewport
                .chat_command_spans()
                .iter()
                .find(|command| command.sequence == record.sequence)
            else {
                continue;
            };
            let row = command.rows.end;
            if matches!(
                viewport.row_kind(row),
                Some(crate::terminal::ProjectedRowKind::BlockSpacer { .. })
            ) {
                let header = self
                    .canvas_chat_row_rect(content, row..row + 1)
                    .shrink2(egui::vec2(6.0, 1.0));
                let avatar = egui::Rect::from_min_size(header.min, egui::vec2(15.0, 15.0));
                painter.rect_filled(avatar, 4, self.block_accent_color());
                paint_reference_icon(
                    painter,
                    avatar.shrink(3.0),
                    jterm_core::reference_icons::Icon::Terminal,
                    Color32::WHITE,
                );
                painter.text(
                    egui::pos2(header.left() + 21.0, header.center().y),
                    egui::Align2::LEFT_CENTER,
                    "Shell · Terminal output",
                    FontId::proportional(10.0),
                    self.theme.terminal_foreground(),
                );
                if let Some(status) = crate::block_mode::badge_text_with_lifecycle(
                    entry.outcome,
                    entry.duration_ms,
                    entry.start_mark_seen,
                    entry.completion_provenance,
                ) {
                    let rect = self
                        .canvas_chat_row_rect(content, row..row + 1)
                        .shrink2(egui::vec2(6.0, 0.0));
                    painter.with_clip_rect(rect).text(
                        rect.right_center(),
                        egui::Align2::RIGHT_CENTER,
                        block_badge_display_text(&status),
                        FontId::proportional(self.font_size),
                        self.block_outcome_color(entry.outcome),
                    );
                }
            }
        }
    }
}

impl TerminalRenderer {
    pub(super) fn draw_reference_foreground_footer(
        &self,
        ui: &mut egui::Ui,
        terminal: &TerminalState,
    ) {
        let Some(rect) = self.foreground_footer_rect else {
            return;
        };
        use jterm_core::{
            reference_icons::Icon,
            reference_shell::{self as shell, color},
        };
        let rgb = |[r, g, b]: [u8; 3]| Color32::from_rgb(r, g, b);
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        painter.rect_filled(rect, shell::COMPOSER_RADIUS, rgb(color::COMPOSER));
        painter.rect_stroke(
            rect,
            shell::COMPOSER_RADIUS,
            egui::Stroke::new(1.0, rgb(color::LIGHT_BORDER)),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.min + egui::vec2(12.0, 18.0),
            egui::Align2::LEFT_CENTER,
            "Input goes to the foreground program",
            FontId::proportional(10.0),
            rgb(color::CANVAS_MUTED),
        );
        let y = rect.bottom() - shell::COMPOSER_CONTROL_BOTTOM - shell::CONTROL_HEIGHT;
        let controls = egui::Rect::from_min_max(
            egui::pos2(rect.left() + shell::COMPOSER_CONTROL_LEFT, y),
            egui::pos2(
                rect.right()
                    - shell::COMPOSER_CONTROL_RIGHT
                    - shell::COMPOSER_SEND_WIDTH
                    - shell::METADATA_GAP,
                rect.bottom() - shell::COMPOSER_CONTROL_BOTTOM,
            ),
        );
        draw_reference_composer_tools(
            ui,
            controls,
            false,
            terminal as *const TerminalState as usize,
        );
        let can_interrupt = terminal.command_records().back().is_some_and(|record| {
            crate::terminal::ChatInputAction::Interrupt {
                sequence: record.sequence,
            }
            .is_current(terminal)
        });
        if !can_interrupt {
            let stop = egui::Rect::from_min_size(
                egui::pos2(
                    rect.right() - shell::COMPOSER_CONTROL_RIGHT - shell::COMPOSER_SEND_WIDTH,
                    y,
                ),
                egui::vec2(shell::COMPOSER_SEND_WIDTH, shell::CONTROL_HEIGHT),
            );
            painter.rect_stroke(
                stop,
                7,
                egui::Stroke::new(1.0, rgb(color::LIGHT_BORDER)),
                egui::StrokeKind::Inside,
            );
            paint_reference_icon(
                &painter,
                egui::Rect::from_center_size(stop.center(), egui::vec2(12.0, 12.0)),
                Icon::Close,
                rgb(color::CANVAS_MUTED),
            );
            ui.interact(stop,ui.id().with("foreground-unavailable-stop"),egui::Sense::hover()).on_hover_text("Stop is unavailable for this input owner; use the foreground program's native keys (Ctrl+C where supported)");
        }
    }

    pub(super) fn draw_chat_controls(
        &mut self,
        ui: &mut Ui,
        terminal: &TerminalState,
        viewport: &ProjectedViewport,
        content: egui::Rect,
        enabled: bool,
    ) -> bool {
        use crate::terminal::ChatInputAction;
        self.chat_control_rect = None;
        let enabled = enabled && self.chat_controls_enabled;
        if !enabled {
            // Once interrupted, the old physical press cannot revive merely
            // because Enter is released before the mouse button is released.
            self.chat_pressed_action = None;
        }
        let Some(record) = terminal.command_records().back() else {
            return false;
        };
        let surface = terminal.chat_surface();
        let action_and_rect = if let Some(composer) = viewport.chat_composer_rows() {
            let row = composer.end - 1;
            if !matches!(
                viewport.row_kind(row),
                Some(crate::terminal::ProjectedRowKind::CompanionStatus { .. })
            ) {
                return false;
            }
            let footer = self.composer_footer_rect(content, composer);
            let button_width = jterm_core::reference_shell::COMPOSER_SEND_WIDTH.min(footer.width());
            let button = egui::Rect::from_min_max(
                egui::pos2(footer.right() - button_width, footer.top()),
                footer.max,
            );
            let label_rect = egui::Rect::from_min_max(
                footer.min,
                egui::pos2(
                    button.left() - jterm_core::reference_shell::METADATA_GAP,
                    footer.bottom(),
                ),
            );
            if label_rect.width()
                < jterm_core::reference_shell::COMPOSER_TOOLS_WIDTH
                    * reference_chrome_width_scale(ui)
            {
                draw_chat_footer_hint(
                    ui,
                    label_rect,
                    egui::Id::new(("chat-footer-hint", record.sequence)),
                    self.font_size.min(10.0),
                    self.theme.terminal_foreground().gamma_multiply(0.5),
                );
            } else {
                draw_reference_composer_tools(
                    ui,
                    label_rect,
                    true,
                    terminal as *const TerminalState as usize,
                );
            }
            Some((
                ChatInputAction::Submit {
                    sequence: record.sequence,
                },
                button,
                "↑",
                "Send Enter immediately when the shell and writer are ready. If unavailable, use the native keyboard; enhanced multi-byte button encodings are unsupported.",
            ))
        } else if let (jterm_core::chat_mode::ChatSurface::Running, Some(footer)) =
            (surface, self.foreground_footer_rect)
        {
            if !(ChatInputAction::Interrupt {
                sequence: record.sequence,
            })
            .is_current(terminal)
            {
                self.chat_pressed_action = None;
                return false;
            }
            let chrome = jterm_core::reference_shell::COMPOSER_CONTROL_RIGHT;
            let width = jterm_core::reference_shell::COMPOSER_SEND_WIDTH;
            let height = jterm_core::reference_shell::CONTROL_HEIGHT;
            let bottom = jterm_core::reference_shell::COMPOSER_CONTROL_BOTTOM;
            Some((ChatInputAction::Interrupt { sequence:record.sequence },egui::Rect::from_min_size(egui::pos2(footer.right()-chrome-width,footer.bottom()-bottom-height),egui::vec2(width,height)),"Stop","Send Ctrl+C to the current foreground command. Native ownership and IME guards are revalidated before writing."))
        } else if surface == jterm_core::chat_mode::ChatSurface::Running {
            viewport.chat_command_spans().iter().find(|command| command.sequence == record.sequence)
                .filter(|command| matches!(viewport.row_kind(command.rows.end), Some(crate::terminal::ProjectedRowKind::BlockSpacer { .. })))
                .map(|command| {
                    let row = self.canvas_chat_row_rect(content, command.rows.end..command.rows.end + 1).shrink2(egui::vec2(4.0, 1.0));
                    let label_width = ui.painter().layout_no_wrap("Interrupt".into(), FontId::proportional(self.font_size), self.theme.terminal_foreground()).size().x + 12.0;
                    let button = egui::Rect::from_min_size(row.min, egui::vec2((self.font_size * 6.0).max(label_width).min(row.width()), row.height()));
                    (ChatInputAction::Interrupt { sequence: record.sequence }, button, "Interrupt", "Send Ctrl+C to this terminal. Status changes only when the shell reports completion.")
                })
        } else {
            None
        };
        let Some((action, rect, label, tooltip)) = action_and_rect else {
            return false;
        };
        if !rect.is_positive() {
            return false;
        }
        // A manual max_rect does not constrain egui's Button minimum size or
        // wrapping. Hide an unreadable control in a tiny pane; the native
        // keyboard remains available. Clip both paint and hit testing to the
        // same footer/header rectangle without changing any global styles.
        let icon_control = matches!(action, ChatInputAction::Submit { .. })
            || self.foreground_footer_rect.is_some();
        let label_size = if icon_control {
            egui::vec2(12.0, 12.0)
        } else {
            ui.painter()
                .layout_no_wrap(
                    label.to_owned(),
                    FontId::proportional(self.font_size),
                    self.theme.terminal_foreground(),
                )
                .size()
        };
        if rect.width() < label_size.x + 12.0 || rect.height() < label_size.y + 2.0 {
            if self
                .chat_pressed_action
                .is_some_and(|(owner, _, _)| owner == terminal as *const TerminalState as usize)
            {
                self.chat_pressed_action = None;
            }
            return false;
        }
        let native_owner = self.chat_native_owner.filter(|(shell, foreground)| {
            *foreground > 0
                && (!matches!(action, ChatInputAction::Submit { .. }) || shell == foreground)
        });
        let supported_encoding = self.chat_action_bytes(ui.ctx(), terminal, action).len() == 1;
        let can_send =
            enabled && action.is_current(terminal) && native_owner.is_some() && supported_encoding;
        let response = ui
            .push_id(
                (
                    "chat-control",
                    terminal as *const TerminalState as usize,
                    record.sequence,
                ),
                |ui| {
                    ui.add_enabled_ui(can_send, |ui| {
                        ui.set_clip_rect(ui.clip_rect().intersect(rect));
                        ui.spacing_mut().interact_size.y = rect.height();
                        ui.spacing_mut().button_padding.y = 0.0;
                        ui.put(
                            rect,
                            egui::Button::new(
                                egui::RichText::new(if icon_control { "" } else { label }).size(if icon_control { 12.0 } else { self.font_size }).color(egui::Color32::WHITE),
                            )
                            .small()
                            .truncate()
                            .fill(if icon_control {
                                let [r, g, b] = jterm_core::reference_shell::color::ACCENT;
                                egui::Color32::from_rgb(r, g, b)
                            } else { self.theme.terminal_background() })
                            .corner_radius(7),
                        )
                    })
                    .inner
                },
            )
            .inner
            .on_hover_text(tooltip)
            .on_disabled_hover_text("This optional control needs a current native foreground and a supported one-byte key. Use the normal keyboard while it is unavailable.");
        self.chat_control_rect = Some(rect);
        if icon_control {
            paint_reference_icon(
                ui.painter(),
                egui::Rect::from_center_size(rect.center(), egui::vec2(12.0, 12.0)),
                if matches!(action, ChatInputAction::Submit { .. }) {
                    jterm_core::reference_icons::Icon::Send
                } else {
                    jterm_core::reference_icons::Icon::Close
                },
                Color32::WHITE,
            );
        }
        let target = (
            terminal as *const TerminalState as usize,
            action,
            native_owner.map_or(0, |(_, foreground)| foreground),
        );
        let pressed = ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary));
        let released =
            ui.input(|input| input.pointer.button_released(egui::PointerButton::Primary));
        // egui also considers nearby widgets within its pointer aim radius.
        // An optional shell action must own the exact clipped physical point.
        let pointer_inside = ui
            .input(|input| input.pointer.interact_pos())
            .is_some_and(|point| rect.intersect(ui.clip_rect()).contains(point));
        if pressed {
            self.chat_pressed_action =
                (can_send && pointer_inside && response.contains_pointer()).then_some(target);
        }
        // Bind the whole pointer gesture, not just its release, to one shell
        // lifecycle. egui may otherwise transfer a click across a moved widget.
        let accepted = released
            && pointer_inside
            && self.chat_pressed_action == Some(target)
            && response.clicked_by(egui::PointerButton::Primary)
            && action.is_current(terminal);
        if released || !ui.input(|input| input.pointer.primary_down()) {
            self.chat_pressed_action = None;
        }
        // Keyboard Enter always uses native input; a focused button does not
        // separately activate from that same key event.
        if accepted {
            self.chat_input_action = Some(target);
        }
        accepted
    }

    pub fn chat_action_bytes(
        &self,
        ctx: &egui::Context,
        terminal: &TerminalState,
        action: crate::terminal::ChatInputAction,
    ) -> Vec<u8> {
        if !action.is_current(terminal) {
            return Vec::new();
        }
        let (key, modifiers) = match action {
            crate::terminal::ChatInputAction::Submit { .. } => {
                (egui::Key::Enter, egui::Modifiers::NONE)
            }
            crate::terminal::ChatInputAction::Interrupt { .. } => {
                (egui::Key::C, egui::Modifiers::CTRL)
            }
        };
        let mut bytes = Vec::new();
        self.handle_keyboard_input(
            ctx,
            &mut bytes,
            &std::collections::HashSet::new(),
            false,
            terminal.keyboard_enhancement_flags(),
            terminal.is_report_all_keys_enabled(),
            terminal.xterm_modify_other_keys(),
            terminal.xterm_format_other_keys(),
            terminal.is_application_cursor_keys(),
            terminal.is_alt_buffer_active(),
            terminal.is_meta_sends_escape(),
            false,
            &[egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
        );
        bytes
    }
}

/// Optional diagnostic export of the actual egui renderer, including its font
/// atlas. No HTML or replacement layout participates in the offscreen image.
#[cfg(test)]
#[allow(dead_code)] // Binary organism integration test owns this optional export.
pub(crate) fn export_native_frame(
    path: &std::path::Path,
    ctx: &egui::Context,
    mut output: egui::FullOutput,
    size: [usize; 2],
) {
    let textures: Vec<_> = output
        .textures_delta
        .set
        .iter()
        .flat_map(|(id, deltas)| {
            deltas.iter().map(move |delta| {
                let egui::ImageData::Color(image) = &delta.image;
                serde_json::json!({
                    "id": format!("{id:?}"), "size": image.size, "pos": delta.pos,
                    "pixels": image.pixels.iter().map(|pixel| pixel.to_array()).collect::<Vec<_>>()
                })
            })
        })
        .collect();
    let meshes: Vec<_> = ctx.tessellate(output.shapes, output.pixels_per_point).into_iter().map(|primitive| {
        let egui::epaint::Primitive::Mesh(mesh) = primitive.primitive else {
            panic!("offscreen CPU fixture unexpectedly produced a GPU callback");
        };
        serde_json::json!({
            "texture": format!("{:?}", mesh.texture_id),
            "clip": [primitive.clip_rect.left(), primitive.clip_rect.top(), primitive.clip_rect.right(), primitive.clip_rect.bottom()],
            "vertices": mesh.vertices.iter().map(|vertex| serde_json::json!({"position": [vertex.pos.x, vertex.pos.y], "uv": [vertex.uv.x, vertex.uv.y], "color": vertex.color.to_array()})).collect::<Vec<_>>(),
            "indices": mesh.indices,
        })
    }).collect();
    let data = serde_json::json!({"source": "Actual Ember TerminalRenderer and OrganismHost CPU paint paths; synthetic non-sensitive PTY fixture; offscreen layout test, not live GUI QA", "size": size, "pixels_per_point": output.pixels_per_point, "textures": textures, "meshes": meshes});
    std::fs::write(path, serde_json::to_vec(&data).unwrap()).unwrap();
    output.textures_delta.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{ChatInputAction, ProjectionPolicy, ProjectionViewState};

    #[test]
    fn reference_tools_keep_their_actual_painted_height_across_main_font_sizes() {
        use jterm_core::reference_shell::{self as shell, color};
        let ctx = egui::Context::default();
        for main_size in [8.0, 12.0, 14.0, 18.0, 28.0, 72.0] {
            let mut output = ctx.run_ui(Default::default(), |ui| {
                crate::theme::configure_workspace_text(ui.style_mut(), main_size);
                draw_reference_composer_tools(
                    ui,
                    egui::Rect::from_min_size(
                        egui::pos2(333.0, 657.0),
                        egui::vec2(643.0, shell::CONTROL_HEIGHT),
                    ),
                    true,
                    0,
                );
            });
            fn collect(shape: &egui::Shape, rects: &mut Vec<egui::Rect>) {
                match shape {
                    egui::Shape::Vec(shapes) => {
                        for shape in shapes {
                            collect(shape, rects);
                        }
                    }
                    egui::Shape::Rect(rect)
                        if rect.fill
                            == Color32::from_rgb(
                                color::CONTROL_SURFACE[0],
                                color::CONTROL_SURFACE[1],
                                color::CONTROL_SURFACE[2],
                            ) =>
                    {
                        rects.push(rect.rect)
                    }
                    _ => {}
                }
            }
            let mut rects = Vec::new();
            for shape in &output.shapes {
                collect(&shape.shape, &mut rects);
            }
            output.textures_delta.clear();
            assert_eq!(rects.len(), 7, "six tool buttons and one model slot");
            for rect in rects {
                assert_eq!(rect.top(), 657.0);
                assert_eq!(
                    rect.height(),
                    shell::CONTROL_HEIGHT,
                    "actual Button paint must respect its reference slot at Main{main_size}"
                );
            }
        }
    }

    fn renderer() -> TerminalRenderer {
        let mut renderer = TerminalRenderer::new(
            16.0,
            12.0,
            1.3,
            crate::config::ScrollbarVisibility::Always,
            crate::theme::Theme::default(),
        );
        renderer.block_mode = true;
        renderer.chat_mode = true;
        renderer.chat_controls_enabled = true;
        renderer.chat_native_owner = Some((7, 7));
        renderer.gpu_rendering = false;
        renderer
    }

    fn prompt(terminal: &mut TerminalState) {
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07echo hello");
    }

    #[test]
    fn chat_composer_tools_do_not_alias_native_keyboard_focus() {
        let ctx = egui::Context::default();
        let mut renderer = renderer();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 800.0));
        let (cols, rows) = renderer.grid_dimensions(screen.size());
        let mut terminal = TerminalState::new(cols, rows);
        prompt(&mut terminal);
        for _ in 0..3 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ui| {
                    let response = renderer.render_in_rect(
                        ui,
                        &mut terminal,
                        true,
                        true,
                        true,
                        &crate::search::SearchState::default(),
                        &[],
                        &None,
                        screen,
                    );
                    response.request_focus();
                    assert!(
                        !crate::ui::block_action_has_focus(ui.ctx()),
                        "native terminal focus must never look like composer Tools focus"
                    );
                },
            );
            output.textures_delta.clear();
        }
    }

    #[test]
    fn chat_neutral_output_joins_canvas_without_hiding_important_surfaces() {
        assert!(chat_output_is_quiet(false, false, false, false, true));
        let source = include_str!("chat.rs");
        for body in [
            source
                .split_once("    fn chat_card_fill(")
                .unwrap()
                .1
                .split_once("    pub(super) fn draw_chat_backgrounds")
                .unwrap()
                .0,
            source
                .split_once("    pub(super) fn draw_chat_foreground(")
                .unwrap()
                .1
                .split_once("    pub(super) fn draw_chat_controls(")
                .unwrap()
                .0,
        ] {
            assert!(body.contains("crate::block_mode::BlockOutcome::Success"));
            assert!(!body.contains("crate::block_mode::BlockOutcome::Background"));
        }

        for flags in [
            (true, false, false, false, true),
            (false, true, false, false, true),
            (false, false, true, false, true),
            (false, false, false, true, true),
            (false, false, false, false, false),
        ] {
            assert!(!chat_output_is_quiet(
                flags.0, flags.1, flags.2, flags.3, flags.4
            ));
        }
    }

    #[test]
    fn chat_buttons_use_native_key_encoding_and_stale_state_emits_nothing() {
        let renderer = renderer();
        let ctx = egui::Context::default();
        let mut terminal = TerminalState::new(80, 24);
        prompt(&mut terminal);
        let sequence = terminal.command_records().back().unwrap().sequence;
        let submit = ChatInputAction::Submit { sequence };
        assert_eq!(renderer.chat_action_bytes(&ctx, &terminal, submit), b"\r");
        terminal.process_input(b"\x1b[>8u");
        assert_eq!(terminal.keyboard_enhancement_flags(), 8);
        let encoded = renderer.chat_action_bytes(&ctx, &terminal, submit);
        // Ember's existing named-Enter encoder remains CR in this mode; Chat
        // must follow that same path rather than invent a second protocol.
        assert_eq!(encoded, b"\r");
        terminal.process_input(b"\r\n\x1b]133;C\x07");
        assert!(renderer
            .chat_action_bytes(&ctx, &terminal, submit)
            .is_empty());
        terminal.process_input(b"\x1b[<u");
        let interrupt = ChatInputAction::Interrupt { sequence };
        assert_eq!(
            renderer.chat_action_bytes(&ctx, &terminal, interrupt),
            b"\x03"
        );
        terminal.process_input(b"\x1b]133;D;0\x07");
        assert!(renderer
            .chat_action_bytes(&ctx, &terminal, interrupt)
            .is_empty());
    }

    #[test]
    fn chat_controls_preserve_pending_ime_until_commit_or_cancel() {
        let renderer = renderer();
        let ctx = egui::Context::default();
        let mut terminal = TerminalState::new(80, 24);
        prompt(&mut terminal);
        let sequence = terminal.command_records().back().unwrap().sequence;
        for action in [
            ChatInputAction::Submit { sequence },
            ChatInputAction::Interrupt { sequence },
        ] {
            if matches!(action, ChatInputAction::Interrupt { .. }) {
                terminal.process_input(b"\r\n\x1b]133;C\x07");
            }
            let expected = renderer.chat_action_bytes(&ctx, &terminal, action);
            assert_eq!(expected.len(), 1);
            // A candidate may start after the control was rendered/pressed.
            // is_current is also checked by the final Session writer, so this
            // rejects stale activations without changing the native IME state.
            terminal.ime_enabled = true;
            terminal.set_preedit("nihao".into(), 5);
            assert!(!action.is_current(&terminal));
            assert!(renderer
                .chat_action_bytes(&ctx, &terminal, action)
                .is_empty());
            assert_eq!(terminal.preedit_text, "nihao");
            assert!(terminal.ime_enabled);
            terminal.clear_preedit();
            assert_eq!(
                renderer.chat_action_bytes(&ctx, &terminal, action),
                expected
            );
            assert!(
                terminal.ime_enabled,
                "enabled IME without preedit is allowed"
            );
        }
    }

    #[test]
    fn chat_interrupt_matches_native_ctrl_c_and_disables_multibyte_clicks() {
        for flags in [0, 1, 8, 9] {
            for modify_other_keys in [0, 1, 2] {
                for format_other_keys in [0, 1] {
                    let ctx = egui::Context::default();
                    let mut renderer = renderer();
                    // Production synchronizes native font metrics before projection.
                    ctx.run_ui(Default::default(), |ui| {
                        renderer.sync_font_metrics(ui.ctx())
                    })
                    .drop_without_applying_deltas();
                    let mut terminal = TerminalState::new(80, 24);
                    prompt(&mut terminal);
                    terminal.process_input(b"\r\n\x1b]133;C\x07");
                    terminal.process_input(
                        format!(
                            "\x1b[>{flags}u\x1b[>4;{modify_other_keys}m\x1b[>4;{format_other_keys}f"
                        )
                        .as_bytes(),
                    );
                    assert_eq!(terminal.keyboard_enhancement_flags(), flags);
                    assert_eq!(terminal.xterm_modify_other_keys(), modify_other_keys);
                    assert_eq!(terminal.xterm_format_other_keys(), format_other_keys);
                    let sequence = terminal.command_records().back().unwrap().sequence;
                    let action = ChatInputAction::Interrupt { sequence };
                    let mut native = Vec::new();
                    renderer.handle_keyboard_input(
                        &ctx,
                        &mut native,
                        &std::collections::HashSet::new(),
                        false,
                        terminal.keyboard_enhancement_flags(),
                        terminal.is_report_all_keys_enabled(),
                        terminal.xterm_modify_other_keys(),
                        terminal.xterm_format_other_keys(),
                        terminal.is_application_cursor_keys(),
                        terminal.is_alt_buffer_active(),
                        terminal.is_meta_sends_escape(),
                        false,
                        &[egui::Event::Key {
                            key: egui::Key::C,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::CTRL,
                        }],
                    );
                    let expected: &[u8] =
                        if flags != 0 || (modify_other_keys == 2 && format_other_keys == 1) {
                            b"\x1b[99;5u"
                        } else if modify_other_keys == 2 {
                            b"\x1b[27;5;99~"
                        } else {
                            b"\x03"
                        };
                    assert_eq!(native, expected);
                    assert_eq!(renderer.chat_action_bytes(&ctx, &terminal, action), native);

                    let screen =
                        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 650.0));
                    let mut policy = ProjectionPolicy::new();
                    policy.set_chat_mode(true);
                    policy.set_block_chrome(true, 0);
                    let mut state = ProjectionViewState::new();
                    let mut pointer = egui::Pos2::ZERO;
                    for phase in 0..3 {
                        let view = renderer.projected_viewport_with_state(
                            &mut terminal,
                            &policy,
                            &mut state,
                        );
                        if phase == 0 {
                            let row = view
                                .chat_command_spans()
                                .iter()
                                .find(|command| command.sequence == sequence)
                                .unwrap()
                                .rows
                                .end;
                            assert!(matches!(
                                view.row_kind(row),
                                Some(crate::terminal::ProjectedRowKind::BlockSpacer { .. })
                            ));
                            let content = renderer.layout_rects(screen).0;
                            pointer = content.min
                                + egui::vec2(20.0, (row as f32 + 0.5) * renderer.line_height);
                        }
                        renderer.set_projection_frame(&terminal, view, &policy);
                        let events = if phase == 0 {
                            Vec::new()
                        } else {
                            vec![
                                egui::Event::PointerMoved(pointer),
                                egui::Event::PointerButton {
                                    pos: pointer,
                                    button: egui::PointerButton::Primary,
                                    pressed: phase == 1,
                                    modifiers: egui::Modifiers::NONE,
                                },
                            ]
                        };
                        let mut output = ctx.run_ui(
                            egui::RawInput {
                                screen_rect: Some(screen),
                                events,
                                ..Default::default()
                            },
                            |ui| {
                                renderer.render_in_rect(
                                    ui,
                                    &mut terminal,
                                    true,
                                    true,
                                    true,
                                    &crate::search::SearchState::default(),
                                    &[],
                                    &None,
                                    screen,
                                );
                            },
                        );
                        output.textures_delta.clear();
                    }
                    assert_eq!(renderer.chat_input_action.is_some(), native.len() == 1);
                    assert!(renderer.cursor_move_input.is_empty());
                    terminal.process_input(b"\x1b]133;D;0\x07");
                    assert!(renderer
                        .chat_action_bytes(&ctx, &terminal, action)
                        .is_empty());
                }
            }
        }
    }

    #[test]
    fn chat_preedit_uses_the_real_body_and_excludes_commands_and_tools() {
        let renderer = renderer();
        let mut terminal = TerminalState::new(80, 16);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07cat\r\n\x1b]133;C\x07");
        let mut policy = ProjectionPolicy::new();
        policy.set_chat_mode(true);
        policy.set_block_chrome(true, 0);
        policy.set_block_spacing(true);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 440.0));
        let content = renderer.layout_rects(screen).0;
        let view = renderer.projected_viewport_with_state(
            &mut terminal,
            &policy,
            &mut ProjectionViewState::new(),
        );
        let entries = renderer
            .block_chrome_snapshot(&terminal, &view, view.rows())
            .unwrap();
        let cards: Vec<_> = entries
            .iter()
            .flat_map(|entry| renderer.chat_cards(&terminal, &view, entry, content))
            .collect();
        let body = cards
            .iter()
            .find(|card| !card.command && !card.composer)
            .unwrap();
        let caret = egui::Rect::from_min_size(
            body.rect.min + egui::vec2(8.0, 0.0),
            egui::vec2(renderer.char_width, renderer.line_height),
        );
        let clip = renderer
            .chat_preedit_clip(&terminal, &view, &entries, content, content, caret)
            .unwrap();
        assert_eq!(clip, body.rect.intersect(content));
        let command = cards.iter().find(|card| card.command).unwrap();
        let command_caret =
            egui::Rect::from_center_size(command.rect.center(), egui::vec2(2.0, 2.0));
        assert!(renderer
            .chat_preedit_clip(&terminal, &view, &entries, content, content, command_caret)
            .is_none());

        terminal.process_input(b"\x1b]133;D;0\x07");
        prompt(&mut terminal);
        let view = renderer.projected_viewport_with_state(
            &mut terminal,
            &policy,
            &mut ProjectionViewState::new(),
        );
        let entries = renderer
            .block_chrome_snapshot(&terminal, &view, view.rows())
            .unwrap();
        let composer = entries
            .iter()
            .flat_map(|entry| renderer.chat_cards(&terminal, &view, entry, content))
            .find(|card| card.composer)
            .unwrap();
        let rows = view.chat_composer_rows().unwrap();
        let footer = renderer.composer_footer_rect(content, rows);
        let caret = egui::Rect::from_min_size(
            composer.rect.min + egui::vec2(16.0, 4.0),
            egui::vec2(renderer.char_width, renderer.line_height),
        );
        let clip = renderer
            .chat_preedit_clip(&terminal, &view, &entries, content, content, caret)
            .unwrap();
        assert!(clip.bottom() <= footer.top());
        let toolbar_caret = egui::Rect::from_center_size(footer.center(), egui::vec2(2.0, 2.0));
        assert!(renderer
            .chat_preedit_clip(&terminal, &view, &entries, content, content, toolbar_caret)
            .is_none());
    }

    #[test]
    fn chat_pointer_origins_match_painted_cells_at_small_large_and_scaled_sizes() {
        for font in [10.0, 16.0, 24.0, 32.0] {
            for width in [120.0, 420.0, 1024.0] {
                let mut renderer = renderer();
                renderer.font_size = font;
                renderer.char_width = font * 0.6;
                renderer.line_height = font * 1.4;
                let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 720.0));
                let (cols, rows) = renderer.grid_dimensions(screen.size());
                let mut terminal = TerminalState::new(cols, rows);
                terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07echo hi\r\n\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07");
                prompt(&mut terminal);
                let mut policy = ProjectionPolicy::new();
                policy.set_chat_mode(true);
                policy.set_block_chrome(true, 0);
                policy.set_block_spacing(true);
                let view = renderer.projected_viewport_with_state(
                    &mut terminal,
                    &policy,
                    &mut ProjectionViewState::new(),
                );
                let content = renderer.layout_rects(screen).0;
                renderer.last_content_rect = Some(content);
                let entries = renderer
                    .block_chrome_snapshot(&terminal, &view, rows)
                    .unwrap();
                for command in view.chat_command_spans() {
                    let entry = entries
                        .iter()
                        .find(|entry| {
                            terminal.command_records()[entry.span.record_index].sequence
                                == command.sequence
                        })
                        .unwrap();
                    let cards = renderer.chat_cards(&terminal, &view, entry, content);
                    let capsule = cards.iter().find(|card| card.command).unwrap();
                    for row in command.rows.clone() {
                        let (left, right) = view.real_column_bounds(row).unwrap();
                        for column in left..right {
                            let center = content.min
                                + egui::vec2(
                                    (column as f32 + 0.5) * renderer.char_width,
                                    (row as f32 + 0.5) * renderer.line_height,
                                );
                            assert!(
                                capsule.rect.contains(center),
                                "painted capsule owns its exact cell centers"
                            );
                            assert!(view.raw_anchor_at(DisplayPoint::new(row, column)).is_some());
                        }
                    }
                    for result in cards.iter().filter(|card| !card.command) {
                        assert!(
                            result.rect.left() <= content.left(),
                            "output retains the native left edge"
                        );
                        assert!(
                            result.rect.top() >= capsule.rect.bottom(),
                            "output does not overlap the capsule"
                        );
                    }
                }
                for row in 0..view.rows() {
                    for col in 0..view.columns() {
                        let point = DisplayPoint::new(row, col);
                        let pointer = content.min
                            + egui::vec2(
                                (col as f32 + 0.5) * renderer.char_width,
                                (row as f32 + 0.5) * renderer.line_height,
                            );
                        let measured = grid_position_from_content(
                            pointer,
                            content,
                            renderer.char_width,
                            renderer.line_height,
                            cols,
                            rows,
                        );
                        assert_eq!(measured, (row, col));
                        if view.raw_anchor_at(point).is_none() && view.is_transformed() {
                            assert!(!renderer
                                .pointer_app_mouse_eligible_projected(&terminal, &view, pointer));
                            assert!(!renderer
                                .pointer_link_eligible_projected(&terminal, &view, pointer));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn chat_control_interruption_retires_press_until_a_fresh_click() {
        let ctx = egui::Context::default();
        let mut renderer = renderer();
        renderer.font_size = 16.0;
        renderer.char_width = 9.6;
        renderer.line_height = 22.4;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 800.0));
        let (cols, rows) = renderer.grid_dimensions(screen.size());
        let mut terminal = TerminalState::new(cols, rows);
        prompt(&mut terminal);
        let mut policy = ProjectionPolicy::new();
        policy.set_chat_mode(true);
        policy.set_block_chrome(true, 0);
        let view = renderer.projected_viewport_with_state(
            &mut terminal,
            &policy,
            &mut ProjectionViewState::new(),
        );
        let content = renderer.layout_rects(screen).0;

        let footer = renderer.composer_footer_rect(content, view.chat_composer_rows().unwrap());
        let pointer = egui::pos2(footer.right() - 14.0, footer.center().y);
        let mut frame = |enabled, pressed: Option<bool>| {
            renderer.chat_controls_enabled = enabled;
            let mut events = vec![egui::Event::PointerMoved(pointer)];
            if let Some(pressed) = pressed {
                events.push(egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |ui| {
                    renderer.draw_chat_controls(ui, &terminal, &view, content, true);
                },
            );
            output.textures_delta.clear();
            renderer.chat_input_action.take()
        };
        assert!(frame(true, None).is_none());
        assert!(frame(true, Some(true)).is_none());
        assert!(frame(false, None).is_none());
        assert!(
            frame(true, Some(false)).is_none(),
            "old press must stay retired"
        );
        assert!(frame(false, Some(true)).is_none());
        assert!(
            frame(true, Some(false)).is_none(),
            "blocked press cannot activate later"
        );
        assert!(frame(true, Some(true)).is_none());
        assert!(
            frame(true, Some(false)).is_some(),
            "fresh click remains available"
        );
    }

    #[test]
    fn default_metrics_keep_run_and_interrupt_visible_and_clickable() {
        for dpi in [1.0, 1.5, 2.0] {
            for size in [8.0, 16.0, 32.0, 72.0] {
                for running in [false, true] {
                    let ctx = egui::Context::default();
                    ctx.set_pixels_per_point(dpi);
                    let mut renderer = renderer();
                    renderer.font_size = size;
                    renderer.line_spacing = crate::config::Config::default().line_spacing;
                    ctx.run_ui(Default::default(), |ui| {
                        renderer.sync_font_metrics(ui.ctx())
                    })
                    .drop_without_applying_deltas();
                    let screen =
                        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 1000.0));
                    let (cols, rows) = renderer.grid_dimensions(screen.size());
                    let mut terminal = TerminalState::new(cols, rows);
                    prompt(&mut terminal);
                    if running {
                        terminal.process_input(b"\r\n\x1b]133;C\x07running");
                    }
                    let mut policy = ProjectionPolicy::new();
                    policy.set_chat_mode(true);
                    policy.set_block_chrome(true, 0);
                    let view = renderer.projected_viewport_with_state(
                        &mut terminal,
                        &policy,
                        &mut ProjectionViewState::new(),
                    );
                    let content = renderer.layout_rects(screen).0;
                    let mut point = None;
                    for phase in 0..3 {
                        let events = point.map_or_else(Vec::new, |pos| {
                            vec![
                                egui::Event::PointerMoved(pos),
                                egui::Event::PointerButton {
                                    pos,
                                    button: egui::PointerButton::Primary,
                                    pressed: phase == 1,
                                    modifiers: egui::Modifiers::NONE,
                                },
                            ]
                        });
                        let mut output = ctx.run_ui(
                            egui::RawInput {
                                screen_rect: Some(screen),
                                events,
                                ..Default::default()
                            },
                            |ui| {
                                crate::theme::configure_workspace_text(ui.style_mut(), size);
                                renderer.draw_chat_controls(ui, &terminal, &view, content, true);
                            },
                        );
                        // Clear before assertions so a failed assertion reports normally
                        // instead of aborting the whole suite in TexturesDelta::drop.
                        output.textures_delta.clear();
                        let target = renderer
                            .chat_control_rect
                            .unwrap_or_else(|| panic!("native optional action remains visible: dpi={dpi}, size={size}, running={running}"));
                        let hits: Vec<_> = ctx
                            .interactive_rects_last_pass()
                            .into_iter()
                            .filter(|hit| target.contains_rect(*hit))
                            .collect();
                        assert_eq!(hits.len(), 1, "one action owns its exact rectangle: dpi={dpi}, size={size}, running={running}");
                        point = Some(hits[0].center());
                        if running {
                            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == "Interrupt")));
                        } else {
                            assert!(
                                output
                                    .shapes
                                    .iter()
                                    .any(|shape| matches!(&shape.shape, egui::Shape::Path(_))),
                                "native vector send icon is painted"
                            );
                        }
                        output.drop_without_applying_deltas();
                    }
                    assert!(
                        renderer.chat_input_action.is_some(),
                        "dpi={dpi}, size={size}, running={running}"
                    );
                }
            }
        }
    }

    #[test]
    fn chat_run_button_stays_inside_narrow_composer_footer() {
        // Real egui allocation, paint clips, hit regions and pointer events.
        // ui.put's max_rect alone is not a hard bound for a wrapping Button.
        for font in [10.0, 16.0, 32.0] {
            for width in [48.0, 64.0, 80.0, 120.0, 240.0] {
                for dpi in [1.0, 1.5, 2.0] {
                    for sample in 0..3 {
                        let ctx = egui::Context::default();
                        ctx.set_pixels_per_point(dpi);
                        let mut renderer = renderer();
                        renderer.font_size = font;
                        renderer.line_spacing = crate::config::Config::default().line_spacing;
                        ctx.run_ui(Default::default(), |ui| {
                            renderer.sync_font_metrics(ui.ctx())
                        })
                        .drop_without_applying_deltas();
                        let screen =
                            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 900.0));
                        let pane = egui::Rect::from_min_size(
                            egui::pos2(100.0, 40.0),
                            egui::vec2(width, 800.0),
                        );
                        let (cols, rows) = renderer.grid_dimensions(pane.size());
                        let mut terminal = TerminalState::new(cols, rows);
                        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
                        let mut policy = ProjectionPolicy::new();
                        policy.set_chat_mode(true);
                        policy.set_block_chrome(true, 0);
                        let view = renderer.projected_viewport_with_state(
                            &mut terminal,
                            &policy,
                            &mut ProjectionViewState::new(),
                        );
                        let content = renderer.layout_rects(pane).0;

                        let footer = renderer
                            .composer_footer_rect(content, view.chat_composer_rows().unwrap());
                        let button = egui::Rect::from_min_max(
                            egui::pos2(footer.right() - 28.0_f32.min(footer.width()), footer.top()),
                            footer.max,
                        );
                        let pointer = match sample {
                            0 => button.center(),
                            1 => egui::pos2(button.center().x, footer.bottom() + 1.0),
                            _ => egui::pos2(button.left() - 1.0, button.center().y),
                        };
                        let mut expected_visible = false;
                        for phase in 0..3 {
                            let events = if phase == 0 {
                                Vec::new()
                            } else {
                                vec![
                                    egui::Event::PointerMoved(pointer),
                                    egui::Event::PointerButton {
                                        pos: pointer,
                                        button: egui::PointerButton::Primary,
                                        pressed: phase == 1,
                                        modifiers: egui::Modifiers::NONE,
                                    },
                                ]
                            };
                            let mut output = ctx.run_ui(
                                egui::RawInput {
                                    screen_rect: Some(screen),
                                    events,
                                    ..Default::default()
                                },
                                |ui| {
                                    let text_size = egui::vec2(14.0, 14.0);
                                    expected_visible = button.width() >= text_size.x + 12.0
                                        && button.height() >= text_size.y + 2.0;
                                    renderer
                                        .draw_chat_controls(ui, &terminal, &view, content, true);
                                },
                            );
                            output.textures_delta.clear();
                            let hits: Vec<_> = ctx
                                .interactive_rects_last_pass()
                                .into_iter()
                                .filter(|hit| button.intersects(*hit))
                                .collect();
                            assert_eq!(
                                hits.len(),
                                usize::from(expected_visible),
                                "font={font}, width={width}"
                            );
                            for hit in &hits {
                                assert!(button.contains_rect(*hit), "font={font}, width={width}: actual egui hit {hit:?} escapes button {button:?}");
                            }
                            let mut send_paths = 0;
                            for shape in &output.shapes {
                                let bounds = shape.shape.visual_bounding_rect();
                                if matches!(shape.shape, egui::Shape::Path(_))
                                    && button.contains_rect(bounds)
                                {
                                    send_paths += 1;
                                    assert!(
                                        footer.contains_rect(bounds),
                                        "send path must fit its native footer"
                                    );
                                }
                                let visible = bounds.intersect(shape.clip_rect);
                                if visible.is_positive() {
                                    assert!(footer.contains_rect(visible), "font={font}, width={width}: painted {visible:?} escapes footer {footer:?}");
                                }
                            }
                            assert_eq!(send_paths, usize::from(expected_visible) * jterm_core::reference_icons::Icon::Send.paths().len(), "vector send paths must be present exactly when the optional action fits");
                        }
                        assert_eq!(
                            renderer.chat_input_action.is_some(),
                            sample == 0 && expected_visible,
                            "font={font}, width={width}, sample={sample}"
                        );
                        assert!(renderer.cursor_move_input.is_empty());
                    }
                }
            }
        }
    }

    #[test]
    fn chat_run_control_does_not_steal_adjacent_raw_row_clicks() {
        for font in [10.0, 16.0, 32.0] {
            for width in [80.0, 120.0, 240.0] {
                let ctx = egui::Context::default();
                let mut renderer = renderer();
                renderer.font_size = font;
                renderer.char_width = font * 0.6;
                renderer.line_height = font * 1.4;
                let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 900.0));
                let pane =
                    egui::Rect::from_min_size(egui::pos2(100.0, 40.0), egui::vec2(width, 800.0));
                let (cols, rows) = renderer.grid_dimensions(pane.size());
                let mut terminal = TerminalState::new(cols, rows);
                terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
                let mut policy = ProjectionPolicy::new();
                policy.set_chat_mode(true);
                policy.set_block_chrome(true, 0);
                let mut state = ProjectionViewState::new();
                let content = renderer.layout_rects(pane).0;
                let mut pane_clicked = false;
                for phase in 0..3 {
                    let view =
                        renderer.projected_viewport_with_state(&mut terminal, &policy, &mut state);
                    let composer = view.chat_composer_rows().unwrap();
                    let footer_row = composer
                        .clone()
                        .find(|&row| {
                            matches!(
                                view.row_kind(row),
                                Some(crate::terminal::ProjectedRowKind::CompanionStatus { .. })
                            )
                        })
                        .expect("explicit synthetic footer rows");
                    assert_eq!(
                        view.row_kind(footer_row - 1),
                        Some(crate::terminal::ProjectedRowKind::Raw)
                    );
                    let pointer = egui::pos2(
                        content.right() - 10.0,
                        content.top() + footer_row as f32 * renderer.line_height - 1.0,
                    );
                    renderer.set_projection_frame(&terminal, view, &policy);
                    let events = if phase == 0 {
                        Vec::new()
                    } else {
                        vec![
                            egui::Event::PointerMoved(pointer),
                            egui::Event::PointerButton {
                                pos: pointer,
                                button: egui::PointerButton::Primary,
                                pressed: phase == 1,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ]
                    };
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(screen),
                            events,
                            ..Default::default()
                        },
                        |ui| {
                            let response = renderer.render_in_rect(
                                ui,
                                &mut terminal,
                                true,
                                true,
                                true,
                                &crate::search::SearchState::default(),
                                &[],
                                &None,
                                pane,
                            );
                            pane_clicked |= response.clicked_by(egui::PointerButton::Primary);
                        },
                    );
                    output.textures_delta.clear();
                    assert!(
                        renderer.chat_input_action.is_none(),
                        "font={font}, width={width}, phase={phase}"
                    );
                }
                assert!(pane_clicked, "native pane must retain the adjacent raw-row click: font={font}, width={width}");
            }
        }
    }

    #[test]
    fn chat_send_click_is_bound_to_its_pointer_down_prompt() {
        for transition in [0, 1, 2, 3] {
            let ctx = egui::Context::default();
            let mut renderer = renderer();
            // Production synchronizes native font metrics before projection.
            ctx.run_ui(Default::default(), |ui| {
                renderer.sync_font_metrics(ui.ctx())
            })
            .drop_without_applying_deltas();
            let mut terminal = TerminalState::new(80, 24);
            prompt(&mut terminal);
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 650.0));
            let mut policy = ProjectionPolicy::new();
            policy.set_chat_mode(true);
            policy.set_block_chrome(true, 0);
            let mut state = ProjectionViewState::new();
            let mut pointer = egui::Pos2::ZERO;
            for phase in 0..3 {
                if phase == 2 && transition == 3 {
                    renderer.chat_native_owner = Some((7, 8));
                }
                if phase == 2 && (transition == 1 || transition == 2) {
                    terminal.process_input(b"\r\n\x1b]133;C\x07running");
                    if transition == 2 {
                        terminal.process_input(b"\r\n\x1b]133;D;0\x07");
                        prompt(&mut terminal);
                    }
                }
                let view =
                    renderer.projected_viewport_with_state(&mut terminal, &policy, &mut state);
                let content = renderer.layout_rects(screen).0;
                if phase == 0 {
                    renderer.workspace_outer_rect = Some(screen);
                    let footer =
                        renderer.composer_footer_rect(content, view.chat_composer_rows().unwrap());
                    pointer = egui::pos2(footer.right() - 14.0, footer.center().y);
                }
                renderer.set_projection_frame(&terminal, view, &policy);
                let events = if phase == 0 {
                    Vec::new()
                } else {
                    vec![
                        egui::Event::PointerMoved(pointer),
                        egui::Event::PointerButton {
                            pos: pointer,
                            button: egui::PointerButton::Primary,
                            pressed: phase == 1,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ]
                };
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        renderer.render_in_rect(
                            ui,
                            &mut terminal,
                            true,
                            true,
                            true,
                            &crate::search::SearchState::default(),
                            &[],
                            &None,
                            screen,
                        );
                    },
                );
                output.textures_delta.clear();
            }
            assert_eq!(
                renderer.chat_input_action.is_some(),
                transition == 0,
                "transition {transition}"
            );
            assert!(renderer.cursor_move_input.is_empty());
        }
    }
}

#[cfg(test)]
mod capsule_geometry_tests {
    use super::*;
    #[test]
    fn docked_composer_cannot_close_a_clipped_history_capsule() {
        assert!(chat_history_edge_closed(4, 12, Some(40), Some(41)));
        assert!(!chat_history_edge_closed(4, 4, Some(40), Some(41)));
        assert!(!chat_history_edge_closed(4, 12, Some(40), Some(99)));
        assert!(!chat_history_edge_closed(4, 12, Some(40), None));
        assert!(!chat_history_edge_closed(0, 12, None, Some(41)));
        assert!(!chat_history_edge_closed(4, 12, Some(usize::MAX), Some(0)));
    }

    #[test]
    fn chat_capsule_corners_share_native_contract_at_all_scales_and_clips() {
        for height in [0.0, 1.0, 8.0, 18.0, 32.0, 64.0] {
            for width in [0.0, 1.0, 8.0, 160.0] {
                for (command, composer, radius) in [
                    (true, false, jterm_core::reference_shell::CARD_RADIUS),
                    (false, false, jterm_core::reference_shell::CARD_RADIUS),
                    (false, true, jterm_core::reference_shell::COMPOSER_RADIUS),
                ] {
                    for top_closed in [false, true] {
                        for bottom_closed in [false, true] {
                            let card = ChatCard {
                                rect: egui::Rect::from_min_size(
                                    egui::pos2(0.0, 0.0),
                                    egui::vec2(width, height),
                                ),
                                command,
                                composer,
                                top_closed,
                                bottom_closed,
                            };
                            let r = chat_card_rounding(card);
                            let limit = width.min(height) * 0.5;
                            let round = radius.min(limit) as u8;
                            let tail = 5.0_f32.min(limit) as u8;
                            assert_eq!(
                                r.nw,
                                if !top_closed {
                                    0
                                } else if !command && !composer {
                                    tail
                                } else {
                                    round
                                }
                            );
                            assert_eq!(r.ne, if top_closed { round } else { 0 });
                            assert_eq!(r.sw, if bottom_closed { round } else { 0 });
                            assert_eq!(
                                r.se,
                                if !bottom_closed {
                                    0
                                } else if command {
                                    tail
                                } else {
                                    round
                                }
                            );
                            assert_eq!(card.rect.size(), egui::vec2(width, height));
                        }
                    }
                }
            }
        }
    }
}
#[cfg(test)]
mod chat_footer_hint_tests {
    use super::*;

    #[test]
    fn footer_hint_respects_parent_clip_and_does_not_claim_input() {
        for width in [12.0, 48.0, 240.0] {
            let ctx = egui::Context::default();
            let clip = egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(width, 16.0));
            let rect = egui::Rect::from_min_size(egui::pos2(10.0, 18.0), egui::vec2(300.0, 20.0));
            let mut output = ctx.run_ui(Default::default(), |ui| {
                ui.set_clip_rect(clip);
                draw_chat_footer_hint(ui, rect, egui::Id::new("hint"), 12.0, egui::Color32::WHITE);
            });
            output.textures_delta.clear();
            let painted = output.shapes.iter().filter(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == CHAT_FOOTER_HINT)).collect::<Vec<_>>();
            assert_eq!(painted.len(), 1);
            assert_eq!(painted[0].clip_rect, clip.intersect(rect));
            assert!(
                ctx.interactive_rects_last_pass().is_empty(),
                "hint must not take clicks or keyboard focus"
            );
            output.drop_without_applying_deltas();
        }
    }

    #[test]
    fn fully_clipped_footer_has_no_paint_or_hover_target() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            ui.set_clip_rect(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(20.0, 20.0),
            ));
            draw_chat_footer_hint(
                ui,
                egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(50.0, 20.0)),
                egui::Id::new("hint"),
                12.0,
                egui::Color32::WHITE,
            );
        });
        output.textures_delta.clear();
        assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == CHAT_FOOTER_HINT)));
        assert!(ctx.interactive_rects_last_pass().is_empty());
        output.drop_without_applying_deltas();
    }

    #[test]
    fn narrow_footer_exposes_complete_hint_on_hover() {
        let ctx = egui::Context::default();
        ctx.style_mut_of(egui::Theme::Dark, |style| {
            style.interaction.tooltip_delay = 0.0;
            style.interaction.show_tooltips_only_when_still = false;
        });
        let mut found_tooltip = false;
        for frame in 0..4 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(frame as f64),
                    events: vec![egui::Event::PointerMoved(egui::pos2(30.0, 28.0))],
                    ..Default::default()
                },
                |ui| {
                    draw_chat_footer_hint(
                        ui,
                        egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(24.0, 18.0)),
                        egui::Id::new("hint"),
                        12.0,
                        egui::Color32::WHITE,
                    );
                },
            );
            output.textures_delta.clear();
            found_tooltip |= output.shapes.iter().filter(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == CHAT_FOOTER_HINT)).count() >= 2;
            output.drop_without_applying_deltas();
        }
        assert!(
            found_tooltip,
            "the clipped hint must also be available as full tooltip text"
        );
    }
}
