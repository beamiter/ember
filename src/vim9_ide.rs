//! Native Vim9 workbench and GUI buffer rendering around a real Vim process.
//! Editing, undo, registers, plugins and modified-buffer protection belong to Vim.

use crate::app::state::TerminalApp;
use eframe::egui;
use jterm_core::vim9::{
    parse_vim9_location as parse_location, vim9_palette_matches, vim9_palette_query, Vim9Command,
    Vim9EntryRequest, Vim9PaletteCommand, Vim9ProjectFile, Vim9ProjectIndex, Vim9ProjectSnapshot,
    Vim9QuickOpenQuery, Vim9SearchCase, Vim9SearchOptions, Vim9Session, Vim9Snapshot, Vim9UiConfig,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const QUICK_OPEN_LIMIT: usize = 40;

/// Wake the native surface only when the shared immutable frame changes.
/// Metadata keeps its existing slower poll; idle buffers do not repaint at 60Hz.
struct RenderWake {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RenderWake {
    fn start(session: Vim9Session, ctx: egui::Context) -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("vim9-gui-repaint".into())
            .spawn(move || {
                let mut previous = None;
                while !worker_stop.load(Ordering::Acquire) {
                    let revision = session.render_snapshot().map(|frame| frame.revision);
                    if revision != previous {
                        previous = revision;
                        ctx.request_repaint();
                    }
                    std::thread::park_timeout(Duration::from_millis(16));
                }
            })
            .ok();
        Self { stop, thread }
    }
}

impl Drop for RenderWake {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

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
    command_matches: Vec<Vim9PaletteCommand>,
    activated_command: Option<(String, Vim9PaletteCommand)>,
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
            command_matches: Vec::new(),
            activated_command: None,
            selected: 0,
            needs_focus: true,
            reveal_selection: true,
            selection_delta: 0,
            query_widget: None,
            confirm_requested: false,
            error: None,
        }
    }

    fn refresh_quick_matches(
        &mut self,
        project: &Vim9ProjectSnapshot,
    ) -> Result<Vim9QuickOpenQuery, &'static str> {
        self.command_matches.clear();
        let parsed = project.quick_open_query(&self.query);
        let changed_files = self
            .ranked_files
            .as_ref()
            .is_none_or(|files| !Arc::ptr_eq(files, &project.files));
        if changed_files || self.ranked_query != self.query {
            self.matches = match &parsed {
                Ok(query) if !query.current_buffer => {
                    project.matches(&query.file_query, QUICK_OPEN_LIMIT)
                }
                _ => Vec::new(),
            };
            self.ranked_query.clone_from(&self.query);
            self.ranked_files = Some(Arc::clone(&project.files));
            self.selected = 0;
            self.reveal_selection = true;
            self.error = None;
        }
        parsed
    }

    fn quick_open_command(
        &mut self,
        project: &Vim9ProjectSnapshot,
    ) -> Result<Vim9Command, &'static str> {
        let query = self.refresh_quick_matches(project)?;
        query.command(self.matches.get(self.selected))
    }

    fn refresh_command_matches(&mut self) {
        let Some(query) = vim9_palette_query(&self.query) else {
            return;
        };
        if self.ranked_query != self.query || self.ranked_files.is_some() {
            self.command_matches = vim9_palette_matches(query, QUICK_OPEN_LIMIT);
            self.matches.clear();
            self.ranked_files = None;
            self.ranked_query.clone_from(&self.query);
            self.selected = 0;
            self.reveal_selection = true;
            self.error = None;
        }
    }

    fn quick_action_command(
        &mut self,
        project: &Vim9ProjectSnapshot,
        snapshot: &Vim9Snapshot,
    ) -> Result<Vim9Command, &'static str> {
        let activated = self.activated_command.take();
        if activated
            .as_ref()
            .is_some_and(|(query, _)| query != &self.query)
        {
            return Err("The command query changed. Choose a command again.");
        }
        if vim9_palette_query(&self.query).is_some() {
            self.refresh_command_matches();
            let command = if let Some((_, command)) = activated {
                if !self.command_matches.contains(&command) {
                    return Err("The command list changed. Choose a command again.");
                }
                command
            } else {
                *self
                    .command_matches
                    .get(self.selected)
                    .ok_or("Choose a matching editor command.")?
            };
            // Availability can change after paint, including when a running
            // search/build finishes. The current snapshot owns activation.
            command.command(snapshot)
        } else if activated.is_some() {
            Err("The command query changed. Choose a command again.")
        } else {
            self.quick_open_command(project)
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let count = if vim9_palette_query(&self.query).is_some() {
            self.command_matches.len()
        } else {
            self.matches.len()
        };
        if delta != 0 && count > 0 {
            self.selected = (self.selected as isize + delta).rem_euclid(count as isize) as usize;
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

fn search_options_summary(options: &Vim9SearchOptions) -> String {
    let mut summary = format!(
        "{} · {}",
        if options.regex { "Regex" } else { "Literal" },
        match options.case {
            Vim9SearchCase::Smart => "smart case",
            Vim9SearchCase::Sensitive => "match case",
            Vim9SearchCase::Ignore => "ignore case",
        }
    );
    if options.whole_word {
        summary.push_str(" · whole word");
    }
    if !options.glob.is_empty() {
        summary.push_str(" · glob: ");
        summary.push_str(&crate::review_text::visible_bounded(&options.glob, 256));
    }
    summary
}

fn diagnostic_kind_label(kind: &str) -> &'static str {
    match kind.to_ascii_uppercase().as_str() {
        "E" | "ERROR" => "Error",
        "W" | "WARNING" => "Warning",
        "I" | "INFO" | "INFORMATION" => "Info",
        "H" | "HINT" => "Hint",
        _ => "Diagnostic",
    }
}

fn diagnostic_location(root: &Path, filename: &str, line: u64, column: u64) -> String {
    let path = Path::new(filename);
    let relative = root
        .is_absolute()
        .then(|| path.strip_prefix(root).ok())
        .flatten()
        .filter(|relative| {
            !relative.as_os_str().is_empty()
                && !relative
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
        });
    let path = relative.unwrap_or(path).to_string_lossy();
    format!(
        "{}:{line}:{column}",
        crate::review_text::visible_bounded(&path, 1024)
    )
}

fn search_owns_empty_results(snapshot: &Vim9Snapshot) -> bool {
    snapshot.quickfix.source == "search"
        && snapshot.quickfix.count == 0
        && snapshot.diagnostics.is_empty()
        && !snapshot.search.query.is_empty()
        && !snapshot.search.running
        && !snapshot.search.cancelled
        && snapshot.search.exit_code == 1
        && snapshot.search.errors.is_empty()
        && snapshot.quickfix.title == format!("Search: {}", snapshot.search.query)
        && snapshot.search.count == 0
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

fn diagnostic_shortcuts_enabled(visible: bool, mode: &str) -> bool {
    // Terminal job/normal mode owns F8, including SimpleTerminal's popup.
    visible && mode != "t" && !mode.starts_with("nt")
}

/// These chords only apply while the Vim grid owns keyboard input. The
/// terminal's ordinary palette, SSH picker and pane-close bindings stay intact
/// in shell tabs; Vim's Ctrl+P, Ctrl+G and Ctrl+W remain untouched.
pub(crate) fn shortcut_command(
    key: egui::Key,
    modifiers: egui::Modifiers,
    diagnostics_visible: bool,
) -> Option<crate::keybindings::Command> {
    use crate::keybindings::Command;
    if modifiers.alt
        && !modifiers.ctrl
        && !modifiers.command
        && !modifiers.mac_cmd
        && !modifiers.shift
    {
        return match key {
            egui::Key::ArrowLeft => Some(Command::Vim9NavigateBack),
            egui::Key::ArrowRight => Some(Command::Vim9NavigateForward),
            _ => None,
        };
    }
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
        // A hidden workbench must leave plugin keys with Vim. In particular,
        // SimpleTerminal owns F8 / Shift-F8 in the focused buffer profile.
        (egui::Key::F8, false, false) if diagnostics_visible => Some(Command::Vim9NextProblem),
        (egui::Key::F8, false, true) if diagnostics_visible => Some(Command::Vim9PreviousProblem),
        _ => None,
    }
}

pub(crate) fn is_editor_command(command: &crate::keybindings::Command) -> bool {
    use crate::keybindings::Command;
    matches!(
        command,
        Command::Vim9QuickOpen
            | Command::Vim9Goto
            | Command::Vim9NavigateBack
            | Command::Vim9NavigateForward
            | Command::Vim9Save
            | Command::Vim9SaveAll
            | Command::Vim9Search
            | Command::Vim9CancelSearch
            | Command::Vim9Build
            | Command::Vim9PreviousBuffer
            | Command::Vim9NextBuffer
            | Command::Vim9CloseBuffer
            | Command::Vim9NextProblem
            | Command::Vim9PreviousProblem
    )
}

pub(crate) fn is_deferred_input(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::Key { .. }
            | egui::Event::Text(_)
            | egui::Event::Paste(_)
            | egui::Event::Copy
            | egui::Event::Cut
            | egui::Event::Ime(_)
    )
}

struct DeferredAction {
    action: Option<crate::keybindings::Command>,
    request: Option<u64>,
    following: Vec<egui::Event>,
    bytes: usize,
    opens_chrome: bool,
}

impl DeferredAction {
    fn new(action: crate::keybindings::Command) -> Self {
        use crate::keybindings::Command;
        let opens_chrome = matches!(
            action,
            Command::Vim9QuickOpen | Command::Vim9Goto | Command::Vim9Search | Command::Vim9Build
        );
        Self {
            action: Some(action),
            request: None,
            following: Vec::new(),
            bytes: 0,
            opens_chrome,
        }
    }

    fn append(&mut self, event: egui::Event) -> bool {
        let bytes = match &event {
            egui::Event::Text(text)
            | egui::Event::Paste(text)
            | egui::Event::Ime(egui::ImeEvent::Commit(text))
            | egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => text.len(),
            _ => 1,
        };
        if self.following.len() >= 8192 || self.bytes.saturating_add(bytes) > 8 * 1024 * 1024 {
            return false;
        }
        self.bytes += bytes;
        self.following.push(event);
        true
    }
}

pub(crate) struct Workbench {
    pub(crate) session: Vim9Session,
    pub(crate) buffer_font_size: Option<f32>,
    pub(crate) native_input: Arc<parking_lot::Mutex<crate::vim9_buffer::DirectInput>>,
    render_subscribed: bool,
    revealed_buffer: Option<i64>,
    render_wake: Option<RenderWake>,
    deferred_action: Option<DeferredAction>,
    snapshot: Arc<Vim9Snapshot>,
    refreshed_at: Instant,
    file_input: String,
    search_input: String,
    search_options: Vim9SearchOptions,
    build_input: String,
    rename_input: String,
    tools_open: bool,
    problems_open: bool,
    error: Option<String>,
    project_index: Option<Vim9ProjectIndex>,
    dialog: Option<EditorDialog>,
    focus_input: Option<ToolInput>,
    chrome_rects: Vec<egui::Rect>,
    chrome_focus: Option<egui::Id>,
    chrome_popup_open: bool,
    return_focus: bool,
}

impl Workbench {
    pub(crate) fn diagnostic_shortcuts_visible(&self, config: &Vim9UiConfig) -> bool {
        diagnostic_shortcuts_enabled(
            config.tool_panel || self.tools_open || self.problems_open,
            &self.snapshot.mode,
        )
    }

    pub(crate) fn new(session: Vim9Session, config: &Vim9UiConfig) -> Self {
        let snapshot = Arc::new(session.snapshot());
        Self {
            session,
            // Inherit live content-font settings until this editor is zoomed.
            buffer_font_size: None,
            native_input: Arc::new(parking_lot::Mutex::new(Default::default())),
            render_subscribed: false,
            revealed_buffer: None,
            render_wake: None,
            deferred_action: None,
            snapshot,
            refreshed_at: Instant::now(),
            file_input: String::new(),
            search_input: String::new(),
            search_options: Vim9SearchOptions::default(),
            build_input: String::new(),
            rename_input: String::new(),
            tools_open: false,
            problems_open: config.tool_panel,
            error: None,
            project_index: None,
            dialog: None,
            focus_input: None,
            chrome_rects: Vec::new(),
            chrome_focus: None,
            chrome_popup_open: false,
            return_focus: false,
        }
    }

    pub(crate) fn font_size(&self, configured_size: f32) -> f32 {
        crate::config::Config::clamp_font_size(self.buffer_font_size.unwrap_or(configured_size))
    }

    pub(crate) fn send(&mut self, command: Vim9Command) {
        let _ = self.send_tracked(command);
    }

    fn send_tracked(&mut self, command: Vim9Command) -> Option<u64> {
        self.native_input.lock().cancel();
        self.return_focus = true;
        match self.session.send(command) {
            Ok(id) => {
                self.error = None;
                Some(id)
            }
            Err(error) => {
                self.error = Some(error.to_string());
                None
            }
        }
    }

    fn dispatch_action(&mut self, command: crate::keybindings::Command) -> Option<u64> {
        use crate::keybindings::Command;
        let request = match command {
            Command::Vim9QuickOpen => {
                self.open_dialog(DialogKind::QuickOpen);
                None
            }
            Command::Vim9Goto => {
                self.open_dialog(DialogKind::Goto);
                None
            }
            Command::Vim9Search => {
                self.tools_open = true;
                self.focus_input = Some(ToolInput::Search);
                None
            }
            Command::Vim9Build => {
                self.tools_open = true;
                self.focus_input = Some(ToolInput::Build);
                None
            }
            Command::Vim9NavigateBack => Some(Vim9Command::NavigateBack),
            Command::Vim9NavigateForward => Some(Vim9Command::NavigateForward),
            Command::Vim9Save => Some(Vim9Command::Save),
            Command::Vim9SaveAll => Some(Vim9Command::SaveAll),
            Command::Vim9CancelSearch => Some(Vim9Command::CancelSearch),
            Command::Vim9PreviousBuffer => Some(Vim9Command::PreviousBuffer),
            Command::Vim9NextBuffer => Some(Vim9Command::NextBuffer),
            Command::Vim9CloseBuffer => Some(Vim9Command::CloseCurrentBuffer),
            Command::Vim9NextProblem => Some(Vim9Command::NextDiagnostic),
            Command::Vim9PreviousProblem => Some(Vim9Command::PreviousDiagnostic),
            _ => None,
        };
        request.and_then(|request| self.send_tracked(request))
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

    fn refresh(&mut self, native_buffer: bool) {
        if self.refreshed_at.elapsed() >= Duration::from_millis(100) {
            self.snapshot = Arc::new(self.session.snapshot());
            if !self.snapshot.connected {
                self.render_subscribed = false;
            } else if self.render_subscribed != native_buffer
                && self
                    .session
                    .send(Vim9Command::SetRender {
                        enabled: native_buffer,
                    })
                    .is_ok()
            {
                self.render_subscribed = native_buffer;
            }
            self.refreshed_at = Instant::now();
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, config: &Vim9UiConfig) {
        let snapshot = Arc::clone(&self.snapshot);
        if config.toolbar {
            ui.horizontal_wrapped(|ui| {
                ui.strong("VIM9 IDE");
                ui.separator();
                if let Some(frame) = self.session.render_snapshot() {
                    if frame.supported {
                        ui.small("GUI buffer · RGB").on_hover_text("Vim buffer text and GUI highlight attributes are drawn directly by egui.");
                    } else {
                        ui.small("Compatible view").on_hover_text(&frame.fallback_reason);
                    }
                    ui.separator();
                }
                ui.label(if snapshot.connected {
                    snapshot.mode.to_uppercase()
                } else {
                    "Connecting to Vim…".to_string()
                });
                ui.add_enabled_ui(snapshot.connected, |ui| {
                    if ui.add_enabled(snapshot.navigation.can_go_back, egui::Button::new("Back"))
                        .on_hover_text("Previous Vim jump location · Alt+Left")
                        .on_disabled_hover_text("No earlier location in this Vim window's jump list")
                        .clicked() {
                        self.send(Vim9Command::NavigateBack);
                    }
                    if ui.add_enabled(snapshot.navigation.can_go_forward, egui::Button::new("Forward"))
                        .on_hover_text("Next Vim jump location · Alt+Right")
                        .on_disabled_hover_text("No later location in this Vim window's jump list")
                        .clicked() {
                        self.send(Vim9Command::NavigateForward);
                    }
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
        }
        if config.tool_panel || self.tools_open {
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
            self.search_tools(ui, &snapshot);
            if !config.tool_panel && ui.small_button("Close tools · Esc").clicked() {
                self.tools_open = false;
                self.focus_input = None;
                self.return_focus = true;
            }
        }
        if config.buffer_tabs {
            let labels = crate::vim9_buffer_labels::vim9_buffer_labels(&snapshot.buffers);
            let current = snapshot
                .buffers
                .iter()
                .find(|buffer| buffer.current)
                .map(|buffer| buffer.id);
            let reveal_current = current != self.revealed_buffer;
            self.revealed_buffer = current;
            egui::ScrollArea::horizontal()
                .id_salt("vim9_buffers")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (buffer, name) in snapshot.buffers.iter().zip(&labels) {
                            ui.push_id(("buffer", buffer.id), |ui| {
                                let label =
                                    format!("{}{}", name, if buffer.modified { " •" } else { "" });
                                let tab = ui.selectable_label(buffer.current, label).on_hover_text(
                                    format!(
                                        "{}\nMiddle-click to close (keeps unsaved changes)",
                                        buffer.name
                                    ),
                                );
                                if reveal_current && buffer.current {
                                    tab.scroll_to_me(Some(egui::Align::Center));
                                }
                                if tab.clicked() {
                                    self.send(Vim9Command::Buffer { buffer: buffer.id });
                                }
                                if tab.middle_clicked() {
                                    self.send(Vim9Command::CloseBuffer { buffer: buffer.id });
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
        }
        if config.status_bar {
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
                let context = snapshot.workbench.status_text();
                if !context.is_empty() {
                    ui.small(crate::review_text::visible_bounded(&context, 1024));
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
                } else if snapshot.search.cancelled {
                    ui.small("Search cancelled");
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
        }
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

    fn search_tools(&mut self, ui: &mut egui::Ui, snapshot: &Vim9Snapshot) {
        ui.horizontal_wrapped(|ui| {
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.search_input)
                    .id_salt("vim9_project_search")
                    .hint_text("Project search · Ctrl+Shift+F")
                    .desired_width(250.0_f32.min((ui.available_width() - 12.0).max(80.0)))
                    .char_limit(4096),
            );
            if self.focus_input == Some(ToolInput::Search) {
                search.request_focus();
                self.focus_input = None;
            }
            let enter = form_submitted(&search);
            let options_label = if self.search_options == Vim9SearchOptions::default() {
                "Search options"
            } else {
                "Search options •"
            };
            egui::containers::menu::MenuButton::new(options_label)
                .config(
                    egui::containers::menu::MenuConfig::new()
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
                )
                .ui(ui, |ui| {
                    ui.set_max_width((ui.ctx().viewport_rect().width() - 32.0).clamp(220.0, 380.0));
                    ui.strong("Options for the next search");
                    ui.horizontal_wrapped(|ui| {
                        ui.radio_value(
                            &mut self.search_options.case,
                            Vim9SearchCase::Smart,
                            "Smart case",
                        );
                        ui.radio_value(
                            &mut self.search_options.case,
                            Vim9SearchCase::Sensitive,
                            "Match case",
                        );
                        ui.radio_value(
                            &mut self.search_options.case,
                            Vim9SearchCase::Ignore,
                            "Ignore case",
                        );
                    });
                    ui.weak("An uppercase query makes smart case sensitive.");
                    ui.checkbox(&mut self.search_options.regex, "Regular expression");
                    ui.checkbox(&mut self.search_options.whole_word, "Whole word");
                    ui.label("File glob");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search_options.glob)
                            .hint_text("For example **/*.rs or !vendor/**")
                            .desired_width(ui.available_width().min(270.0))
                            .char_limit(4096),
                    );
                    ui.weak("One pattern; spaces and commas belong to the filename.");
                    if ui.button("Reset options").clicked() {
                        self.search_options = Vim9SearchOptions::default();
                    }
                })
                .0
                .on_hover_text(search_options_summary(&self.search_options));
            if (ui
                .add_enabled(
                    snapshot.connected && !self.search_input.is_empty(),
                    egui::Button::new("Find"),
                )
                .on_hover_text("Run a project search with the selected options")
                .clicked()
                || enter)
                && snapshot.connected
                && !self.search_input.is_empty()
            {
                self.send(Vim9Command::SearchWithOptions {
                    query: self.search_input.clone(),
                    options: self.search_options.clone(),
                });
                self.problems_open = true;
            }
            if snapshot.search.running && ui.button("Stop search").clicked() {
                self.send(Vim9Command::CancelSearch);
            }
        });
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
        egui::ScrollArea::vertical().id_salt("vim9_problems").auto_shrink([false, false]).show(ui, |ui| {
            if !snapshot.quickfix.title.is_empty() || snapshot.quickfix.count > 0 {
                let source = match snapshot.quickfix.source.as_str() {
                    "search" => "Project search",
                    "build" => "Build",
                    _ => "Vim quickfix",
                };
                ui.label(format!("Quickfix · {source} · {}/{} · {}",
                    snapshot.quickfix.index, snapshot.quickfix.count,
                    crate::review_text::visible_bounded(&snapshot.quickfix.title, 1024)))
                    .on_hover_text("The current Vim quickfix list. Window-local diagnostics may also appear below.");
                ui.separator();
            }
            if !snapshot.search.query.is_empty() {
                ui.strong(format!("Project search · {}", snapshot.search.query));
                ui.weak(search_options_summary(&snapshot.search.options));
                if snapshot.search.running {
                    ui.label(format!("Searching… {} matches so far. Previous quickfix entries remain until search completes.", snapshot.search.count));
                } else if snapshot.search.cancelled {
                    ui.label(format!("Search cancelled · {} matches found before cancellation", snapshot.search.count));
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
            if search_owns_empty_results(&snapshot) {
                ui.weak("No matches for this project search. Change the query or Search options and run Find again.");
            } else if snapshot.diagnostics.is_empty() {
                ui.weak("No quickfix entries. Run a build or project search to populate this panel.");
            }
            for diagnostic in &snapshot.diagnostics {
                let kind = diagnostic_kind_label(&diagnostic.kind);
                let color = match kind {
                    "Error" => ui.visuals().error_fg_color,
                    "Warning" => ui.visuals().warn_fg_color,
                    "Info" | "Hint" => ui.visuals().hyperlink_color,
                    _ => ui.visuals().text_color(),
                };
                let location = diagnostic_location(self.session.root(), &diagnostic.filename, diagnostic.line, diagnostic.column);
                let text = crate::review_text::visible_bounded(&diagnostic.text, 512);
                let full_location = diagnostic_location(Path::new(""), &diagnostic.filename, diagnostic.line, diagnostic.column);
                let tooltip = format!("{kind} · {full_location}\n{}", crate::review_text::visible_bounded(&diagnostic.text, 4096));
                let row = ui.add_sized([ui.available_width(), 24.0],
                    egui::Button::new(egui::RichText::new(format!("{kind}  {location}  {text}")).color(color))
                        .frame(false).truncate()).on_hover_text(tooltip);
                if row.clicked() && !diagnostic.filename.is_empty() {
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
        let project = self
            .project_index
            .as_ref()
            .map(Vim9ProjectIndex::snapshot)
            .unwrap_or_default();
        let mut current_buffer = false;
        let mut command_mode = false;
        let mut command_available = false;
        let mut quick_valid = false;
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
                        let context = self.snapshot.workbench.status_text();
                        if !context.is_empty() {
                            ui.weak(crate::review_text::visible_bounded(&context, 1024));
                        }
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
                            DialogKind::QuickOpen => "File[:line[:column]], :line, or >command…",
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

                command_mode = dialog.kind == DialogKind::QuickOpen
                    && vim9_palette_query(&dialog.query).is_some();
                if command_mode {
                    quick_valid = true;
                    dialog.refresh_command_matches();
                    let delta = std::mem::take(&mut dialog.selection_delta);
                    dialog.move_selection(delta);
                    let snapshot = self.session.snapshot();
                    command_available = dialog.command_matches.get(dialog.selected)
                        .is_some_and(|command| command.unavailable_reason(&snapshot).is_none());
                    ui.small(format!("Editor commands · {} matches", dialog.command_matches.len()));
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .id_salt(("vim9_command_results", session_id))
                        .max_height((ctx.viewport_rect().height() * 0.45).min(360.0))
                        .show(ui, |ui| {
                            if dialog.command_matches.is_empty() {
                                ui.weak("No matching editor commands. Try >save, >split, or >definition.");
                            }
                            for (index, command) in dialog.command_matches.clone().into_iter().enumerate() {
                                let reason = command.unavailable_reason(&snapshot);
                                let label = if command.shortcut().is_empty() {
                                    command.title().to_owned()
                                } else {
                                    format!("{}    {}", command.title(), command.shortcut())
                                };
                                let row = ui.push_id((command.title(), &dialog.query), |ui| {
                                    ui.add_enabled(reason.is_none(), egui::Button::new(label)
                                        .selected(index == dialog.selected)
                                        .truncate()
                                        .min_size(egui::vec2(ui.available_width(), 24.0)))
                                        .on_disabled_hover_text(reason.unwrap_or_default())
                                }).inner;
                                if index == dialog.selected && dialog.reveal_selection {
                                    row.scroll_to_me(Some(egui::Align::Center));
                                }
                                if let Some(reason) = reason {
                                    ui.weak(reason);
                                }
                                if row.clicked() {
                                    dialog.selected = index;
                                    dialog.activated_command = Some((dialog.query.clone(), command));
                                    dialog.confirm_requested = true;
                                }
                            }
                        });
                    dialog.reveal_selection = false;
                    ui.weak("↑ ↓ select · Enter runs · Esc returns to Vim · remove > to find files");
                } else if dialog.kind == DialogKind::QuickOpen {
                    let parsed = dialog.refresh_quick_matches(&project);
                    current_buffer = parsed.as_ref().is_ok_and(|query| query.current_buffer);
                    quick_valid = parsed.is_ok();
                    match &parsed {
                        Ok(query) => {
                            if let Some((line, column)) = query.location {
                                let target = if query.current_buffer { "Current buffer" } else { "Selected file" };
                                ui.label(format!("{target} · line {line}{}", column.map(|column| format!(" · byte column {column}")).unwrap_or_default()));
                            }
                        }
                        Err(error) => { ui.colored_label(ui.visuals().error_fg_color, *error); }
                    }
                    if current_buffer || !quick_valid {
                        dialog.selection_delta = 0;
                    }
                    if current_buffer {
                        ui.weak("Enter jumps in the current buffer · Esc returns to Vim");
                    } else if quick_valid {
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
                                    let row = ui.push_id((&file.path, &dialog.query), |ui| ui.add_sized(
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
                        ui.weak("↑ ↓ select · Enter opens · Esc returns to Vim · > for editor commands");
                    }
                }
                if let Some(error) = &dialog.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let label = if command_mode { "Run selected" } else if dialog.kind == DialogKind::Goto || current_buffer { "Go" } else { "Open selected" };
                    let command_id = dialog.command_matches.get(dialog.selected).map(|command| command.title()).unwrap_or("file");
                    let enabled = if command_mode { command_available } else {
                        dialog.kind == DialogKind::Goto || (quick_valid && (current_buffer || !dialog.matches.is_empty()))
                    };
                    if ui.push_id(("confirm", &dialog.query, command_id), |ui|
                        ui.add_enabled(enabled, egui::Button::new(label))
                    ).inner.clicked() {
                        if command_mode {
                            dialog.activated_command = dialog.command_matches.get(dialog.selected)
                                .map(|command| (dialog.query.clone(), *command));
                        }
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
                DialogKind::QuickOpen => {
                    dialog.quick_action_command(&project, &self.session.snapshot())
                }
            };
            match command {
                Ok(command) => {
                    if self.send_tracked(command).is_some() {
                        return;
                    }
                    self.return_focus = false;
                    dialog.error = Some(self.error.clone().unwrap_or_else(|| {
                        "The Vim action could not be queued. Try again when Vim is ready.".into()
                    }));
                    dialog.needs_focus = true;
                }
                Err(error) => {
                    dialog.error = (dialog.kind != DialogKind::QuickOpen || quick_valid)
                        .then(|| error.to_string());
                    dialog.needs_focus = true;
                }
            }
        }
        self.dialog = Some(dialog);
    }
}

/// The managed shell's Vim launcher and explicit `--ide` actions enter the
/// same native editor. OsString preserves every shell-expanded Vim argument.
pub(crate) fn startup_target() -> Result<Option<Vim9EntryRequest>, String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if let Some(request) = jterm_core::vim9::shell_vim_request(&args)? {
        return Ok(Some(request));
    }
    jterm_core::vim9::explicit_ide_target(&args)
        .map(|target| target.map(Vim9EntryRequest::command_line))
}

pub(crate) fn launch_button(ui: &egui::Ui, rect: egui::Rect, font_size: f32) -> bool {
    let id = ui.id().with("vim9_launch");
    // Terminal Space/Enter must never become a launcher activation. Retire
    // focus from an older frame before egui can synthesize a keyboard click,
    // and keep this mouse shortcut out of keyboard focus navigation. Explicit
    // accessibility clicks still work through Response::clicked().
    ui.memory_mut(|memory| memory.surrender_focus(id));
    let response = ui.interact(rect, id, egui::Sense::CLICK);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            "Open native Vim9 IDE",
        )
    });
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

impl Drop for Workbench {
    fn drop(&mut self) {
        self.session.revoke();
    }
}

impl TerminalApp {
    pub(crate) fn poll_shell_vim(&mut self, ctx: &egui::Context) {
        // A shell may be waiting without producing any terminal output.
        ctx.request_repaint_after(Duration::from_millis(100));
        for invocation in jterm_core::vim9::take_shell_vim_requests() {
            let origin = self
                .session_manager
                .sessions()
                .iter()
                .find(|session| invocation.matches_pid(session.get_shell_pid()))
                .map(|session| session.metadata.session_id.clone());
            let Some(origin) = origin else { continue };
            if let Some(index) = self.session_manager.index_of(&origin) {
                self.activate_session(index);
            }
            self.open_vim9_ide(invocation.request);
            let editor = self
                .session_manager
                .get_active_session_mut()
                .metadata
                .session_id
                .clone();
            if editor != origin && self.vim9_editors.contains_key(&editor) {
                self.vim9_shell_tabs
                    .insert(editor, (origin, invocation.completion));
            }
        }
    }

    /// A shell launch is a synchronous editor command: its original shell
    /// resumes when this process returns Vim's real status, not a replacement
    /// shell tab. Other ordinary editor launches keep their workspace behavior.
    pub(crate) fn finish_shell_vim_exit(
        &mut self,
        id: &str,
        code: Option<i32>,
        ctx: &egui::Context,
    ) -> bool {
        if let Some((origin, completion)) = self.vim9_shell_tabs.remove(id) {
            completion.finish(shell_exit_status(code));
            if let Some(index) = self.session_manager.index_of(id) {
                self.close_session_or_owning_tab(index);
            }
            if let Some(index) = self.session_manager.index_of(&origin) {
                self.activate_session(index);
            }
            self.schedule_session_save();
            return true;
        }
        if self.vim9_shell_entry_session.as_deref() != Some(id) {
            return false;
        }
        self.vim9_shell_entry_session = None;
        self.vim9_window_close_pending = false;
        crate::VIM9_SHELL_EXIT_STATUS.store(
            shell_exit_status(code),
            std::sync::atomic::Ordering::Relaxed,
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        true
    }

    /// Bind direct input to the exact native image last painted in this pane.
    /// Every split keeps a single scroll transaction; native pointer ownership
    /// suppresses the parallel PTY/local-selection path for its entire gesture.
    pub(crate) fn handle_vim9_input(&mut self, ctx: &egui::Context, enabled: bool) -> bool {
        let keyboard = self.frame_events.iter().any(is_keyboard_event)
            || ctx.input(|input| input.events.iter().any(is_keyboard_event));
        let (point, down, pressed, released, any_down, any_pressed, now, modifiers, focused) = ctx
            .input(|input| {
                (
                    input.pointer.interact_pos().or(input.pointer.hover_pos()),
                    input.pointer.button_down(egui::PointerButton::Primary),
                    input.pointer.button_pressed(egui::PointerButton::Primary),
                    input.pointer.button_released(egui::PointerButton::Primary),
                    input.pointer.any_down(),
                    input.pointer.any_pressed(),
                    input.time,
                    input.modifiers,
                    input.viewport().focused.unwrap_or(true),
                )
            });
        let active_id = self
            .session_manager
            .get_active_session_mut()
            .metadata
            .session_id
            .clone();
        let blocked = !enabled || !focused || keyboard || !self.config.vim9.native_buffer;
        let mut claimed = false;
        for (id, editor) in &self.vim9_editors {
            let mut input = editor.native_input.lock();
            input.retire_unpainted(ctx);
            if blocked || id != &active_id {
                if any_pressed && point.is_some_and(|point| input.presented_fraction_at(point)) {
                    input.discard_press();
                }
                input.cancel();
                claimed |= input.finish_cancelled_pointer(any_down);
            }
        }
        if blocked {
            return claimed;
        }
        let renderer = if self.layout().panes().len() > 1 {
            let Some(index) = self
                .layout()
                .panes()
                .iter()
                .position(|pane| pane.id == self.layout().focused_pane_id)
            else {
                return claimed;
            };
            let Some(renderer) = self.pane_renderers.get(index) else {
                return claimed;
            };
            renderer
        } else {
            &self.renderer
        };
        let Some(content) = renderer.last_content_rect else {
            return claimed;
        };
        let geometry = jterm_core::vim9::Vim9RenderMetrics {
            origin_x: content.left(),
            origin_y: content.top(),
            cell_width: renderer.char_width,
            line_height: renderer.line_height,
        };
        let session = self.session_manager.get_active_session_mut();
        let terminal = session.terminal.lock();
        let (columns, rows) = terminal.get_dimensions();
        let owns_native = session.purpose != crate::session::SessionPurpose::RetainedCommand
            && terminal.selection.is_none()
            && terminal.scroll_offset == 0
            && terminal.preedit_text.is_empty()
            && session.pending_input.is_empty()
            && matches!(session.shell.input_drained(), Ok(true));
        drop(terminal);
        let Some(editor) = self.vim9_editors.get_mut(&active_id) else {
            return claimed;
        };
        let mut direct = editor.native_input.lock();
        let fractional_press =
            any_pressed && point.is_some_and(|point| direct.presented_fraction_at(point));
        let snapshot = editor.session.input_snapshot();
        let Some(frame) = snapshot.frame.as_ref().filter(|frame| {
            owns_native
                && editor.deferred_action.is_none()
                && geometry.is_valid()
                && renderer.native_vim9_identity == Some(editor.session.entry_id())
                && crate::vim9_buffer::usable(frame, rows, columns)
        }) else {
            if fractional_press {
                direct.discard_press();
            }
            direct.cancel();
            return claimed | direct.finish_cancelled_pointer(any_down);
        };
        if frame.has_overlays() {
            // Vim owns popup/completion mouse selection and wheel behavior. Stop
            // obsolete buffer motion before allowing its original PTY input.
            direct.cancel();
            return claimed | direct.finish_cancelled_pointer(any_down);
        }
        direct.set_smooth(self.config.vim9.smooth_scroll);
        direct.reconcile(&snapshot, geometry);
        let plain = !modifiers.ctrl && !modifiers.command && !modifiers.shift && !modifiers.alt;
        if !plain || (any_pressed && !pressed) {
            if fractional_press {
                direct.discard_press();
            }
            direct.cancel();
            return claimed | direct.finish_cancelled_pointer(any_down);
        }
        // Reconciliation can invalidate the image's source or geometry before
        // the press is dispatched. Swallow that old-image gesture as a whole.
        if fractional_press && point.is_some_and(|point| !direct.presented_fraction_at(point)) {
            direct.discard_press();
        }
        if direct.discarding_pointer() {
            return claimed | direct.finish_cancelled_pointer(any_down);
        }
        if pressed && direct.pointer_captured() {
            direct.cancel();
        }
        if pressed && !direct.pointer_captured() && self.terminal_mouse_capture.is_none() {
            if let Some(point) = point.filter(|point| content.contains(*point)) {
                claimed |= direct.press(frame, geometry, point, now, &editor.session);
            }
        }
        if direct.discarding_pointer() {
            return claimed | direct.finish_cancelled_pointer(any_down);
        }
        if direct.pointer_captured() {
            claimed |= direct.pointer(
                frame,
                geometry,
                if down || released { point } else { None },
                released || !down,
                &editor.session,
            );
        } else if pressed || any_down {
            direct.cancel();
            return claimed;
        }
        if !down && !pressed && matches!(frame.mode.as_str(), "n" | "i") {
            if let Some(point) = point.filter(|point| content.contains(*point)) {
                let window = direct
                    .wheel_window_at(frame, geometry, point)
                    .and_then(|id| frame.windows.iter().find(|window| window.window == id));
                let rows = window
                    .map_or(frame.view.rows, |window| window.view.rows)
                    .max(1);
                let (count, delta, discrete) = self
                    .frame_events
                    .iter()
                    .filter_map(|event| {
                        crate::vim9_buffer::wheel_lines(event, geometry.line_height, rows)
                            .map(|delta| (delta, crate::vim9_buffer::wheel_is_discrete(event)))
                    })
                    .fold(
                        (0usize, 0.0f64, true),
                        |(count, total, discrete), (delta, notch)| {
                            (
                                count + 1,
                                (total + delta).clamp(-120.0, 120.0),
                                discrete && notch,
                            )
                        },
                    );
                let accepted = if count == 0 {
                    false
                } else if direct.stale_presentation_at(frame, geometry, point) {
                    direct.cancel();
                    true
                } else {
                    window.is_some_and(|window| {
                        if discrete {
                            direct.wheel_discrete(
                                frame,
                                window.window,
                                geometry,
                                delta,
                                &editor.session,
                            )
                        } else {
                            direct.wheel(frame, window.window, geometry, delta, &editor.session)
                        }
                    })
                };
                if accepted {
                    self.frame_events.retain(|event| {
                        crate::vim9_buffer::wheel_lines(event, geometry.line_height, rows).is_none()
                    });
                    ctx.input_mut(|input| {
                        input.events.retain(|event| {
                            crate::vim9_buffer::wheel_lines(event, geometry.line_height, rows)
                                .is_none()
                        });
                        input.smooth_scroll_delta.y = 0.0;
                    });
                    ctx.request_repaint();
                }
            }
        }
        // Pointer-owned fractions stay fixed until the real selection settles.
        if !direct.pointer_captured() && !claimed && matches!(frame.mode.as_str(), "n" | "i") {
            direct.flush_scroll(&editor.session);
        }
        if direct.needs_frame() {
            ctx.request_repaint();
        } else if direct.needs_wake(&editor.session) {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        claimed
    }

    pub(crate) fn defer_vim9_action(
        &mut self,
        ctx: &egui::Context,
        command: crate::keybindings::Command,
        following: Vec<egui::Event>,
    ) {
        let id = &self.session_manager.sessions()[self.session_manager.active_index()]
            .metadata
            .session_id;
        let Some(editor) = self.vim9_editors.get_mut(id) else {
            return;
        };
        let mut deferred = DeferredAction::new(command);
        for event in following {
            if !deferred.append(event) {
                editor.error =
                    Some("Vim input buffer is full; wait before entering more input".into());
                break;
            }
        }
        editor.deferred_action = Some(deferred);
        ctx.request_repaint();
    }

    /// One owner spans keyboard admission, the actual PTY writer, and the
    /// channel reply. A tab change cannot retarget the action or its suffix.
    pub(crate) fn advance_vim9_deferred_input(&mut self, ctx: &egui::Context) {
        let active = self.session_manager.sessions()[self.session_manager.active_index()]
            .metadata
            .session_id
            .clone();
        let ui_owns_input = self.terminal_input_blocked(ctx);
        let focused = ctx.input(|input| input.focused);
        if !ui_owns_input {
            if let Some(editor) = self.vim9_editors.get_mut(&active) {
                if let Some(deferred) = &mut editor.deferred_action {
                    let mut full = false;
                    self.frame_events.retain(|event| {
                        if is_deferred_input(event) {
                            full |= !deferred.append(event.clone());
                            false
                        } else {
                            true
                        }
                    });
                    ctx.input_mut(|input| input.events.retain(|event| !is_deferred_input(event)));
                    if full {
                        editor.error = Some(
                            "Vim input buffer is full; wait before entering more input".into(),
                        );
                    }
                }
            }
        }

        let mut replay = Vec::new();
        for (id, editor) in &mut self.vim9_editors {
            let Some(mut deferred) = editor.deferred_action.take() else {
                continue;
            };
            let Some(index) = self.session_manager.index_of(id) else {
                continue;
            };
            let session = &self.session_manager.sessions()[index];
            if deferred.action.is_some() {
                if !session.pending_input.is_empty() || self.osc_paste_input_barriers.blocks(id) {
                    editor.deferred_action = Some(deferred);
                    ctx.request_repaint_after(Duration::from_millis(5));
                    continue;
                }
                match session.shell.input_drained() {
                    Ok(false) => {
                        editor.deferred_action = Some(deferred);
                        ctx.request_repaint_after(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => {
                        editor.error = Some(format!("Vim action cancelled: {error}"));
                        continue;
                    }
                    Ok(true) => {}
                }
                if let Some(action) = deferred.action.take() {
                    deferred.request = editor.dispatch_action(action);
                }
                // The UI must first paint a newly opened input field/dialog.
                // Its queued query/Enter then sees the established owner.
                editor.deferred_action = Some(deferred);
                ctx.request_repaint();
                continue;
            }
            let complete = deferred
                .request
                .is_none_or(|request| !editor.session.command_pending(request));
            if complete && deferred.following.is_empty() {
                continue;
            }
            if complete && id == &active && focused && (!ui_owns_input || deferred.opens_chrome) {
                replay.append(&mut deferred.following);
            } else {
                editor.deferred_action = Some(deferred);
                if !complete {
                    ctx.request_repaint_after(Duration::from_millis(5));
                }
            }
        }
        if !replay.is_empty() {
            ctx.input_mut(|input| {
                let mut events = replay.clone();
                events.append(&mut input.events);
                input.events = events;
            });
            replay.append(&mut self.frame_events);
            self.frame_events = replay;
        }
    }

    pub(crate) fn vim9_buffer_font_size(&self) -> f32 {
        self.active_vim9_editor()
            .map_or(self.config.font_size, |editor| {
                editor.font_size(self.config.font_size)
            })
    }

    pub(crate) fn set_vim9_buffer_font_size(&mut self, size: f32) -> bool {
        let Some(session) = self
            .session_manager
            .sessions()
            .get(self.session_manager.active_index())
        else {
            return false;
        };
        let Some(editor) = self.vim9_editors.get_mut(&session.metadata.session_id) else {
            return false;
        };
        editor.native_input.lock().cancel();
        editor.buffer_font_size = Some(crate::config::Config::clamp_font_size(size));
        self.force_resize_session = true;
        true
    }

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
        // Menus can extend beyond the toolbar, and egui closes a popup before
        // application logic sees its cancelling Escape. Retain that previous
        // owner so Escape + Enter cannot leak from an options field into Vim.
        if editor.chrome_popup_open || egui::Popup::is_any_open(ctx) {
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
        let _ = editor.dispatch_action(command);
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
                editor.tools_open = false;
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
        editor.chrome_popup_open = !return_focus && egui::Popup::is_any_open(ctx);
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

    pub(crate) fn vim9_project_target(&mut self) -> PathBuf {
        let session = self.session_manager.get_active_session_mut();
        jterm_core::process::process_cwd(session.get_shell_pid())
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub(crate) fn open_vim9_ide(&mut self, request: Vim9EntryRequest) {
        let session = match Vim9Session::start(request) {
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
        self.vim9_editors.insert(
            created.session_id,
            Workbench::new(session, &self.config.vim9),
        );
        self.tabs.on_session_inserted(created.session_index);
        self.tabs.insert_tab_after_active(created.session_index);
        self.activate_session(created.session_index);
        self.force_resize_session = true;
        if self.config.vim9.project_panel {
            self.sidebar.visible = true;
            self.sidebar.view = crate::sidebar::SidebarView::Files;
            if let Some(error) = self.sidebar.set_current_dir(root) {
                self.set_status_for(
                    format!("Vim9 opened; file tree: {error}"),
                    Duration::from_secs(8),
                );
            }
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
        let id = self
            .session_manager
            .get_active_session_mut()
            .metadata
            .session_id
            .clone();
        let Some(editor) = self.vim9_editors.get_mut(&id) else {
            return;
        };
        if editor.render_wake.is_none() {
            editor.render_wake = Some(RenderWake::start(
                editor.session.clone(),
                root_ui.ctx().clone(),
            ));
        }
        editor.refresh(self.config.vim9.native_buffer);
        editor.chrome_rects.clear();
        let config = &self.config.vim9;
        let chrome_visible = config.toolbar
            || config.buffer_tabs
            || config.status_bar
            || config.tool_panel
            || editor.tools_open
            || editor.error.is_some()
            || editor.snapshot.error.is_some();
        // Frame revisions wake the native renderer directly. Hidden workbench
        // metadata need not redraw the whole app ten times per second, and an
        // editor in a background tab must not keep a shell tab repainting.
        let metadata_interval = if chrome_visible
            || editor.problems_open
            || editor.dialog.is_some()
            || editor.deferred_action.is_some()
            || editor.snapshot.pending > 0
            || !editor.render_subscribed
            || editor
                .session
                .render_snapshot()
                .is_none_or(|frame| !frame.supported)
        {
            100
        } else {
            500
        };
        root_ui
            .ctx()
            .request_repaint_after(Duration::from_millis(metadata_interval));
        if chrome_visible {
            let toolbar = egui::Panel::top(egui::Id::new(("vim9_workbench", &id)))
                .resizable(false)
                .show(root_ui, |ui| {
                    editor.toolbar(ui, config);
                });
            editor.chrome_rects.push(toolbar.response.rect);
        }
        if editor.problems_open {
            let problems = egui::Panel::bottom(egui::Id::new(("vim9_problems_panel", &id)))
                .resizable(true)
                .default_size(240.0)
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

fn shell_exit_status(code: Option<i32>) -> i32 {
    match code {
        Some(code) if code >= 0 => code.min(255),
        Some(signal) => 128i32.saturating_add(signal.saturating_abs()).min(255),
        None => 1,
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
    fn launcher_never_activates_from_terminal_space_or_enter() {
        for activation in [egui::Key::Space, egui::Key::Enter] {
            let ctx = egui::Context::default();
            let rect = egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(52.0, 24.0));
            let mut launcher = None;
            ctx.run_ui(egui::RawInput::default(), |ui| {
                assert!(!launch_button(ui, rect, 14.0));
                let id = ui.id().with("vim9_launch");
                // Reproduce the stale focus left by a previous launcher click.
                ui.memory_mut(|memory| memory.request_focus(id));
                launcher = Some(id);
            })
            .drop_without_applying_deltas();
            let events = vec![key(activation), egui::Event::Text(" ".into())];
            ctx.run_ui(
                egui::RawInput {
                    events: events.clone(),
                    ..Default::default()
                },
                |ui| {
                    assert!(!launch_button(ui, rect, 14.0), "{activation:?} opened Vim9");
                    assert!(!ui.memory(|memory| memory.has_focus(launcher.unwrap())));
                    assert_eq!(ui.input(|input| input.events.clone()), events);
                },
            )
            .drop_without_applying_deltas();
        }
    }

    #[test]
    fn launcher_click_opens_once_and_leaves_terminal_focus_available() {
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(52.0, 24.0));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(200.0, 100.0),
            )),
            ..Default::default()
        };
        let mut launches = 0;
        for events in [
            vec![],
            vec![
                egui::Event::PointerMoved(rect.center()),
                egui::Event::PointerButton {
                    pos: rect.center(),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            vec![egui::Event::PointerButton {
                pos: rect.center(),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            vec![],
            vec![key(egui::Key::Space), egui::Event::Text(" ".into())],
            vec![key(egui::Key::Enter)],
        ] {
            ctx.run_ui(
                egui::RawInput {
                    events,
                    ..input.clone()
                },
                |ui| {
                    launches += usize::from(launch_button(ui, rect, 14.0));
                    assert!(!ui.memory(|memory| memory.has_focus(ui.id().with("vim9_launch"))));
                },
            )
            .drop_without_applying_deltas();
        }
        assert_eq!(launches, 1);
    }

    #[test]
    fn launcher_keeps_an_explicit_accessibility_click_action() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let rect = egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(52.0, 24.0));
        let mut id = None;
        let mut output = ctx.run_ui(Default::default(), |ui| {
            assert!(!launch_button(ui, rect, 14.0));
            id = Some(ui.id().with("vim9_launch"));
        });
        output.textures_delta.clear();
        let id = id.unwrap();
        let node = output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find_map(|(node_id, node)| (*node_id == id.accesskit_id()).then_some(node))
            .expect("the pointer launcher remains available to assistive technology");
        assert_eq!(node.role(), egui::accesskit::Role::Button);
        assert_eq!(node.label(), Some("Open native Vim9 IDE"));
        assert!(node.supports_action(egui::accesskit::Action::Click));
        ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::AccessKitActionRequest(
                    egui::accesskit::ActionRequest {
                        action: egui::accesskit::Action::Click,
                        target_tree: egui::accesskit::TreeId::ROOT,
                        target_node: id.accesskit_id(),
                        data: None,
                    },
                )],
                ..Default::default()
            },
            |ui| assert!(launch_button(ui, rect, 14.0)),
        )
        .drop_without_applying_deltas();
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
        let shortcut_command = |key, modifiers| super::shortcut_command(key, modifiers, true);
        assert_eq!(
            shortcut_command(egui::Key::Enter, egui::Modifiers::NONE),
            None
        );
        for key in [
            egui::Key::P,
            egui::Key::G,
            egui::Key::W,
            egui::Key::O,
            egui::Key::I,
        ] {
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
        for (key, command) in [
            (egui::Key::ArrowLeft, Command::Vim9NavigateBack),
            (egui::Key::ArrowRight, Command::Vim9NavigateForward),
        ] {
            assert_eq!(shortcut_command(key, egui::Modifiers::ALT), Some(command));
            for modifiers in [
                egui::Modifiers::NONE,
                egui::Modifiers::CTRL,
                egui::Modifiers::ALT | egui::Modifiers::CTRL,
                egui::Modifiers::ALT | egui::Modifiers::SHIFT,
            ] {
                assert_eq!(shortcut_command(key, modifiers), None);
            }
        }
    }

    #[test]
    fn hidden_diagnostic_panels_preserve_simpleterminal_keys() {
        for (modifiers, action) in [
            (egui::Modifiers::NONE, Command::Vim9NextProblem),
            (egui::Modifiers::SHIFT, Command::Vim9PreviousProblem),
        ] {
            assert_eq!(shortcut_command(egui::Key::F8, modifiers, false), None);
            assert_eq!(
                shortcut_command(egui::Key::F8, modifiers, true),
                Some(action)
            );
        }
        for visible in [false, true] {
            assert_eq!(
                shortcut_command(egui::Key::F7, egui::Modifiers::NONE, visible),
                None
            );
            assert_eq!(
                shortcut_command(egui::Key::F8, egui::Modifiers::CTRL, visible),
                None
            );
            assert_eq!(
                shortcut_command(egui::Key::F8, egui::Modifiers::ALT, visible),
                None
            );
            assert_eq!(
                shortcut_command(egui::Key::S, egui::Modifiers::CTRL, visible),
                Some(Command::Vim9Save)
            );
        }
    }

    #[test]
    #[ignore = "requires a Vim 9 executable and local bridge sockets"]
    fn editor_font_inherits_live_configuration_until_explicit_local_zoom() {
        // Workbench is a binary module. Keep this check out of the buffer
        // tests that are also compiled through the smaller library target.
        if !jterm_core::child_env::inherited_environment_is_captured() {
            jterm_core::child_env::capture_inherited_environment().unwrap();
        }
        let root = std::env::temp_dir().join(format!("vim9-font-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let session = Vim9Session::start(Vim9EntryRequest::user_action(root.clone())).unwrap();
        let mut editor = Workbench::new(session, &Default::default());
        assert_eq!(editor.buffer_font_size, None);
        assert_eq!(editor.font_size(12.0), 12.0);
        assert_eq!(
            editor.font_size(21.0),
            21.0,
            "settings changes reach an editor that has not been zoomed"
        );
        editor.buffer_font_size = Some(18.0);
        assert_eq!(editor.font_size(12.0), 18.0);
        assert_eq!(
            editor.font_size(24.0),
            18.0,
            "an intentional per-editor zoom survives global settings changes"
        );
        drop(editor);
        std::fs::remove_dir_all(root).unwrap();
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
    fn quick_open_locations_revalidate_query_and_clear_stale_file_targets() {
        let project = Vim9ProjectSnapshot {
            files: Arc::new(
                ["src/main.rs", "src/other.rs", "notes:42"]
                    .into_iter()
                    .map(|relative| Vim9ProjectFile {
                        path: PathBuf::from("/project").join(relative),
                        relative: relative.to_owned(),
                    })
                    .collect(),
            ),
            ..Default::default()
        };
        let mut picker = EditorDialog::new(DialogKind::QuickOpen);
        picker.query = "main:42:8".into();
        assert!(
            matches!(picker.quick_open_command(&project), Ok(Vim9Command::Open { path, line: Some(42), column: Some(8) }) if path == "/project/src/main.rs")
        );
        picker.query = "other:7".into();
        assert!(
            matches!(picker.quick_open_command(&project), Ok(Vim9Command::Open { path, line: Some(7), column: None }) if path == "/project/src/other.rs")
        );
        picker.query = "other:0".into();
        assert!(picker.quick_open_command(&project).is_err());
        assert!(
            picker.matches.is_empty(),
            "invalid location retained the previous file"
        );
        picker.query = "notes:42".into();
        assert!(
            matches!(picker.quick_open_command(&project), Ok(Vim9Command::Open { path, line: None, column: None }) if path == "/project/notes:42")
        );
        picker.query = ":19:3".into();
        let unavailable_index = Vim9ProjectSnapshot {
            scanning: true,
            error: Some("index unavailable".into()),
            ..Default::default()
        };
        assert!(matches!(
            picker.quick_open_command(&unavailable_index),
            Ok(Vim9Command::Goto {
                line: 19,
                column: Some(3)
            })
        ));
        assert!(picker.matches.is_empty());
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

    fn palette_snapshot() -> Vim9Snapshot {
        Vim9Snapshot {
            connected: true,
            mode: "n".into(),
            ..Default::default()
        }
    }

    #[test]
    fn diagnostic_labels_and_locations_keep_full_external_path_context() {
        for (kind, label) in [
            ("E", "Error"),
            ("w", "Warning"),
            ("INFO", "Info"),
            ("", "Diagnostic"),
        ] {
            assert_eq!(diagnostic_kind_label(kind), label);
        }
        let root = Path::new("/project");
        assert_eq!(
            diagnostic_location(root, "/project/src/main.rs", 4, 2),
            "src/main.rs:4:2"
        );
        assert_eq!(
            diagnostic_location(root, "/project-other/main.rs", 4, 2),
            "/project-other/main.rs:4:2"
        );
        assert_eq!(
            diagnostic_location(root, "/project/src/../../other.rs", 4, 2),
            "/project/src/../../other.rs:4:2"
        );
        assert_eq!(
            diagnostic_location(Path::new(""), "/project/src/main.rs", 4, 2),
            "/project/src/main.rs:4:2"
        );
    }

    #[test]
    fn search_empty_message_requires_current_search_quickfix_and_no_merged_diagnostics() {
        let mut snapshot = palette_snapshot();
        snapshot.quickfix.source = "search".into();
        snapshot.search.query = "needle".into();
        snapshot.quickfix.title = "Search: needle".into();
        snapshot.search.exit_code = 1;
        assert!(search_owns_empty_results(&snapshot));
        snapshot.quickfix.source = "build".into();
        assert!(!search_owns_empty_results(&snapshot));
        snapshot.quickfix.source = "search".into();
        snapshot.diagnostics.push(Default::default());
        assert!(!search_owns_empty_results(&snapshot));
        snapshot.diagnostics.clear();
        snapshot.search.running = true;
        assert!(!search_owns_empty_results(&snapshot));
        snapshot.search.running = false;
        snapshot.search.cancelled = true;
        assert!(!search_owns_empty_results(&snapshot));
        snapshot.search.cancelled = false;
        snapshot.search.exit_code = 2;
        assert!(!search_owns_empty_results(&snapshot));
        snapshot.search.exit_code = 1;
        snapshot.search.errors.push("search failed".into());
        assert!(!search_owns_empty_results(&snapshot));
        snapshot.search.errors.clear();
        snapshot.quickfix.title = "Search: previous query".into();
        assert!(!search_owns_empty_results(&snapshot));
    }

    #[test]
    fn command_mode_works_without_an_index_and_preserves_file_and_location_modes() {
        let project = Vim9ProjectSnapshot {
            scanning: true,
            error: Some("index unavailable".into()),
            files: Arc::new(vec![Vim9ProjectFile {
                path: PathBuf::from("/project/>save"),
                relative: ">save".into(),
            }]),
            ..Default::default()
        };
        let mut picker = EditorDialog::new(DialogKind::QuickOpen);
        picker.query = ">save".into();
        assert!(matches!(
            picker.quick_action_command(&project, &palette_snapshot()),
            Ok(Vim9Command::Save)
        ));
        assert!(picker.matches.is_empty());
        picker.query = "./>save".into();
        assert!(matches!(
            picker.quick_action_command(&project, &palette_snapshot()),
            Ok(Vim9Command::Open { path, .. }) if path == "/project/>save"
        ));
        assert!(picker.command_matches.is_empty());
        picker.query = ":42:8".into();
        assert!(matches!(
            picker.quick_action_command(&project, &palette_snapshot()),
            Ok(Vim9Command::Goto {
                line: 42,
                column: Some(8)
            })
        ));
        picker.query = ">no-such-editor-command-987654321".into();
        assert!(picker
            .quick_action_command(&project, &palette_snapshot())
            .is_err());
        assert!(picker.matches.is_empty());
        assert!(picker.command_matches.is_empty());
    }

    #[test]
    fn command_activation_rechecks_query_stable_identity_and_current_availability() {
        let project = Vim9ProjectSnapshot::default();
        let mut picker = EditorDialog::new(DialogKind::QuickOpen);
        picker.query = ">".into();
        picker.refresh_command_matches();
        picker.activated_command = Some((picker.query.clone(), Vim9PaletteCommand::Save));
        picker.command_matches.reverse();
        assert!(matches!(
            picker.quick_action_command(&project, &palette_snapshot()),
            Ok(Vim9Command::Save)
        ));
        picker.activated_command = Some((picker.query.clone(), Vim9PaletteCommand::Save));
        picker.query = ">split".into();
        assert!(picker
            .quick_action_command(&project, &palette_snapshot())
            .unwrap_err()
            .contains("query changed"));
        picker.query = ">cancel search".into();
        picker.refresh_command_matches();
        let mut snapshot = palette_snapshot();
        snapshot.search.running = true;
        assert!(Vim9PaletteCommand::CancelSearch
            .unavailable_reason(&snapshot)
            .is_none());
        picker.activated_command = Some((picker.query.clone(), Vim9PaletteCommand::CancelSearch));
        snapshot.search.running = false;
        assert_eq!(
            picker
                .quick_action_command(&project, &snapshot)
                .unwrap_err(),
            "No project search is running"
        );
        assert!(picker
            .command_matches
            .contains(&Vim9PaletteCommand::CancelSearch));
    }

    #[test]
    fn command_confirmation_uses_same_batch_edits_and_owns_the_suffix() {
        let mut picker = EditorDialog::new(DialogKind::QuickOpen);
        let mut events = vec![
            egui::Event::Text(">save".into()),
            key(egui::Key::Enter),
            egui::Event::Text("must-not-reach-vim".into()),
        ];
        assert!(!route_dialog_events(&mut picker, true, &mut events));
        assert!(picker.confirm_requested);
        assert_eq!(events, vec![egui::Event::Text(">save".into())]);
        picker.query = ">save".into();
        assert!(matches!(
            picker.quick_action_command(&Vim9ProjectSnapshot::default(), &palette_snapshot()),
            Ok(Vim9Command::Save)
        ));
        picker.query = ">".into();
        picker.refresh_command_matches();
        let count = picker.command_matches.len();
        assert!(count > 1);
        picker.move_selection(-1);
        assert_eq!(picker.selected, count - 1);
        picker.move_selection(1);
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn editor_commands_round_trip_for_custom_keybindings() {
        for name in [
            "ide:quick_open",
            "ide:goto",
            "ide:back",
            "ide:forward",
            "ide:save",
            "ide:save_all",
            "ide:search",
            "ide:cancel_search",
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
    #[test]
    fn terminal_modes_keep_diagnostic_function_keys_with_vim() {
        for visible in [false, true] {
            for mode in ["t", "nt", "ntT"] {
                for modifiers in [egui::Modifiers::NONE, egui::Modifiers::SHIFT] {
                    assert_eq!(
                        shortcut_command(
                            egui::Key::F8,
                            modifiers,
                            diagnostic_shortcuts_enabled(visible, mode)
                        ),
                        None
                    );
                }
            }
        }
        for mode in ["n", "i", "v", "no"] {
            assert!(diagnostic_shortcuts_enabled(true, mode));
            assert!(!diagnostic_shortcuts_enabled(false, mode));
        }
    }
}
