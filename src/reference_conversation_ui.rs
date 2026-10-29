//! Egui components over the shared conversation model; no HTML or PTY writes.
use crate::{app::state::TerminalApp, reference_workspace::rgb};
use egui::{Color32, RichText, Stroke};
use jterm_core::{
    reference_conversation as model,
    reference_icons::Icon,
    reference_shell::{self as shell, color},
};

pub(crate) fn icon(ui: &egui::Ui, rect: egui::Rect, icon: Icon, tint: Color32) {
    let scale = rect.width().min(rect.height()) / 24.0;
    let stroke = Stroke::new(1.5 * scale, tint);
    for path in icon.paths() {
        let points: Vec<_> = path
            .iter()
            .map(|&(x, y)| rect.min + egui::vec2(x, y) * scale)
            .collect();
        ui.painter().add(egui::Shape::line(points.clone(), stroke));
        if let (Some(first), Some(last)) = (points.first(), points.last()) {
            ui.painter().circle_filled(*first, stroke.width / 2.0, tint);
            ui.painter().circle_filled(*last, stroke.width / 2.0, tint);
        }
    }
}

pub(crate) fn icon_button(ui: &mut egui::Ui, kind: Icon, label: &str) -> egui::Response {
    let response = ui.add_sized(
        [28.0, 28.0],
        egui::Button::new("")
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0, rgb(color::LIGHT_BORDER)))
            .corner_radius(7),
    );
    icon(
        ui,
        response.rect.shrink(6.0),
        kind,
        ui.visuals().text_color(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    response.on_hover_text(label)
}

fn state_icon(ui: &egui::Ui, rect: egui::Rect, state: model::StepState) {
    let (kind, tint) = match state {
        model::StepState::Complete => (Icon::Check, Color32::from_rgb(79, 162, 128)),
        model::StepState::Running => (Icon::Agent, rgb(color::ACCENT)),
        model::StepState::Queued => (Icon::History, rgb(color::CANVAS_MUTED)),
    };
    icon(ui, rect, kind, tint);
}

/// Rich message roles inherit the text size of their owning surface. The
/// central transcript owns Main; a floating AI Chats window owns UI. Reference
/// roles retain their reference hierarchy at the existing Config default.
/// BODY is the default role size, not the font setting's normalization value.
fn content_scale(ui: &egui::Ui) -> f32 {
    crate::theme::workspace_font_size(ui) / crate::config::DEFAULT_FONT_SIZE
}

fn pill(ui: &mut egui::Ui, text: &str, key: &str, complete: bool) {
    let scale = content_scale(ui);
    egui::Frame::NONE
        .fill(if complete {
            Color32::from_rgb(229, 244, 235)
        } else {
            rgb(color::ACCENT_PALE)
        })
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(6, 0))
        .show(ui, |ui| {
            ui.spacing_mut().interact_size.y = shell::METADATA_PILL_HEIGHT * scale;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(12.0 * scale, shell::METADATA_PILL_HEIGHT * scale),
                    egui::Sense::hover(),
                );
                icon(
                    ui,
                    egui::Rect::from_center_size(
                        rect.center(),
                        egui::vec2(12.0 * scale, 12.0 * scale),
                    ),
                    Icon::from_key(key),
                    rgb(color::CANVAS_MUTED),
                );
                ui.label(
                    RichText::new(text)
                        .size(9.0 * scale)
                        .color(rgb(color::CANVAS_MUTED)),
                );
            });
        });
}

fn draw_steps(ui: &mut egui::Ui, steps: &[model::Step]) {
    let scale = content_scale(ui);
    egui::Frame::NONE
        .fill(Color32::from_rgb(239, 244, 253))
        .corner_radius(8)
        .stroke(Stroke::new(1.0, rgb(color::LIGHT_BORDER)))
        .inner_margin(egui::Margin::symmetric(8, 0))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 0.0);
            ui.spacing_mut().interact_size.y = shell::TASK_ROW_HEIGHT * scale;
            for (index, step) in steps.iter().enumerate() {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(16.0 * scale, shell::TASK_ROW_HEIGHT * scale),
                        egui::Sense::hover(),
                    );
                    state_icon(ui, rect.shrink(1.0), step.state);
                    ui.add(
                        egui::Label::new(
                            RichText::new(&step.title)
                                .size(10.0 * scale)
                                .color(rgb(color::MESSAGE_STEP_TEXT)),
                        )
                        .truncate(),
                    )
                    .on_hover_text(&step.detail);
                    if let Some(elapsed) = &step.elapsed {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(elapsed)
                                    .size(9.0 * scale)
                                    .color(rgb(color::CANVAS_MUTED)),
                            );
                        });
                    }
                });
                if index + 1 < steps.len() {
                    let y = ui.cursor().top();
                    ui.painter().line_segment(
                        [
                            egui::pos2(ui.min_rect().left(), y),
                            egui::pos2(ui.max_rect().right(), y),
                        ],
                        Stroke::new(1.0, rgb(color::LIGHT_BORDER)),
                    );
                }
            }
        });
}

fn diff_gutter_width(ui: &egui::Ui, diff: &model::CodeDiff, scale: f32) -> f32 {
    let widest = diff
        .lines
        .iter()
        .flat_map(|line| [line.old_line, line.new_line])
        .flatten()
        .map(|number| {
            ui.painter()
                .layout_no_wrap(
                    number.to_string(),
                    egui::FontId::monospace(9.0 * scale),
                    rgb(color::CANVAS_MUTED),
                )
                .size()
                .x
        })
        .fold(0.0, f32::max);
    (shell::DIFF_GUTTER_COLUMN_WIDTH * scale).max(widest + 8.0 * scale)
}

fn draw_diff(ui: &mut egui::Ui, diff: &model::CodeDiff) {
    let scale = content_scale(ui);
    let gutter_width = diff_gutter_width(ui, diff, scale);
    egui::Frame::NONE
        .fill(Color32::WHITE)
        .corner_radius(10)
        .stroke(Stroke::new(1.0, rgb(color::LIGHT_BORDER)))
        .show(ui, |ui| {
            ui.spacing_mut().interact_size.y =
                crate::theme::workspace_control_height(shell::type_size::BODY * scale);
            egui::Frame::NONE
                .fill(Color32::from_rgb(243, 246, 253))
                .inner_margin(egui::Margin::symmetric(8, 1))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // The message renderer uses zero-width spacing for its
                        // outer row; diff header groups keep their own fixed gap.
                        ui.spacing_mut().item_spacing.x = shell::METADATA_GAP;
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(16.0 * scale, 16.0 * scale),
                            egui::Sense::hover(),
                        );
                        icon(ui, rect, Icon::Code, rgb(color::CANVAS_MUTED));
                        let natural = ui
                            .painter()
                            .layout_no_wrap(
                                diff.path.clone(),
                                egui::FontId::proportional(10.0 * scale),
                                rgb(color::CANVAS_TEXT),
                            )
                            .size()
                            .x;
                        let width =
                            natural.min((ui.available_width() - 210.0 * scale).max(24.0 * scale));
                        ui.add_sized(
                            [width, 28.0 * scale],
                            egui::Label::new(
                                RichText::new(&diff.path)
                                    .size(10.0 * scale)
                                    .color(rgb(color::CANVAS_TEXT)),
                            )
                            .truncate(),
                        )
                        .on_hover_text(&diff.path);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = shell::METADATA_GAP;
                            ui.add_enabled(
                                false,
                                egui::Button::new(
                                    RichText::new("Apply")
                                        .size(9.0 * scale)
                                        .color(rgb(color::DISABLED_TEXT)),
                                )
                                .min_size(egui::vec2(43.0 * scale, 24.0 * scale))
                                .fill(rgb(color::CONTROL_SURFACE))
                                .stroke(Stroke::new(1.0, rgb(color::CONTROL_BORDER)))
                                .corner_radius(6),
                            )
                            .on_disabled_hover_text("No authorized edit target is connected");
                            let copy = ui.add_sized(
                                [55.0 * scale, 24.0 * scale],
                                egui::Button::new(
                                    RichText::new("Copy")
                                        .size(9.0 * scale)
                                        .color(rgb(color::CANVAS_MUTED)),
                                )
                                .fill(rgb(color::CONTROL_SURFACE))
                                .stroke(Stroke::new(1.0, rgb(color::CONTROL_BORDER)))
                                .corner_radius(6),
                            );
                            if copy.on_hover_text("Copy displayed code").clicked() {
                                ui.ctx().copy_text(
                                    diff.lines
                                        .iter()
                                        .filter(|line| line.kind != model::DiffKind::Removed)
                                        .map(|line| line.text.as_str())
                                        .collect::<Vec<_>>()
                                        .join("\n"),
                                );
                            }
                            ui.label(
                                RichText::new(&diff.language)
                                    .size(9.0 * scale)
                                    .color(rgb(color::CANVAS_MUTED)),
                            );
                            ui.label(
                                RichText::new(format!("+{} −{}", diff.added, diff.removed))
                                    .size(9.0 * scale)
                                    .color(Color32::from_rgb(69, 145, 111)),
                            );
                        });
                    });
                });
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(shell::DIFF_BODY_VERTICAL_PADDING * scale);
            for line in &diff.lines {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), shell::DIFF_LINE_HEIGHT * scale),
                    egui::Sense::hover(),
                );
                let fill = match line.kind {
                    model::DiffKind::Added => Color32::from_rgb(231, 246, 235),
                    model::DiffKind::Removed => Color32::from_rgb(253, 239, 240),
                    _ => Color32::WHITE,
                };
                ui.painter().rect_filled(rect, 0, fill);
                for (index, number) in [line.old_line, line.new_line].into_iter().enumerate() {
                    ui.painter().text(
                        egui::pos2(
                            rect.left() + (index + 1) as f32 * gutter_width - 4.0 * scale,
                            rect.center().y,
                        ),
                        egui::Align2::RIGHT_CENTER,
                        number.map(|n| n.to_string()).unwrap_or_default(),
                        egui::FontId::monospace(9.0 * scale),
                        rgb(color::CANVAS_MUTED),
                    );
                }
                let sign = match line.kind {
                    model::DiffKind::Added => "+",
                    model::DiffKind::Removed => "−",
                    _ => " ",
                };
                let x = rect.left() + 2.0 * gutter_width;
                ui.painter().text(
                    egui::pos2(x + shell::DIFF_SIGN_WIDTH * scale * 0.5, rect.center().y),
                    egui::Align2::CENTER_CENTER,
                    sign,
                    egui::FontId::monospace(10.0 * scale),
                    rgb(color::CANVAS_TEXT),
                );
                ui.painter().with_clip_rect(rect).text(
                    egui::pos2(x + shell::DIFF_SIGN_WIDTH * scale, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &line.text,
                    egui::FontId::monospace(10.0 * scale),
                    rgb(color::CANVAS_TEXT),
                );
            }
            ui.add_space(shell::DIFF_BODY_VERTICAL_PADDING * scale);
        });
}

fn draw_terminal(ui: &mut egui::Ui, title: &str, text: &str, max_height: f32) {
    let scale = content_scale(ui);
    egui::Frame::NONE
        .fill(rgb(color::TERMINAL))
        .corner_radius(9)
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.spacing_mut().interact_size.y =
                crate::theme::workspace_control_height(shell::type_size::BODY * scale);
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                icon(ui, rect, Icon::Terminal, rgb(color::CHROME_MUTED));
                ui.label(
                    RichText::new(title)
                        .size(9.0 * scale)
                        .color(rgb(color::CHROME_MUTED)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_button(ui, Icon::Copy, "Copy terminal text").clicked() {
                        ui.ctx().copy_text(text.to_owned());
                    }
                });
            });
            egui::ScrollArea::both()
                .max_height(max_height)
                .id_salt(("terminal", title))
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(text)
                                .monospace()
                                .size(10.0 * scale)
                                .color(rgb(color::CHROME_TEXT)),
                        )
                        .selectable(true),
                    );
                });
        });
}

pub(crate) fn draw_message(ui: &mut egui::Ui, message: &model::Message) {
    let scale = content_scale(ui);
    if message.role == model::Role::User {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            ui.spacing_mut().interact_size.y =
                crate::theme::workspace_control_height(shell::type_size::BODY * scale);
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(9.0);
            let (avatar, _) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
            ui.painter()
                .circle_filled(avatar.center(), 12.0, rgb(color::USER_BUBBLE));
            ui.painter().text(
                avatar.center(),
                egui::Align2::CENTER_CENTER,
                &message.avatar,
                egui::FontId::proportional(10.0 * scale),
                rgb(color::ACCENT),
            );
            ui.add_space(10.0);
            egui::Frame::NONE
                .fill(rgb(color::USER_BUBBLE))
                .corner_radius(shell::USER_BUBBLE_RADIUS)
                .inner_margin(10.0)
                .show(ui, |ui| {
                    ui.set_width((shell::USER_BUBBLE_WIDTH - 20.0).min(ui.available_width()));
                    ui.set_min_height((shell::USER_BUBBLE_HEIGHT - 20.0) * scale);
                    for part in &message.parts {
                        if let model::Part::Paragraph(text) = part {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(text)
                                        .size(12.0 * scale)
                                        .color(rgb(color::MESSAGE_USER_TEXT)),
                                )
                                .halign(egui::Align::Min)
                                .wrap()
                                .selectable(true),
                            );
                        }
                    }
                });
        });
        return;
    }
    ui.horizontal_top(|ui| {
        ui.spacing_mut().interact_size.y =
            crate::theme::workspace_control_height(shell::type_size::BODY * scale);
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(8.0);
        let (avatar, _) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
        ui.painter().rect_filled(avatar, 7, rgb(color::ACCENT));
        icon(
            ui,
            avatar.shrink(5.0),
            if message.role == model::Role::Assistant {
                Icon::Agent
            } else {
                Icon::Terminal
            },
            Color32::WHITE,
        );
        ui.add_space(8.0);
        ui.vertical(|ui| {
            ui.set_width(ui.available_width() - 26.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&message.author)
                        .size(shell::type_size::ROLE * scale)
                        .family(egui::FontFamily::Name("reference-chrome-bold".into()))
                        .strong()
                        .color(rgb(color::CANVAS_TEXT)),
                );
                if let Some(timing) = &message.timing {
                    ui.label(
                        RichText::new(format!("· {timing}"))
                            .size(9.0 * scale)
                            .color(rgb(color::CANVAS_MUTED)),
                    );
                }
            });
            ui.add_space(6.0);
            for part in &message.parts {
                match part {
                    model::Part::Heading(text) => {
                        ui.add(
                            egui::Label::new(
                                RichText::new(text)
                                    .size(shell::type_size::HEADLINE * scale)
                                    .family(egui::FontFamily::Name("reference-chrome-bold".into()))
                                    .strong()
                                    .color(rgb(color::CANVAS_TEXT)),
                            )
                            .wrap(),
                        );
                    }
                    model::Part::Paragraph(text) => {
                        ui.add(
                            egui::Label::new(
                                RichText::new(text)
                                    .size(12.0 * scale)
                                    .color(rgb(color::MESSAGE_BODY)),
                            )
                            .wrap()
                            .selectable(true),
                        );
                    }
                    model::Part::Metadata(metadata) => {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = shell::METADATA_GAP;
                            for (index, text) in metadata.iter().enumerate() {
                                pill(
                                    ui,
                                    text,
                                    model::METADATA_ICON_KEYS
                                        .get(index)
                                        .copied()
                                        .unwrap_or("files"),
                                    index + 1 == metadata.len(),
                                );
                            }
                        });
                    }
                    model::Part::Steps(steps) => draw_steps(ui, steps),
                    model::Part::Diff(diff) => draw_diff(ui, diff),
                    model::Part::Terminal { title, text } => draw_terminal(ui, title, text, 130.0),
                }
                ui.add_space(
                    shell::MESSAGE_PART_GAP
                        + if matches!(part, model::Part::Paragraph(_)) {
                            7.0
                        } else {
                            0.0
                        },
                );
            }
        });
    });
}

pub(crate) fn draw_inspector(
    ui: &mut egui::Ui,
    inspector: &model::Inspector,
    fixture: bool,
    scale: f32,
) {
    egui::Frame::NONE
        .fill(rgb(color::INSPECTOR_CARD))
        .corner_radius(12)
        .stroke(Stroke::new(1.0, rgb(color::DARK_BORDER)))
        .inner_margin(13.0)
        .show(ui, |ui| {
            // The inner width already excludes both panel margins and this
            // frame's padding/stroke. A larger hard minimum shifts egui's
            // right-panel clamp and steals two pixels from the inspector.
            ui.set_min_width(ui.available_width());
            ui.set_min_height(573.0);
            ui.spacing_mut().interact_size.y = 18.0;
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = shell::METADATA_GAP;
                let heading_height = shell::INSPECTOR_BADGE_HEIGHT * scale;
                let icon_size = 18.0 * scale;
                let badge_font =
                    egui::FontId::proportional(shell::INSPECTOR_BADGE_FONT_SIZE * scale);
                let badge_text_width = ui
                    .painter()
                    .layout_no_wrap(inspector.badge.clone(), badge_font, rgb(color::CHROME_TEXT))
                    .size()
                    .x;
                let padding_x = shell::INSPECTOR_BADGE_HORIZONTAL_PADDING * scale;
                let padding_y = shell::INSPECTOR_BADGE_VERTICAL_PADDING * scale;
                let badge_width = (badge_text_width + 2.0 * padding_x).min(
                    (ui.available_width() - icon_size - 2.0 * shell::METADATA_GAP - 24.0 * scale)
                        .max(2.0 * padding_x),
                );
                let title_available =
                    (ui.available_width() - icon_size - 2.0 * shell::METADATA_GAP - badge_width)
                        .max(0.0);
                let title_width = ui
                    .painter()
                    .layout_no_wrap(
                        inspector.title.clone(),
                        egui::FontId::proportional(11.0 * scale),
                        rgb(color::CHROME_TEXT),
                    )
                    .size()
                    .x
                    .min(title_available);
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(icon_size, heading_height),
                    egui::Sense::hover(),
                );
                icon(ui, rect, Icon::Agent, rgb(color::ACCENT));
                ui.add_sized(
                    [title_width, heading_height],
                    egui::Label::new(RichText::new(&inspector.title).strong().size(11.0 * scale))
                        .truncate(),
                )
                .on_hover_text(&inspector.title);
                let idle = matches!(
                    inspector.badge.as_str(),
                    "No task" | "Unavailable" | "Unsupported"
                );
                egui::Frame::NONE
                    .fill(rgb(if idle {
                        color::NAVIGATION
                    } else {
                        color::NAVIGATION_SELECTED
                    }))
                    .corner_radius(shell::INSPECTOR_BADGE_RADIUS * scale)
                    .inner_margin(egui::Margin::symmetric(
                        padding_x.round() as i8,
                        padding_y.round() as i8,
                    ))
                    .show(ui, |ui| {
                        ui.add_sized(
                            [
                                (badge_width - 2.0 * padding_x).max(0.0),
                                (heading_height - 2.0 * padding_y).max(0.0),
                            ],
                            egui::Label::new(
                                RichText::new(&inspector.badge)
                                    .size(shell::INSPECTOR_BADGE_FONT_SIZE * scale)
                                    .color(rgb(if idle {
                                        color::CHROME_MUTED
                                    } else {
                                        color::CHROME_TEXT
                                    })),
                            )
                            .truncate(),
                        );
                    })
                    .response
                    .on_hover_text(&inspector.badge);
            });
            ui.label(
                RichText::new(&inspector.subtitle)
                    .size(9.0 * scale)
                    .color(rgb(color::CHROME_MUTED)),
            );
            if fixture {
                ui.label(
                    RichText::new("Visual acceptance fixture")
                        .size(8.0 * scale)
                        .color(rgb(color::CHROME_MUTED)),
                );
            }
            ui.add_space((shell::INSPECTOR_PROGRESS_TARGET_Y - ui.cursor().top()).max(0.0));
            let progress = if inspector.total == 0 {
                0.0
            } else {
                inspector.completed as f32 / inspector.total as f32
            };
            let (progress_row, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 12.0), egui::Sense::hover());
            let track = egui::Rect::from_min_size(
                progress_row.min,
                egui::vec2((progress_row.width() - 62.0).max(0.0), 4.0),
            );
            ui.painter().rect_filled(track, 2, rgb(color::NAVIGATION));
            ui.painter().rect_filled(
                egui::Rect::from_min_size(track.min, egui::vec2(track.width() * progress, 4.0)),
                2,
                rgb(color::ACCENT),
            );
            ui.painter().text(
                egui::pos2(progress_row.right(), track.center().y),
                egui::Align2::RIGHT_CENTER,
                if inspector.total == 0 {
                    shell::NO_PROGRESS_LABEL.to_owned()
                } else {
                    format!("{} / {} steps", inspector.completed, inspector.total)
                },
                egui::FontId::proportional(8.0 * scale),
                rgb(color::CHROME_MUTED),
            );
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space((shell::INSPECTOR_METADATA_TARGET_Y - 4.0 - ui.cursor().top()).max(0.0));
            for (label, value) in &inspector.metadata {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), shell::INSPECTOR_METADATA_ROW_HEIGHT),
                    egui::Sense::hover(),
                );
                ui.painter().text(
                    rect.left_center(),
                    egui::Align2::LEFT_CENTER,
                    label,
                    egui::FontId::proportional(9.0 * scale),
                    rgb(color::CHROME_MUTED),
                );
                let value_rect =
                    egui::Rect::from_min_max(rect.min + egui::vec2(70.0, 0.0), rect.max);
                ui.put(
                    value_rect,
                    egui::Label::new(RichText::new(value).size(9.0 * scale))
                        .halign(egui::Align::Max)
                        .truncate(),
                )
                .on_hover_text(value);
            }
            ui.add_space(10.0);
            ui.label(
                RichText::new("CURRENT STEP")
                    .size(8.0 * scale)
                    .color(rgb(color::CHROME_MUTED)),
            );
            ui.add_space((shell::INSPECTOR_CURRENT_TARGET_Y - ui.cursor().top()).max(0.0));
            egui::Frame::NONE
                .fill(rgb(color::NAVIGATION))
                .corner_radius(8)
                .inner_margin(9.0)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.set_min_height(shell::INSPECTOR_CURRENT_HEIGHT - 18.0);
                    if let Some(step) = &inspector.current_step {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            state_icon(ui, rect, step.state);
                            ui.label(RichText::new(&step.title).size(9.0 * scale));
                        });
                        ui.label(
                            RichText::new(&step.detail)
                                .size(8.0 * scale)
                                .color(rgb(color::CHROME_MUTED)),
                        );
                    } else {
                        ui.label(
                            RichText::new("No current step")
                                .size(9.0 * scale)
                                .color(rgb(color::CHROME_MUTED)),
                        );
                    }
                });
            ui.add_space(13.0);
            ui.label(
                RichText::new("TASK QUEUE")
                    .size(8.0 * scale)
                    .color(rgb(color::CHROME_MUTED)),
            );
            ui.add_space(shell::METADATA_GAP);
            if inspector.queue.is_empty() {
                ui.label(
                    RichText::new("No queued tasks")
                        .size(9.0 * scale)
                        .color(rgb(color::CHROME_MUTED)),
                );
            }
            ui.spacing_mut().item_spacing.y = shell::INSPECTOR_QUEUE_GAP;
            for step in &inspector.queue {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), shell::INSPECTOR_QUEUE_ROW_HEIGHT),
                    egui::Sense::hover(),
                );
                if step.state == model::StepState::Running {
                    ui.painter()
                        .rect_filled(rect, 6, rgb(color::NAVIGATION_SELECTED));
                }
                state_icon(
                    ui,
                    egui::Rect::from_center_size(
                        egui::pos2(rect.left() + 10.0, rect.center().y),
                        egui::vec2(13.0, 13.0),
                    ),
                    step.state,
                );
                let text_rect = egui::Rect::from_min_max(
                    rect.min + egui::vec2(24.0, 0.0),
                    rect.max - egui::vec2(4.0, 0.0),
                );
                let mut title_rect = text_rect;
                if let Some(elapsed) = &step.elapsed {
                    let elapsed_galley = ui.painter().layout_no_wrap(
                        elapsed.clone(),
                        egui::FontId::proportional(8.0 * scale),
                        rgb(color::CHROME_MUTED),
                    );
                    title_rect.max.x =
                        (text_rect.right() - elapsed_galley.size().x - shell::METADATA_GAP)
                            .max(title_rect.left());
                    ui.painter().galley(
                        egui::pos2(
                            text_rect.right() - elapsed_galley.size().x,
                            text_rect.center().y - elapsed_galley.size().y * 0.5,
                        ),
                        elapsed_galley,
                        rgb(color::CHROME_MUTED),
                    );
                }
                // A 24px queue row is a single baseline, not a two-line card.
                // Keep the full detail available without changing row/grid height.
                let mut title_job = egui::text::LayoutJob::simple(
                    step.title.clone(),
                    egui::FontId::proportional(9.0 * scale),
                    rgb(color::CHROME_TEXT),
                    title_rect.width(),
                );
                title_job.wrap.max_rows = 1;
                title_job.wrap.break_anywhere = true;
                title_job.wrap.overflow_character = Some('…');
                let title_galley = ui.painter().layout_job(title_job);
                ui.painter().with_clip_rect(title_rect).galley(
                    egui::pos2(
                        title_rect.left(),
                        title_rect.center().y - title_galley.size().y * 0.5,
                    ),
                    title_galley,
                    rgb(color::CHROME_TEXT),
                );
                let response = ui.interact(
                    rect,
                    ui.id().with(("queue", &step.title)),
                    egui::Sense::hover(),
                );
                response.widget_info(|| {
                    let mut label = format!("{}\n{}", step.title, step.detail);
                    if let Some(elapsed) = &step.elapsed {
                        label.push('\n');
                        label.push_str(elapsed);
                    }
                    egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), label)
                });
                response.on_hover_text(format!("{}\n{}", step.title, step.detail));
            }
            let target = ui.min_rect().top() + 425.0;
            ui.add_space((target - ui.cursor().top()).max(8.0));
            ui.label(RichText::new(&inspector.output_title).size(9.0 * scale));
            egui::Frame::NONE
                .fill(rgb(color::TERMINAL))
                .corner_radius(8)
                .inner_margin(9.0)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.set_min_height(shell::INSPECTOR_OUTPUT_HEIGHT - 18.0);
                    egui::ScrollArea::both()
                        .id_salt("inspector-output")
                        .max_height(shell::INSPECTOR_OUTPUT_HEIGHT - 18.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&inspector.output)
                                        .monospace()
                                        .size(9.0 * scale)
                                        .color(rgb(color::CHROME_MUTED)),
                                )
                                .selectable(true),
                            );
                        });
                });
        });
}

impl TerminalApp {
    pub(crate) fn render_reference_rich_canvas(&mut self, ui: &mut egui::Ui) {
        if !self.config.chat_mode_enabled() || self.layout().panes().len() != 1 {
            return;
        }
        let Some(outer) = self.renderer.workspace_outer_rect else {
            return;
        };
        let Some(content) = self.renderer.last_content_rect else {
            return;
        };
        let Some(start) = self.renderer.canvas_geometry.composer_start else {
            return;
        };
        let composer_top = self
            .renderer
            .shared_composer_rect(content, start..start + 1)
            .top();
        let header = self.reference_layout(ui.ctx()).header;
        let background = egui::Rect::from_min_max(
            egui::pos2(outer.left(), header.y + header.height),
            egui::pos2(outer.right(), composer_top),
        );
        let rect = egui::Rect::from_min_max(
            background.min,
            egui::pos2(background.right(), composer_top - 10.0),
        );
        if !rect.is_positive() {
            return;
        }
        if !model::fixture_enabled() {
            return;
        }
        let identity = self
            .session_manager
            .get_active_session_mut()
            .metadata
            .session_id
            .clone();
        // The fixture document owns its whole canvas background, including
        // its bottom content inset. Leaving that gap unpainted exposes the
        // already-rendered native terminal and its unrelated scrollbar.
        // This changes no PTY cells, native viewport, copy or input ownership.
        crate::reference_workspace::paint_canvas_background(ui.painter(), background);
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.set_clip_rect(rect.intersect(ui.clip_rect()));
            *ui.visuals_mut() = egui::Visuals::light();
            ui.visuals_mut().override_text_color = Some(rgb(color::CANVAS_TEXT));
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 4.0);

            let conversation = model::acceptance_fixture();
            egui::ScrollArea::vertical()
                .id_salt((identity.as_str(), "fixture-rich-canvas"))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let (separator, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), shell::CONVERSATION_TOP_PADDING),
                        egui::Sense::hover(),
                    );
                    ui.painter().text(
                        separator.center(),
                        egui::Align2::CENTER_CENTER,
                        model::FIXTURE_DATE_SEPARATOR,
                        egui::FontId::proportional(8.0 * content_scale(ui)),
                        rgb(color::CANVAS_MUTED),
                    );
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (index, message) in conversation.messages.iter().enumerate() {
                        ui.push_id(index, |ui| draw_message(ui, message));
                        ui.add_space(11.0);
                    }
                });
        });
    }
}

#[cfg(test)]
mod reference_font_tests {
    use super::*;

    fn text_roles(shape: &egui::Shape, roles: &mut Vec<(String, f32, egui::Vec2)>) {
        match shape {
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    text_roles(shape, roles);
                }
            }
            egui::Shape::Text(text) => {
                for section in &text.galley.job.sections {
                    roles.push((
                        text.galley.text().to_owned(),
                        section.format.font_id.size,
                        text.galley.size(),
                    ));
                }
            }
            _ => {}
        }
    }

    #[test]
    fn inspector_painted_roles_follow_ui_font_without_global_zoom() {
        for dpi in [1.0, 1.5] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(dpi);
            // egui applies the requested display scale at the next frame.
            ctx.run_ui(Default::default(), |_| {})
                .drop_without_applying_deltas();
            let initial_zoom = ctx.zoom_factor();
            let mut previous_width = 0.0;
            for ui_size in [12.0, 14.0, 15.0, 18.0] {
                let scale = ui_size / crate::config::DEFAULT_FONT_SIZE;
                let fixture = model::acceptance_fixture().inspector;
                let mut output = ctx.run_ui(Default::default(), |ui| {
                    crate::reference_workspace::chrome_style(ui, scale);
                    ui.set_width(277.0);
                    draw_inspector(ui, &fixture, true, scale);
                });
                let mut roles = Vec::new();
                for shape in &output.shapes {
                    text_roles(&shape.shape, &mut roles);
                }
                // Consume the atlas delta before assertions so their original
                // failure remains readable even in debug/unwind builds.
                output.textures_delta.clear();
                for (name, reference_size) in [
                    ("Canvas renderer", 11.0),
                    ("Visual acceptance fixture", 8.0),
                    ("Model", 9.0),
                    ("CURRENT STEP", 8.0),
                    ("TASK QUEUE", 8.0),
                ] {
                    let role = roles.iter().find(|role| role.0 == name).unwrap();
                    assert_eq!(role.1, reference_size * scale, "painted role: {name}");
                }
                let heading = roles.iter().find(|role| role.0 == "TASK QUEUE").unwrap();
                assert!(heading.2.x > previous_width, "actual galley must grow");
                previous_width = heading.2.x;
                assert_eq!(
                    ctx.zoom_factor(),
                    initial_zoom,
                    "UI zoom must not scale viewport"
                );
            }
        }
    }

    fn content_test_context() -> egui::Context {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let regular = fonts.families[&egui::FontFamily::Proportional].clone();
        fonts.families.insert(
            egui::FontFamily::Name("reference-chrome-bold".into()),
            regular,
        );
        ctx.set_fonts(fonts);
        ctx
    }

    #[test]
    fn painted_content_roles_follow_the_owning_body_without_global_zoom() {
        for dpi in [1.0, 1.25, 1.5] {
            let ctx = content_test_context();
            ctx.set_pixels_per_point(dpi);
            ctx.run_ui(Default::default(), |_| {})
                .drop_without_applying_deltas();
            let fixture = model::acceptance_fixture();
            let initial_zoom = ctx.zoom_factor();
            let mut previous_widths = std::collections::HashMap::new();
            // Default 14 preserves the shared 20/12/10 role hierarchy. The old
            // explicitly configured fixture12 remains a nondefault case.
            for owner_size in [8.0, 12.0, 13.0, 14.0, 15.0, 24.0] {
                let mut output = ctx.run_ui(Default::default(), |ui| {
                    crate::theme::configure_workspace_text(ui.style_mut(), owner_size);
                    ui.set_width(1200.0);
                    for message in &fixture.messages {
                        draw_message(ui, message);
                    }
                });
                let mut roles = Vec::new();
                for shape in &output.shapes {
                    text_roles(&shape.shape, &mut roles);
                }
                output.textures_delta.clear();
                let scale = owner_size / crate::config::DEFAULT_FONT_SIZE;
                let mut selected = Vec::new();
                for (name, reference_size) in [
                    ("One continuous canvas, a stable reading position", 20.0),
                    ("Read files", 9.0),
                    // The shared step role is 10px; keep the independent
                    // painted-shape assertion aligned with that UI contract.
                    ("Inspect the current renderer", 10.0),
                    ("src/canvas.rs", 10.0),
                    ("38", 9.0),
                    ("fn viewport_offset(row: f64) -> f64 {", 10.0),
                ] {
                    let role = roles.iter().find(|role| role.0 == name).unwrap();
                    assert_eq!(role.1, reference_size * scale, "painted role {name}");
                    if let Some(previous) = previous_widths.insert(name, role.2.x) {
                        assert!(role.2.x > previous, "actual galley must grow: {name}");
                    }
                    selected.push(serde_json::json!({"text":role.0,"font":role.1,"extent":[role.2.x,role.2.y]}));
                }
                let body = roles
                    .iter()
                    .find(|role| role.0.starts_with("The message surface keeps"))
                    .unwrap();
                assert_eq!(body.1, 12.0 * scale);
                selected.push(
                    serde_json::json!({"text":body.0,"font":body.1,"extent":[body.2.x,body.2.y]}),
                );
                assert_eq!(ctx.zoom_factor(), initial_zoom);
                eprintln!(
                    "CONTENT_ROLE_MEASURE {}",
                    serde_json::json!({"owner":owner_size,"dpi":dpi,"roles":selected})
                );
            }
        }
    }

    #[test]
    fn sibling_content_uses_its_ui_owner_instead_of_the_central_main_group() {
        let ctx = content_test_context();
        for main_size in [12.0, 16.0] {
            for ui_size in [14.0, 15.0] {
                let mut measured = [0.0; 2];
                let mut output = ctx.run_ui(Default::default(), |ui| {
                    crate::theme::configure_workspace_text(ui.style_mut(), ui_size);
                    ui.scope(|ui| {
                        crate::theme::configure_workspace_text(ui.style_mut(), main_size);
                        measured[0] = content_scale(ui);
                        pill(ui, "Central content", "files", false);
                    });
                    // Like AI Chats' Window, this sibling does not inherit the
                    // central transcript's local Main style.
                    measured[1] = content_scale(ui);
                    pill(ui, "Floating content", "files", false);
                });
                let mut roles = Vec::new();
                for shape in &output.shapes {
                    text_roles(&shape.shape, &mut roles);
                }
                output.textures_delta.clear();
                assert_eq!(
                    measured,
                    [
                        main_size / crate::config::DEFAULT_FONT_SIZE,
                        ui_size / crate::config::DEFAULT_FONT_SIZE
                    ]
                );
                for (label, owner) in [
                    ("Central content", main_size),
                    ("Floating content", ui_size),
                ] {
                    let painted = roles.iter().find(|role| role.0 == label).unwrap();
                    assert_eq!(
                        painted.1,
                        9.0 * (owner / crate::config::DEFAULT_FONT_SIZE),
                        "{label}"
                    );
                }
            }
        }
    }

    #[test]
    fn zoomed_diff_gutters_and_rows_fit_their_actual_text() {
        let ctx = content_test_context();
        let fixture = model::acceptance_fixture();
        let mut diff = fixture
            .messages
            .iter()
            .flat_map(|message| &message.parts)
            .find_map(|part| {
                if let model::Part::Diff(diff) = part {
                    Some(diff.clone())
                } else {
                    None
                }
            })
            .unwrap();
        let mut reference_gutter = 0.0;
        ctx.run_ui(Default::default(), |ui| {
            crate::theme::configure_workspace_text(
                ui.style_mut(),
                crate::config::DEFAULT_FONT_SIZE,
            );
            reference_gutter = diff_gutter_width(ui, &diff, content_scale(ui));
        })
        .drop_without_applying_deltas();
        assert_eq!(
            reference_gutter,
            shell::DIFF_GUTTER_COLUMN_WIDTH,
            "the accepted two-digit reference gutter must not grow"
        );
        for (index, line) in diff.lines.iter_mut().enumerate() {
            line.old_line = Some(999990 + index as u32);
            line.new_line = Some(999990 + index as u32);
        }
        for owner_size in [8.0, 12.0, 24.0, 32.0] {
            let mut gutter = 0.0;
            let mut output = ctx.run_ui(Default::default(), |ui| {
                crate::theme::configure_workspace_text(ui.style_mut(), owner_size);
                ui.set_width(1200.0);
                gutter = diff_gutter_width(ui, &diff, content_scale(ui));
                draw_diff(ui, &diff);
            });
            let mut text_shapes = Vec::new();
            fn collect<'a>(shape: &'a egui::Shape, out: &mut Vec<&'a egui::epaint::TextShape>) {
                match shape {
                    egui::Shape::Vec(shapes) => {
                        for shape in shapes {
                            collect(shape, out);
                        }
                    }
                    egui::Shape::Text(text) => out.push(text),
                    _ => {}
                }
            }
            for shape in &output.shapes {
                collect(&shape.shape, &mut text_shapes);
            }
            // Clone measurements before clearing the output's texture delta.
            let measured: Vec<_> = text_shapes
                .iter()
                .map(|text| (text.galley.text().to_owned(), text.galley.size(), text.pos))
                .collect();
            output.textures_delta.clear();
            let scale = owner_size / crate::config::DEFAULT_FONT_SIZE;
            for (_, extent, _) in measured
                .iter()
                .filter(|(text, _, _)| text.starts_with("99999"))
            {
                assert!(extent.x + 8.0 * scale <= gutter + 0.01);
                assert!(extent.y <= shell::DIFF_LINE_HEIGHT * scale);
            }
            let code: Vec<_> = measured
                .iter()
                .filter(|(text, _, _)| {
                    text.contains("row: f64") || text.contains("let anchor = row;")
                })
                .collect();
            assert_eq!(code.len(), 2);
            for (_, extent, _) in code {
                assert!(extent.y <= shell::DIFF_LINE_HEIGHT * scale);
            }
        }
    }
}
