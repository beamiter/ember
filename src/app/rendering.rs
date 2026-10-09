// Rendering coordination module

use super::state::TerminalApp;
use crate::theme::ThemeExt as _;
use crate::{command_palette, config, config_panel, layout, search_replace_panel, theme};
use eframe::egui;

fn history_picker_rect(screen: egui::Rect) -> egui::Rect {
    let available = screen.shrink(16.0);
    let size = egui::vec2(
        available.width().clamp(1.0, 720.0),
        available.height().clamp(1.0, 520.0),
    );
    let top = (screen.top() + (screen.height() * 0.12).max(16.0))
        .min(available.bottom() - size.y)
        .max(available.top());
    egui::Rect::from_min_size(egui::pos2(screen.center().x - size.x / 2.0, top), size)
}

// egui defaults to a 64px minimum scroll viewport even when max_height is
// smaller. Search reserves its measured footer first, so honor that remainder.
fn block_search_result_scroll_area(max_height: f32) -> egui::ScrollArea {
    egui::ScrollArea::vertical()
        .min_scrolled_height(1.0)
        .max_height(max_height)
}

// Compact labels and wrapped rows must not change keyboard control identity.
fn block_search_control(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    selected: Option<bool>,
) -> egui::Response {
    let id = ui.make_persistent_id(("block-search-control", id));
    ui.scope_builder(egui::UiBuilder::new().id(id), |ui| match selected {
        Some(selected) => ui.selectable_label(selected, label),
        None => ui.button(label),
    })
    .inner
}

/// Fixed-height, clipped rows keep long commands and cwd text in separate
/// lanes. The semantic button owns pointer, keyboard and AccessKit actions.
fn history_picker_row(
    ui: &mut egui::Ui,
    id: egui::Id,
    record: &jterm_core::command_history::CommandHistoryRecord,
    selected: bool,
    current_theme: &theme::Theme,
) -> egui::Response {
    let line_height = ui.text_style_height(&egui::TextStyle::Body);
    let (_, rect) = ui.allocate_space(egui::vec2(ui.available_width(), line_height * 2.0 + 10.0));
    let response = ui.interact(rect, id, egui::Sense::click());
    if selected || response.hovered() {
        let accent = crate::theme::Theme::rgb_to_color32(current_theme.tabbar.active_border);
        ui.painter().rect_filled(
            rect,
            3.0,
            accent.gamma_multiply(if selected { 0.18 } else { 0.08 }),
        );
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            3.0,
            ui.visuals().selection.stroke,
            egui::StrokeKind::Inside,
        );
    }
    let command = crate::history_picker::display_command(&record.command);
    let cwd = record
        .cwd
        .as_deref()
        .map(|cwd| {
            crate::review_text::visible_bounded(&crate::pane_header::abbreviate_home(cwd), 512)
        })
        .unwrap_or_else(|| "Directory not recorded".into());
    let status = if record.exit_code == 0 {
        "Succeeded".to_owned()
    } else {
        format!("Exit {}", record.exit_code)
    };
    let inner = rect.shrink2(egui::vec2(6.0, 4.0));
    let command_rect = egui::Rect::from_min_size(inner.min, egui::vec2(inner.width(), line_height));
    let detail_rect = egui::Rect::from_min_size(
        inner.min + egui::vec2(0.0, line_height + 2.0),
        egui::vec2(inner.width(), line_height),
    );
    ui.place(
        command_rect,
        egui::Label::new(&command)
            .selectable(false)
            .truncate()
            .halign(egui::Align::Min),
    );
    ui.place(
        detail_rect,
        egui::Label::new(
            egui::RichText::new(format!("{status} · {cwd}"))
                .size(10.0)
                .color(ui.visuals().weak_text_color()),
        )
        .selectable(false)
        .truncate()
        .halign(egui::Align::Min),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            ui.is_enabled(),
            selected,
            format!("Recall {command}; {status}; {cwd}; fills prompt only"),
        )
    });
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[derive(Debug, PartialEq, Eq)]
enum HistoryPickerAction {
    Close,
    Fill(String),
}

fn draw_history_picker(
    ctx: &egui::Context,
    state: &mut crate::history_picker::HistoryPickerState,
    current_theme: &theme::Theme,
) -> Option<HistoryPickerAction> {
    let mut accepted_history_command = None;
    let mut hovered_history_index = None;
    let pointer_moved = ctx.input(|input| input.pointer.delta() != egui::Vec2::ZERO);
    {
        let picker_rect = history_picker_rect(ctx.content_rect());
        let picker_height = picker_rect.height();

        egui::Window::new("Command History")
            .title_bar(false)
            .resizable(false)
            .movable(false)
            .fixed_rect(picker_rect)
            .frame(egui::Frame {
                fill: crate::theme::Theme::rgb_to_color32(current_theme.ui.panel_bg),
                stroke: egui::Stroke::new(
                    1.0,
                    crate::theme::Theme::rgb_to_color32(current_theme.ui.border),
                ),
                corner_radius: egui::CornerRadius::same(10),
                inner_margin: egui::Margin::same(8),
                ..Default::default()
            })
            .show(ctx, |ui| {
                // 搜索输入框：编辑即重置高亮（与 frost 的 on_input 一致）。
                ui.horizontal(|ui| {
                    ui.label("↺");
                    let search_response = ui.add_sized(
                        [ui.available_width(), ui.spacing().interact_size.y],
                        egui::TextEdit::singleline(state.query_buffer_mut())
                            .hint_text("Recall a command…"),
                    );
                    if search_response.changed() {
                        state.sync_query();
                    }
                    if state.needs_focus {
                        search_response.request_focus();
                        state.needs_focus = false;
                    }
                });

                ui.separator();

                // Borrow cached visible rows. Only an accepted command needs a
                // copy; repainting long history must not clone every payload.
                let scroll_to_selected = state.take_scroll_to_selected();
                let results = state.filtered();
                let selected_index = state.selected;

                egui::ScrollArea::vertical()
                    .max_height((picker_height - 96.0).max(24.0))
                    .show(ui, |ui| {
                        for (idx, record) in results.iter().enumerate() {
                            let is_selected = idx == selected_index;

                            let click_response = history_picker_row(
                                ui,
                                ui.make_persistent_id(("history-row", state.query(), idx)),
                                record,
                                is_selected,
                                current_theme,
                            );
                            if (is_selected && scroll_to_selected) || click_response.gained_focus()
                            {
                                click_response.scroll_to_me(Some(egui::Align::Center));
                            }
                            if (click_response.hovered() && pointer_moved)
                                || click_response.gained_focus()
                            {
                                hovered_history_index = Some(idx);
                            }
                            if block_search_result_render_activation(&click_response) {
                                accepted_history_command = Some(record.command.clone());
                            }

                            ui.separator();
                        }

                        if results.is_empty() {
                            let hint = if state.query().is_empty() {
                                "No persisted commands yet (recorded via OSC 133 shell integration)"
                            } else {
                                "No commands match"
                            };
                            ui.label(
                                egui::RichText::new(hint).color(ui.visuals().weak_text_color()),
                            );
                        }
                    });

                // 底部提示：Enter 只回填，不执行
                ui.separator();
                ui.add(
                    egui::Label::new(
                        egui::RichText::new("↑↓ Navigate · Enter Fill at Prompt · Esc Cancel")
                            .size(10.0)
                            .color(ui.visuals().weak_text_color()),
                    )
                    .wrap(),
                );
            });
    }

    if let Some(index) = hovered_history_index {
        state.select_hovered(index);
    }
    if state.take_confirm_request() {
        Some(
            state
                .selected_command()
                .map_or(HistoryPickerAction::Close, HistoryPickerAction::Fill),
        )
    } else {
        accepted_history_command.map(HistoryPickerAction::Fill)
    }
}

fn workflow_frame(current_theme: &theme::Theme) -> egui::Frame {
    egui::Frame {
        fill: theme::Theme::rgb_to_color32(current_theme.ui.panel_bg),
        stroke: egui::Stroke::new(1.0, theme::Theme::rgb_to_color32(current_theme.ui.border)),
        corner_radius: egui::CornerRadius::same(10),
        inner_margin: egui::Margin::same(8),
        ..Default::default()
    }
}

#[derive(Debug)]
enum WorkflowPickerAction {
    Close,
    Accept(crate::workflows::Workflow),
}

fn draw_workflow_picker(
    ctx: &egui::Context,
    state: &mut crate::workflow_picker::WorkflowPickerState,
    current_theme: &theme::Theme,
) -> Option<WorkflowPickerAction> {
    let mut accepted = None;
    let mut selected = None;
    let pointer_moved = ctx.input(|input| input.pointer.delta() != egui::Vec2::ZERO);
    let rect = history_picker_rect(ctx.content_rect());
    let window_id = egui::Id::new("Workflows");
    ctx.memory_mut(|memory| {
        memory.set_modal_layer(egui::LayerId::new(egui::Order::Foreground, window_id))
    });
    egui::Window::new("Workflows")
        .id(window_id)
        .order(egui::Order::Foreground)
        .title_bar(false)
        .resizable(false)
        .movable(false)
        .fixed_rect(rect)
        .frame(workflow_frame(current_theme))
        .show(ctx, |ui| {
            let response = ui.add_sized(
                [ui.available_width(), ui.spacing().interact_size.y],
                egui::TextEdit::singleline(state.query_buffer_mut()).hint_text("Search workflows…"),
            );
            if response.changed() {
                state.sync_query();
            }
            if state.needs_focus {
                response.request_focus();
                state.needs_focus = false;
            }
            ui.separator();
            let reveal = state.take_scroll_to_selected();
            egui::ScrollArea::vertical()
                .id_salt("workflow-results")
                .max_height((rect.height() - 96.0).max(1.0))
                .show(ui, |ui| {
                    let results = state.filtered();
                    for (index, workflow) in results.iter().enumerate() {
                        let selected_row = state.selected() == index;
                        let response = workflow_picker_row(
                            ui,
                            ui.make_persistent_id(("workflow-row", state.query(), index)),
                            workflow,
                            selected_row,
                            current_theme,
                        );
                        if (selected_row && reveal) || response.gained_focus() {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }
                        if response.gained_focus() || (response.hovered() && pointer_moved) {
                            selected = Some(index);
                        }
                        if block_search_result_render_activation(&response) {
                            accepted = Some((*workflow).clone());
                        }
                        ui.separator();
                    }
                    if results.is_empty() {
                        let hint = if state.query().is_empty() {
                            let directory = crate::workflows::user_workflow_dir()
                                .map(|path| path.display().to_string())
                                .unwrap_or_else(|| "~/.config/ember/workflows/".into());
                            format!("No workflows yet. Add templates in {directory}")
                        } else {
                            "No workflows match".into()
                        };
                        ui.add(egui::Label::new(hint).wrap());
                    }
                });
            ui.separator();
            ui.add(
                egui::Label::new(
                    egui::RichText::new("↑↓ Navigate · Enter Select · Esc Cancel")
                        .size(10.0)
                        .color(ui.visuals().weak_text_color()),
                )
                .wrap(),
            );
        });
    if let Some(index) = selected {
        state.select(index);
    }
    if state.take_confirm_request() {
        Some(
            state
                .selected_workflow()
                .cloned()
                .map_or(WorkflowPickerAction::Close, WorkflowPickerAction::Accept),
        )
    } else {
        accepted.map(WorkflowPickerAction::Accept)
    }
}

fn workflow_row_label(ui: &mut egui::Ui, rect: egui::Rect, label: egui::Label, align: egui::Align) {
    let mut lane = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(align)),
    );
    lane.set_clip_rect(ui.clip_rect().intersect(rect));
    lane.add(label);
}

fn workflow_picker_row(
    ui: &mut egui::Ui,
    id: egui::Id,
    workflow: &crate::workflows::Workflow,
    selected: bool,
    current_theme: &theme::Theme,
) -> egui::Response {
    let line_height = ui.text_style_height(&egui::TextStyle::Body);
    let (_, rect) = ui.allocate_space(egui::vec2(ui.available_width(), line_height * 2.0 + 10.0));
    let response = ui.interact(rect, id, egui::Sense::click());
    if selected || response.hovered() {
        ui.painter().rect_filled(
            rect,
            3.0,
            theme::Theme::rgb_to_color32(current_theme.tabbar.active_border)
                .gamma_multiply(if selected { 0.18 } else { 0.08 }),
        );
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            3.0,
            ui.visuals().selection.stroke,
            egui::StrokeKind::Inside,
        );
    }
    let name = crate::workflow_picker::display_label(&workflow.name);
    let detail = if workflow.description.is_empty() {
        crate::workflow_picker::display_command_preview(&workflow.command)
    } else {
        crate::workflow_picker::display_label(&workflow.description)
    };
    let tags = crate::workflow_picker::display_label(&workflow.tags.join(", "));
    let inner = rect.shrink2(egui::vec2(6.0, 4.0));
    let tag_width = if tags.is_empty() {
        0.0
    } else {
        (inner.width() * 0.3).min(150.0)
    };
    workflow_row_label(
        ui,
        egui::Rect::from_min_size(
            inner.min,
            egui::vec2((inner.width() - tag_width).max(1.0), line_height),
        ),
        egui::Label::new(egui::RichText::new(&name).strong())
            .selectable(false)
            .truncate()
            .halign(egui::Align::Min),
        egui::Align::Min,
    );
    if tag_width > 0.0 {
        workflow_row_label(
            ui,
            egui::Rect::from_min_size(
                egui::pos2(inner.right() - tag_width, inner.top()),
                egui::vec2(tag_width, line_height),
            ),
            egui::Label::new(
                egui::RichText::new(&tags)
                    .size(10.0)
                    .color(ui.visuals().weak_text_color()),
            )
            .selectable(false)
            .truncate()
            .halign(egui::Align::Max),
            egui::Align::Max,
        );
    }
    workflow_row_label(
        ui,
        egui::Rect::from_min_size(
            inner.min + egui::vec2(0.0, line_height + 2.0),
            egui::vec2(inner.width(), line_height),
        ),
        egui::Label::new(
            egui::RichText::new(&detail)
                .size(10.0)
                .color(ui.visuals().weak_text_color()),
        )
        .selectable(false)
        .truncate()
        .halign(egui::Align::Min),
        egui::Align::Min,
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            ui.is_enabled(),
            selected,
            format!("Workflow {name}; {detail}; {tags}; opens parameters or fills prompt only"),
        )
    });
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[derive(Debug, PartialEq, Eq)]
enum WorkflowArgsAction {
    Cancel,
    Submit,
}

fn draw_workflow_args(
    ctx: &egui::Context,
    state: &mut crate::workflow_picker::WorkflowArgsState,
    current_theme: &theme::Theme,
) -> Option<WorkflowArgsAction> {
    let confirm = state.take_confirm_request();
    // egui schedules backward Tab focus for its next pass. Keep the request
    // until that focus is visible, so Shift+Tab+Enter cannot submit a form
    // while the user's destination is Cancel.
    let defer_focus = confirm
        && ctx.input(|input| {
            input.events.iter().any(|event|
        matches!(event, egui::Event::Key { key: egui::Key::Tab, pressed: true, modifiers, .. }
            if modifiers.shift_only()))
        });
    let confirm = confirm && !defer_focus;
    let mut action = None;
    let available = history_picker_rect(ctx.content_rect());
    let rect = egui::Rect::from_center_size(
        available.center(),
        egui::vec2(available.width().min(560.0), available.height()),
    );
    // Actions stay outside the scrolling reading area, even for all 64 legal arguments.
    let window_id = egui::Id::new("Workflow Parameters");
    ctx.memory_mut(|memory| {
        memory.set_modal_layer(egui::LayerId::new(egui::Order::Foreground, window_id))
    });
    egui::Window::new("Workflow Parameters")
        .id(window_id)
        .order(egui::Order::Foreground)
        .title_bar(false)
        .resizable(false)
        .movable(false)
        .fixed_rect(rect)
        .frame(workflow_frame(current_theme))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("workflow-arguments")
                .min_scrolled_height(1.0)
                .max_height((rect.height() - 88.0).max(1.0))
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!(
                                "Workflow: {}",
                                crate::workflow_picker::display_label(&state.workflow().name)
                            ))
                            .strong(),
                        )
                        .wrap(),
                    );
                    if !state.workflow().description.is_empty() {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(crate::workflow_picker::display_label(
                                    &state.workflow().description,
                                ))
                                .size(10.0),
                            )
                            .wrap(),
                        );
                    }
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(crate::workflow_picker::display_command_preview(
                                &state.workflow().command,
                            ))
                            .monospace()
                            .size(10.0),
                        )
                        .wrap(),
                    );
                    ui.separator();
                    let mut focus_first = std::mem::take(&mut state.needs_focus);
                    for index in 0..state.arg_count() {
                        let required = state.is_missing(index);
                        let narrow = ui.available_width() < 380.0;
                        let row = |ui: &mut egui::Ui| {
                            let Some((arg, value)) = state.row_mut(index) else {
                                return;
                            };
                            let labels = |ui: &mut egui::Ui| {
                                let name = crate::workflow_picker::display_label(&arg.name);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(if required {
                                            format!("{name} *")
                                        } else {
                                            name
                                        })
                                        .size(11.0),
                                    )
                                    .wrap(),
                                );
                                if !arg.description.is_empty() {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(
                                                crate::workflow_picker::display_label(
                                                    &arg.description,
                                                ),
                                            )
                                            .size(9.0)
                                            .color(ui.visuals().weak_text_color()),
                                        )
                                        .wrap(),
                                    );
                                }
                            };
                            if narrow {
                                labels(ui);
                            } else {
                                ui.vertical(|ui| {
                                    ui.set_width(140.0);
                                    labels(ui);
                                });
                            }
                            let response = ui.add_sized(
                                [ui.available_width(), ui.spacing().interact_size.y],
                                egui::TextEdit::singleline(value)
                                    // The app owns confirmation; a refused Enter must
                                    // leave this field ready for keyboard correction.
                                    .return_key(None)
                                    .id_salt(("workflow-argument", index)),
                            );
                            if focus_first {
                                response.request_focus();
                                focus_first = false;
                            }
                            if response.gained_focus() {
                                response.scroll_to_me(Some(egui::Align::Center));
                            } else if response.changed() {
                                response.scroll_to_me(None);
                            }
                        };
                        if narrow {
                            ui.vertical(row);
                        } else {
                            ui.horizontal(row);
                        }
                        ui.add_space(2.0);
                    }
                    state.sync();
                });
            ui.separator();
            ui.horizontal(|ui| {
                let insert = ui.button("Insert command");
                if block_search_result_render_activation(&insert) {
                    action = Some(WorkflowArgsAction::Submit);
                }
                let cancel = ui.button("Cancel");
                // Enter belongs to the focused action after this frame's focus navigation.
                // A picker-opening Enter has no argument-stage request and cannot activate it.
                if block_search_result_render_activation(&cancel) || (confirm && cancel.has_focus())
                {
                    action = Some(WorkflowArgsAction::Cancel);
                }
            });
            if let Some(error) = state.error.as_deref() {
                let error = crate::workflow_picker::display_label(error);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&error).color(egui::Color32::from_rgb(255, 100, 100)),
                    )
                    .truncate(),
                )
                .on_hover_text(error);
            } else {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(
                            "Enter Insert at Prompt · Esc Cancel · * needs a value",
                        )
                        .size(10.0)
                        .color(ui.visuals().weak_text_color()),
                    )
                    .wrap(),
                );
            }
        });
    if defer_focus && action.is_none() {
        state.request_confirm();
        ctx.request_repaint();
    }
    action.or(if confirm {
        Some(WorkflowArgsAction::Submit)
    } else {
        None
    })
}

const MIN_FRAME_BUDGET: usize = 16 * 1024;
const MAX_FRAME_BUDGET: usize = 256 * 1024;
const TARGET_PARSE_TIME: std::time::Duration = std::time::Duration::from_millis(4);
const MIN_ADAPTIVE_SAMPLE_BYTES: usize = 4 * 1024;

// Keep this height independent of selection, status and command length. A
// toolbar state change must never reflow the PTY or move a pointer hit target.
const BLOCK_WORKSPACE_HEIGHT: f32 = 64.0;
const BLOCK_WORKSPACE_ROW_HEIGHT: f32 = 24.0;

#[derive(Clone, Default)]
struct BlockWorkspaceFocus {
    controls: Vec<egui::Id>,
    control_rects: Vec<egui::Rect>,
    popups: Vec<egui::Id>,
}

fn block_workspace_focus_id() -> egui::Id {
    egui::Id::new("block-workspace-focus")
}

/// Keyboard ownership is intentionally separate from terminal pointer
/// interaction. Clicking the terminal must still be able to leave the toolbar.
pub(crate) fn block_workspace_has_focus(ctx: &egui::Context) -> bool {
    let Some(state) =
        ctx.data(|data| data.get_temp::<BlockWorkspaceFocus>(block_workspace_focus_id()))
    else {
        return false;
    };
    let focused = ctx.memory(|memory| memory.focused());
    state.controls.iter().any(|id| {
        focused == Some(*id)
            || ctx.input(|input| {
                input.has_accesskit_action_request(*id, egui::accesskit::Action::Focus)
                    || input.has_accesskit_action_request(*id, egui::accesskit::Action::Click)
            })
    }) || state
        .popups
        .iter()
        .any(|id| egui::Popup::is_id_open(ctx, *id))
        || ctx.input(|input| {
            input.pointer.button_pressed(egui::PointerButton::Primary)
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|pos| state.control_rects.iter().any(|rect| rect.contains(pos)))
        })
}

fn block_workspace_has_room(available: egui::Vec2, line_height: f32) -> bool {
    available.x >= 180.0 && available.y >= BLOCK_WORKSPACE_HEIGHT + line_height.max(1.0) * 4.0
}

#[derive(Clone, Debug, Default)]
struct BlockWorkspaceSnapshot {
    session_id: String,
    completed_count: usize,
    selected_count: usize,
    active_record_id: Option<String>,
    command_preview: String,
    status: String,
    failed: bool,
    bookmarked: bool,
    collapsed: bool,
    collapse_available: bool,
    in_history: bool,
    unseen_completed: usize,
    prompt_ready: bool,
    has_prompt_marks: bool,
    read_only: bool,
    alternate_screen: bool,
    search_shortcut: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum BlockWorkspaceAction {
    Command(crate::keybindings::Command),
    Target(crate::block_mode::BlockMenuAction),
    Deselect,
    Live,
    Review,
}

/// Scope selection to the focused session; a selection left in another pane
/// is never described as the current toolbar's batch.
fn block_workspace_selection<'a>(
    selection: Option<&'a crate::block_mode::BlockSelection>,
    session_id: &str,
) -> Option<&'a crate::block_mode::BlockSelection> {
    selection.filter(|selection| selection.session_id == session_id)
}

pub(crate) fn block_workspace_status(
    record: &crate::terminal::CommandRecord,
    newest: bool,
) -> (String, bool) {
    use crate::block_mode::BlockOutcome;
    let outcome = crate::block_mode::classify_outcome(
        record.command.as_deref(),
        record.command_truncated,
        record.exit_code,
        record.state,
        record.complete,
        newest,
    );
    let label = match outcome {
        BlockOutcome::Prompt => "Prompt ready".to_owned(),
        BlockOutcome::Running => "Running".to_owned(),
        BlockOutcome::Background => "Background output".to_owned(),
        BlockOutcome::Success => "Succeeded".to_owned(),
        BlockOutcome::Failed(code) => format!("Failed · exit {code}"),
        BlockOutcome::Unknown => "Exit status unknown".to_owned(),
    };
    let duration = if outcome == BlockOutcome::Running {
        record
            .started_at
            .and_then(|start| start.elapsed().ok())
            .map(|elapsed| elapsed.as_millis().min(u64::MAX as u128) as u64)
    } else {
        record.duration_ms
    };
    let label = if let Some(duration) = duration {
        format!(
            "{label} · {}",
            crate::block_mode::format_block_duration(duration)
        )
    } else {
        label
    };
    (label, matches!(outcome, BlockOutcome::Failed(_)))
}

fn block_workspace_control(
    ui: &mut egui::Ui,
    focus: &mut BlockWorkspaceFocus,
    label: &str,
    enabled: bool,
    tooltip: &str,
) -> egui::Response {
    let response = ui
        .push_id(label, |ui| {
            ui.add_enabled(
                enabled,
                egui::Button::new(label).min_size(egui::vec2(0.0, 24.0)),
            )
        })
        .inner;
    focus.controls.push(response.id);
    if response.enabled() {
        focus.control_rects.push(response.rect);
    }
    response
        .on_hover_text(tooltip)
        .on_disabled_hover_text(tooltip)
}

fn block_workspace_menu_item(
    ui: &mut egui::Ui,
    focus: &mut BlockWorkspaceFocus,
    action: &mut Option<BlockWorkspaceAction>,
    label: &str,
    enabled: bool,
    tooltip: &str,
    choice: BlockWorkspaceAction,
) {
    if block_workspace_control(ui, focus, label, enabled, tooltip).clicked() {
        *action = Some(choice);
        ui.close();
    }
}

fn block_workspace_selection_menu(
    ui: &mut egui::Ui,
    focus: &mut BlockWorkspaceFocus,
    snapshot: &BlockWorkspaceSnapshot,
    action: &mut Option<BlockWorkspaceAction>,
) {
    use crate::block_mode::BlockMenuAction as Target;
    let valid = snapshot.active_record_id.is_some();
    block_workspace_menu_item(
        ui,
        focus,
        action,
        "Review selected blocks",
        valid,
        "Inspect full commands, captured output and provenance in terminal order",
        BlockWorkspaceAction::Review,
    );
    ui.set_min_width(220.0);
    ui.label(egui::RichText::new(format!("{} selected", snapshot.selected_count)).strong());
    if snapshot.unseen_completed > 0 {
        ui.strong(format!(
            "{} new completions while reading",
            snapshot.unseen_completed
        ));
    }
    if !snapshot.command_preview.is_empty() {
        ui.add(egui::Label::new(&snapshot.command_preview).wrap());
    }
    if !snapshot.status.is_empty() {
        ui.weak(&snapshot.status);
    }
    ui.separator();
    for (label, choice, tooltip) in [
        (
            "Copy commands",
            Target::CopyCommands,
            "Copy selected commands in terminal order",
        ),
        (
            "Copy output",
            Target::CopyOutputs,
            "Copy output from the selected blocks",
        ),
        (
            "Copy blocks",
            Target::CopyBlocks,
            "Copy selected commands and output as plain text",
        ),
        (
            "Copy as Markdown",
            Target::CopyMarkdown,
            "Copy the selection as a Markdown document",
        ),
    ] {
        block_workspace_menu_item(
            ui,
            focus,
            action,
            label,
            valid,
            tooltip,
            BlockWorkspaceAction::Target(choice),
        );
    }
    ui.separator();
    block_workspace_menu_item(ui, focus, action, "Fill prompt", valid && !snapshot.read_only,
        "Insert selected commands at an empty prompt for review. Nothing is executed. Safe replay checks still apply.",
        BlockWorkspaceAction::Review);
    block_workspace_menu_item(
        ui,
        focus,
        action,
        if snapshot.bookmarked {
            "Remove bookmark"
        } else {
            "Bookmark active block"
        },
        valid,
        "Toggle the bookmark on the active block in this selection",
        BlockWorkspaceAction::Target(Target::ToggleBookmark),
    );
    block_workspace_menu_item(ui, focus, action,
        if snapshot.collapsed { "Expand active output" } else { "Collapse active output" },
        valid && (snapshot.collapsed || snapshot.collapse_available),
        "Collapse or expand exact retained output for the active block; terminal output is preserved",
        BlockWorkspaceAction::Target(if snapshot.collapsed { Target::ExpandOutput } else { Target::CollapseOutput }));
    for (label, choice) in [
        ("Go to block start", Target::ScrollTop),
        ("Go to block end", Target::ScrollBottom),
    ] {
        block_workspace_menu_item(
            ui,
            focus,
            action,
            label,
            valid,
            "Reveal the active block's edge in the terminal",
            BlockWorkspaceAction::Target(choice),
        );
    }
    ui.separator();
    block_workspace_menu_item(
        ui,
        focus,
        action,
        "Deselect blocks",
        true,
        "Clear the selection and return keyboard control to the terminal",
        BlockWorkspaceAction::Deselect,
    );
    block_workspace_menu_item(
        ui,
        focus,
        action,
        "Return to live terminal",
        true,
        "Clear selection and follow the newest output",
        BlockWorkspaceAction::Live,
    );
}

fn block_workspace_overflow(
    ui: &mut egui::Ui,
    focus: &mut BlockWorkspaceFocus,
    snapshot: &BlockWorkspaceSnapshot,
    action: &mut Option<BlockWorkspaceAction>,
) {
    use crate::keybindings::Command;
    ui.set_min_width(220.0);
    ui.weak("Focused pane");
    if snapshot.unseen_completed > 0 {
        ui.strong(format!(
            "{} new completions while reading",
            snapshot.unseen_completed
        ));
    }
    for (label, command, needs_blocks, tooltip) in [
        (
            "Search blocks",
            Command::BlockSearchToggle,
            false,
            "Find commands and captured output in this session",
        ),
        (
            "Previous block",
            Command::BlockSelectPrev,
            true,
            "Select and reveal an older completed block",
        ),
        (
            "Next block",
            Command::BlockSelectNext,
            true,
            "Select and reveal a newer completed block",
        ),
        (
            "First failed command",
            Command::BlockJumpFirstFailed,
            true,
            "Jump to the oldest retained failed command",
        ),
        (
            "Previous bookmark",
            Command::BlockJumpPrevBookmark,
            true,
            "Navigate to an older bookmarked command",
        ),
        (
            "Next bookmark",
            Command::BlockJumpNextBookmark,
            true,
            "Navigate to a newer bookmarked command",
        ),
        (
            "Select all blocks",
            Command::BlockSelectAll,
            true,
            "Select completed blocks in this pane; live input stays separate",
        ),
    ] {
        block_workspace_menu_item(
            ui,
            focus,
            action,
            label,
            !needs_blocks || snapshot.completed_count > 0,
            tooltip,
            BlockWorkspaceAction::Command(command),
        );
    }
    block_workspace_menu_item(
        ui,
        focus,
        action,
        "Return to live terminal",
        true,
        "Clear block selection and follow the newest terminal output",
        BlockWorkspaceAction::Live,
    );
    ui.separator();
    for (label, command) in [
        (
            "Export session as Markdown",
            Command::BlockExportSessionMarkdown,
        ),
        ("Export session as JSON", Command::BlockExportSessionJson),
    ] {
        block_workspace_menu_item(
            ui,
            focus,
            action,
            label,
            snapshot.completed_count > 0,
            "Export retained completed blocks to a private file",
            BlockWorkspaceAction::Command(command),
        );
    }
    ui.separator();
    block_workspace_menu_item(ui, focus, action, "Clear completed blocks", snapshot.completed_count > 0,
        "Clear completed block cards in this pane. Terminal text remains, and Undo clear restores the cards.",
        BlockWorkspaceAction::Command(Command::BlockClear));
    block_workspace_menu_item(
        ui,
        focus,
        action,
        "Undo clear",
        true,
        "Restore the most recently cleared blocks in this pane",
        BlockWorkspaceAction::Command(Command::BlockUndoClear),
    );
    ui.separator();
    block_workspace_menu_item(
        ui,
        focus,
        action,
        "Block appearance settings",
        true,
        "Open Settings to change block mode and compact spacing",
        BlockWorkspaceAction::Command(Command::ConfigOpen),
    );
}

/// Paint two fixed rows into an already allocated rectangle. Long text is
/// truncated; controls move into menus before they can overlap terminal cells.
fn draw_block_workspace(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    snapshot: &BlockWorkspaceSnapshot,
    focus: &mut BlockWorkspaceFocus,
    accent: egui::Color32,
) -> Option<BlockWorkspaceAction> {
    use crate::block_mode::BlockMenuAction as Target;
    use crate::keybindings::Command;
    let mut action = None;
    let inner = rect.shrink2(egui::vec2(8.0, 6.0));
    let roomy = inner.width() >= 650.0;
    let compact = inner.width() < 340.0;
    for row in 0..2 {
        let row_rect = egui::Rect::from_min_size(
            inner.min + egui::vec2(0.0, row as f32 * (BLOCK_WORKSPACE_ROW_HEIGHT + 4.0)),
            egui::vec2(inner.width(), BLOCK_WORKSPACE_ROW_HEIGHT),
        );
        let mut row_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(("block-workspace-row", row))
                .max_rect(row_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        row_ui.set_clip_rect(rect.intersect(ui.clip_rect()));
        row_ui.spacing_mut().item_spacing.x = 6.0;
        let ui = &mut row_ui;
        if row == 0 {
            ui.label(egui::RichText::new("Blocks").strong().color(accent))
                .on_hover_text("Command history for the focused pane");
            if roomy {
                ui.weak(format!("{} retained", snapshot.completed_count));
                ui.separator();
            }
            let search_tip = if snapshot.search_shortcut.is_empty() {
                "Search command text and captured output".to_owned()
            } else {
                format!(
                    "Search command text and captured output · {}",
                    snapshot.search_shortcut
                )
            };
            if block_workspace_control(ui, focus, "Search", true, &search_tip).clicked() {
                action = Some(BlockWorkspaceAction::Command(Command::BlockSearchToggle));
            }
            if roomy {
                for (label, command, tip) in [
                    (
                        "Previous",
                        Command::BlockSelectPrev,
                        "Select an older completed block",
                    ),
                    (
                        "Next",
                        Command::BlockSelectNext,
                        "Select a newer completed block",
                    ),
                ] {
                    if block_workspace_control(ui, focus, label, snapshot.completed_count > 0, tip)
                        .clicked()
                    {
                        action = Some(BlockWorkspaceAction::Command(command));
                    }
                }
            }
            if !compact
                && block_workspace_control(
                    ui,
                    focus,
                    if snapshot.in_history || snapshot.selected_count > 0 {
                        "Go live"
                    } else {
                        "Live"
                    },
                    snapshot.in_history || snapshot.selected_count > 0,
                    "Return to the newest output and clear block selection. No command is run.",
                )
                .clicked()
            {
                action = Some(BlockWorkspaceAction::Live);
            }
            let response = block_workspace_control(
                ui,
                focus,
                "More",
                true,
                "Block navigation, bookmarks, session export and appearance",
            );
            focus
                .popups
                .push(egui::Popup::default_response_id(&response));
            egui::Popup::menu(&response)
                .show(|ui| block_workspace_overflow(ui, focus, snapshot, &mut action));
            if roomy {
                let status = if snapshot.in_history {
                    "Browsing history"
                } else if snapshot.read_only {
                    "Retained session"
                } else if snapshot.prompt_ready {
                    "Prompt ready"
                } else {
                    "Live terminal"
                };
                let status = if snapshot.unseen_completed > 0 {
                    format!("{status} · {} new completions", snapshot.unseen_completed)
                } else {
                    status.to_owned()
                };
                ui.add(egui::Label::new(egui::RichText::new(status).weak()).truncate());
            }
        } else if snapshot.alternate_screen {
            ui.add(egui::Label::new(egui::RichText::new("Full-screen app · Block tools resume at the shell").weak()).truncate())
                .on_hover_text("Block controls are paused while this pane is using the alternate screen. Other panes keep their current size.");
        } else if snapshot.selected_count > 0 {
            ui.label(
                egui::RichText::new(format!("{} selected", snapshot.selected_count))
                    .strong()
                    .color(accent),
            )
            .on_hover_text("Actions apply to the selected completed blocks in this pane");
            let valid = snapshot.active_record_id.is_some();
            if inner.width() >= 300.0
                && block_workspace_control(
                    ui,
                    focus,
                    "Review",
                    valid,
                    "Inspect selected command blocks without running anything",
                )
                .clicked()
            {
                action = Some(BlockWorkspaceAction::Review);
            }
            if inner.width() >= 480.0
                && block_workspace_control(
                    ui,
                    focus,
                    "Copy",
                    valid,
                    "Copy selected commands and output as plain text",
                )
                .clicked()
            {
                action = Some(BlockWorkspaceAction::Target(Target::CopyBlocks));
            }
            let response = block_workspace_control(
                ui,
                focus,
                "Actions",
                true,
                "Copy formats, fill prompt, bookmarks and active-block output",
            );
            focus
                .popups
                .push(egui::Popup::default_response_id(&response));
            egui::Popup::menu(&response)
                .show(|ui| block_workspace_selection_menu(ui, focus, snapshot, &mut action));
            if !compact
                && block_workspace_control(
                    ui,
                    focus,
                    "Deselect",
                    true,
                    "Clear the block selection and return keyboard control to the terminal",
                )
                .clicked()
            {
                action = Some(BlockWorkspaceAction::Deselect);
            }
            if roomy {
                let status = if valid {
                    snapshot.status.as_str()
                } else {
                    "Selection no longer retained"
                };
                let color = if snapshot.failed {
                    ui.visuals().error_fg_color
                } else {
                    ui.visuals().text_color()
                };
                ui.add(egui::Label::new(egui::RichText::new(status).color(color)).truncate())
                    .on_hover_text(format!("{status}\n{}", snapshot.command_preview));
                if inner.width() >= 960.0 {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&snapshot.command_preview)
                                .monospace()
                                .small(),
                        )
                        .truncate(),
                    )
                    .on_hover_text(&snapshot.command_preview);
                }
            }
        } else {
            let hint = if snapshot.read_only {
                "Retained session · Select a block to copy or export"
            } else if !snapshot.has_prompt_marks {
                "Waiting for shell integration"
            } else if snapshot.completed_count == 0 {
                "Run a command to create your first block"
            } else if snapshot.in_history {
                "Browsing history · Go live to follow new output"
            } else {
                "Select a block for copy, bookmarks and safe reuse"
            };
            if roomy && !snapshot.status.is_empty() {
                let color = if snapshot.failed {
                    ui.visuals().error_fg_color
                } else {
                    ui.visuals().text_color()
                };
                ui.label(egui::RichText::new(&snapshot.status).color(color));
                ui.separator();
            }
            let hint = if snapshot.unseen_completed > 0 {
                format!(
                    "{} new completions · Go live when ready",
                    snapshot.unseen_completed
                )
            } else {
                hint.to_owned()
            };
            ui.add(egui::Label::new(egui::RichText::new(hint).weak()).truncate())
                .on_hover_text("Click a command header to select it; use the left ⋯ button for actions. Shift-click extends a range; Ctrl+Shift-click toggles a block. Block cards require OSC 133 shell integration.");
        }
    }
    action
}

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

fn block_search_bookmarks_have_indexed_text(
    cache: &[crate::block_mode::CachedBlockSearchRecord],
    live_record_sequences: &std::collections::HashMap<String, u64>,
    bookmarked_sequences: &std::collections::HashSet<u64>,
    scope: crate::block_mode::BlockSearchScope,
) -> bool {
    cache.iter().any(|record| {
        live_record_sequences
            .get(&record.record_id)
            .is_some_and(|sequence| bookmarked_sequences.contains(sequence))
            && super::commands::metadata_browse_display(record, scope).is_some()
    })
}

fn block_search_bookmark_accessible_label(
    hit: &crate::block_mode::BlockSearchHit,
    index: usize,
    hit_count: usize,
    bookmarked: bool,
) -> String {
    let action = if bookmarked {
        "Remove bookmark from"
    } else {
        "Bookmark"
    };
    let context = if hit.is_output_line {
        let owner = if hit.command_preview.is_empty() {
            "a commandless block"
        } else {
            hit.command_preview.as_str()
        };
        hit.line_no.map_or_else(
            || format!("output for {owner}"),
            |line_no| format!("output line {line_no} for {owner}"),
        )
    } else {
        format!("command {}", hit.line_text)
    };
    format!(
        "{action} result {} of {hit_count}; {context}",
        index.saturating_add(1).min(hit_count)
    )
}

/// Paint the compact star while registering one authoritative semantic button
/// node. Using `interact` directly avoids the otherwise duplicated AccessKit
/// click event from a glyph-labelled `Button` followed by a second semantic
/// override.
fn block_search_bookmark_button(
    ui: &mut egui::Ui,
    id: egui::Id,
    rect: egui::Rect,
    bookmarked: bool,
    accessible_label: &str,
) -> egui::Response {
    let response = ui.interact(rect, id, egui::Sense::click());
    let visuals = ui.style().interact_selectable(&response, bookmarked);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        if bookmarked { "★" } else { "☆" },
        egui::TextStyle::Button.resolve(ui.style()),
        visuals.text_color(),
    );
    if response.has_focus() {
        ui.painter()
            .rect_stroke(rect, 2.0, visuals.fg_stroke, egui::StrokeKind::Inside);
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            ui.is_enabled(),
            bookmarked,
            accessible_label,
        )
    });
    response
}

fn block_search_bookmark_owns_selection(response: &egui::Response) -> bool {
    response.has_focus()
        || (response.clicked() && !response.clicked_by(egui::PointerButton::Primary))
}

/// Result Enter/Shift+Enter is owned by the app input prepass. `Response::clicked`
/// also reports egui's keyboard fake click for a focused button, so accepting it
/// here would reveal twice (and Shift+Enter would close on the second reveal).
/// Render owns genuine primary-pointer activation, standard focused-button
/// Space, and a targeted AccessKit Click action; all still flow through the
/// stable hit revalidation path.
fn block_search_result_render_activation(response: &egui::Response) -> bool {
    response.clicked_by(egui::PointerButton::Primary)
        || (response.clicked()
            && response.ctx.input(|input| {
                input.key_pressed(egui::Key::Space)
                    || input
                        .has_accesskit_action_request(response.id, egui::accesskit::Action::Click)
            }))
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

        let clicked = egui::Panel::bottom("bottom_bar")
            .exact_size(jterm_core::bottom_bar::BAR_HEIGHT)
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show(root_ui, |ui| {
                crate::bottom_bar::draw(ui, &self.current_theme, &content)
            })
            .inner;
        if clicked == Some(jterm_core::bottom_bar::SegmentKind::Cwd) {
            self.reveal_files_at_active_cwd();
        }
    }

    fn reveal_files_at_active_cwd(&mut self) {
        let session_idx = self.session_manager.active_index();
        self.reveal_files_at_session_cwd(session_idx);
    }

    fn reveal_files_at_session_cwd(&mut self, session_idx: usize) {
        let Some(session) = self.session_manager.sessions().get(session_idx) else {
            return;
        };
        let reported = session
            .terminal
            .lock()
            .current_working_dir
            .clone()
            .or_else(|| jterm_core::process::process_cwd(session.get_shell_pid()));
        let Some(path) = crate::bottom_bar::local_files_path(reported.as_deref()) else {
            self.set_status("Working directory is not an absolute local path");
            return;
        };
        self.sidebar.note_files_user_intent();
        self.sidebar.visible = true;
        self.sidebar.view = crate::sidebar::SidebarView::Files;
        self.sidebar.set_follow_local_cwd(true);
        if let Some(error) = self
            .sidebar
            .set_location(crate::remote_fs::FsLocation::Local)
        {
            self.set_status_for(
                format!("Files location switch failed: {error}"),
                std::time::Duration::from_secs(5),
            );
            return;
        }
        if let Some(error) = self.sidebar.follow_to_dir(path) {
            self.set_status_for(
                format!("Files directory switch failed: {error}"),
                std::time::Duration::from_secs(5),
            );
        }
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
                    .on_hover_text("Click to show this directory in Files · drag to rearrange");
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
            let click_reveal = self
                .pane_drag
                .as_ref()
                .filter(|drag| !drag.active)
                .and_then(|drag| self.session_manager.index_of(&drag.session_id));
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
            } else if let Some(session_idx) = click_reveal {
                self.reveal_files_at_session_cwd(session_idx);
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

    fn render_block_workspace(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, enabled: bool) {
        self.reading_history.retain_sessions(
            self.session_manager
                .sessions()
                .iter()
                .map(|s| s.metadata.session_id.as_str()),
        );
        let active_index = self.session_manager.active_index();
        let Some(session) = self.session_manager.sessions().get(active_index) else {
            return;
        };
        let terminal = session.terminal.lock();
        if !self.config.block_mode
            || !block_workspace_has_room(ui.available_size(), self.renderer.line_height)
        {
            drop(terminal);
            if block_workspace_has_focus(ctx) {
                self.return_focus_to_terminal(ctx);
            }
            ctx.data_mut(|data| data.remove::<BlockWorkspaceFocus>(block_workspace_focus_id()));
            return;
        }
        let session_id = &session.metadata.session_id;
        let selection = block_workspace_selection(self.block_selection.as_ref(), session_id);
        let records = terminal.command_records();
        let selected_count = selection.map_or(0, |selection| selection.selected_ids.len());
        let retained_ids: std::collections::HashSet<&str> = records
            .iter()
            .filter(|record| record.complete)
            .map(|record| record.id.as_str())
            .collect();
        let active = selection
            .filter(|selection| {
                selection
                    .selected_ids
                    .iter()
                    .all(|id| retained_ids.contains(id.as_str()))
            })
            .and_then(|selection| terminal.command_record(&selection.active_id))
            .filter(|record| record.complete);
        let running = records.back().filter(|record| {
            record.state == crate::terminal::CommandState::Running && !record.complete
        });
        let status_record = active
            .or(running)
            .or_else(|| records.iter().rev().find(|record| record.complete));
        let (status, failed) = status_record.map_or_else(
            || (String::new(), false),
            |record| block_workspace_status(record, active.is_none()),
        );
        if running.is_some() && active.is_none() && !terminal.is_alt_buffer_active() {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        let in_history = if session.projection_policy.is_identity() {
            terminal.scroll_offset > 0
        } else {
            session.projection_view_state.offset_from_bottom() > 0
        };
        let unseen_completed = self.reading_history.update(
            session_id,
            records,
            in_history || selected_count > 0 || self.block_review.is_some(),
        );
        let snapshot = BlockWorkspaceSnapshot {
            unseen_completed,
            session_id: session_id.clone(),
            completed_count: retained_ids.len(),
            selected_count,
            active_record_id: active.map(|record| record.id.clone()),
            command_preview: active
                .and_then(|record| record.command.as_deref())
                .map(|command| {
                    crate::review_text::visible_bounded(command, 240).replace(['\n', '\r'], " ")
                })
                .unwrap_or_else(|| "Background output".to_owned()),
            status,
            failed,
            bookmarked: active.is_some_and(|record| {
                self.block_bookmarks
                    .get(session_id)
                    .is_some_and(|bookmarks| bookmarks.contains(&record.sequence))
            }),
            collapsed: active
                .is_some_and(|record| session.projection_policy.is_collapsed(record.sequence)),
            collapse_available: active
                .is_some_and(|record| terminal.finished_output_range(record.sequence).is_some()),
            in_history: if session.projection_policy.is_identity() {
                terminal.scroll_offset > 0
            } else {
                session.projection_view_state.offset_from_bottom() > 0
            },
            prompt_ready: terminal.shell_is_prompt_ready(),
            has_prompt_marks: terminal.has_prompt_marks(),
            read_only: session.purpose == crate::session::SessionPurpose::RetainedCommand,
            alternate_screen: terminal.is_alt_buffer_active(),
            search_shortcut: self
                .keybindings
                .pretty_bindings_for("block:search")
                .join(" / "),
        };
        drop(retained_ids);
        drop(terminal);
        if snapshot.alternate_screen && self.block_chrome_owns_keyboard(ctx) {
            self.return_focus_to_terminal(ctx);
        }
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), BLOCK_WORKSPACE_HEIGHT),
            egui::Sense::hover(),
        );
        let fill = crate::theme::Theme::rgb_to_color32(self.current_theme.ui.panel_bg);
        let border = crate::theme::Theme::rgb_to_color32(self.current_theme.ui.border);
        let accent = crate::theme::Theme::rgb_to_color32(self.current_theme.tabbar.active_border);
        ui.painter().rect_filled(rect, 0.0, fill);
        ui.painter().hline(
            rect.x_range(),
            rect.bottom(),
            egui::Stroke::new(1.0, border),
        );
        let mut focus = BlockWorkspaceFocus::default();
        let mut toolbar_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(("block-workspace", &snapshot.session_id))
                .max_rect(rect),
        );
        if !enabled || snapshot.alternate_screen {
            toolbar_ui.disable();
        }
        let action = draw_block_workspace(&mut toolbar_ui, rect, &snapshot, &mut focus, accent);
        ctx.data_mut(|data| data.insert_temp(block_workspace_focus_id(), focus));
        let Some(action) = action else {
            return;
        };
        match action {
            BlockWorkspaceAction::Review => self.open_block_review(),
            BlockWorkspaceAction::Command(command) => {
                self.dispatch_command(ctx, command);
            }
            BlockWorkspaceAction::Target(action) => {
                if let Some(record_id) = snapshot.active_record_id {
                    self.execute_block_menu_action(
                        &snapshot.session_id,
                        crate::block_mode::BlockMenuRequest { record_id, action },
                    );
                    if action == crate::block_mode::BlockMenuAction::Reinput
                        && self.block_selection.is_none()
                    {
                        self.return_focus_to_terminal(ctx);
                    }
                }
            }
            BlockWorkspaceAction::Deselect | BlockWorkspaceAction::Live => {
                self.clear_block_selection();
                if action == BlockWorkspaceAction::Live {
                    let session = self.session_manager.get_active_session_mut();
                    session.terminal.lock().scroll_to_bottom();
                    session.projection_view_state.scroll_to_bottom();
                    self.smooth_scroll_velocity = 0.0;
                    self.smooth_scroll_pixel_offset = 0.0;
                    self.renderer.scroll_pixel_offset = 0.0;
                    for renderer in &mut self.pane_renderers {
                        renderer.scroll_pixel_offset = 0.0;
                    }
                }
                self.return_focus_to_terminal(ctx);
            }
        }
        ctx.request_repaint();
    }

    pub fn render_terminal_content(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        frame_pointer_input_blocked: bool,
    ) {
        // Capture keyboard invocation before the modal disables its workspace.
        self.remember_block_review_invoker(ctx);
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
        // Toolbar space is reserved before BOTH pane layout and PTY sizing;
        // renderer hit testing and shell rows therefore use the same geometry.
        self.render_block_workspace(ui, ctx, terminal_interaction_enabled);
        // A toolbar action can open Search/Settings in this very frame. Keep
        // earlier blocking sticky and prevent the remaining terminal render
        // from accepting input beneath the newly opened surface.
        let interaction_enabled = interaction_enabled && !self.terminal_input_blocked(ctx);
        let terminal_interaction_enabled = interaction_enabled && !workspace_drag_active;
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
        self.render_block_review(ctx);
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
                        let search_response = ui.add(
                            egui::TextEdit::singleline(&mut self.search_state.query)
                                .hint_text("Find in terminal"),
                        );

                        // 自动 focus 搜索框
                        if self.search_state.search_focused {
                            ui.memory_mut(|mem| mem.request_focus(search_response.id));
                            self.search_state.search_focused = false;
                        }

                        // Aa / .* 切换按钮:用 selectable_label 表达 on/off 状态。
                        // 切换后需要立刻按新选项重新搜索,否则用户看不到效果。
                        let case_btn = ui
                            .selectable_label(self.search_state.case_sensitive, "Aa")
                            .on_hover_text("Match case");
                        if case_btn.clicked() {
                            self.search_state.case_sensitive = !self.search_state.case_sensitive;
                        }
                        let regex_btn = ui
                            .selectable_label(self.search_state.use_regex, ".*")
                            .on_hover_text("Regular expression");
                        if regex_btn.clicked() {
                            self.search_state.use_regex = !self.search_state.use_regex;
                        }

                        if search_response.changed() || case_btn.clicked() || regex_btn.clicked() {
                            if search_response.changed() {
                                let query = std::mem::take(&mut self.search_state.query);
                                self.search_state.set_query(query);
                            }
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
        let mut accepted_ask_ai = None;
        if self.command_palette.is_open {
            let screen_rect = ctx.viewport_rect();
            let palette_width = (screen_rect.width() - 32.0).clamp(360.0, 720.0);
            let palette_height = (screen_rect.height() - 96.0).clamp(300.0, 520.0);
            let palette_pos = egui::pos2(
                screen_rect.center().x - palette_width / 2.0,
                screen_rect.top() + (screen_rect.height() * 0.12).max(24.0),
            );
            let ask_ai_mode = self.command_palette.ask_ai_mode();
            let ask_ai_request = self.command_palette.ask_ai_request();
            let ask_ai_too_large = self.command_palette.ask_ai_request_too_large();

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
                        ui.label(if ask_ai_mode { "✨" } else { "🔍" });
                        let search_response = ui.add(
                            egui::TextEdit::singleline(&mut self.command_palette.search_query)
                                .hint_text("Search commands... (? asks AI)"),
                        );
                        if search_response.changed() {
                            let query = std::mem::take(&mut self.command_palette.search_query);
                            self.command_palette.set_query(query);
                        }
                        if self.command_palette.needs_focus {
                            search_response.request_focus();
                            self.command_palette.needs_focus = false;
                        }
                    });

                    ui.separator();

                    if ask_ai_mode {
                        // anvil/forge 的 Ask-AI 模式：固定命令列表被替换为一条
                        // AI 行。Enter/点击只起草命令供审阅——绝不直接执行。
                        match ask_ai_request {
                            Some(request) => {
                                let row = ui.horizontal(|ui| {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(150, 150, 255),
                                        "[Terminal]",
                                    );
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!("Ask AI: {request}"))
                                                .strong(),
                                        );
                                        ui.label(
                                            egui::RichText::new(
                                                "Draft a shell command with the configured AI provider; the result is inserted for review only and never runs automatically",
                                            )
                                            .size(10.0)
                                            .color(ui.visuals().weak_text_color()),
                                        );
                                    });
                                });
                                let click_response = ui
                                    .interact(
                                        row.response.rect,
                                        row.response.id.with("palette_ask_ai"),
                                        egui::Sense::click(),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                if click_response.clicked() {
                                    accepted_ask_ai = Some(request);
                                }
                            }
                            None if ask_ai_too_large => {
                                // Fail closed and say so: core would elide the
                                // middle of a request this long, so the draft
                                // would answer an instruction with a hole in it.
                                ui.colored_label(
                                    ui.visuals().error_fg_color,
                                    format!(
                                        "AI request is too large ({} KiB limit)",
                                        crate::command_palette::MAX_AI_QUERY_BYTES / 1024
                                    ),
                                );
                            }
                            None => {
                                ui.label(
                                    egui::RichText::new(
                                        "Describe the command you want after the ? — e.g. ? find large files",
                                    )
                                    .color(ui.visuals().weak_text_color()),
                                );
                            }
                        }
                    } else {
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
                            }

                            // 如果没有结果
                            if results.is_empty() {
                                ui.label(
                                    egui::RichText::new("No commands found")
                                        .color(ui.visuals().weak_text_color()),
                                );
                            }
                        });
                    }

                    // 底部提示
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(if ask_ai_mode {
                                "Enter Draft command  Esc Cancel"
                            } else {
                                "↑↓ Navigate  Enter Execute  Esc Cancel  ·  ? Ask AI"
                            })
                            .size(10.0)
                            .color(ui.visuals().weak_text_color()),
                        );
                    });
                });
        }

        if let Some(index) = hovered_palette_index {
            self.command_palette.selected_index = index;
        }
        if let Some(request) = accepted_ask_ai {
            self.command_palette.close();
            self.start_ai_command_suggestion(request);
        }
        if let Some(command) = clicked_palette_command {
            self.dispatch_palette_command(ctx, command);
        }

        // 历史命令选择器（history:picker）：与命令面板同款的中央浮层。
        // Enter/点击只回填提示符，绝不执行。
        if let Some(state) = self.history_picker.as_mut() {
            if let Some(action) = draw_history_picker(ctx, state, &self.current_theme) {
                self.history_picker = None;
                if let HistoryPickerAction::Fill(command) = action {
                    self.fill_prompt_with_history_command(&command);
                }
            }
        }

        if let Some(state) = self.workflow_picker.as_mut() {
            if let Some(action) = draw_workflow_picker(ctx, state, &self.current_theme) {
                match action {
                    WorkflowPickerAction::Close => self.workflow_picker = None,
                    WorkflowPickerAction::Accept(workflow) => self.workflow_picker_accept(workflow),
                }
            }
        }
        if let Some(state) = self.workflow_args.as_mut() {
            match draw_workflow_args(ctx, state, &self.current_theme) {
                Some(WorkflowArgsAction::Cancel) => self.workflow_args = None,
                Some(WorkflowArgsAction::Submit) => self.submit_workflow_args(),
                None => {}
            }
        }

        // 跨块搜索选择器(block:search):与命令面板同款的中央浮层。
        let mut clicked_hit_index = None;
        let mut clicked_bookmark_target = None;
        let mut clicked_bookmark_preserve_focus = false;
        let mut hovered_hit_index = None;
        let mut focused_hit_index = None;
        let mut restored_bookmark_focus = false;
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
            let bookmarked_sequences = picker_session_id
                .as_deref()
                .and_then(|session_id| self.block_bookmarks.get(session_id))
                .cloned()
                .unwrap_or_default();
            // "Is any bookmark still on a live block?" is the one question the
            // cache cannot answer: it distinguishes a bookmark whose block has
            // been evicted from one whose text simply is not indexed, and the
            // two produce different empty-state messages. Answer it by
            // scanning the deque, which allocates nothing, instead of by
            // materializing every completed record's id.
            let (pane_has_prompt_marks, has_live_bookmarks) = self
                .session_manager
                .sessions()
                .get(active_index)
                .map(|session| {
                    let terminal = session.terminal.lock();
                    let live_bookmarks = picker_session_id.as_deref()
                        == Some(session.metadata.session_id.as_str())
                        && terminal.command_records().iter().any(|record| {
                            record.complete && bookmarked_sequences.contains(&record.sequence)
                        });
                    (terminal.has_prompt_marks(), live_bookmarks)
                })
                .unwrap_or_default();
            let live_record_sequences = std::sync::Arc::clone(&self.block_search.record_sequences);
            let has_bookmarked_indexed_text = block_search_bookmarks_have_indexed_text(
                &self.block_search.cache,
                &live_record_sequences,
                &bookmarked_sequences,
                self.block_search.scope,
            );
            let pane_has_completed_blocks = self
                .block_search
                .record_version
                .is_some_and(|version| version.len > 0);

            let picker_rect = history_picker_rect(ctx.content_rect());
            let compact_controls = picker_rect.height() < 340.0;
            let mut intent_control_focused = false;

            egui::Window::new("Block Search")
                .title_bar(false)
                .resizable(false)
                .movable(false)
                .fixed_rect(picker_rect)
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
                        let search_response = ui.add_sized(
                            [ui.available_width(), ui.spacing().interact_size.y],
                            egui::TextEdit::singleline(&mut self.block_search.query)
                                .hint_text("Search block commands and output…"),
                        );
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
                            self.block_search.needs_bookmark_focus = false;
                        }
                    });

                    // Short windows retain every control and the virtualized
                    // result list. Compact labels keep two control rows usable.
                    ui.horizontal_wrapped(|ui| {
                        if compact_controls {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            ui.spacing_mut().button_padding.x = 3.0;
                        }
                        if !compact_controls {
                            ui.label(egui::RichText::new("Match").small());
                        }
                        let case_button = block_search_control(ui, "match-case", "Aa", Some(self.block_search.case_sensitive))
                            .on_hover_text("Match case");
                        let regex_button = block_search_control(ui, "match-regex", ".*", Some(self.block_search.regex))
                            .on_hover_text("Regular expression");
                        let whole_word_button = block_search_control(ui, "match-word", "W", Some(self.block_search.whole_word))
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
                        let refresh_button = block_search_control(ui, "refresh", if compact_controls { "↻" } else { "Refresh" }, None)
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
                        let reset_button = block_search_control(ui, "reset", if compact_controls { "↺" } else { "Reset" }, None)
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
                        if compact_controls {
                            ui.separator();
                        } else {
                            ui.end_row();
                            ui.label(egui::RichText::new("Scope").small());
                        }
                        for (label, scope) in [
                            ("All", crate::block_mode::BlockSearchScope::All),
                            ("Cmd", crate::block_mode::BlockSearchScope::Command),
                            ("Out", crate::block_mode::BlockSearchScope::Output),
                        ] {
                            let scope_button = block_search_control(ui, ("scope", label), label,
                                Some(self.block_search.scope == scope));
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
                            let display_label = if compact_controls {
                                match filter {
                                    crate::block_search::BlockSearchFilter::Bookmarked => "★",
                                    _ => label,
                                }
                            } else { label };
                            let filter_button = block_search_control(ui, ("filter", label), display_label,
                                Some(self.block_search.filter == filter)).on_hover_text(format!("Block filter: {label}"));
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
                        let label = egui::Label::new(
                            egui::RichText::new(error).small().color(egui::Color32::RED),
                        );
                        ui.add(if compact_controls { label.truncate() } else { label.wrap() })
                            .on_hover_text(error);
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
                    const FULL_HELP: &str = "F5 Refresh  ↑↓ Navigate  Enter Jump  Shift+Enter Jump & Next  Ctrl+Shift+B Bookmark  Ctrl+U Clear  Ctrl+Shift+U Reset  Esc Close";
                    let help = if compact_controls {
                        "↑↓ Navigate · Enter Jump · Esc Close"
                    } else { FULL_HELP };
                    let help_height = ui.painter().layout(
                        help.to_owned(), egui::FontId::proportional(10.0),
                        ui.visuals().weak_text_color(), ui.available_width(),
                    ).size().y;
                    let list_height = (ui.available_height() - help_height
                        - ui.spacing().item_spacing.y * 2.0 - 8.0).max(1.0);
                    let scroll_to_selected =
                        std::mem::take(&mut self.block_search.scroll_to_selected);

                    if hit_count > 0 {
                        let mut scroll = block_search_result_scroll_area(list_height);
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
                                    let response = ui.interact(
                                        row_rect,
                                        ui.make_persistent_id((
                                            "block-search-result",
                                            stable_row_identity,
                                        )),
                                        egui::Sense::click(),
                                    );
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
                                                    let owner = if hit.command_preview.is_empty() {
                                                        "(no command)"
                                                    } else {
                                                        hit.command_preview.as_str()
                                                    };
                                                    hit.line_no.map_or_else(
                                                        || owner.to_string(),
                                                        |line_no| format!("{owner} · L{line_no}"),
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
                                    let bookmark_hover_label = if bookmarked {
                                        "Remove bookmark from this block"
                                    } else {
                                        "Bookmark this block for this running session"
                                    };
                                    let bookmark_accessible_label =
                                        block_search_bookmark_accessible_label(
                                            hit,
                                            idx,
                                            hit_count,
                                            bookmarked,
                                        );
                                    let bookmark_response = block_search_bookmark_button(
                                        ui,
                                        ui.make_persistent_id((
                                            "block-search-bookmark",
                                            stable_row_identity,
                                        )),
                                        bookmark_rect.shrink2(egui::vec2(2.0, 4.0)),
                                        bookmarked,
                                        &bookmark_accessible_label,
                                    )
                                    .on_hover_text(bookmark_hover_label);
                                    if self.block_search.needs_bookmark_focus && is_selected {
                                        bookmark_response.request_focus();
                                        restored_bookmark_focus = true;
                                    }
                                    let bookmark_has_focus = bookmark_response.has_focus();
                                    intent_control_focused |= bookmark_has_focus;
                                    if block_search_bookmark_owns_selection(&bookmark_response) {
                                        // Tab focus and AccessKit focus/Click are
                                        // explicit row choices. Keep picker-wide
                                        // shortcuts, count text and refresh anchors
                                        // aligned with the star being operated.
                                        focused_hit_index = Some(idx);
                                    }
                                    if bookmark_response.clicked() {
                                        clicked_bookmark_target =
                                            Some(self.block_search.bookmark_target(idx));
                                        clicked_bookmark_preserve_focus = !bookmark_response
                                            .clicked_by(egui::PointerButton::Primary);
                                    }

                                    let response = response
                                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                                    let row_accessible_label = if hit.is_output_line {
                                        let owner = if hit.command_preview.is_empty() {
                                            "a commandless block"
                                        } else {
                                            hit.command_preview.as_str()
                                        };
                                        hit.line_no.map_or_else(
                                            || {
                                                format!(
                                                    "Result {} of {}; {}; output for {owner}",
                                                    idx + 1,
                                                    hit_count,
                                                    hit.line_text,
                                                )
                                            },
                                            |line_no| {
                                                format!(
                                                    "Result {} of {}; {}; output line {line_no} for {owner}",
                                                    idx + 1,
                                                    hit_count,
                                                    hit.line_text,
                                                )
                                            },
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
                                    if block_search_result_render_activation(&response) {
                                        clicked_hit_index = Some(idx);
                                    }
                                }
                            },
                        );
                    } else if query_error.is_none() {
                        block_search_result_scroll_area(list_height)
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
                                        has_bookmarked_indexed_text,
                                        self.block_search.scope,
                                    )
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
                    let help_response = ui.add(egui::Label::new(
                        egui::RichText::new(help).size(10.0)
                            .color(ui.visuals().weak_text_color()),
                    ).wrap()).on_hover_text(FULL_HELP);
                    help_response.widget_info(|| egui::WidgetInfo::labeled(
                        egui::WidgetType::Label, true, FULL_HELP,
                    ));
                });
            if restored_bookmark_focus {
                self.block_search.needs_bookmark_focus = false;
            }
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
            let toggled = self.block_search_toggle_bookmark(target);
            if clicked_bookmark_preserve_focus {
                // Keyboard and AccessKit activation stay on the operated star
                // after both success and a stale-target refresh. Under
                // Bookmarked, the stable anchor has already moved to the
                // nearest surviving row; an empty result set falls back to the
                // query editor.
                self.block_search.restore_focus_after_bookmark_activation();
            }
            if toggled || clicked_bookmark_preserve_focus {
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
        // AI chats library panel: harvest streaming replies every frame (also
        // while hidden, so background chats complete and persist), then render.
        self.ai_chat_panel.drive(ctx);
        if self.ai_chat_panel.is_open {
            self.ai_chat_panel.show(ctx, &self.config);
        }
        // Palette `?` AI command suggestion: harvest the reply, render the
        // review card for its bound session, and apply an accept through the
        // same guarded prompt-write path as command correction. The generated
        // command is insert-only; Enter remains the user's own keypress.
        let suggestion_outcome = if let Some(suggestion) = self.ai_command_suggestion.as_mut() {
            suggestion.drive(&self.config, ctx);
            let suggestion_active = self
                .session_manager
                .sessions()
                .get(self.session_manager.active_index())
                .map(|session| session.metadata.session_id.as_str());
            let suggestion_prompt_clean_idle = self
                .session_manager
                .sessions()
                .get(self.session_manager.active_index())
                .map(|session| {
                    let clean = {
                        let terminal = session.terminal.lock();
                        terminal.shell_is_prompt_ready()
                            && !terminal.is_alt_buffer()
                            && terminal.prompt_input_is_empty()
                    };
                    clean && session.pending_input.is_empty()
                })
                .unwrap_or(false);
            suggestion.show(
                ctx,
                &self.config,
                &self.current_theme,
                suggestion_active,
                suggestion_prompt_clean_idle,
            )
        } else {
            crate::ai_command_suggestion::SuggestionUiOutcome::None
        };
        // Dismiss/Escape/✕ must actually remove the card: the session is
        // otherwise cleared only by a *successful* insert or by closing the
        // bound terminal, and `show` re-renders it on the very next frame.
        if matches!(
            suggestion_outcome,
            crate::ai_command_suggestion::SuggestionUiOutcome::Dismissed
        ) {
            self.ai_command_suggestion = None;
        }
        if let crate::ai_command_suggestion::SuggestionUiOutcome::Accepted(effect) =
            suggestion_outcome
        {
            let generation = effect.generation;
            let session_id = effect.session_id.clone();
            let result = self.apply_ai_command_suggestion(&effect);
            if result.is_ok() {
                self.ai_command_suggestion = None;
            } else if let Some(suggestion) = self.ai_command_suggestion.as_mut() {
                // Keep the card open with the refusal inline (anvil keeps the
                // card and shows the reason in place). A newer `?` request may
                // have replaced the session meanwhile; settle only the exact
                // one the effect came from.
                if suggestion.session_id() == session_id {
                    suggestion.complete_accept(generation, result);
                }
            }
        }
        // Review-first command correction: harvest worker replies, enforce the
        // shared deadline, render the active session's card, then apply an
        // accepted decision through the same guarded prompt-write path as
        // history Fill/Run. The card keeps the reason inline on any refusal.
        let correction_agent_active = self.agent_panel.session_active();
        self.command_correction
            .drive(&self.config, correction_agent_active, ctx);
        let (correction_session_id, correction_prompt_clean_idle) = self
            .session_manager
            .sessions()
            .get(self.session_manager.active_index())
            .map(|session| {
                let clean = {
                    let terminal = session.terminal.lock();
                    terminal.shell_is_prompt_ready()
                        && !terminal.is_alt_buffer()
                        && terminal.prompt_input_is_empty()
                };
                (
                    Some(session.metadata.session_id.clone()),
                    clean && session.pending_input.is_empty(),
                )
            })
            .unwrap_or((None, false));
        if let crate::command_correction::CorrectionUiOutcome::Accepted(effect) =
            self.command_correction.show(
                ctx,
                &self.current_theme,
                correction_session_id.as_deref(),
                correction_prompt_clean_idle,
            )
        {
            let session_id = effect.session_id.clone();
            let generation = effect.generation;
            let result = self.apply_command_correction(&effect);
            self.command_correction
                .complete_accept(&session_id, generation, result);
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
        let ssh_retry = self
            .ssh_files_follow
            .retry_available_for_observation(&self.active_ssh_files_observation())
            && (message.contains("Click Retry") || message.contains("configure one and retry"));
        let mut retry_clicked = false;

        egui::Area::new(egui::Id::new("status_toast"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .order(egui::Order::Tooltip)
            .interactable(ssh_retry)
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
                    if ssh_retry
                        && ui
                            .button("Retry Remote Files")
                            .on_hover_text("Re-check the same live SSH process and retry safely")
                            .clicked()
                    {
                        retry_clicked = true;
                    }
                });
            });

        if retry_clicked {
            self.ssh_files_follow.request_retry();
            self.status_message = "Retrying remote Files…".to_string();
            self.status_expires_at =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
        }

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
                ui.heading("⚠ Confirm paste");
                ui.label(
                    egui::RichText::new(format!(
                        "Paste contains {line_count} lines / {byte_len} bytes. Review it before sending:"
                    ))
                    .color(text_color),
                );
                if had_embedded_marker {
                    ui.label(
                        egui::RichText::new(
                            "⚠ The clipboard contained an embedded bracketed-paste terminator (ESC[201~); it was removed. That usually means someone wanted the remaining text executed by the shell.",
                        )
                        .color(text_color),
                    );
                }
                if had_visual_spoofing {
                    ui.label(
                        egui::RichText::new(
                            "⚠ The clipboard contains invisible, bidirectional, or nonstandard whitespace; the preview shows it escaped. This kind of paste always requires confirmation.",
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
                                        egui::RichText::new("… (preview truncated)").color(text_color),
                                    );
                                }
                            });
                    });
                ui.add_space(8.0);
                if !had_visual_spoofing {
                    ui.add_enabled(
                        decision_armed,
                        egui::Checkbox::new(&mut dont_ask_again, "Don't ask again (re-enable in Settings)"),
                    );
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(decision_armed, egui::Button::new("Cancel"))
                        .clicked()
                    {
                        decision = Some(false);
                    }
                    if ui
                        .add_enabled(decision_armed, egui::Button::new("Paste"))
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

    // egui 0.36 debug-asserts when a TexturesDelta with unapplied deltas is
    // dropped. These tests own no texture atlas, so clear it explicitly.
    fn run_frame(
        ctx: &egui::Context,
        input: egui::RawInput,
        f: impl FnMut(&mut egui::Ui),
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(input, f);
        output.textures_delta.clear();
        output
    }

    #[test]
    fn workflow_picker_same_frame_query_edit_selects_latest_match() {
        let ctx = egui::Context::default();
        let mut first = workflow_ui_fixture(0);
        first.name = "choice A".into();
        let mut second = first.clone();
        second.name = "choice B".into();
        second.command = "printf B".into();
        let mut state = crate::workflow_picker::WorkflowPickerState::new(vec![first, second]);
        *state.query_buffer_mut() = "choice ".into();
        state.sync_query();
        let input = |events| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(280.0, 200.0),
            )),
            events,
            ..Default::default()
        };
        for _ in 0..3 {
            run_frame(&ctx, input(vec![]), |_ui| {
                assert!(draw_workflow_picker(&ctx, &mut state, &theme::Theme::default()).is_none());
            });
        }
        state.request_confirm();
        let mut action = None;
        run_frame(&ctx, input(vec![egui::Event::Text("B".into())]), |_ui| {
            action = draw_workflow_picker(&ctx, &mut state, &theme::Theme::default());
        });
        match action {
            Some(WorkflowPickerAction::Accept(workflow)) => {
                assert_eq!(workflow.command, "printf B")
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn workflow_picker_manual_scroll_survives_idle_repaint() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut state = crate::workflow_picker::WorkflowPickerState::new(
            (0..15)
                .map(|index| {
                    let mut workflow = workflow_ui_fixture(0);
                    workflow.name = format!("Choice {index:02}");
                    workflow
                })
                .collect(),
        );
        let mut frame = |events| {
            run_frame(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 300.0),
                    )),
                    events,
                    ..Default::default()
                },
                |_ui| {
                    assert!(
                        draw_workflow_picker(&ctx, &mut state, &theme::Theme::default()).is_none()
                    );
                },
            )
        };
        for _ in 0..3 {
            frame(vec![]);
        }
        let position = egui::pos2(350.0, 120.0);
        frame(vec![egui::Event::PointerMoved(position)]);
        frame(vec![egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            phase: egui::TouchPhase::Move,
            delta: egui::vec2(0.0, -500.0),
            modifiers: egui::Modifiers::NONE,
        }]);
        let mut output = frame(vec![]);
        for _ in 0..30 {
            output = frame(vec![]);
        }
        let nodes = &output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes;
        let first = nodes
            .iter()
            .find(|(_, n)| {
                n.label()
                    .is_some_and(|l| l.starts_with("Workflow Choice 00;"))
            })
            .unwrap()
            .1
            .bounds()
            .unwrap();
        assert!(
            first.y1 < 60.0,
            "idle repaint must not drag the reading position back to selected first row: {first:?}"
        );
    }

    #[test]
    fn workflow_arguments_same_frame_tab_then_confirm_honors_cancel() {
        for reverse in [false, true] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut state = crate::workflow_picker::WorkflowArgsState::new(workflow_ui_fixture(1));
            let mut frame = |events, confirm| {
                if confirm {
                    state.request_confirm();
                }
                let mut action = None;
                let output = run_frame(
                    &ctx,
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(800.0, 640.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        let _ = ui.button("Background action");
                        action = draw_workflow_args(&ctx, &mut state, &theme::Theme::default());
                    },
                );
                (output, action)
            };
            frame(vec![], false);
            frame(vec![], false);
            let output = frame(vec![], false).0;
            if !reverse {
                let insert = output
                    .platform_output
                    .accesskit_update
                    .unwrap()
                    .nodes
                    .iter()
                    .find(|(_, node)| {
                        node.role() == egui::accesskit::Role::Button
                            && node.label() == Some("Insert command")
                    })
                    .unwrap()
                    .0;
                frame(
                    vec![egui::Event::AccessKitActionRequest(
                        egui::accesskit::ActionRequest {
                            action: egui::accesskit::Action::Focus,
                            target_tree: egui::accesskit::TreeId::ROOT,
                            target_node: insert,
                            data: None,
                        },
                    )],
                    false,
                );
            }
            let modifiers = egui::Modifiers {
                shift: reverse,
                ..egui::Modifiers::NONE
            };
            let (_, action) = frame(
                vec![egui::Event::Key {
                    key: egui::Key::Tab,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
                true,
            );
            if reverse {
                assert_eq!(action, None);
                assert_eq!(frame(vec![], false).1, Some(WorkflowArgsAction::Cancel));
            } else {
                assert_eq!(action, Some(WorkflowArgsAction::Cancel));
            }
        }
    }

    #[test]
    fn workflow_picker_assistive_click_preserves_full_command_and_query_identity() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut workflow = workflow_ui_fixture(0);
        workflow.name = "First workflow".into();
        workflow.command = format!("printf {}", "参数🙂".repeat(4000));
        let expected = workflow.command.clone();
        let mut other = workflow_ui_fixture(0);
        other.name = "Second workflow".into();
        let mut state = crate::workflow_picker::WorkflowPickerState::new(vec![workflow, other]);
        let frame = |state: &mut crate::workflow_picker::WorkflowPickerState, events| {
            let mut action = None;
            let output = run_frame(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(280.0, 200.0),
                    )),
                    events,
                    ..Default::default()
                },
                |_ui| {
                    action = draw_workflow_picker(&ctx, state, &theme::Theme::default());
                },
            );
            (output, action)
        };
        frame(&mut state, vec![]);
        frame(&mut state, vec![]);
        let output = frame(&mut state, vec![]).0;
        let node = output
            .platform_output
            .accesskit_update
            .unwrap()
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Button
                    && node
                        .label()
                        .is_some_and(|label| label.starts_with("Workflow First workflow;"))
            })
            .unwrap()
            .0;
        let click = || {
            egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::Click,
                target_tree: egui::accesskit::TreeId::ROOT,
                target_node: node,
                data: None,
            })
        };
        match frame(&mut state, vec![click()]).1 {
            Some(WorkflowPickerAction::Accept(workflow)) => assert_eq!(workflow.command, expected),
            action => panic!("unexpected {action:?}"),
        }
        *state.query_buffer_mut() = "Second".into();
        state.sync_query();
        frame(&mut state, vec![]);
        assert!(
            frame(&mut state, vec![click()]).1.is_none(),
            "stale assistive click must not retarget a different workflow"
        );
    }

    #[test]
    fn workflow_refused_enter_keeps_the_field_ready_for_correction() {
        let ctx = egui::Context::default();
        let mut workflow = workflow_ui_fixture(1);
        workflow.args[0].default = None;
        let mut state = crate::workflow_picker::WorkflowArgsState::new(workflow);
        let input = |events| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 640.0),
            )),
            events,
            ..Default::default()
        };
        for _ in 0..3 {
            run_frame(&ctx, input(vec![]), |_ui| {
                draw_workflow_args(&ctx, &mut state, &theme::Theme::default());
            });
        }
        let field = ctx
            .memory(|memory| memory.focused())
            .expect("argument input focused");
        state.request_confirm();
        let mut action = None;
        run_frame(
            &ctx,
            input(vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]),
            |_ui| {
                action = draw_workflow_args(&ctx, &mut state, &theme::Theme::default());
            },
        );
        assert_eq!(action, Some(WorkflowArgsAction::Submit));
        state.error = Some(state.render().unwrap_err());
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(field));
        run_frame(
            &ctx,
            input(vec![egui::Event::Text("RECOVERED".into())]),
            |_ui| {
                assert_eq!(
                    draw_workflow_args(&ctx, &mut state, &theme::Theme::default()),
                    None
                );
            },
        );
        assert_eq!(state.render().unwrap(), "printf RECOVERED");
        assert!(
            state.error.is_none(),
            "a real edit clears the obsolete refusal"
        );
    }

    #[test]
    fn workflow_refusal_stays_visible_outside_a_long_form() {
        for size in [egui::vec2(280.0, 200.0), egui::vec2(1000.0, 680.0)] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut workflow = workflow_ui_fixture(64);
            for arg in &mut workflow.args {
                arg.default = None;
            }
            workflow.command = format!(
                "printf {}",
                workflow
                    .args
                    .iter()
                    .map(|arg| format!("{{{}}}", arg.name))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            let mut state = crate::workflow_picker::WorkflowArgsState::new(workflow);
            state.error = Some(state.render().unwrap_err());
            let accessible_error =
                crate::workflow_picker::display_label(state.error.as_deref().unwrap());
            assert!(accessible_error.len() > 200, "exercise visual truncation");
            let mut output = None;
            for _ in 0..3 {
                output = Some(run_frame(
                    &ctx,
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        ..Default::default()
                    },
                    |_ui| {
                        draw_workflow_args(&ctx, &mut state, &theme::Theme::default());
                    },
                ));
            }
            let output = output.unwrap();
            let error = output
                .platform_output
                .accesskit_update
                .unwrap()
                .nodes
                .into_iter()
                .find(|(_, node)| {
                    node.value()
                        .is_some_and(|value| value.contains("missing values: arg0"))
                })
                .expect("the refusal remains accessible")
                .1;
            assert_eq!(
                error.value(),
                Some(accessible_error.as_str()),
                "visual ellipsis must preserve the full bounded accessible error"
            );
            let bounds = error.bounds().unwrap();
            assert!(
                bounds.y0 >= 0.0 && bounds.y1 <= size.y as f64,
                "error is outside the viewport: {bounds:?}"
            );
            assert!(
                state.error.is_some(),
                "idle repaint does not erase feedback"
            );
        }
    }

    fn workflow_ui_fixture(count: usize) -> crate::workflows::Workflow {
        crate::workflows::Workflow {
            name: "Review workflow".into(),
            description: "A bounded parameter form".into(),
            command: "printf {arg0}".into(),
            tags: vec!["qa".into()],
            shell: None,
            source_path: None,
            args: (0..count)
                .map(|index| crate::workflows::WorkflowArg {
                    name: format!("arg{index}"),
                    description: format!("Argument {index}"),
                    default: Some("A".into()),
                })
                .collect(),
        }
    }

    #[test]
    fn workflow_arguments_keep_actions_inside_small_and_large_windows() {
        for size in [egui::vec2(280.0, 200.0), egui::vec2(1000.0, 680.0)] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut state = crate::workflow_picker::WorkflowArgsState::new(workflow_ui_fixture(64));
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            let mut output = None;
            for _ in 0..3 {
                output = Some(run_frame(
                    &ctx,
                    egui::RawInput {
                        screen_rect: Some(screen),
                        ..Default::default()
                    },
                    |_ui| {
                        assert_eq!(
                            draw_workflow_args(&ctx, &mut state, &theme::Theme::default()),
                            None
                        );
                    },
                ));
            }
            let output = output.unwrap();
            let nodes = &output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes;
            for label in ["Insert command", "Cancel"] {
                let node = nodes
                    .iter()
                    .find(|(_, node)| {
                        node.role() == egui::accesskit::Role::Button && node.label() == Some(label)
                    })
                    .unwrap();
                let bounds = node.1.bounds().unwrap();
                assert!(
                    bounds.x0 >= 0.0
                        && bounds.y0 >= 0.0
                        && bounds.x1 <= size.x as f64
                        && bounds.y1 <= size.y as f64,
                    "{label} outside {size:?}: {bounds:?}"
                );
            }
        }
    }

    #[test]
    fn workflow_arguments_focused_cancel_enter_returns_cancel() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut state = crate::workflow_picker::WorkflowArgsState::new(workflow_ui_fixture(1));
        let mut frame = |events, confirm| {
            if confirm {
                state.request_confirm();
            }
            let mut action = None;
            let output = run_frame(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 640.0),
                    )),
                    events,
                    ..Default::default()
                },
                |_ui| {
                    action = draw_workflow_args(&ctx, &mut state, &theme::Theme::default());
                },
            );
            (output, action)
        };
        frame(vec![], false);
        frame(vec![], false);
        let output = frame(vec![], false).0;
        let cancel = output
            .platform_output
            .accesskit_update
            .unwrap()
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Button && node.label() == Some("Cancel")
            })
            .unwrap()
            .0;
        let focus = egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            action: egui::accesskit::Action::Focus,
            target_tree: egui::accesskit::TreeId::ROOT,
            target_node: cancel,
            data: None,
        });
        assert_eq!(frame(vec![focus], false).1, None);
        assert_eq!(frame(vec![], true).1, Some(WorkflowArgsAction::Cancel));
    }

    #[test]
    fn workflow_arguments_confirmation_observes_actual_text_edit() {
        let ctx = egui::Context::default();
        let mut state = crate::workflow_picker::WorkflowArgsState::new(workflow_ui_fixture(1));
        let input = |events| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 640.0),
            )),
            events,
            ..Default::default()
        };
        for _ in 0..3 {
            run_frame(&ctx, input(vec![]), |_ui| {
                draw_workflow_args(&ctx, &mut state, &theme::Theme::default());
            });
        }
        state.request_confirm();
        let mut action = None;
        run_frame(&ctx, input(vec![egui::Event::Text("B".into())]), |_ui| {
            action = draw_workflow_args(&ctx, &mut state, &theme::Theme::default());
        });
        assert_eq!(action, Some(WorkflowArgsAction::Submit));
        assert_eq!(state.render().unwrap(), "printf AB");
    }

    #[test]
    fn compact_block_search_controls_preserve_focus_across_resize() {
        let ctx = egui::Context::default();
        let frame = |compact: bool, events, focus: bool| {
            let mut result = None;
            run_frame(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(280.0, 240.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if !compact {
                            ui.label("Match");
                        }
                        let _ = block_search_control(ui, "match-case", "Aa", Some(false));
                        let response = block_search_control(
                            ui,
                            "reset",
                            if compact { "↺" } else { "Reset" },
                            None,
                        );
                        if focus {
                            response.request_focus();
                        }
                        result = Some((response.id, response.has_focus(), response.clicked()));
                    });
                },
            );
            result.unwrap()
        };
        let original = frame(false, vec![], true).0;
        assert!(frame(false, vec![], false).1);
        let resized = frame(true, vec![], false);
        assert_eq!(resized.0, original);
        assert!(
            resized.1,
            "compact labels must not transfer focus to another action"
        );
        let pressed = frame(
            true,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            false,
        );
        assert!(pressed.2, "Enter still activates the same Reset button");
    }

    #[test]
    fn compact_block_search_viewport_stays_bounded_and_virtualized() {
        for height in [1.0, 24.0, 40.0, 120.0] {
            let ctx = egui::Context::default();
            let mut drawn = 0;
            let mut last_visible = false;
            for _ in 0..3 {
                run_frame(
                    &ctx,
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(280.0, 240.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        let output = block_search_result_scroll_area(height)
                            .vertical_scroll_offset(9_999.0 * 44.0)
                            .show_rows(ui, 40.0, 10_000, |ui, range| {
                                drawn = range.len();
                                last_visible = range.contains(&9_999);
                                for _ in range {
                                    ui.allocate_exact_size(
                                        egui::vec2(240.0, 40.0),
                                        egui::Sense::hover(),
                                    );
                                }
                            });
                        assert!(output.inner_rect.height() <= height + 0.1);
                        assert!(ui.label("Search keyboard help").rect.bottom() <= 240.0);
                    },
                );
            }
            assert!(drawn <= 5, "only visible rows may be built: {drawn}");
            assert!(
                last_visible,
                "navigation can still reveal the final indexed result"
            );
        }
    }

    #[test]
    fn history_picker_bounds_follow_narrow_and_short_viewports() {
        for (width, height) in [
            (1000.0, 680.0),
            (360.0, 640.0),
            (360.0, 300.0),
            (280.0, 200.0),
        ] {
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
            let panel = history_picker_rect(screen);
            assert!(screen.contains_rect(panel));
            assert!(panel.width() <= width - 32.0);
            assert!(panel.height() <= height - 32.0);
        }
    }

    #[test]
    fn workflow_picker_row_text_does_not_steal_pointer_activation() {
        let ctx = egui::Context::default();
        let record = workflow_ui_fixture(0);
        let theme = theme::Theme::default();
        let mut rect = egui::Rect::NOTHING;
        let mut clicked = false;
        let mut frame = |events| {
            run_frame(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(360.0, 200.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    ui.set_width(240.0);
                    let response = workflow_picker_row(
                        ui,
                        egui::Id::new("pointer-workflow-row"),
                        &record,
                        false,
                        &theme,
                    );
                    rect = response.rect;
                    clicked = response.clicked_by(egui::PointerButton::Primary);
                },
            );
            (rect, clicked)
        };
        let (row, _) = frame(vec![]);
        frame(vec![]);
        let pos = row.min + egui::vec2(8.0, 8.0);
        frame(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        let (_, clicked) = frame(vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(
            clicked,
            "row labels are display-only; their hit boxes must not intercept the button"
        );
    }

    #[test]
    fn history_picker_row_text_does_not_steal_pointer_activation() {
        let ctx = egui::Context::default();
        let record = jterm_core::command_history::CommandHistoryRecord {
            command: "printf row-click".into(),
            cwd: Some("/work/ember".into()),
            exit_code: 0,
            end_time_ms: None,
        };
        let theme = theme::Theme::default();
        let mut rect = egui::Rect::NOTHING;
        let mut clicked = false;
        let mut frame = |events| {
            run_frame(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(360.0, 200.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    ui.set_width(240.0);
                    let response = history_picker_row(
                        ui,
                        egui::Id::new("pointer-history-row"),
                        &record,
                        false,
                        &theme,
                    );
                    rect = response.rect;
                    clicked = response.clicked_by(egui::PointerButton::Primary);
                },
            );
            (rect, clicked)
        };
        let (row, _) = frame(vec![]);
        frame(vec![]);
        let pos = row.min + egui::vec2(8.0, 8.0);
        frame(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        let (_, clicked) = frame(vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(
            clicked,
            "row labels are display-only; their hit boxes must not intercept the button"
        );
    }

    #[test]
    fn history_picker_row_is_bounded_accessible_and_activates_exact_payload() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let record = jterm_core::command_history::CommandHistoryRecord {
            command: format!("printf {}", "编译🙂".repeat(200)),
            cwd: Some(format!("/{}", "very-long-directory/".repeat(200))),
            exit_code: 101,
            end_time_ms: None,
        };
        let theme = theme::Theme::default();
        let row_id = egui::Id::new("history-test-row");
        let mut clicked = false;
        let render = |ui: &mut egui::Ui| {
            ui.set_width(240.0);
            let response = history_picker_row(ui, row_id, &record, true, &theme);
            assert!(response.rect.width() <= 240.0);
            response.clicked()
        };
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(280.0, 200.0),
            )),
            ..Default::default()
        };
        let output = run_frame(&ctx, input.clone(), |ui| {
            clicked = render(ui);
        });
        assert!(!clicked);
        let node = output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find_map(|(id, node)| {
                (node.role() == egui::accesskit::Role::Button
                    && node
                        .label()
                        .is_some_and(|label| label.starts_with("Recall printf")))
                .then_some((*id, node))
            })
            .expect("row exposes a semantic recall button");
        let bounds = node.1.bounds().unwrap();
        assert!(bounds.x0 >= 0.0 && bounds.x1 <= 280.0);
        assert!(node.1.label().unwrap().contains("fills prompt only"));
        let mut click = input;
        click.events.push(egui::Event::AccessKitActionRequest(
            egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::Click,
                target_tree: egui::accesskit::TreeId::ROOT,
                target_node: node.0,
                data: None,
            },
        ));
        run_frame(&ctx, click, |ui| {
            clicked = render(ui);
        });
        assert!(clicked, "direct assistive activation reaches the real row");
        assert_eq!(
            record.command.len(),
            "printf ".len() + "编译🙂".len() * 200,
            "display never rewrites the accepted payload"
        );
    }

    #[test]
    fn history_picker_assistive_activation_returns_full_review_only_command() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let command = format!("printf {}", "long-argument-".repeat(200));
        let mut state = crate::history_picker::HistoryPickerState::new(vec![
            jterm_core::command_history::CommandHistoryRecord {
                command: command.clone(),
                cwd: Some("/work/ember".into()),
                exit_code: 0,
                end_time_ms: None,
            },
        ]);
        let theme = theme::Theme::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(360.0, 640.0),
            )),
            ..Default::default()
        };
        let mut accepted = None;
        let mut output = run_frame(&ctx, input.clone(), |_ui| {
            accepted = draw_history_picker(&ctx, &mut state, &theme);
        });
        for _ in 0..2 {
            output = run_frame(&ctx, input.clone(), |_ui| {
                accepted = draw_history_picker(&ctx, &mut state, &theme);
            });
        }
        assert!(accepted.is_none());
        let node = output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find_map(|(id, node)| {
                (node.role() == egui::accesskit::Role::Button
                    && node
                        .label()
                        .is_some_and(|label| label.starts_with("Recall printf")))
                .then_some(*id)
            })
            .expect("history result is an accessible button");
        let mut click = input;
        click.events.push(egui::Event::AccessKitActionRequest(
            egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::Click,
                target_tree: egui::accesskit::TreeId::ROOT,
                target_node: node,
                data: None,
            },
        ));
        let output = run_frame(&ctx, click, |_ui| {
            accepted = draw_history_picker(&ctx, &mut state, &theme);
        });
        assert_eq!(
            accepted,
            Some(HistoryPickerAction::Fill(command.clone())),
            "the display ellipsis never enters the returned payload"
        );
        assert!(
            output.platform_output.commands.is_empty(),
            "the picker returns intent only; it cannot execute or copy"
        );
    }

    #[test]
    fn history_picker_row_focus_and_stale_actions_respect_current_query() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut state = crate::history_picker::HistoryPickerState::new(
            ["cargo test", "git status", "pwd"]
                .into_iter()
                .map(
                    |command| jterm_core::command_history::CommandHistoryRecord {
                        command: command.into(),
                        cwd: None,
                        exit_code: 0,
                        end_time_ms: None,
                    },
                )
                .collect(),
        );
        let theme = theme::Theme::default();
        let frame = |state: &mut crate::history_picker::HistoryPickerState, events| {
            let mut accepted = None;
            let output = run_frame(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(360.0, 640.0),
                    )),
                    events,
                    ..Default::default()
                },
                |_ui| {
                    accepted = draw_history_picker(&ctx, state, &theme);
                },
            );
            (output, accepted)
        };
        let mut output = frame(&mut state, vec![]).0;
        for _ in 0..2 {
            output = frame(&mut state, vec![]).0;
        }
        let old_node = output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find_map(|(id, node)| {
                (node.role() == egui::accesskit::Role::Button
                    && node
                        .label()
                        .is_some_and(|label| label.starts_with("Recall cargo test")))
                .then_some(*id)
            })
            .unwrap();
        let action = |kind| {
            egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                action: kind,
                target_tree: egui::accesskit::TreeId::ROOT,
                target_node: old_node,
                data: None,
            })
        };
        assert!(
            frame(&mut state, vec![action(egui::accesskit::Action::Focus)])
                .1
                .is_none()
        );
        state.select_next();
        frame(&mut state, vec![]);
        assert_eq!(
            state.selected_command().as_deref(),
            Some("git status"),
            "old row focus cannot undo keyboard navigation"
        );
        state.set_query("git");
        assert!(
            frame(&mut state, vec![action(egui::accesskit::Action::Click)])
                .1
                .is_none(),
            "a stale click cannot retarget the replacement result at the same row index"
        );
        assert_eq!(state.selected_command().as_deref(), Some("git status"));
    }

    #[test]
    fn history_confirmation_uses_same_frame_text_paste_and_ime_commit() {
        for kind in ["text", "paste", "ime", "no-match", "unsafe"] {
            let ctx = egui::Context::default();
            let mut picker = Some(crate::history_picker::HistoryPickerState::new(
                ["printf EMBER_RACE_A", "printf EMBER_RACE_B"]
                    .into_iter()
                    .map(
                        |command| jterm_core::command_history::CommandHistoryRecord {
                            command: command.into(),
                            cwd: None,
                            exit_code: 0,
                            end_time_ms: None,
                        },
                    )
                    .collect(),
            ));
            picker.as_mut().unwrap().set_query("EMBER_RACE_");
            let theme = theme::Theme::default();
            let input = |events| egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 640.0),
                )),
                events,
                ..Default::default()
            };
            for _ in 0..3 {
                run_frame(&ctx, input(vec![]), |_ui| {
                    assert!(draw_history_picker(&ctx, picker.as_mut().unwrap(), &theme).is_none());
                });
            }
            if kind == "ime" {
                run_frame(
                    &ctx,
                    input(vec![egui::Event::Ime(egui::ImeEvent::Preedit {
                        text: "B".into(),
                        active_range_chars: Some(0..1),
                    })]),
                    |_ui| {
                        assert!(
                            draw_history_picker(&ctx, picker.as_mut().unwrap(), &theme).is_none()
                        );
                    },
                );
            }
            let edit = match kind {
                "paste" => egui::Event::Paste("B".into()),
                "ime" => egui::Event::Ime(egui::ImeEvent::Commit("B".into())),
                "no-match" => egui::Event::Text("MISSING".into()),
                "unsafe" => egui::Event::Text("\u{202e}".into()),
                _ => egui::Event::Text("B".into()),
            };
            let events = vec![
                edit,
                egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ];
            let mut routed = events.clone();
            let mut held = super::super::input::PromptFillEnterLatch::default();
            assert!(super::super::input::route_history_picker_events(
                &mut picker,
                &mut held,
                &mut routed
            ));
            let mut action = None;
            let output = run_frame(&ctx, input(events), |_ui| {
                action = draw_history_picker(&ctx, picker.as_mut().unwrap(), &theme);
            });
            let expected = if matches!(kind, "no-match" | "unsafe") {
                HistoryPickerAction::Close
            } else {
                HistoryPickerAction::Fill("printf EMBER_RACE_B".into())
            };
            assert_eq!(
                action,
                Some(expected),
                "{kind} must be applied before confirming"
            );
            assert!(output.platform_output.commands.is_empty());
            assert_eq!(held, super::super::input::PromptFillEnterLatch::Held);
            assert!(!picker.as_mut().unwrap().take_confirm_request());
        }
    }

    #[test]
    fn history_picker_offscreen_layout_snapshots() {
        let destination = std::env::var_os("EMBER_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        if let Some(path) = &destination {
            std::fs::create_dir_all(path).unwrap();
        }
        for (name, width, height) in [
            ("history-wide", 1000, 680),
            ("history-narrow", 360, 640),
            ("history-short", 280, 200),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let theme = theme::Theme::default();
            crate::apply_theme_visuals(&ctx, &theme);
            let mut state = crate::history_picker::HistoryPickerState::new(
                (0..15)
                    .map(|index| jterm_core::command_history::CommandHistoryRecord {
                        command: format!(
                            "cargo test {} {index}",
                            "long-command-argument-".repeat(40)
                        ),
                        cwd: Some(format!("/work/{}", "deep-directory/".repeat(40))),
                        exit_code: if index == 0 { 101 } else { 0 },
                        end_time_ms: None,
                    })
                    .collect(),
            );
            let mut capture = super::super::visual_test_support::OffscreenCapture::default();
            for pass in 0..3 {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        time: Some(pass as f64 * 0.25),
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width as f32, height as f32),
                        )),
                        ..Default::default()
                    },
                    |_ui| {
                        assert!(draw_history_picker(&ctx, &mut state, &theme).is_none());
                    },
                );
                if let Some(path) = &destination {
                    capture.save(
                        &ctx,
                        &mut output,
                        [width, height],
                        &path.join(format!("{name}.png")),
                    );
                } else {
                    output.textures_delta.clear();
                }
                if pass == 2 {
                    for (_, node) in &output
                        .platform_output
                        .accesskit_update
                        .as_ref()
                        .unwrap()
                        .nodes
                    {
                        if node.role() == egui::accesskit::Role::Button
                            && node
                                .label()
                                .is_some_and(|label| label.starts_with("Recall "))
                        {
                            let bounds = node.bounds().unwrap();
                            assert!(
                                bounds.x0 >= 0.0 && bounds.x1 <= width as f64,
                                "{name}: row exceeds viewport {bounds:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn history_picker_keyboard_selection_survives_repaint() {
        let ctx = egui::Context::default();
        let mut state = crate::history_picker::HistoryPickerState::new(
            ["first", "second", "third"]
                .into_iter()
                .map(
                    |command| jterm_core::command_history::CommandHistoryRecord {
                        command: command.into(),
                        cwd: None,
                        exit_code: 0,
                        end_time_ms: None,
                    },
                )
                .collect(),
        );
        let theme = theme::Theme::default();
        let frame = |state: &mut crate::history_picker::HistoryPickerState| {
            run_frame(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 640.0),
                    )),
                    ..Default::default()
                },
                |_ui| {
                    assert!(draw_history_picker(&ctx, state, &theme).is_none());
                },
            );
        };
        frame(&mut state);
        state.select_next();
        assert_eq!(state.selected, 1);
        frame(&mut state);
        assert_eq!(
            state.selected, 1,
            "render must not undo ArrowDown navigation"
        );
        assert_eq!(state.selected_command().as_deref(), Some("second"));
        frame(&mut state);
        assert_eq!(state.selected, 1, "idle frames preserve keyboard selection");
    }

    #[test]
    fn history_picker_stationary_pointer_does_not_undo_keyboard_selection() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut state = crate::history_picker::HistoryPickerState::new(
            ["first", "second", "third"]
                .into_iter()
                .map(
                    |command| jterm_core::command_history::CommandHistoryRecord {
                        command: command.into(),
                        cwd: None,
                        exit_code: 0,
                        end_time_ms: None,
                    },
                )
                .collect(),
        );
        let theme = theme::Theme::default();
        let frame = |state: &mut crate::history_picker::HistoryPickerState, events| {
            run_frame(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 640.0),
                    )),
                    events,
                    ..Default::default()
                },
                |_ui| {
                    assert!(draw_history_picker(&ctx, state, &theme).is_none());
                },
            )
        };
        let mut output = frame(&mut state, vec![]);
        for _ in 0..2 {
            output = frame(&mut state, vec![]);
        }
        let bounds = output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find_map(|(_, node)| {
                (node.value() == Some("first") || node.label() == Some("first"))
                    .then(|| node.bounds())
                    .flatten()
            })
            .expect("first history row has accessible text bounds");
        let pointer = egui::pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        frame(&mut state, vec![egui::Event::PointerMoved(pointer)]);
        assert_eq!(state.selected, 0);
        state.select_next();
        frame(&mut state, vec![]);
        assert_eq!(
            state.selected, 1,
            "continued hover is not new pointer input"
        );
        frame(&mut state, vec![]);
        assert_eq!(state.selected, 1);
    }

    #[test]
    fn block_workspace_selection_is_scoped_to_focused_session() {
        let selection =
            crate::block_mode::BlockSelection::single("left".to_owned(), "record".to_owned());
        assert_eq!(
            block_workspace_selection(Some(&selection), "left"),
            Some(&selection)
        );
        assert!(block_workspace_selection(Some(&selection), "right").is_none());
        assert!(block_workspace_selection(None, "left").is_none());
    }

    #[test]
    fn block_workspace_preserves_minimum_terminal_room() {
        assert!(block_workspace_has_room(egui::vec2(180.0, 144.0), 20.0));
        assert!(!block_workspace_has_room(egui::vec2(179.0, 500.0), 20.0));
        assert!(!block_workspace_has_room(egui::vec2(800.0, 143.0), 20.0));
        assert!(!block_workspace_has_room(egui::vec2(800.0, 180.0), 32.0));
        assert!(!block_workspace_has_room(egui::vec2(f32::NAN, 500.0), 20.0));
    }

    #[test]
    fn block_workspace_status_keeps_unknown_failed_and_running_distinct() {
        let mut terminal = crate::terminal::TerminalState::new(80, 24);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;C;id=status\x07output\r\n\x1b]133;D;0;id=status\x07",
        );
        let mut record = terminal
            .command_records()
            .back()
            .expect("completed record")
            .clone();
        record.command = Some("cargo test".to_owned());
        record.duration_ms = Some(1234);
        record.exit_code = Some(0);
        assert_eq!(
            block_workspace_status(&record, true),
            ("Succeeded · 1.2s".to_owned(), false)
        );
        record.exit_code = None;
        assert_eq!(
            block_workspace_status(&record, true),
            ("Exit status unknown · 1.2s".to_owned(), false)
        );
        record.exit_code = Some(2);
        assert_eq!(
            block_workspace_status(&record, true),
            ("Failed · exit 2 · 1.2s".to_owned(), true)
        );
        record.state = crate::terminal::CommandState::Running;
        record.complete = false;
        record.started_at = None;
        assert_eq!(
            block_workspace_status(&record, true),
            ("Running".to_owned(), false)
        );
    }

    #[test]
    fn block_workspace_controls_fit_narrow_and_wide_rows_without_changing_grid_rect() {
        for width in [180.0, 240.0, 360.0, 500.0, 720.0, 1200.0] {
            for selected_count in [0, 1, 1024] {
                for alternate_screen in [false, true] {
                    let ctx = egui::Context::default();
                    let snapshot = BlockWorkspaceSnapshot {
                        selected_count,
                        active_record_id: (selected_count > 0).then(|| "stable-record".to_owned()),
                        completed_count: 1024,
                        command_preview: "long command ".repeat(20),
                        status: "Failed · exit 255 · 1h25m".to_owned(),
                        has_prompt_marks: true,
                        alternate_screen,
                        ..Default::default()
                    };
                    let input = egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width + 16.0, 400.0),
                        )),
                        ..Default::default()
                    };
                    let _output = run_frame(&ctx, input, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let top = ui.available_rect_before_wrap().top();
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(width, BLOCK_WORKSPACE_HEIGHT),
                            egui::Sense::hover(),
                        );
                        let terminal_rect = ui.available_rect_before_wrap();
                        let mut focus = BlockWorkspaceFocus::default();
                        let mut toolbar_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                        if alternate_screen {
                            toolbar_ui.disable();
                        }
                        assert_eq!(
                            draw_block_workspace(
                                &mut toolbar_ui,
                                rect,
                                &snapshot,
                                &mut focus,
                                egui::Color32::WHITE
                            ),
                            None
                        );
                        assert_eq!(ui.available_rect_before_wrap(), terminal_rect);
                        assert_eq!(terminal_rect.top(), top + BLOCK_WORKSPACE_HEIGHT);
                        for control in &focus.control_rects {
                            assert!(
                                rect.contains_rect(*control),
                                "control {control:?} outside {rect:?} at width {width}"
                            );
                        }
                        for (index, control) in focus.control_rects.iter().enumerate() {
                            for next in &focus.control_rects[index + 1..] {
                                assert!(
                                    !control.intersects(*next),
                                    "overlapping controls at width {width}"
                                );
                            }
                        }
                    });
                }
            }
        }
    }

    #[test]
    fn block_workspace_search_is_a_real_accessible_button_and_claims_focus() {
        use std::cell::{Cell, RefCell};
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let id = Cell::new(None);
        let action = RefCell::new(None);
        let snapshot = BlockWorkspaceSnapshot::default();
        let render = |ui: &mut egui::Ui| {
            let mut focus = BlockWorkspaceFocus::default();
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(500.0, BLOCK_WORKSPACE_HEIGHT),
                egui::Sense::hover(),
            );
            *action.borrow_mut() =
                draw_block_workspace(ui, rect, &snapshot, &mut focus, egui::Color32::WHITE);
            id.set(focus.controls.first().copied());
            ui.ctx()
                .data_mut(|data| data.insert_temp(block_workspace_focus_id(), focus));
        };
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(600.0, 200.0),
            )),
            ..Default::default()
        };
        let first = run_frame(&ctx, input.clone(), render);
        let id = id.get().expect("search control");
        let update = first
            .platform_output
            .accesskit_update
            .expect("accessible toolbar tree");
        let node = update
            .nodes
            .iter()
            .find_map(|(node_id, node)| (*node_id == id.accesskit_id()).then_some(node))
            .expect("search node");
        assert_eq!(node.role(), egui::accesskit::Role::Button);
        assert_eq!(node.label(), Some("Search"));
        assert!(node.supports_action(egui::accesskit::Action::Click));
        assert!(node.supports_action(egui::accesskit::Action::Focus));
        let mut clicked = input.clone();
        clicked.events.push(egui::Event::AccessKitActionRequest(
            egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::Click,
                target_tree: egui::accesskit::TreeId::ROOT,
                target_node: id.accesskit_id(),
                data: None,
            },
        ));
        let _clicked = run_frame(&ctx, clicked, render);
        assert_eq!(
            *action.borrow(),
            Some(BlockWorkspaceAction::Command(
                crate::keybindings::Command::BlockSearchToggle
            ))
        );
        ctx.memory_mut(|memory| memory.request_focus(id));
        assert!(block_workspace_has_focus(&ctx));
        ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("unrelated-terminal")));
        // Move to a clean frame; the previous AccessKit click correctly owns
        // its original frame even after focus has changed.
        let _clean = run_frame(&ctx, input, render);
        assert!(!block_workspace_has_focus(&ctx));
    }

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
    fn bookmarked_scope_text_is_independent_of_query_matches() {
        let cache = vec![
            crate::block_mode::CachedBlockSearchRecord::new(
                "command-only",
                Some("build"),
                Some("\n\n".to_string()),
            ),
            crate::block_mode::CachedBlockSearchRecord::new(
                "with-output",
                Some("test"),
                Some("\nmeaningful output".to_string()),
            ),
        ];
        let live = std::collections::HashMap::from([
            ("command-only".to_string(), 41),
            ("with-output".to_string(), 42),
        ]);
        let command_only = std::collections::HashSet::from([41]);
        assert!(block_search_bookmarks_have_indexed_text(
            &cache,
            &live,
            &command_only,
            crate::block_mode::BlockSearchScope::Command,
        ));
        assert!(!block_search_bookmarks_have_indexed_text(
            &cache,
            &live,
            &command_only,
            crate::block_mode::BlockSearchScope::Output,
        ));
        assert!(block_search_bookmarks_have_indexed_text(
            &cache,
            &live,
            &std::collections::HashSet::from([42]),
            crate::block_mode::BlockSearchScope::Output,
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
    fn bookmark_accessible_labels_identify_their_result_and_real_context() {
        let output = crate::block_mode::BlockSearchHit {
            record_id: "record-a".to_string(),
            is_output_line: true,
            line_no: Some(3),
            match_span: None,
            line_text: "failed assertion".to_string(),
            command_preview: "cargo test".to_string(),
        };
        assert_eq!(
            block_search_bookmark_accessible_label(&output, 1, 5, false),
            "Bookmark result 2 of 5; output line 3 for cargo test"
        );
        assert_eq!(
            block_search_bookmark_accessible_label(&output, 1, 5, true),
            "Remove bookmark from result 2 of 5; output line 3 for cargo test"
        );

        let missing_line = crate::block_mode::BlockSearchHit {
            line_no: None,
            ..output
        };
        assert_eq!(
            block_search_bookmark_accessible_label(&missing_line, 0, 1, false),
            "Bookmark result 1 of 1; output for cargo test",
            "defensive malformed output metadata must never announce line zero"
        );
    }

    #[test]
    fn bookmark_control_exposes_and_executes_real_accesskit_semantics() {
        use std::cell::Cell;

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let widget_id = Cell::new(None);
        let clicked = Cell::new(false);
        let focused = Cell::new(false);
        let owns_selection = Cell::new(false);
        let render = |ui: &mut egui::Ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 24.0), egui::Sense::hover());
            let response = block_search_bookmark_button(
                ui,
                ui.make_persistent_id("block-search-accesskit-bookmark"),
                rect,
                false,
                "Bookmark result 2 of 5; output line 3 for cargo test",
            );
            widget_id.set(Some(response.id));
            clicked.set(response.clicked());
            focused.set(response.has_focus());
            owns_selection.set(block_search_bookmark_owns_selection(&response));
        };
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(200.0, 80.0),
            )),
            ..Default::default()
        };
        let first = run_frame(&ctx, input.clone(), render);
        let id = widget_id.get().expect("stable bookmark widget id");
        let update = first
            .platform_output
            .accesskit_update
            .expect("AccessKit tree update");
        let node = update
            .nodes
            .iter()
            .find_map(|(node_id, node)| (*node_id == id.accesskit_id()).then_some(node))
            .expect("bookmark node in AccessKit tree");
        assert_eq!(node.role(), egui::accesskit::Role::Button);
        assert_eq!(
            node.label(),
            Some("Bookmark result 2 of 5; output line 3 for cargo test")
        );
        assert_eq!(node.toggled(), Some(egui::accesskit::Toggled::False));
        assert!(node.supports_action(egui::accesskit::Action::Focus));
        assert!(node.supports_action(egui::accesskit::Action::Click));

        let mut activated_input = input.clone();
        activated_input.events = vec![egui::Event::AccessKitActionRequest(
            egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::Click,
                target_tree: egui::accesskit::TreeId::ROOT,
                target_node: id.accesskit_id(),
                data: None,
            },
        )];
        let activated = run_frame(&ctx, activated_input, render);
        assert!(
            !focused.get(),
            "a direct AccessKit Click need not synthesize Focus"
        );
        assert!(
            clicked.get(),
            "AccessKit Click must activate the real control"
        );
        assert!(
            owns_selection.get(),
            "a direct AccessKit Click must still select its stable result before refresh"
        );
        assert_eq!(activated.platform_output.events.len(), 1);
        assert!(matches!(
            &activated.platform_output.events[0],
            egui::output::OutputEvent::Clicked(info)
                if info.label.as_deref()
                    == Some("Bookmark result 2 of 5; output line 3 for cargo test")
                    && info.selected == Some(false)
        ));

        let mut focus_input = input;
        focus_input.events = vec![egui::Event::AccessKitActionRequest(
            egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::Focus,
                target_tree: egui::accesskit::TreeId::ROOT,
                target_node: id.accesskit_id(),
                data: None,
            },
        )];
        let focused_output = run_frame(&ctx, focus_input, render);
        assert!(focused.get(), "AccessKit Focus must focus the real control");
        assert_eq!(
            focused_output
                .platform_output
                .accesskit_update
                .expect("focused AccessKit update")
                .focus,
            id.accesskit_id()
        );
    }

    #[test]
    fn focused_result_shift_enter_is_prepass_only_and_accesskit_click_is_render_owned() {
        use std::cell::Cell;

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let widget_id = Cell::new(None);
        let request_initial_focus = Cell::new(true);
        let raw_clicked = Cell::new(false);
        let render_activated = Cell::new(false);
        let render = |ui: &mut egui::Ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(160.0, 28.0), egui::Sense::hover());
            let response = ui.interact(
                rect,
                ui.make_persistent_id("block-search-accesskit-result"),
                egui::Sense::click(),
            );
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Button,
                    true,
                    true,
                    "Result 1 of 2; cargo test; command",
                )
            });
            if request_initial_focus.replace(false) {
                response.request_focus();
            }
            widget_id.set(Some(response.id));
            raw_clicked.set(response.clicked());
            render_activated.set(block_search_result_render_activation(&response));
        };
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(240.0, 80.0),
            )),
            ..Default::default()
        };
        let _ = run_frame(&ctx, input.clone(), render);
        let id = widget_id.get().expect("stable result widget id");

        let mut shift_enter = input.clone();
        shift_enter.events = vec![
            egui::Event::ModifiersChanged(egui::Modifiers::SHIFT),
            egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: Some(egui::Key::Enter),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::SHIFT,
            },
        ];
        let _ = run_frame(&ctx, shift_enter, render);
        assert!(
            raw_clicked.get(),
            "egui exposes focused Shift+Enter as a keyboard fake click"
        );
        assert!(
            !render_activated.get(),
            "render must not accept the same Shift+Enter owned by the prepass"
        );
        let prepass_accept_count = 1usize;
        let render_accept_count = usize::from(render_activated.get());
        assert_eq!(prepass_accept_count + render_accept_count, 1);

        let mut space = input.clone();
        space.events = vec![egui::Event::Key {
            key: egui::Key::Space,
            physical_key: Some(egui::Key::Space),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }];
        let _ = run_frame(&ctx, space, render);
        assert!(raw_clicked.get());
        assert!(
            render_activated.get(),
            "focused Space remains a standard single button activation"
        );
        let prepass_accept_count = 0usize;
        let render_accept_count = usize::from(render_activated.get());
        assert_eq!(prepass_accept_count + render_accept_count, 1);

        let mut accesskit_click = input;
        accesskit_click.events = vec![egui::Event::AccessKitActionRequest(
            egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::Click,
                target_tree: egui::accesskit::TreeId::ROOT,
                target_node: id.accesskit_id(),
                data: None,
            },
        )];
        let _ = run_frame(&ctx, accesskit_click, render);
        assert!(raw_clicked.get());
        assert!(
            render_activated.get(),
            "a targeted AccessKit Click remains a render-owned activation"
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
    /// Set EMBER_UI_SNAPSHOT_DIR to export actual egui/CPU-terminal meshes.
    /// The fixture intentionally needs no display server or GPU socket.
    #[test]
    fn block_workspace_offscreen_visual_smoke() {
        let destination = std::env::var_os("EMBER_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        if let Some(path) = &destination {
            std::fs::create_dir_all(path).unwrap();
        }
        for (name, width, selected, running) in [
            ("wide-empty", 1000, 0, false),
            ("wide-idle", 1000, 0, false),
            ("wide-collapsed", 1000, 1, false),
            ("wide-failed", 1000, 1, false),
            ("wide-menu", 1000, 1, false),
            ("wide-multiple", 1000, 2, false),
            ("wide-running", 1000, 0, true),
            ("narrow-failed", 360, 1, false),
            ("narrow-running", 360, 0, true),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let theme = crate::theme::Theme::default();
            crate::apply_theme_visuals(&ctx, &theme);
            let accent = crate::theme::Theme::rgb_to_color32(theme.tabbar.active_border);
            let mut renderer = crate::ui::TerminalRenderer::new(
                14.0,
                8.0,
                1.35,
                crate::config::ScrollbarVisibility::Always,
                theme.clone(),
            );
            renderer.gpu_rendering = false;
            let mut terminal = None;
            let mut capture = super::super::visual_test_support::OffscreenCapture::default();
            let mut action_node = None;
            for pass in 0..3 {
                let mut output = ctx.run_ui(egui::RawInput {
                    time: Some(pass as f64 * 0.25),
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width as f32, 640.0))),
                    events: if name == "wide-menu" && pass == 1 {
                        vec![egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                            action: egui::accesskit::Action::Click,
                            target_tree: egui::accesskit::TreeId::ROOT,
                            target_node: action_node.expect("completed command has an accessible action button"),
                            data: None,
                        })]
                    } else { Vec::new() },
                    ..Default::default()
                }, |ui| {
                    let size = ui.available_size() - egui::vec2(0.0, BLOCK_WORKSPACE_HEIGHT + ui.spacing().item_spacing.y);
                    let (cols, rows) = renderer.grid_dimensions(size);
                    let terminal = terminal.get_or_insert_with(|| {
                        let mut terminal = crate::terminal::TerminalState::new(cols, rows);
                        if name != "wide-empty" {
                        terminal.process_input(b"\x1b]133;A\x07\x1b[36m~/projects/ember\x1b[0m $ \x1b]133;B;jsh_id=setup\x07git status --short\r\n\x1b]133;C;jsh_id=setup;cmdline_url=git%20status%20--short\x07 M src/ui.rs\r\n M src/app/rendering.rs\r\n\x1b]133;D;0;jsh_id=setup\x07\r\n");
                        terminal.process_input(b"\x1b]133;A\x07\x1b[36m~/projects/ember\x1b[0m $ \x1b]133;B;jsh_id=test\x07cargo test --lib\r\n\x1b]133;C;jsh_id=test;cmdline_url=cargo%20test%20--lib\x07running 42 tests\r\n\x1b[32mtest block_layout ... ok\x1b[0m\r\n\x1b[31mtest narrow_toolbar ... FAILED\x1b[0m\r\nExpected: controls stay inside the pane\r\n\x1b]133;D;101;jsh_id=test\x07\r\n");
                        }
                        terminal.process_input(b"\x1b]133;A\x07\x1b[36m~/projects/ember\x1b[0m $ \x1b]133;B;jsh_id=live\x07");
                        if running {
                            terminal.process_input(b"cargo build\r\n\x1b]133;C;jsh_id=live;cmdline_url=cargo%20build\x07   Compiling ember v0.4.0\r\n");
                        }
                        terminal
                    });
                    let selection = if selected > 0 {
                        let mut selection = crate::block_mode::BlockSelection::single("visual".into(), "test".into());
                        if selected > 1 { selection.selected_ids.insert(0, "setup".into()); }
                        Some(selection)
                    } else { None };
                    renderer.set_block_selection(selection.as_ref());
                    let status_record = if selected > 0 { terminal.command_record("test") }
                        else if running { terminal.command_record("live") }
                        else { terminal.command_records().iter().rev().find(|record| record.complete) };
                    let (status, failed) = status_record.map_or_else(|| (String::new(), false), |record| block_workspace_status(record, running));
                    let snapshot = BlockWorkspaceSnapshot {
                        session_id: "visual".into(), completed_count: if name == "wide-empty" { 0 } else { 2 }, selected_count: selected,
                        active_record_id: (selected > 0).then(|| "test".into()),
                        command_preview: "cargo test --lib".into(),
                        status, failed, prompt_ready: !running, has_prompt_marks: true,
                        collapsed: name == "wide-collapsed",
                        collapse_available: true, search_shortcut: "Ctrl+Shift+F".into(),
                        ..Default::default()
                    };
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), BLOCK_WORKSPACE_HEIGHT), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 0.0, crate::theme::Theme::rgb_to_color32(theme.ui.panel_bg));
                    let mut focus = BlockWorkspaceFocus::default();
                    draw_block_workspace(ui, rect, &snapshot, &mut focus, accent);
                    assert!(focus.control_rects.iter().all(|control| rect.contains_rect(*control)), "{name}: toolbar targets must remain in their reserved area");
                    if name == "wide-collapsed" {
                        let mut policy = crate::terminal::ProjectionPolicy::default();
                        policy.collapse(terminal.command_record("test").unwrap().sequence);
                        let mut view_state = crate::terminal::ProjectionViewState::default();
                        let viewport = renderer.projected_viewport_with_state(terminal, &policy, &mut view_state);
                        renderer.set_projection_frame(terminal, viewport, &policy);
                    }
                    renderer.render(ui, terminal, true, true, &crate::search::SearchState::default(), &[], &None);
                    assert!(renderer.last_content_rect.unwrap().top() >= rect.bottom(), "{name}: chrome must never overlap terminal rows");
                });
                assert!(!output.shapes.is_empty());
                if selected > 0 && pass == 1 {
                    let review = output
                        .platform_output
                        .accesskit_update
                        .as_ref()
                        .unwrap()
                        .nodes
                        .iter()
                        .find_map(|(_, node)| (node.label() == Some("Review")).then_some(node))
                        .expect("primary Review remains visible at narrow widths");
                    let bounds = review.bounds().expect("Review has visible bounds");
                    assert!(bounds.x0 >= 0.0 && bounds.x1 <= width as f64 && bounds.y1 <= 80.0);
                }
                if pass == 0 {
                    action_node =
                        output
                            .platform_output
                            .accesskit_update
                            .as_ref()
                            .and_then(|update| {
                                update.nodes.iter().find_map(|(id, node)| {
                                    node.label()
                                        .is_some_and(|label| {
                                            label.starts_with("Block actions for cargo test")
                                        })
                                        .then_some(*id)
                                })
                            });
                }
                if name == "wide-menu" && pass > 0 {
                    assert!(
                        egui::Popup::is_any_open(&ctx),
                        "the real card actions menu must open and remain stable"
                    );
                }
                if let Some(path) = &destination {
                    capture.save(
                        &ctx,
                        &mut output,
                        [width, 640],
                        &path.join(format!("{name}.png")),
                    );
                } else {
                    output.textures_delta.clear();
                }
                if pass == 1 {
                    assert!(output.platform_output.accesskit_update.is_some());
                }
            }
        }
    }
}
