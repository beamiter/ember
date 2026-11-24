use jterm_core::notebook::{
    CellOutcome, CellRun, CellRunView, NotebookDocument, NotebookPart, MAX_LOCAL_NOTEBOOK_BYTES,
};
use jterm_core::review_input::{safe_inline_display, safe_multiline_display};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

const REFRESH_INTERVAL: Duration = Duration::from_millis(50);
const MAX_CONCURRENT_CELLS: usize = 8;

pub(crate) fn is_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".jtnb.md"))
}

#[derive(Default)]
pub(crate) struct Panel {
    content: Option<Content>,
    stopping: Vec<CellRun>,
}

enum Content {
    Loading(Receiver<Result<Document, String>>),
    Ready(Document),
    Error(String),
}

impl Panel {
    pub(crate) fn is_open(&self) -> bool {
        self.content.is_some()
    }

    pub(crate) fn open(&mut self, path: PathBuf) {
        self.close();
        let (send, receive) = mpsc::channel();
        self.content = Some(
            match std::thread::Builder::new()
                .name("ember-notebook-load".into())
                .spawn(move || {
                    let _ = send.send(Document::load(&path));
                }) {
                Ok(_) => Content::Loading(receive),
                Err(error) => Content::Error(error.to_string()),
            },
        );
    }

    fn close(&mut self) {
        if let Some(Content::Ready(document)) = self.content.take() {
            for run in document.cells.into_values().filter_map(|cell| cell.run) {
                run.stop();
                self.stopping.push(run);
            }
        }
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) {
        // The shared job marks a run finished after reaping its process group.
        // Keep the handle until that receipt to avoid a worker join during paint.
        self.stopping.retain(CellRun::is_running);
        if !self.stopping.is_empty() {
            ctx.request_repaint_after(REFRESH_INTERVAL);
        }
        if let Some(Content::Loading(receiver)) = &self.content {
            let result = match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("Notebook reader stopped.".into())),
            };
            if let Some(result) = result {
                self.content = Some(match result {
                    Ok(document) => Content::Ready(document),
                    Err(error) => Content::Error(error),
                });
            }
        }
        let Some(content) = &mut self.content else {
            return;
        };
        let size = ctx.content_rect().size();
        let modal = egui::Modal::new(egui::Id::new("local-notebook")).show(ctx, |ui| {
            ui.set_width((size.x - 40.0).clamp(80.0, 760.0));
            match content {
                Content::Ready(document) => document.show(ui, size.y, self.stopping.len()),
                Content::Loading(_) => {
                    ui.spinner();
                    ui.label("Opening notebook…");
                    ctx.request_repaint_after(REFRESH_INTERVAL);
                }
                Content::Error(error) => {
                    ui.heading("Could not open notebook");
                    ui.label(safe_inline_display(error, 768));
                }
            }
            ui.separator();
            ui.button("Close").clicked()
        });
        if modal.inner || modal.should_close() {
            self.close();
        }
    }
}

struct Document {
    document: NotebookDocument,
    display: Vec<String>,
    cells: HashMap<usize, Cell>,
    error: Option<String>,
}

impl Document {
    fn load(path: &Path) -> Result<Self, String> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Select a named notebook file.")?;
        let file = crate::local_preview::open_regular(path)?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_LOCAL_NOTEBOOK_BYTES as u64 {
            return Err("Notebook must be a regular file of at most 1 MiB.".into());
        }
        let mut source = String::new();
        file.take(MAX_LOCAL_NOTEBOOK_BYTES as u64 + 1)
            .read_to_string(&mut source)
            .map_err(|error| error.to_string())?;
        let document = NotebookDocument::open_local_snapshot(name.into(), &source)
            .map_err(|error| error.to_string())?;
        let display = document
            .view()
            .parts
            .iter()
            .map(|part| match part {
                NotebookPart::Text { text } => {
                    safe_multiline_display(text, MAX_LOCAL_NOTEBOOK_BYTES)
                }
                NotebookPart::Cell { source, .. } => {
                    safe_multiline_display(source, MAX_LOCAL_NOTEBOOK_BYTES)
                }
            })
            .collect();
        Ok(Self {
            document,
            display,
            cells: HashMap::new(),
            error: None,
        })
    }

    fn show(&mut self, ui: &mut egui::Ui, viewport_height: f32, stopping: usize) {
        let document = &self.document;
        let cells = &mut self.cells;
        for cell in cells.values_mut() {
            cell.poll(ui.ctx());
        }
        let mut active = stopping + cells.values().filter(|cell| cell.run.is_some()).count();
        ui.heading(&document.view().name);
        ui.weak("Cells run in a temporary working directory.");
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, safe_inline_display(error, 768));
        }
        egui::ScrollArea::vertical()
            .max_height((viewport_height - 170.0).max(50.0))
            .show(ui, |ui| {
                for (part, display) in document.view().parts.iter().zip(&self.display) {
                    let NotebookPart::Cell {
                        index, interpreter, ..
                    } = part
                    else {
                        ui.add(egui::Label::new(display.as_str()).selectable(true).wrap());
                        continue;
                    };
                    ui.push_id(*index, |ui| {
                        ui.separator();
                        ui.horizontal(|ui| {
                            if let Some(program) = interpreter {
                                ui.strong(program);
                            } else {
                                ui.weak("Read-only cell");
                            }
                            if let Some(run) = cells.get(index).and_then(|cell| cell.run.as_ref()) {
                                if ui.button("Stop").clicked() {
                                    run.stop();
                                }
                            } else if interpreter.is_some()
                                && ui
                                    .add_enabled(
                                        active < MAX_CONCURRENT_CELLS,
                                        egui::Button::new("Run"),
                                    )
                                    .clicked()
                            {
                                match document.start(*index) {
                                    Ok(run) => {
                                        self.error = None;
                                        cells.insert(*index, Cell::new(run));
                                        active += 1;
                                        ui.ctx().request_repaint_after(REFRESH_INTERVAL);
                                    }
                                    Err(error) => self.error = Some(error.to_string()),
                                }
                            }
                        });
                        monospace(ui, display);
                        if let Some(cell) = cells.get(index) {
                            cell.show(ui);
                        }
                    });
                }
            });
    }
}

struct Cell {
    run: Option<CellRun>,
    view: CellRunView,
}

impl Cell {
    fn new(run: CellRun) -> Self {
        let view = run.view();
        Self {
            run: Some(run),
            view: Self::display_receipt(view),
        }
    }

    fn display_receipt(mut view: CellRunView) -> CellRunView {
        view.stdout = safe_multiline_display(&view.stdout, MAX_LOCAL_NOTEBOOK_BYTES);
        view.stderr = safe_multiline_display(&view.stderr, MAX_LOCAL_NOTEBOOK_BYTES);
        view
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(run) = &self.run else {
            return;
        };
        if let Some(view) = run.view_after(self.view.revision) {
            self.view = Self::display_receipt(view);
        }
        if matches!(self.view.outcome, CellOutcome::Running) {
            ctx.request_repaint_after(REFRESH_INTERVAL);
        } else {
            self.run = None;
        }
    }

    fn show(&self, ui: &mut egui::Ui) {
        let status = match &self.view.outcome {
            CellOutcome::Running => "Running".to_owned(),
            CellOutcome::Exited { code, signal } => exit_label("Exited", *code, *signal),
            CellOutcome::Stopped { code, signal } => exit_label("Stopped", *code, *signal),
            CellOutcome::Failed { reason } => safe_inline_display(reason, 768),
        };
        ui.label(status);
        if self.view.truncated {
            ui.weak("Output limit reached");
        }
        for (stream, text) in [("stdout", &self.view.stdout), ("stderr", &self.view.stderr)] {
            if !text.is_empty() {
                ui.strong(stream);
                monospace(ui, text);
            }
        }
    }
}

fn exit_label(action: &str, code: Option<i32>, signal: Option<i32>) -> String {
    match (code, signal) {
        (Some(code), _) => format!("{action} (code {code})"),
        (_, Some(signal)) => format!("{action} (signal {signal})"),
        (None, None) => action.to_owned(),
    }
}

fn monospace(ui: &mut egui::Ui, text: &str) {
    ui.add(
        egui::Label::new(egui::RichText::new(text).monospace())
            .selectable(true)
            .wrap(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;
    use std::time::Instant;

    #[test]
    fn selected_notebook_runs_and_stops_with_separate_output_receipts() {
        let directory =
            std::env::temp_dir().join(format!("ember-notebook-test-{}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let path = directory.join("fixture.jtnb.md");
        std::fs::write(&path, "# Fixture\n\n```sh\nprintf 'out\\n'\nprintf 'err\\n' >&2\nexit 7\n```\n\n```sh\nprintf 'before-stop\\n'\nsleep 30\n```\n").unwrap();
        let document = Document::load(&path).unwrap();
        let cells: Vec<_> = document
            .document
            .view()
            .parts
            .iter()
            .filter_map(|part| match part {
                NotebookPart::Cell {
                    index,
                    interpreter: Some(_),
                    ..
                } => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(cells.len(), 2);
        let ctx = egui::Context::default();
        let mut run = Cell::new(document.document.start(cells[0]).unwrap());
        let first_id = run.view.run_id;
        wait_until(|| {
            run.poll(&ctx);
            run.run.is_none()
        });
        assert_eq!(run.view.run_id, first_id);
        assert!(matches!(
            run.view.outcome,
            CellOutcome::Exited {
                code: Some(7),
                signal: None
            }
        ));
        assert_eq!(run.view.stdout, "out\n");
        assert_eq!(run.view.stderr, "err\n");

        let mut stopped = Cell::new(document.document.start(cells[1]).unwrap());
        assert_ne!(stopped.view.run_id, first_id);
        wait_until(|| {
            stopped.poll(&ctx);
            stopped.view.stdout == "before-stop\n"
        });
        stopped.run.as_ref().unwrap().stop();
        wait_until(|| {
            stopped.poll(&ctx);
            stopped.run.is_none()
        });
        assert!(matches!(stopped.view.outcome, CellOutcome::Stopped { .. }));
        assert_eq!(stopped.view.stdout, "before-stop\n");
    }

    fn wait_until(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !predicate() {
            assert!(Instant::now() < deadline, "notebook receipt timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
