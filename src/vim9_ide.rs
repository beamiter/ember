//! Native workbench controls around a real Vim9 process in the terminal grid.
//! Editing, undo, registers, plugins and modified-buffer protection belong to Vim.

use crate::app::state::TerminalApp;
use eframe::egui;
use jterm_core::vim9::{Vim9Command, Vim9ProjectFile, Vim9ProjectIndex, Vim9Session, Vim9Snapshot};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const QUICK_OPEN_LIMIT: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DialogKind {
    QuickOpen,
    Goto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolInput {
    Search,
    Build,
}

struct EditorDialog {
    kind: DialogKind,
    query: String,
    ranked_query: String,
    ranked_files: Option<Arc<Vec<Vim9ProjectFile>>>,
    matches: Vec<Vim9ProjectFile>,
    selected: usize,
    needs_focus: bool,
    reveal_selection: bool,
    selection_delta: isize,
    query_widget: Option<egui::Id>,
    confirm_requested: bool,
    error: Option<String>,
}

impl EditorDialog {
    fn new(kind: DialogKind) -> Self {
        Self {
            kind,
            query: String::new(),
            ranked_query: String::new(),
            ranked_files: None,
            matches: Vec::new(),
            selected: 0,
            needs_focus: true,
            reveal_selection: true,
            selection_delta: 0,
            query_widget: None,
            confirm_requested: false,
            error: None,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if delta != 0 && !self.matches.is_empty() {
            self.selected =
                (self.selected as isize + delta).rem_euclid(self.matches.len() as isize) as usize;
            self.reveal_selection = true;
        }
    }
}

fn is_keyboard_event(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::Key { .. }
            | egui::Event::Text(_)
            | egui::Event::Paste(_)
            | egui::Event::Ime(_)
            | egui::Event::Copy
            | egui::Event::Cut
    )
}

/// A focused Cancel/Refresh/result button owns Enter. Query confirmation is
/// deferred until painting, and keeps only text that preceded that Enter.
fn route_dialog_events(
    dialog: &mut EditorDialog,
    query_owns_keyboard: bool,
    events: &mut Vec<egui::Event>,
) -> bool {
    let mut cancel = false;
    let mut claimed_suffix = false;
    let mut focus_may_have_moved = false;
    events.retain(|event| {
        if claimed_suffix {
            return !is_keyboard_event(event);
        }
        if matches!(event, egui::Event::AccessKitActionRequest(_)) {
            focus_may_have_moved = true;
        }
        let egui::Event::Key {
            key,
            pressed: true,
            repeat,
            modifiers,
            ..
        } = event
        else {
            return true;
        };
        if *key == egui::Key::Tab {
            focus_may_have_moved = true;
        }
        if modifiers.any() {
            return true;
        }
        match key {
            egui::Key::Escape => {
                cancel = true;
                claimed_suffix = true;
                false
            }
            egui::Key::Enter if *repeat => false,
            egui::Key::Enter if query_owns_keyboard && !focus_may_have_moved => {
                dialog.confirm_requested = true;
                claimed_suffix = true;
                false
            }
            egui::Key::ArrowUp | egui::Key::ArrowDown
                if query_owns_keyboard
                    && !focus_may_have_moved
                    && dialog.kind == DialogKind::QuickOpen =>
            {
                dialog.selection_delta += if *key == egui::Key::ArrowDown { 1 } else { -1 };
                false
            }
            _ => true,
        }
    });
    if cancel {
        events.retain(|event| !is_keyboard_event(event));
    }
    cancel
}

fn parse_location(input: &str) -> Result<(u64, Option<u64>), &'static str> {
    let mut parts = input.trim().split(':');
    let parse = |part: &str| {
        part.trim()
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or("Enter a positive line number, optionally followed by :column")
    };
    let line = parse(parts.next().unwrap_or_default())?;
    let column = parts.next().map(parse).transpose()?;
    if parts.next().is_some() {
        return Err("Use line or line:column, for example 42:8");
    }
    Ok((line, column))
}

fn focused_chrome_widget(ctx: &egui::Context, rects: &[egui::Rect]) -> Option<egui::Id> {
    let id = ctx.memory(|memory| memory.focused())?;
    let response = ctx.read_response(id)?;
    (response.has_focus()
        && rects
            .iter()
            .any(|rect| rect.contains(response.rect.center())))
    .then_some(id)
}

/// egui's `lost_focus` can stay true for two frames, including after focus has
/// moved to a button. Only Enter owned by this field may submit its contents.
fn form_submitted(response: &egui::Response) -> bool {
    if !response.lost_focus()
        || !response
            .ctx
            .memory(|memory| memory.had_focus_last_frame(response.id))
    {
        return false;
    }
    response.ctx.input(|input| {
        for event in &input.events {
            match event {
                egui::Event::AccessKitActionRequest(_)
                | egui::Event::PointerButton { pressed: true, .. } => return false,
                egui::Event::Key {
                    key: egui::Key::Escape | egui::Key::Tab,
                    pressed: true,
                    ..
                } => return false,
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } if !modifiers.any() => return true,
                _ => {}
            }
        }
        false
    })
}

/// These chords only apply while the Vim grid owns keyboard input. The
/// terminal's ordinary palette, SSH picker and pane-close bindings stay intact
/// in shell tabs; Vim's Ctrl+P, Ctrl+G and Ctrl+W remain untouched.
pub(crate) fn shortcut_command(
    key: egui::Key,
    modifiers: egui::Modifiers,
) -> Option<crate::keybindings::Command> {
    use crate::keybindings::Command;
    if modifiers.alt || modifiers.mac_cmd {
        return None;
    }
    let control = modifiers.ctrl || modifiers.command;
    match (key, control, modifiers.shift) {
        (egui::Key::P, true, true) => Some(Command::Vim9QuickOpen),
        (egui::Key::G, true, true) => Some(Command::Vim9Goto),
        (egui::Key::S, true, false) => Some(Command::Vim9Save),
        (egui::Key::S, true, true) => Some(Command::Vim9SaveAll),
        (egui::Key::F, true, true) => Some(Command::Vim9Search),
        (egui::Key::B, true, true) => Some(Command::Vim9Build),
        (egui::Key::PageUp, true, true) => Some(Command::Vim9PreviousBuffer),
        (egui::Key::PageDown, true, true) => Some(Command::Vim9NextBuffer),
        (egui::Key::W, true, true) => Some(Command::Vim9CloseBuffer),
        (egui::Key::F8, false, false) => Some(Command::Vim9NextProblem),
        (egui::Key::F8, false, true) => Some(Command::Vim9PreviousProblem),
        _ => None,
    }
}

pub(crate) fn is_editor_command(command: &crate::keybindings::Command) -> bool {
    use crate::keybindings::Command;
    matches!(
        command,
        Command::Vim9QuickOpen
            | Command::Vim9Goto
            | Command::Vim9Save
            | Command::Vim9SaveAll
            | Command::Vim9Search
            | Command::Vim9Build
            | Command::Vim9PreviousBuffer
            | Command::Vim9NextBuffer
            | Command::Vim9CloseBuffer
            | Command::Vim9NextProblem
            | Command::Vim9PreviousProblem
    )
}

pub(crate) struct Workbench {
    pub(crate) session: Vim9Session,
    snapshot: Arc<Vim9Snapshot>,
    refreshed_at: Instant,
    file_input: String,
    search_input: String,
    build_input: String,
    rename_input: String,
    problems_open: bool,
    error: Option<String>,
    project_index: Option<Vim9ProjectIndex>,
    dialog: Option<EditorDialog>,
    focus_input: Option<ToolInput>,
    chrome_rects: Vec<egui::Rect>,
    chrome_focus: Option<egui::Id>,
    return_focus: bool,
}

impl Workbench {
    pub(crate) fn new(session: Vim9Session) -> Self {
        let snapshot = Arc::new(session.snapshot());
        Self {
            session,
            snapshot,
            refreshed_at: Instant::now(),
            file_input: String::new(),
            search_input: String::new(),
            build_input: String::new(),
            rename_input: String::new(),
            problems_open: false,
            error: None,
            project_index: None,
            dialog: None,
            focus_input: None,
            chrome_rects: Vec::new(),
            chrome_focus: None,
            return_focus: false,
        }
    }

    pub(crate) fn send(&mut self, command: Vim9Command) {
        self.error = self.session.send(command).err().map(|e| e.to_string());
        self.return_focus = true;
    }

    fn open_dialog(&mut self, kind: DialogKind) {
        if kind == DialogKind::QuickOpen && self.project_index.is_none() {
            self.project_index = Some(Vim9ProjectIndex::start(self.session.root()));
        }
        self.dialog = Some(EditorDialog::new(kind));
    }

    fn close_current_buffer(&mut self) {
        // Resolve the current buffer inside Vim when this command executes;
        // a preceding buffer switch may be newer than the native snapshot.
        self.send(Vim9Command::CloseCurrentBuffer);
    }

    fn refresh(&mut self) {
        if self.refreshed_at.elapsed() >= Duration::from_millis(100) {
            self.snapshot = Arc::new(self.session.snapshot());
            self.refreshed_at = Instant::now();
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let snapshot = Arc::clone(&self.snapshot);
        ui.horizontal_wrapped(|ui| {
            ui.strong("VIM9 IDE");
            ui.separator();
            ui.label(if snapshot.connected {
                snapshot.mode.to_uppercase()
            } else {
                "Connecting to Vim…".to_string()
            });
            ui.add_enabled_ui(snapshot.connected, |ui| {
                if ui.button("Quick Open").on_hover_text("Find project files · Ctrl+Shift+P").clicked() {
                    self.open_dialog(DialogKind::QuickOpen);
                }
                if ui.button("Go to line").on_hover_text("Jump to line:column · Ctrl+Shift+G").clicked() {
                    self.open_dialog(DialogKind::Goto);
                }
                if ui.button("Save").on_hover_text("Ctrl+S").clicked() { self.send(Vim9Command::Save); }
                if ui.button("Save all").on_hover_text("Ctrl+Shift+S").clicked() { self.send(Vim9Command::SaveAll); }
                ui.menu_button("Windows", |ui| {
                    if ui.button("Vertical split").clicked() {
                        self.send(Vim9Command::Split { vertical: true });
                        ui.close();
                    }
                    if ui.button("Horizontal split").clicked() {
                        self.send(Vim9Command::Split { vertical: false });
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Previous buffer · Ctrl+Shift+PageUp").clicked() {
                        self.send(Vim9Command::PreviousBuffer);
                        ui.close();
                    }
                    if ui.button("Next buffer · Ctrl+Shift+PageDown").clicked() {
                        self.send(Vim9Command::NextBuffer);
                        ui.close();
                    }
                    if ui.button("Close buffer · Ctrl+Shift+W").on_hover_text("Keeps modified buffers open; never forces a discard").clicked() {
                        self.close_current_buffer();
                        ui.close();
                    }
                });
                ui.menu_button("Code", |ui| {
                    for (label, capability, command) in [
                        ("Go to definition", "definition", Vim9Command::Definition),
                        ("Hover documentation", "hover", Vim9Command::Hover),
                        ("Find references", "references", Vim9Command::References),
                        ("Format buffer", "format", Vim9Command::Format),
                    ] {
                        if ui.add_enabled(snapshot.capabilities.iter().any(|s| s == capability), egui::Button::new(label))
                            .on_disabled_hover_text("Configure a supported Vim language server plugin to enable this action")
                            .clicked() {
                            self.send(command);
                            ui.close();
                        }
                    }
                    ui.separator();
                    ui.add(egui::TextEdit::singleline(&mut self.rename_input).hint_text("New symbol name").char_limit(256));
                    if ui.add_enabled(!self.rename_input.is_empty() && snapshot.capabilities.iter().any(|s| s == "rename"), egui::Button::new("Rename symbol"))
                        .on_disabled_hover_text("Enter a name and configure a supported Vim language server plugin")
                        .clicked() {
                        self.send(Vim9Command::Rename { name: self.rename_input.clone() });
                        ui.close();
                    }
                });
                if ui.selectable_label(self.problems_open, format!("Problems ({})", snapshot.diagnostics.len())).clicked() {
                    self.problems_open = !self.problems_open;
                }
                if ui.button("Previous issue").on_hover_text("Shift+F8").clicked() { self.send(Vim9Command::PreviousDiagnostic); }
                if ui.button("Next issue").on_hover_text("F8").clicked() { self.send(Vim9Command::NextDiagnostic); }
                if ui.button("Close editor").on_hover_text("Ask Vim to quit; modified buffers are protected").clicked() {
                    self.send(Vim9Command::Quit);
                }
            });
        });
        egui::ScrollArea::horizontal()
            .id_salt("vim9_tools")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.file_input)
                            .hint_text("Open file · relative to project")
                            .desired_width(230.0)
                            .char_limit(4096),
                    );
                    let enter = form_submitted(&edit);
                    if (ui
                        .add_enabled(
                            snapshot.connected && !self.file_input.is_empty(),
                            egui::Button::new("Open"),
                        )
                        .clicked()
                        || enter)
                        && !self.file_input.is_empty()
                    {
                        self.send(Vim9Command::Open {
                            path: self.file_input.clone(),
                            line: None,
                            column: None,
                        });
                    }
                    let search = ui.add(
                        egui::TextEdit::singleline(&mut self.search_input)
                            .hint_text("Project search")
                            .desired_width(180.0)
                            .char_limit(4096),
                    );
                    if self.focus_input == Some(ToolInput::Search) {
                        search.request_focus();
                        self.focus_input = None;
                    }
                    let enter = form_submitted(&search);
                    if (ui
                        .add_enabled(
                            snapshot.connected && !self.search_input.is_empty(),
                            egui::Button::new("Find"),
                        )
                        .clicked()
                        || enter)
                        && !self.search_input.is_empty()
                    {
                        self.send(Vim9Command::Search {
                            query: self.search_input.clone(),
                        });
                        self.problems_open = true;
                    }
                    let build = ui.add(
                        egui::TextEdit::singleline(&mut self.build_input)
                            .hint_text("Build command · blank uses Vim makeprg")
                            .desired_width(240.0)
                            .char_limit(4096),
                    );
                    if self.focus_input == Some(ToolInput::Build) {
                        build.request_focus();
                        self.focus_input = None;
                    }
                    let build_enter = form_submitted(&build);
                    if snapshot.build.running {
                        if ui.button("Stop build").clicked() {
                            self.send(Vim9Command::CancelBuild);
                        }
                    } else if ui
                        .add_enabled(snapshot.connected, egui::Button::new("Build"))
                        .clicked()
                        || (snapshot.connected && build_enter)
                    {
                        self.send(Vim9Command::Build {
                            command: self.build_input.clone(),
                        });
                        self.problems_open = true;
                    }
                    if ui
                        .add_enabled(
                            snapshot.connected && !snapshot.build.running,
                            egui::Button::new("Save + Build"),
                        )
                        .on_hover_text(
                            "Save all modified buffers first; abort the build if a write fails",
                        )
                        .clicked()
                    {
                        self.send(Vim9Command::SaveAndBuild {
                            command: self.build_input.clone(),
                        });
                        self.problems_open = true;
                    }
                });
            });
        egui::ScrollArea::horizontal()
            .id_salt("vim9_buffers")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for buffer in &snapshot.buffers {
                        ui.push_id(("buffer", buffer.id), |ui| {
                            let name = Path::new(&buffer.name)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .filter(|n| !n.is_empty())
                                .unwrap_or("[No Name]");
                            let label =
                                format!("{}{}", name, if buffer.modified { " •" } else { "" });
                            if ui
                                .selectable_label(buffer.current, label)
                                .on_hover_text(&buffer.name)
                                .clicked()
                            {
                                self.send(Vim9Command::Buffer { buffer: buffer.id });
                            }
                            if ui
                                .small_button("×")
                                .on_hover_text(format!(
                                    "Close {name}; modified buffers remain protected"
                                ))
                                .clicked()
                            {
                                self.send(Vim9Command::CloseBuffer { buffer: buffer.id });
                            }
                        });
                    }
                    if snapshot.buffers_truncated {
                        ui.weak("More buffers in :ls");
                    }
                });
            });
        ui.horizontal_wrapped(|ui| {
            let file = if snapshot.file.is_empty() {
                "[No Name]"
            } else {
                &snapshot.file
            };
            ui.small(format!(
                "{}{}   Ln {}/{}, Col {}",
                file,
                if snapshot.modified {
                    "  • modified"
                } else {
                    ""
                },
                snapshot.line,
                snapshot.line_count,
                snapshot.column
            ));
            if !snapshot.filetype.is_empty() {
                ui.small(&snapshot.filetype);
            }
            if !snapshot.encoding.is_empty() {
                ui.small(&snapshot.encoding);
            }
            ui.small(format!("{} Vim window(s)", snapshot.window_count));
            if snapshot.build.running {
                ui.small(format!("Building: {}", snapshot.build.command));
            } else if snapshot.build.cancelled {
                ui.small("Build cancelled");
            } else if let Some(code) = snapshot.build.exit_code {
                ui.small(format!("Build exit: {code}"));
            }
            if snapshot.search.running {
                ui.small(format!(
                    "Searching: {} · {} matches",
                    snapshot.search.query, snapshot.search.count
                ));
            } else if !snapshot.search.query.is_empty() {
                if (0..=1).contains(&snapshot.search.exit_code) {
                    ui.small(format!("Search: {} matches", snapshot.search.count));
                } else {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        format!("Search failed (exit {})", snapshot.search.exit_code),
                    );
                }
            }
            if snapshot.waiting_for_input {
                ui.small("Waiting for Vim input to finish…");
            }
            if snapshot.pending > 0 {
                ui.small(format!("{} pending", snapshot.pending));
            }
        });
        if let Some(error) = self.error.clone().or_else(|| snapshot.error.clone()) {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(ui.visuals().error_fg_color, error);
                if ui.small_button("Dismiss").clicked() {
                    self.error = None;
                    self.session.clear_error();
                }
            });
        }
    }

    fn problems(&mut self, ui: &mut egui::Ui) {
        let snapshot = Arc::clone(&self.snapshot);
        ui.horizontal(|ui| {
            ui.strong(format!(
                "Problems / search results · {}",
                snapshot.diagnostics.len()
            ));
            if ui.small_button("Hide").clicked() {
                self.problems_open = false;
            }
        });
        egui::ScrollArea::vertical().id_salt("vim9_problems").show(ui, |ui| {
            if !snapshot.search.query.is_empty() {
                ui.strong(format!("Project search · {}", snapshot.search.query));
                if snapshot.search.running {
                    ui.label(format!("Searching… {} matches so far. Previous quickfix entries remain until search completes.", snapshot.search.count));
                } else if (0..=1).contains(&snapshot.search.exit_code) {
                    ui.label(format!("Search completed · {} matches", snapshot.search.count));
                } else {
                    ui.colored_label(ui.visuals().error_fg_color, format!("Search failed (exit {}). Previous quickfix entries were retained.", snapshot.search.exit_code));
                }
                if snapshot.search.truncated > 0 {
                    ui.label(format!("Search result limit reached: {} matches omitted. Narrow your query to see other matches.", snapshot.search.truncated));
                }
                for error in &snapshot.search.errors {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.separator();
            }
            if snapshot.diagnostics.is_empty() { ui.weak("No quickfix entries. Run a build or project search to populate this panel."); }
            for diagnostic in &snapshot.diagnostics {
                let label = format!("{}  {}:{}:{}  {}", diagnostic.kind, diagnostic.filename, diagnostic.line, diagnostic.column, diagnostic.text);
                if ui.selectable_label(false, label).clicked() && !diagnostic.filename.is_empty() {
                    self.send(Vim9Command::Open { path: diagnostic.filename.clone(), line: Some(diagnostic.line.max(1)), column: Some(diagnostic.column.max(1)) });
                }
            }
            if snapshot.diagnostics_truncated { ui.weak("Quickfix list truncated; view the complete list with :copen in Vim."); }
            if !snapshot.build.output.is_empty() {
                ui.separator();
                ui.strong("Build output · latest lines");
                for line in &snapshot.build.output { ui.monospace(line); }
                if snapshot.build.truncated > 0 {
                    ui.weak(format!("{} output lines omitted", snapshot.build.truncated));
                }
            }
        });
    }

    fn render_dialog(&mut self, ctx: &egui::Context, session_id: &str) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        let project = self.project_index.as_ref().map(Vim9ProjectIndex::snapshot);
        let mut cancel = false;
        let mut refresh = false;
        let response = egui::Modal::new(egui::Id::new(("vim9_dialog", session_id)))
            .show(ctx, |ui| {
                ui.set_width((ctx.viewport_rect().width() - 48.0).clamp(240.0, 720.0));
                ui.heading(match dialog.kind {
                    DialogKind::QuickOpen => "Quick Open",
                    DialogKind::Goto => "Go to line",
                });
                match dialog.kind {
                    DialogKind::QuickOpen => {
                        ui.weak(crate::review_text::visible_bounded(
                            &self.session.root().to_string_lossy(), 1024,
                        ));
                    }
                    DialogKind::Goto => {
                        ui.label(format!(
                            "Current line {} of {} · column {}",
                            self.snapshot.line, self.snapshot.line_count, self.snapshot.column
                        ));
                    }
                }
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut dialog.query)
                        .id_salt(("vim9_dialog_query", session_id))
                        .desired_width(f32::INFINITY)
                        .hint_text(match dialog.kind {
                            DialogKind::QuickOpen => "Type a filename or path fragment…",
                            DialogKind::Goto => "Line or line:column, for example 42:8",
                        })
                        .char_limit(match dialog.kind {
                            DialogKind::QuickOpen => 256,
                            DialogKind::Goto => 48,
                        }),
                );
                dialog.query_widget = Some(edit.id);
                if dialog.needs_focus {
                    edit.request_focus();
                    dialog.needs_focus = false;
                }
                if edit.changed() {
                    dialog.error = None;
                }

                if dialog.kind == DialogKind::QuickOpen {
                    if let Some(project) = &project {
                        let changed_files = dialog.ranked_files.as_ref()
                            .is_none_or(|files| !Arc::ptr_eq(files, &project.files));
                        if changed_files || dialog.ranked_query != dialog.query {
                            dialog.matches = project.matches(&dialog.query, QUICK_OPEN_LIMIT);
                            dialog.ranked_query.clone_from(&dialog.query);
                            dialog.ranked_files = Some(Arc::clone(&project.files));
                            dialog.selected = 0;
                            dialog.reveal_selection = true;
                        }
                        let delta = std::mem::take(&mut dialog.selection_delta);
                        dialog.move_selection(delta);
                        ui.horizontal(|ui| {
                            if project.scanning { ui.spinner(); }
                            ui.small(format!(
                                "{} indexed files · showing {} matches{}",
                                project.files.len(), dialog.matches.len(),
                                if project.scanning { " · scanning…" } else { "" }
                            ));
                            refresh = ui.small_button("Refresh files").clicked();
                        });
                        if let Some(error) = &project.error {
                            ui.colored_label(ui.visuals().error_fg_color, error);
                        }
                        if project.truncated {
                            ui.weak("Project file limit reached. Open a narrower project or use the path field.");
                        }
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .id_salt(("vim9_quick_open_results", session_id))
                            .max_height((ctx.viewport_rect().height() * 0.45).min(360.0))
                            .show(ui, |ui| {
                                if dialog.matches.is_empty() {
                                    ui.weak(if project.scanning { "Indexing project files…" } else { "No matching files" });
                                }
                                for (index, file) in dialog.matches.iter().enumerate() {
                                    let row = ui.push_id(&file.path, |ui| ui.add_sized(
                                        [ui.available_width(), 24.0],
                                        egui::Button::new(crate::review_text::visible_bounded(&file.relative, 1024))
                                            .selected(index == dialog.selected)
                                            .truncate(),
                                    ).on_hover_text(crate::review_text::visible_bounded(&file.relative, 4096))).inner;
                                    if index == dialog.selected && dialog.reveal_selection {
                                        row.scroll_to_me(Some(egui::Align::Center));
                                    }
                                    if row.clicked() {
                                        dialog.selected = index;
                                        dialog.confirm_requested = true;
                                    }
                                }
                            });
                        dialog.reveal_selection = false;
                        ui.weak("↑ ↓ select · Enter opens · Esc returns to Vim");
                    }
                }
                if let Some(error) = &dialog.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let label = if dialog.kind == DialogKind::QuickOpen { "Open selected" } else { "Go" };
                    if ui.add_enabled(
                        dialog.kind == DialogKind::Goto || !dialog.matches.is_empty(),
                        egui::Button::new(label),
                    ).clicked() {
                        dialog.confirm_requested = true;
                    }
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if cancel || response.should_close() {
            self.return_focus = true;
            return;
        }
        if refresh {
            self.project_index = Some(Vim9ProjectIndex::start(self.session.root()));
            dialog.confirm_requested = false;
            dialog.needs_focus = true;
        }
        if std::mem::take(&mut dialog.confirm_requested) {
            let command = match dialog.kind {
                DialogKind::Goto => parse_location(&dialog.query)
                    .map(|(line, column)| Vim9Command::Goto { line, column }),
                DialogKind::QuickOpen => dialog
                    .matches
                    .get(dialog.selected)
                    .ok_or("Choose a matching project file")
                    .and_then(|file| file.path.to_str().ok_or("The filename is not valid UTF-8"))
                    .map(|path| Vim9Command::Open {
                        path: path.to_owned(),
                        line: None,
                        column: None,
                    }),
            };
            match command {
                Ok(command) => {
                    self.send(command);
                    return;
                }
                Err(error) => {
                    dialog.error = Some(error.to_string());
                    dialog.needs_focus = true;
                }
            }
        }
        self.dialog = Some(dialog);
    }
}

/// Only `--ide` consumes a following path; other terminal invocations retain
/// their previous behavior. OsString keeps non-UTF8 input intact until core
/// can report a proper path diagnostic.
pub(crate) fn startup_target() -> Option<PathBuf> {
    let mut args = std::env::args_os().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--ide" {
            return Some(
                args.next()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(".")),
            );
        }
    }
    None
}

pub(crate) fn launch_button(ui: &egui::Ui, rect: egui::Rect, font_size: f32) -> bool {
    let response = ui.interact(rect, ui.id().with("vim9_launch"), egui::Sense::click());
    let visuals = ui.style().interact(&response);
    ui.painter().rect_filled(rect, 5.0, visuals.weak_bg_fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "Vim9",
        egui::FontId::proportional(font_size),
        visuals.text_color(),
    );
    response
        .on_hover_text("Open this project in the native Vim9 IDE")
        .clicked()
}

impl TerminalApp {
    pub(crate) fn active_vim9_editor(&self) -> Option<&Workbench> {
        self.session_manager
            .sessions()
            .get(self.session_manager.active_index())
            .and_then(|session| self.vim9_editors.get(&session.metadata.session_id))
    }

    pub(crate) fn vim9_dialog_is_open(&self) -> bool {
        self.active_vim9_editor()
            .is_some_and(|editor| editor.dialog.is_some() || editor.focus_input.is_some())
    }

    pub(crate) fn vim9_chrome_owns_keyboard(&self, ctx: &egui::Context) -> bool {
        let Some(editor) = self.active_vim9_editor() else {
            return false;
        };
        if editor.focus_input.is_some() {
            return true;
        }
        // Escape clears egui focus in begin_pass, before application logic.
        // Keep the previous owner so its cancellation batch cannot submit a
        // form or fall through to Vim after that early focus transition.
        if focused_chrome_widget(ctx, &editor.chrome_rects).is_some()
            || editor
                .chrome_focus
                .is_some_and(|id| ctx.memory(|memory| memory.had_focus_last_frame(id)))
        {
            return true;
        }
        let pointer_press = ctx.input(|input| {
            input
                .pointer
                .any_pressed()
                .then(|| input.pointer.interact_pos())
                .flatten()
        });
        editor
            .chrome_rects
            .iter()
            .any(|rect| pointer_press.is_some_and(|point| rect.contains(point)))
    }

    pub(crate) fn dispatch_vim9_command(&mut self, command: crate::keybindings::Command) {
        use crate::keybindings::Command;
        let id = self
            .session_manager
            .get_active_session_mut()
            .metadata
            .session_id
            .clone();
        let Some(editor) = self.vim9_editors.get_mut(&id) else {
            self.set_status_for(
                "Open a Vim9 IDE tab to use editor commands",
                Duration::from_secs(4),
            );
            return;
        };
        match command {
            Command::Vim9QuickOpen => editor.open_dialog(DialogKind::QuickOpen),
            Command::Vim9Goto => editor.open_dialog(DialogKind::Goto),
            Command::Vim9Save => editor.send(Vim9Command::Save),
            Command::Vim9SaveAll => editor.send(Vim9Command::SaveAll),
            Command::Vim9Search => editor.focus_input = Some(ToolInput::Search),
            Command::Vim9Build => editor.focus_input = Some(ToolInput::Build),
            Command::Vim9PreviousBuffer => editor.send(Vim9Command::PreviousBuffer),
            Command::Vim9NextBuffer => editor.send(Vim9Command::NextBuffer),
            Command::Vim9CloseBuffer => editor.close_current_buffer(),
            Command::Vim9NextProblem => editor.send(Vim9Command::NextDiagnostic),
            Command::Vim9PreviousProblem => editor.send(Vim9Command::PreviousDiagnostic),
            _ => {}
        }
    }

    /// Reserve dialog navigation before the terminal encoder. Confirmation is
    /// deferred until after egui applies the current query edits, so typing a
    /// final character and Enter in one frame opens the newly filtered file.
    pub(crate) fn handle_vim9_dialog_input(&mut self, ctx: &egui::Context) -> bool {
        let chrome_owns_keyboard = self.vim9_chrome_owns_keyboard(ctx);
        let id = self
            .session_manager
            .get_active_session_mut()
            .metadata
            .session_id
            .clone();
        let Some(editor) = self.vim9_editors.get_mut(&id) else {
            return false;
        };
        let Some(dialog) = editor.dialog.as_mut() else {
            // Native path/search/build text fields use Escape to return to
            // the editing grid, including the first frame of a focus request.
            let escape = chrome_owns_keyboard && self.frame_events.iter().any(|event| matches!(event,
                egui::Event::Key { key: egui::Key::Escape, pressed: true, modifiers, .. } if !modifiers.any()));
            if escape {
                editor.focus_input = None;
                editor.return_focus = true;
                // The cancelling frame still belongs to the form. In
                // particular, an Enter after Escape must not submit a build
                // or activate the button that receives focus next.
                ctx.input_mut(|input| {
                    input.events.retain(|event| !is_keyboard_event(event));
                });
                self.frame_events.retain(|event| !is_keyboard_event(event));
            }
            return escape;
        };
        let enter_seen = self.frame_events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    ..
                }
            )
        });
        let query_owns_keyboard = dialog.needs_focus
            || dialog
                .query_widget
                .is_some_and(|id| ctx.memory(|memory| memory.has_focus(id)));
        let cancel = ctx
            .input_mut(|input| route_dialog_events(dialog, query_owns_keyboard, &mut input.events));
        if cancel {
            editor.dialog = None;
            editor.return_focus = true;
        }
        self.frame_events.retain(|event| !is_keyboard_event(event));
        if enter_seen {
            self.claim_enter_for_prompt_recall();
        }
        // A cancelling Escape still owns all keyboard events in this frame.
        true
    }

    pub(crate) fn render_vim9_dialog(&mut self, ctx: &egui::Context) {
        let id = self
            .session_manager
            .get_active_session_mut()
            .metadata
            .session_id
            .clone();
        let Some(editor) = self.vim9_editors.get_mut(&id) else {
            return;
        };
        editor.render_dialog(ctx, &id);
        let return_focus = std::mem::take(&mut editor.return_focus);
        editor.chrome_focus = if return_focus {
            None
        } else {
            focused_chrome_widget(ctx, &editor.chrome_rects)
        };
        if return_focus {
            self.claim_enter_for_prompt_recall();
            self.return_focus_to_terminal(ctx);
        }
    }

    pub(crate) fn guard_vim9_window_close(&mut self, ctx: &egui::Context) {
        // eframe can run logic without UI for an invisible window. Close
        // protection must therefore live in logic, not only in the paint pass.
        if ctx.input(|input| input.viewport().close_requested()) && !self.vim9_editors.is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.vim9_window_close_pending = true;
            let indexes: Vec<_> = (0..self.session_manager.len()).collect();
            self.request_vim9_close(&indexes);
        }
        if self.vim9_window_close_pending && self.vim9_editors.is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    pub(crate) fn open_vim9_ide(&mut self, target: Option<PathBuf>) {
        let target = target.unwrap_or_else(|| {
            let session = self.session_manager.get_active_session_mut();
            jterm_core::process::process_cwd(session.get_shell_pid())
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."))
        });
        let session = match Vim9Session::start(&target) {
            Ok(session) => session,
            Err(error) => {
                self.set_status_for(
                    format!("Vim9 IDE could not start: {error}"),
                    Duration::from_secs(10),
                );
                return;
            }
        };
        let root = session.root().to_path_buf();
        let name = format!(
            "Vim9 · {}",
            root.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("project")
        );
        let created = match self.session_manager.new_command_session_in_cwd(
            name,
            session.launch_argv(),
            &root,
            self.cols,
            self.rows,
            self.config.scrollback_lines,
        ) {
            Ok(created) => created,
            Err(error) => {
                self.set_status_for(
                    format!("Vim9 editor process could not start: {error}"),
                    Duration::from_secs(10),
                );
                return;
            }
        };
        self.vim9_editors
            .insert(created.session_id, Workbench::new(session));
        self.tabs.on_session_inserted(created.session_index);
        self.tabs.insert_tab_after_active(created.session_index);
        self.activate_session(created.session_index);
        self.force_resize_session = true;
        self.sidebar.visible = true;
        self.sidebar.view = crate::sidebar::SidebarView::Files;
        if let Some(error) = self.sidebar.set_current_dir(root) {
            self.set_status_for(
                format!("Vim9 opened; file tree: {error}"),
                Duration::from_secs(8),
            );
        }
        self.schedule_session_save();
    }

    pub(crate) fn vim9_open_file(&mut self, path: &Path) -> bool {
        let id = self
            .session_manager
            .get_active_session_mut()
            .metadata
            .session_id
            .clone();
        if let Some(editor) = self.vim9_editors.get_mut(&id) {
            match path.to_str() {
                Some(path) => editor.send(Vim9Command::Open {
                    path: path.to_string(),
                    line: None,
                    column: None,
                }),
                None => {
                    editor.error =
                        Some("Vim9 cannot open a filename that is not valid UTF-8".into())
                }
            }
            true
        } else {
            false
        }
    }

    pub(crate) fn render_vim9_workbench(&mut self, root_ui: &mut egui::Ui) {
        self.vim9_editors
            .retain(|id, _| self.session_manager.index_of(id).is_some());
        if !self.vim9_editors.is_empty() {
            root_ui
                .ctx()
                .request_repaint_after(Duration::from_millis(100));
        }
        let id = self
            .session_manager
            .get_active_session_mut()
            .metadata
            .session_id
            .clone();
        let Some(editor) = self.vim9_editors.get_mut(&id) else {
            return;
        };
        editor.refresh();
        editor.chrome_rects.clear();
        let toolbar = egui::Panel::top(egui::Id::new(("vim9_workbench", &id)))
            .resizable(false)
            .show(root_ui, |ui| {
                editor.toolbar(ui);
            });
        editor.chrome_rects.push(toolbar.response.rect);
        if editor.problems_open {
            let problems = egui::Panel::bottom(egui::Id::new(("vim9_problems_panel", &id)))
                .resizable(true)
                .default_size(160.0)
                .min_size(70.0)
                .max_size(300.0)
                .show(root_ui, |ui| {
                    editor.problems(ui);
                });
            editor.chrome_rects.push(problems.response.rect);
        }
    }

    pub(crate) fn flush_vim9_tab_closes(&mut self, ctx: &egui::Context) {
        // Closing a mixed editor/shell split must finish closing its original
        // panes once every Vim accepted quit. Stable members prevent a newly
        // inserted pane from becoming an accidental part of the close.
        for members in std::mem::take(&mut self.vim9_pending_tab_closes) {
            if members.iter().any(|id| self.vim9_editors.contains_key(id)) {
                self.vim9_pending_tab_closes.push(members);
                continue;
            }
            let mut indexes: Vec<_> = members
                .iter()
                .filter_map(|id| self.session_manager.index_of(id))
                .collect();
            indexes.sort_unstable_by(|a, b| b.cmp(a));
            for index in indexes {
                if self.session_manager.len() == 1 {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    break;
                }
                self.close_session_or_owning_tab(index);
            }
            self.schedule_session_save();
        }
    }

    /// A close click asks Vim to quit through its channel. Never terminate a
    /// live editor PTY as a side effect of removing a native tab.
    pub(crate) fn request_vim9_close(&mut self, indexes: &[usize]) -> bool {
        let mut requested = false;
        for &index in indexes {
            if let Some(id) = self
                .session_manager
                .sessions()
                .get(index)
                .map(|s| &s.metadata.session_id)
            {
                if let Some(editor) = self.vim9_editors.get_mut(id) {
                    editor.send(Vim9Command::Quit);
                    requested = true;
                }
            }
        }
        if requested {
            self.set_status_for("Vim is checking modified buffers. Save your changes or use :qa! in Vim to discard them.", Duration::from_secs(8));
        }
        requested
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::Command;

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn form_enter_respects_egui_focus_and_cancellation_order() {
        for (events, expected) in [
            (vec![key(egui::Key::Enter)], true),
            (vec![key(egui::Key::Tab), key(egui::Key::Enter)], false),
            (vec![key(egui::Key::Escape), key(egui::Key::Enter)], false),
        ] {
            let ctx = egui::Context::default();
            let mut text = String::from("build command");
            let mut owner = None;
            ctx.run_ui(egui::RawInput::default(), |ui| {
                let field = ui.text_edit_singleline(&mut text);
                field.request_focus();
                owner = focused_chrome_widget(ui.ctx(), &[field.rect]);
                let _ = ui.button("Another action");
            })
            .drop_without_applying_deltas();
            let owner = owner.expect("the field owns the previous frame");
            let mut submitted = false;
            ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    // Escape has already cleared current focus here. The
                    // prior owner is still available for cancellation routing.
                    assert!(ui.memory(|memory| memory.had_focus_last_frame(owner)));
                    let field = ui.text_edit_singleline(&mut text);
                    submitted = form_submitted(&field);
                    let _ = ui.button("Another action");
                },
            )
            .drop_without_applying_deltas();
            assert_eq!(submitted, expected);
        }
    }

    #[test]
    fn dialog_confirmation_keeps_current_edits_but_claims_its_suffix() {
        let mut dialog = EditorDialog::new(DialogKind::QuickOpen);
        let mut events = vec![
            egui::Event::Text("latest-query".into()),
            key(egui::Key::Enter),
            egui::Event::Text("suffix".into()),
            key(egui::Key::Enter),
        ];
        assert!(!route_dialog_events(&mut dialog, true, &mut events));
        assert!(dialog.confirm_requested);
        assert_eq!(events, vec![egui::Event::Text("latest-query".into())]);
    }

    #[test]
    fn dialog_cancel_claims_escape_enter_and_text_batch() {
        let mut dialog = EditorDialog::new(DialogKind::QuickOpen);
        let mut events = vec![
            egui::Event::Text("query".into()),
            key(egui::Key::Escape),
            key(egui::Key::Enter),
        ];
        assert!(route_dialog_events(&mut dialog, true, &mut events));
        assert!(!dialog.confirm_requested);
        assert!(events.is_empty());
    }

    #[test]
    fn dialog_buttons_and_same_batch_focus_traversal_own_enter() {
        for (query_focus, mut events) in [
            (false, vec![key(egui::Key::Enter)]),
            (true, vec![key(egui::Key::Tab), key(egui::Key::Enter)]),
        ] {
            let mut dialog = EditorDialog::new(DialogKind::QuickOpen);
            assert!(!route_dialog_events(&mut dialog, query_focus, &mut events));
            assert!(!dialog.confirm_requested);
            assert!(events.contains(&key(egui::Key::Enter)));
        }
    }

    #[test]
    fn native_shortcuts_preserve_vim_control_keys() {
        for key in [egui::Key::P, egui::Key::G, egui::Key::W] {
            assert_eq!(shortcut_command(key, egui::Modifiers::CTRL), None);
        }
        assert_eq!(
            shortcut_command(egui::Key::S, egui::Modifiers::CTRL),
            Some(Command::Vim9Save)
        );
        assert_eq!(
            shortcut_command(egui::Key::P, egui::Modifiers::CTRL | egui::Modifiers::SHIFT),
            Some(Command::Vim9QuickOpen)
        );
        assert_eq!(
            shortcut_command(egui::Key::F8, egui::Modifiers::SHIFT),
            Some(Command::Vim9PreviousProblem)
        );
        assert_eq!(
            shortcut_command(
                egui::Key::P,
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT | egui::Modifiers::ALT
            ),
            None
        );
    }

    #[test]
    fn shell_binding_table_keeps_palette_remote_and_close_pane() {
        let bindings = crate::keybindings::KeyBindings::default_bindings();
        assert_eq!(
            bindings.get_command("ctrl+shift+p"),
            Some(Command::CommandPaletteToggle)
        );
        assert_eq!(
            bindings.get_command("ctrl+shift+s"),
            Some(Command::RemotePicker)
        );
        assert_eq!(
            bindings.get_command("ctrl+shift+w"),
            Some(Command::TerminalClosePane)
        );
    }

    #[test]
    fn goto_rejects_ambiguous_zero_and_overflow_positions() {
        assert_eq!(parse_location(" 42 : 8 "), Ok((42, Some(8))));
        assert_eq!(parse_location("1"), Ok((1, None)));
        for invalid in [
            "",
            "0",
            "1:0",
            "-1",
            "1:",
            "1:2:3",
            "1x",
            "18446744073709551616",
        ] {
            assert!(parse_location(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn quick_open_keyboard_selection_wraps_without_idle_scroll() {
        let mut picker = EditorDialog::new(DialogKind::QuickOpen);
        picker.move_selection(1);
        assert_eq!(picker.selected, 0);
        picker.matches = ["src/a.rs", "src/b.rs"]
            .iter()
            .map(|relative| Vim9ProjectFile {
                path: PathBuf::from(relative),
                relative: relative.to_string(),
            })
            .collect();
        picker.move_selection(-1);
        assert_eq!(picker.selected, 1);
        picker.move_selection(1);
        assert_eq!(picker.selected, 0);
        picker.reveal_selection = false;
        picker.move_selection(0);
        assert!(!picker.reveal_selection);
    }

    #[test]
    fn editor_commands_round_trip_for_custom_keybindings() {
        for name in [
            "ide:quick_open",
            "ide:goto",
            "ide:save",
            "ide:save_all",
            "ide:search",
            "ide:build",
            "ide:previous_buffer",
            "ide:next_buffer",
            "ide:close_buffer",
            "ide:next_problem",
            "ide:previous_problem",
        ] {
            let command: Command = name.parse().unwrap();
            assert!(is_editor_command(&command));
            assert_eq!(command.to_string(), name);
        }
    }
}
