//! Native workbench controls around a real Vim9 process in the terminal grid.
//! Editing, undo, registers, plugins and modified-buffer protection belong to Vim.

use crate::app::state::TerminalApp;
use eframe::egui;
use jterm_core::vim9::{Vim9Command, Vim9Session, Vim9Snapshot};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
        }
    }

    pub(crate) fn send(&mut self, command: Vim9Command) {
        self.error = self.session.send(command).err().map(|e| e.to_string());
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
                if ui.button("Save").clicked() { self.send(Vim9Command::Save); }
                if ui.button("Save all").clicked() { self.send(Vim9Command::SaveAll); }
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
                if ui.button("Previous issue").clicked() { self.send(Vim9Command::PreviousDiagnostic); }
                if ui.button("Next issue").clicked() { self.send(Vim9Command::NextDiagnostic); }
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
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
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
                    let enter =
                        search.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
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
                    ui.add(
                        egui::TextEdit::singleline(&mut self.build_input)
                            .hint_text("Build command · blank uses Vim makeprg")
                            .desired_width(240.0)
                            .char_limit(4096),
                    );
                    if snapshot.build.running {
                        if ui.button("Stop build").clicked() {
                            self.send(Vim9Command::CancelBuild);
                        }
                    } else if ui
                        .add_enabled(snapshot.connected, egui::Button::new("Build"))
                        .clicked()
                    {
                        self.send(Vim9Command::Build {
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
                        let name = Path::new(&buffer.name)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .filter(|n| !n.is_empty())
                            .unwrap_or("[No Name]");
                        let label = format!("{}{}", name, if buffer.modified { " •" } else { "" });
                        if ui
                            .selectable_label(buffer.current, label)
                            .on_hover_text(&buffer.name)
                            .clicked()
                        {
                            self.send(Vim9Command::Buffer { buffer: buffer.id });
                        }
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
                "{}{}   Ln {}, Col {}",
                file,
                if snapshot.modified {
                    "  • modified"
                } else {
                    ""
                },
                snapshot.line,
                snapshot.column
            ));
            if snapshot.build.running {
                ui.small(format!("Building: {}", snapshot.build.command));
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
            }
        });
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
        egui::Panel::top("vim9_workbench")
            .resizable(false)
            .show(root_ui, |ui| {
                editor.toolbar(ui);
            });
        if editor.problems_open {
            egui::Panel::bottom("vim9_problems_panel")
                .resizable(true)
                .default_size(160.0)
                .min_size(70.0)
                .max_size(300.0)
                .show(root_ui, |ui| {
                    editor.problems(ui);
                });
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
