//! Read-only source-bound previews, with one worker and one current inspector page.
use crate::local_reference::SourceLocationCandidate;
use crate::preview_navigation::{match_background, ImageZoom, SourceMatches};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

#[derive(Clone)]
struct ReadTarget {
    path: PathBuf,
    fallback: Option<(PathBuf, SourceLocationCandidate)>,
    remote: Option<RemoteRead>,
}

#[derive(Clone)]
struct RemoteRead {
    host: jterm_core::jsh_remote::RemoteHostConfig,
    label: String,
    context: crate::sidebar::FilesIntentContext,
}

impl ReadTarget {
    fn exact(path: PathBuf) -> Self {
        Self {
            path,
            fallback: None,
            remote: None,
        }
    }

    fn reference(target: &str, cwd: &Path) -> Result<Self, String> {
        let candidate = crate::local_reference::parse_candidate(target)
            .ok_or_else(|| "Unsupported local file reference".to_string())?;
        if !cwd.is_absolute() {
            return Err("Local working directory must be absolute".into());
        }
        let join = |text: &str| -> Result<PathBuf, String> {
            if let Some(rest) = text.strip_prefix("~/") {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
                    .ok_or_else(|| "Local home directory unavailable".to_string())?;
                Ok(home.join(rest))
            } else {
                Ok(cwd.join(text))
            }
        };
        let path = join(&candidate.exact_path)?;
        let fallback = candidate
            .source_location
            .map(|location| join(&location.path).map(|path| (path, location)))
            .transpose()?;
        Ok(Self {
            path,
            fallback,
            remote: None,
        })
    }

    fn resolve(self) -> Result<(PathBuf, Option<SourceLocationCandidate>), String> {
        if self.remote.is_some() {
            return Ok((self.path, None));
        }
        match std::fs::symlink_metadata(&self.path) {
            Ok(_) => Ok((self.path, None)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => self
                .fallback
                .map(|(path, location)| (path, Some(location)))
                .ok_or_else(|| "Local file not found".into()),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                Err("Permission denied reading the local file".into())
            }
            Err(_) => Err("Cannot inspect local file".into()),
        }
    }
}

struct LoadedPreview {
    path: PathBuf,
    location: Option<SourceLocationCandidate>,
    content: crate::local_preview::PreviewContent,
    remote: Option<RemoteRead>,
}

struct PreviewText {
    source: String,
    starts: Vec<usize>,
}

impl PreviewText {
    fn new(source: String) -> Self {
        let mut starts = vec![0];
        starts.extend(source.match_indices('\n').map(|(offset, _)| offset + 1));
        Self { source, starts }
    }

    fn line(&self, index: usize) -> &str {
        let start = self.starts[index];
        let end = self
            .starts
            .get(index + 1)
            .copied()
            .unwrap_or(self.source.len());
        self.source[start..end].trim_end_matches(['\r', '\n'])
    }
}

type PreviewResult = Result<LoadedPreview, String>;

#[derive(Default)]
pub struct LocalPreviewPanel {
    session_id: String,
    path: PathBuf,
    visible: bool,
    pending: Option<(u64, Receiver<PreviewResult>)>,
    text: Option<PreviewText>,
    texture: Option<egui::TextureHandle>,
    error: Option<String>,
    candidate: Option<ReadTarget>,
    last_request: Option<ReadTarget>,
    location: Option<SourceLocationCandidate>,
    seek_pending: bool,
    line_input: String,
    selected_line: Option<usize>,
    line_error: Option<String>,
    find_open: bool,
    find_query: String,
    ignore_case: bool,
    matches: SourceMatches,
    zoom: ImageZoom,
    content_revision: u64,
}

impl LocalPreviewPanel {
    pub fn open(&mut self, session_id: String, target: String, cwd: PathBuf) -> Result<(), String> {
        let target = ReadTarget::reference(&target, &cwd)?;
        self.present(session_id, &target.path);
        self.candidate = Some(target);
        Ok(())
    }

    pub fn open_tree_file(&mut self, session_id: String, path: PathBuf) {
        if !path.is_absolute() {
            return;
        }
        self.present(session_id, &path);
        self.last_request = Some(ReadTarget::exact(path));
        self.start_read();
    }

    pub(crate) fn open_remote_tree_file(
        &mut self,
        session_id: String,
        path: PathBuf,
        host: jterm_core::jsh_remote::RemoteHostConfig,
        label: String,
        context: crate::sidebar::FilesIntentContext,
    ) {
        if !path.is_absolute() {
            return;
        }
        self.present(session_id, &path);
        self.last_request = Some(ReadTarget {
            path,
            fallback: None,
            remote: Some(RemoteRead {
                host,
                label,
                context,
            }),
        });
        self.start_read();
    }

    pub(crate) fn validate_files_source(&mut self, sidebar: &crate::sidebar::Sidebar) {
        let stale = self
            .last_request
            .as_ref()
            .and_then(|target| target.remote.as_ref())
            .is_some_and(|remote| !sidebar.files_intent_is_current(&remote.context));
        if stale {
            self.close();
        }
    }

    fn present(&mut self, session_id: String, path: &Path) {
        self.close();
        self.session_id = session_id;
        self.path = path.to_path_buf();
        self.visible = true;
    }

    fn start_read(&mut self) {
        if self.pending.is_some() {
            self.error = Some("Another preview is still reading. Try again shortly.".into());
            return;
        }
        let Some(target) = self
            .candidate
            .as_ref()
            .or(self.last_request.as_ref())
            .cloned()
        else {
            return;
        };
        let (tx, rx) = mpsc::sync_channel(1);
        let worker_target = target.clone();
        if std::thread::Builder::new()
            .name("local-file-preview".into())
            .spawn(move || {
                let result = if let Some(remote) = worker_target.remote.clone() {
                    let path = worker_target.path;
                    crate::remote_fs::read_preview(
                        &remote.host,
                        &path,
                        crate::local_preview::MAX_IMAGE_BYTES,
                    )
                    .map_err(|error| error.to_string())
                    .and_then(crate::local_preview::decode_bytes)
                    .map(|content| LoadedPreview {
                        path,
                        location: None,
                        content,
                        remote: Some(remote),
                    })
                } else {
                    worker_target.resolve().and_then(|(path, location)| {
                        crate::local_preview::load(&path).map(|content| LoadedPreview {
                            path,
                            location,
                            content,
                            remote: None,
                        })
                    })
                };
                let _ = tx.send(result);
            })
            .is_err()
        {
            self.error = Some("Could not start the preview reader".into());
            return;
        }
        self.candidate = None;
        self.last_request = Some(target);
        self.pending = Some((self.content_revision, rx));
        self.error = None;
    }

    pub fn is_visible_for(&self, session_id: &str) -> bool {
        self.visible && self.session_id == session_id
    }

    pub fn close(&mut self) {
        // Keep the receiver until completion so a blocked read still holds the worker slot.
        self.content_revision = self.content_revision.wrapping_add(1);
        self.visible = false;
        self.path.clear();
        self.candidate = None;
        self.last_request = None;
        self.text = None;
        self.texture = None;
        self.error = None;
        self.location = None;
        self.seek_pending = false;
        self.line_input.clear();
        self.selected_line = None;
        self.line_error = None;
        self.find_open = false;
        self.find_query.clear();
        self.ignore_case = false;
        self.matches = SourceMatches::default();
        self.zoom = ImageZoom::Fit;
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        let Some((revision, receiver)) = &self.pending else {
            return;
        };
        let revision = *revision;
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Disconnected) => Err("Preview reader stopped".into()),
            Err(mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
                return;
            }
        };
        self.pending = None;
        if !self.visible || revision != self.content_revision {
            return;
        }
        match result {
            Ok(loaded) => {
                self.path = loaded.path;
                self.last_request = Some(ReadTarget {
                    path: self.path.clone(),
                    fallback: None,
                    remote: loaded.remote,
                });
                if let Some(location) = loaded.location {
                    self.selected_line = Some(location.line.get() as usize);
                    self.location = Some(location);
                }
                match loaded.content {
                    crate::local_preview::PreviewContent::Text(text) => {
                        if self.text.is_none() {
                            if let Some(line) = self.selected_line {
                                self.line_input = line.to_string();
                                self.seek_pending = true;
                            }
                        }
                        self.texture = None;
                        self.matches
                            .update(&text, &self.find_query, !self.ignore_case);
                        if self
                            .text
                            .as_ref()
                            .is_none_or(|previous| previous.source != text)
                        {
                            self.text = Some(PreviewText::new(text));
                        }
                    }
                    crate::local_preview::PreviewContent::Image(image) => {
                        self.text = None;
                        self.matches = SourceMatches::default();
                        self.texture = Some(ctx.load_texture(
                            "local-file-preview",
                            image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
            }
            Err(error) => {
                self.error = Some(if self.text.is_some() || self.texture.is_some() {
                    format!("Reload failed. Showing the last preview.\n{error}")
                } else {
                    error
                });
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> bool {
        let close = ui
            .horizontal(|ui| {
                let remote = self
                    .last_request
                    .as_ref()
                    .and_then(|target| target.remote.as_ref());
                ui.strong(if remote.is_some() {
                    "Remote file preview"
                } else {
                    "Local file preview"
                });
                let close = ui.button("Close");
                crate::ui::remember_inspector_control(ui, &close);
                close.clicked()
            })
            .inner;
        if close {
            self.close();
            return true;
        }
        if let Some(remote) = self
            .last_request
            .as_ref()
            .and_then(|target| target.remote.as_ref())
        {
            ui.add(
                egui::Label::new(format!(
                    "Source: {}",
                    crate::review_text::visible_bounded(&remote.label, 100)
                ))
                .truncate(),
            )
            .on_hover_text(&remote.label);
        }
        let path = display_path(&self.path);
        if self.candidate.is_none() {
            ui.add(egui::Label::new(&path).truncate())
                .on_hover_text(&path);
        }
        ui.separator();

        let reading = self
            .pending
            .as_ref()
            .is_some_and(|(revision, _)| *revision == self.content_revision);
        if let Some(target) = &self.candidate {
            let confirmed = egui::ScrollArea::vertical()
                .id_salt(("local_preview_confirmation", self.content_revision))
                .show(ui, |ui| {
                    ui.label("Confirm that the terminal reference belongs to this computer before reading.");
                    ui.label(&path);
                    if let Some((fallback, location)) = &target.fallback {
                        ui.label(format!(
                            "If this filename is missing, read {} ({})",
                            display_path(fallback),
                            location_label(location),
                        ));
                    }
                    if let Some(error) = &self.error {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                    let confirm = ui.button("Preview local file");
                    crate::ui::remember_inspector_control(ui, &confirm);
                    confirm.clicked()
                }).inner;
            if confirmed {
                self.start_read();
                ui.ctx().request_repaint();
            }
            return false;
        }
        {
            let ready = self.text.is_some() || self.texture.is_some();
            ui.horizontal_wrapped(|ui| {
                let label = if ready || reading { "Reload" } else { "Retry" };
                let reload = ui.add_enabled(!reading, egui::Button::new(label));
                crate::ui::remember_inspector_control(ui, &reload);
                if reload.clicked() {
                    self.start_read();
                    ui.ctx().request_repaint();
                }
                if ready {
                    let copy_path = ui.button("Copy path");
                    crate::ui::remember_inspector_control(ui, &copy_path);
                    if copy_path.clicked() {
                        ui.ctx().copy_text(path);
                    }
                    if let Some(text) = &self.text {
                        let copy = ui.button("Copy text");
                        crate::ui::remember_inspector_control(ui, &copy);
                        if copy.clicked() {
                            ui.ctx().copy_text(text.source.clone());
                        }
                        let line = self.selected_line.filter(|&line| {
                            !text.source.is_empty() && (1..=text.starts.len()).contains(&line)
                        });
                        let copy_line = ui
                            .add_enabled(line.is_some(), egui::Button::new("Copy line"))
                            .on_hover_text("Copy the highlighted line without its line number");
                        crate::ui::remember_inspector_control(ui, &copy_line);
                        if let Some(line) = line.filter(|_| copy_line.clicked()) {
                            ui.ctx().copy_text(text.line(line - 1).to_owned());
                        }
                    }
                }
            });
        }
        if reading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(if self.text.is_some() || self.texture.is_some() {
                    "Reloading file…"
                } else {
                    "Reading file…"
                });
            });
        }
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if self.text.is_some() {
            self.show_source(ui);
        }
        if let Some(texture) = &self.texture {
            let size = texture.size();
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{} × {} pixels", size[0], size[1]));
                let zoom = egui::ComboBox::from_id_salt("local_preview_zoom")
                    .selected_text(self.zoom.to_string())
                    .show_ui(ui, |ui| {
                        for option in ImageZoom::OPTIONS {
                            ui.selectable_value(&mut self.zoom, option, option.to_string());
                        }
                    });
                crate::ui::remember_inspector_control(
                    ui,
                    &zoom.response.on_hover_text("Image zoom"),
                );
            });
            self.show_image(ui, texture);
        }
        false
    }

    fn show_source(&mut self, ui: &mut egui::Ui) {
        let Some(text) = &self.text else {
            return;
        };
        if text.source.is_empty() {
            ui.label("Empty file");
            return;
        }
        let count = text.starts.len();
        let mut jump = false;
        let mut focus_find = false;
        ui.horizontal_wrapped(|ui| {
            let input = ui.add(
                egui::TextEdit::singleline(&mut self.line_input)
                    .hint_text("Line")
                    .char_limit(10)
                    .desired_width(57.0),
            );
            crate::ui::remember_inspector_control(ui, &input);
            let button = ui.button("Go to line");
            crate::ui::remember_inspector_control(ui, &button);
            jump = button.clicked()
                || (input.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)));
            let find = ui.toggle_value(&mut self.find_open, "Find");
            crate::ui::remember_inspector_control(ui, &find);
            focus_find = find.clicked() && self.find_open;
        });
        if jump {
            match self.line_input.trim().parse::<usize>() {
                Ok(line) if (1..=count).contains(&line) => {
                    self.selected_line = Some(line);
                    self.matches.clear_active();
                    self.location = None;
                    self.seek_pending = true;
                    self.line_error = None;
                }
                _ => self.line_error = Some(format!("Choose a line from 1 to {count}.")),
            }
        }
        if self.find_open {
            let step = ui
                .horizontal_wrapped(|ui| {
                    let input = ui
                        .add(
                            egui::TextEdit::singleline(&mut self.find_query)
                                .hint_text("Find text")
                                .char_limit(256)
                                .desired_width(96.0),
                        )
                        .on_hover_text("Search for literal text in this file");
                    crate::ui::remember_inspector_control(ui, &input);
                    if focus_find {
                        input.request_focus();
                    }
                    let mut match_case = !self.ignore_case;
                    let case = ui.checkbox(&mut match_case, "Match case");
                    crate::ui::remember_inspector_control(ui, &case);
                    if input.changed() || case.changed() {
                        self.ignore_case = !match_case;
                        self.matches.clear_active();
                        self.matches
                            .update(&text.source, &self.find_query, match_case);
                    }
                    let available = !self.matches.is_empty();
                    let previous = ui
                        .add_enabled(available, egui::Button::new("Prev"))
                        .on_hover_text("Previous match");
                    let next = ui
                        .add_enabled(available, egui::Button::new("Next"))
                        .on_hover_text("Next match");
                    for control in [&previous, &next] {
                        crate::ui::remember_inspector_control(ui, control);
                    }
                    if previous.clicked() {
                        Some(true)
                    } else if next.clicked()
                        || (input.lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter)))
                    {
                        Some(false)
                    } else {
                        None
                    }
                })
                .inner;
            if let Some(hit) =
                step.and_then(|previous| self.matches.step(self.selected_line, previous))
            {
                self.selected_line = Some(hit.line);
                self.line_input = hit.line.to_string();
                self.location = None;
                self.seek_pending = true;
                self.line_error = None;
            }
            if !self.find_query.is_empty() {
                ui.small(self.matches.status());
            }
        }
        let target = self.selected_line.filter(|line| *line <= count);
        if let Some(error) = &self.line_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        } else if self.selected_line.is_some() && target.is_none() {
            ui.label(format!(
                "Requested line is outside this file · {count} lines"
            ));
        } else if let Some(location) = &self.location {
            ui.label(format!("{} · {count} lines", location_label(location)));
        } else {
            ui.label(match target {
                Some(line) => format!("Line {line} · {count} lines"),
                None => format!("{count} lines"),
            });
        }
        let font =
            egui::FontId::monospace(ui.style().text_styles[&egui::TextStyle::Monospace].size);
        let row_height = ui.fonts_mut(|fonts| fonts.row_height(&font));
        let mut scroll = egui::ScrollArea::both()
            .id_salt(("local_preview_content", self.content_revision))
            .auto_shrink([false, false]);
        let active = self.find_open.then(|| self.matches.active()).flatten();
        if self.seek_pending {
            let index = target.unwrap_or(1) - 1;
            let x = active
                .filter(|hit| hit.line == index + 1)
                .map_or(0.0, |hit| {
                    let prefix = format!("{:>6}  {}", hit.line, &text.line(index)[..hit.start]);
                    let width = ui.fonts_mut(|fonts| {
                        fonts
                            .layout_no_wrap(prefix, font.clone(), egui::Color32::WHITE)
                            .size()
                            .x
                    });
                    (width - ui.available_width() * 0.2).max(0.0)
                });
            scroll = scroll.horizontal_scroll_offset(x);
            let stride = row_height + ui.spacing().item_spacing.y;
            let context = (ui.available_height() - stride).max(0.0) / 2.0;
            scroll = scroll.vertical_scroll_offset((index as f32 * stride - context).max(0.0));
            self.seek_pending = false;
        }
        scroll.show_rows(ui, row_height, count, |ui, range| {
            for index in range {
                let line = text.line(index);
                let row = index + 1;
                let hits = if self.find_open {
                    self.matches.hits(Some(row))
                } else {
                    &[]
                };
                let format = egui::TextFormat {
                    font_id: font.clone(),
                    color: ui.visuals().text_color(),
                    background: if target == Some(row) {
                        ui.visuals().selection.bg_fill
                    } else {
                        egui::Color32::TRANSPARENT
                    },
                    ..Default::default()
                };
                let mut job = egui::text::LayoutJob::default();
                job.append(&format!("{row:>6}  "), 0.0, format.clone());
                let mut start = 0;
                for hit in hits {
                    job.append(&line[start..hit.start], 0.0, format.clone());
                    let [red, green, blue] = match_background(active == Some(*hit));
                    let marked = egui::TextFormat {
                        background: egui::Color32::from_rgb(red, green, blue),
                        color: egui::Color32::from_rgb(35, 23, 8),
                        ..format.clone()
                    };
                    job.append(&line[hit.start..hit.end], 0.0, marked);
                    start = hit.end;
                }
                job.append(&line[start..], 0.0, format);
                ui.add(
                    egui::Label::new(job)
                        .selectable(true)
                        .wrap_mode(egui::TextWrapMode::Extend),
                );
            }
        });
    }

    fn show_image(&self, ui: &mut egui::Ui, texture: &egui::TextureHandle) {
        let size = texture.size_vec2();
        let available = ui.available_size().max(egui::Vec2::splat(1.0));
        let native = size / ui.ctx().pixels_per_point();
        let scale = self.zoom.scale().unwrap_or_else(|| {
            (available.x / native.x)
                .min(available.y / native.y)
                .min(1.0)
        });
        let display = native * scale;
        egui::ScrollArea::both()
            .id_salt(("local_preview_image", self.content_revision, self.zoom))
            .show(ui, |ui| {
                ui.image((texture.id(), display));
            });
    }
}

fn display_path(path: &Path) -> String {
    let value = path
        .to_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{:?}", path.as_os_str()));
    jterm_core::review_input::safe_inline_display(&value, 32 * 1024)
}

fn location_label(location: &SourceLocationCandidate) -> String {
    match location.column {
        Some(column) => format!("Line {}, column {column} (reported)", location.line),
        None => format!("Line {}", location.line),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(
        target: &str,
        cwd: &Path,
    ) -> Result<(PathBuf, Option<SourceLocationCandidate>), String> {
        ReadTarget::reference(target, cwd)?.resolve()
    }
    #[test]
    fn candidate_requires_explicit_local_choice_before_io() {
        let mut panel = LocalPreviewPanel::default();
        panel
            .open(
                "one".into(),
                "missing.tsx".into(),
                PathBuf::from("/does-not-exist"),
            )
            .unwrap();
        assert!(panel.pending.is_none());
        assert!(panel.candidate.is_some());
        assert!(panel.text.is_none());
    }
    #[test]
    fn source_location_keeps_exact_colon_filename_first() {
        let dir =
            std::env::temp_dir().join(format!("ember-preview-resolve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("view.tsx"), "source").unwrap();
        std::fs::write(dir.join("view.tsx:12:3"), "literal").unwrap();
        assert_eq!(
            resolve("view.tsx:12:3", &dir).unwrap().0,
            dir.join("view.tsx:12:3")
        );
        assert!(resolve("view.tsx:12:3", &dir).unwrap().1.is_none());
        std::fs::remove_file(dir.join("view.tsx:12:3")).unwrap();
        let location = resolve("view.tsx:12:3", &dir).unwrap().1.unwrap();
        assert_eq!(location.line.get(), 12);
        assert_eq!(location.column.unwrap().get(), 3);
        assert_eq!(
            resolve("view.tsx:12:3", &dir).unwrap().0,
            dir.join("view.tsx")
        );
        assert!(resolve("https://host/x.tsx", &dir).is_err());
        assert!(resolve("view.tsx", std::path::Path::new("relative")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn punctuation_candidates_never_fall_back_to_other_real_files() {
        let dir =
            std::env::temp_dir().join(format!("ember-preview-punctuation-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let detector = crate::link::LinkDetector::new(Default::default());
        std::fs::write(dir.join("report.md"), "plain").unwrap();
        for suffix in [".", "。", "?", "!", ",", ";", ":", ":4:2."] {
            let name = format!("report.md{suffix}");
            std::fs::write(dir.join(&name), "literal").unwrap();
            let target = format!("./{name}");
            let links = detector.detect_links_in_line(&target, 0);
            assert_eq!(links.len(), 1);
            assert_eq!(links[0].text, target);
            let (path, location) = resolve(&links[0].text, &dir).unwrap();
            assert!(location.is_none());
            assert_eq!(std::fs::read_to_string(path).unwrap(), "literal");
            std::fs::remove_file(dir.join(&name)).unwrap();
            assert!(
                resolve(&links[0].text, &dir).is_err(),
                "must not fall back: {target}"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn indexed_lines_preserve_unicode_crlf_and_empty_tail() {
        let text = PreviewText::new("α\r\n界\n".into());
        assert_eq!(text.starts.len(), 3);
        assert_eq!(text.line(0), "α");
        assert_eq!(text.line(1), "界");
        assert_eq!(text.line(2), "");
    }

    fn frame(
        panel: &mut LocalPreviewPanel,
        ctx: &egui::Context,
        session: &str,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 600.0),
                )),
                events,
                ..Default::default()
            },
            |root_ui| {
                panel.poll(root_ui.ctx());
                if panel.is_visible_for(session) {
                    egui::CentralPanel::default().show(root_ui, |ui| panel.show(ui));
                }
            },
        );
        output.textures_delta.clear();
        output
    }
    fn labels(output: &egui::FullOutput) -> Vec<(String, egui::Pos2)> {
        fn collect(shape: &egui::epaint::Shape, result: &mut Vec<(String, egui::Pos2)>) {
            match shape {
                egui::epaint::Shape::Text(text) => {
                    result.push((text.galley.job.text.clone(), text.pos))
                }
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, result);
                    }
                }
                _ => {}
            }
        }
        let mut result = Vec::new();
        for shape in &output.shapes {
            collect(&shape.shape, &mut result);
        }
        result
    }
    #[test]
    fn egui_find_reveals_the_second_occurrence_and_copies_the_source_line() {
        let line = format!("界needle {} NEEDLE", "x".repeat(240));
        let mut panel = LocalPreviewPanel {
            session_id: "owner".into(),
            visible: true,
            path: PathBuf::from("/tmp/preview.txt"),
            find_open: true,
            find_query: "needle".into(),
            text: Some(PreviewText::new(format!("{line}\r\nnext row\n"))),
            ..Default::default()
        };
        panel.matches.update(&line, "needle", true);
        let ctx = egui::Context::default();
        let click = |panel: &mut LocalPreviewPanel, label: &str| {
            let output = frame(panel, &ctx, "owner", vec![]);
            let pos = labels(&output)
                .into_iter()
                .find(|(value, _)| value == label)
                .unwrap()
                .1
                + egui::vec2(3.0, 3.0);
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            frame(
                panel,
                &ctx,
                "owner",
                vec![egui::Event::PointerMoved(pos), button(true), button(false)],
            )
        };
        let _ = frame(&mut panel, &ctx, "owner", vec![]);
        click(&mut panel, "Match case");
        assert_eq!(panel.matches.status(), "2 matches");
        click(&mut panel, "Next");
        assert_eq!(panel.matches.active().unwrap().start, "界".len());
        click(&mut panel, "Next");
        assert_eq!(
            panel.matches.active().unwrap().start,
            line.len() - "NEEDLE".len()
        );
        let output = frame(&mut panel, &ctx, "owner", vec![]);
        assert!(labels(&output)
            .iter()
            .any(|(value, pos)| value.ends_with("NEEDLE") && pos.x < -100.0));
        let copied = click(&mut panel, "Copy line");
        assert!(copied.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::CopyText(text) if text == &line)
        }));
    }

    #[test]
    fn remote_target_never_uses_local_resolution_or_fallback() {
        let host = crate::config::default_remote_hosts()[0].clone();
        let target = ReadTarget {
            path: PathBuf::from("/remote-only-no-local-copy"),
            fallback: None,
            remote: Some(RemoteRead {
                host,
                label: "SSH selected".into(),
                context: crate::sidebar::Sidebar::test_files_intent_context(99),
            }),
        };
        assert_eq!(
            target.resolve().unwrap().0,
            PathBuf::from("/remote-only-no-local-copy")
        );
    }

    #[test]
    fn remote_reload_preserves_authority_and_source_change_retires_it() {
        let host = crate::config::default_remote_hosts()[0].clone();
        let remote = RemoteRead {
            host: host.clone(),
            label: "SSH selected".into(),
            context: crate::sidebar::Sidebar::test_files_intent_context(99),
        };
        let mut panel = LocalPreviewPanel::default();
        panel.present("owner".into(), Path::new("/same-name.txt"));
        let (tx, rx) = mpsc::sync_channel(1);
        panel.pending = Some((panel.content_revision, rx));
        tx.send(Ok(LoadedPreview {
            path: PathBuf::from("/same-name.txt"),
            location: None,
            content: crate::local_preview::PreviewContent::Text("remote only".into()),
            remote: Some(remote),
        }))
        .unwrap();
        panel.poll(&egui::Context::default());
        assert_eq!(
            panel
                .last_request
                .as_ref()
                .unwrap()
                .remote
                .as_ref()
                .unwrap()
                .host,
            host
        );
        panel.validate_files_source(&crate::sidebar::Sidebar::new());
        assert!(!panel.visible && panel.last_request.is_none() && panel.text.is_none());
    }

    #[test]
    fn egui_frames_seek_and_virtualize_without_stealing_other_session() {
        let mut panel = LocalPreviewPanel::default();
        panel
            .open(
                "owner".into(),
                "view.tsx:1500:3".into(),
                PathBuf::from("/tmp"),
            )
            .unwrap();
        panel.candidate = None;
        let (tx, rx) = mpsc::sync_channel(1);
        panel.pending = Some((panel.content_revision, rx));
        tx.send(Ok(LoadedPreview {
            remote: None,
            path: PathBuf::from("/tmp/view.tsx"),
            location: crate::local_reference::parse_candidate("view.tsx:1500:3")
                .unwrap()
                .source_location,
            content: crate::local_preview::PreviewContent::Text(
                (1..=2000).map(|line| format!("row_{line}\n")).collect(),
            ),
        }))
        .unwrap();
        let ctx = egui::Context::default();
        assert!(labels(&frame(&mut panel, &ctx, "other", vec![])).is_empty());
        assert!(panel.seek_pending, "hidden tab must not consume the seek");
        let _ = frame(&mut panel, &ctx, "owner", vec![]);
        let output = frame(&mut panel, &ctx, "owner", vec![]);
        let labels = labels(&output);
        assert!(
            labels.iter().any(|(text, _)| text.contains("row_1500")),
            "{labels:?}"
        );
        assert!(!labels.iter().any(|(text, _)| text.ends_with("row_1")));
        assert!(labels.len() < 100, "only viewport rows may be laid out");
        assert!(!panel.seek_pending);
    }
    #[test]
    fn egui_close_button_cancels_candidate_without_file_read() {
        let mut panel = LocalPreviewPanel::default();
        panel
            .open(
                "owner".into(),
                "missing.tsx".into(),
                PathBuf::from("/missing"),
            )
            .unwrap();
        let ctx = egui::Context::default();
        let _ = frame(&mut panel, &ctx, "owner", vec![]);
        let output = frame(&mut panel, &ctx, "owner", vec![]);
        let pos = labels(&output)
            .into_iter()
            .find(|(text, _)| text == "Close")
            .unwrap()
            .1
            + egui::vec2(3.0, 3.0);
        let events = vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        let _ = frame(&mut panel, &ctx, "owner", events);
        assert!(!panel.visible);
        assert!(panel.pending.is_none());
        assert!(panel.candidate.is_none());
    }
    #[test]
    fn closed_preview_discards_late_result_without_freeing_worker_slot_early() {
        let mut panel = LocalPreviewPanel::default();
        panel
            .open("owner".into(), "view.md".into(), PathBuf::from("/tmp"))
            .unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        panel.pending = Some((panel.content_revision, rx));
        panel.text = Some(PreviewText::new("old content".into()));
        panel.error = Some("old error".into());
        panel.close();
        assert!(panel.text.is_none() && panel.error.is_none() && panel.candidate.is_none());
        panel
            .open("owner".into(), "other.md".into(), PathBuf::from("/tmp"))
            .unwrap();
        panel.start_read();
        assert!(
            panel.candidate.is_some(),
            "busy reader must keep the new confirmation"
        );
        tx.send(Ok(LoadedPreview {
            remote: None,
            path: PathBuf::from("/tmp/view.md"),
            location: None,
            content: crate::local_preview::PreviewContent::Image(egui::ColorImage::filled(
                [2, 2],
                egui::Color32::RED,
            )),
        }))
        .unwrap();
        let ctx = egui::Context::default();
        let _ = frame(&mut panel, &ctx, "owner", vec![]);
        assert!(panel.pending.is_none());
        assert!(panel.texture.is_none() && panel.text.is_none());
        assert!(panel.visible && panel.candidate.is_some());
        assert_eq!(panel.path, PathBuf::from("/tmp/other.md"));
    }

    #[test]
    fn closed_preview_worker_failure_stays_dismissed() {
        for disconnect in [false, true] {
            let mut panel = LocalPreviewPanel::default();
            panel
                .open("owner".into(), "view.md".into(), PathBuf::from("/tmp"))
                .unwrap();
            let (tx, rx) = mpsc::sync_channel(1);
            panel.pending = Some((panel.content_revision, rx));
            panel.close();
            if !disconnect {
                tx.send(Err("late failure".into())).unwrap();
            }
            drop(tx);
            let ctx = egui::Context::default();
            assert!(labels(&frame(&mut panel, &ctx, "owner", vec![])).is_empty());
            assert!(panel.pending.is_none() && panel.error.is_none());
            assert!(!panel.visible);
        }
    }

    #[test]
    fn egui_confirmation_click_loads_inert_source_and_locates_line() {
        let dir = std::env::temp_dir().join(format!("ember-preview-egui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("view.tsx"),
            "first\n<script>throw new Error('inert')</script>\nlast",
        )
        .unwrap();
        let mut panel = LocalPreviewPanel::default();
        panel
            .open("owner".into(), "view.tsx:2:4".into(), dir.clone())
            .unwrap();
        let ctx = egui::Context::default();
        let _ = frame(&mut panel, &ctx, "owner", vec![]);
        let output = frame(&mut panel, &ctx, "owner", vec![]);
        assert!(panel.pending.is_none());
        assert!(panel.text.is_none());
        let pos = labels(&output)
            .into_iter()
            .find(|(text, _)| text == "Preview local file")
            .unwrap()
            .1
            + egui::vec2(3.0, 3.0);
        let events = vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        let _ = frame(&mut panel, &ctx, "owner", events);
        assert!(panel.candidate.is_none());
        for _ in 0..100 {
            let _ = frame(&mut panel, &ctx, "owner", vec![]);
            if panel.pending.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(panel.path, dir.join("view.tsx"));
        assert_eq!(panel.location.as_ref().unwrap().line.get(), 2);
        assert!(panel.text.as_ref().unwrap().line(1).starts_with("<script>"));
        assert!(panel.error.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn reload_reads_the_resolved_path_even_when_a_literal_name_appears() {
        let dir = std::env::temp_dir().join(format!("ember-preview-reload-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let source = dir.join("view.tsx");
        let literal = dir.join("view.tsx:2");
        std::fs::write(&source, "first\nsecond").unwrap();
        let mut panel = LocalPreviewPanel::default();
        panel
            .open("owner".into(), "view.tsx:2".into(), dir.clone())
            .unwrap();
        let ctx = egui::Context::default();
        let read = |panel: &mut LocalPreviewPanel| {
            for _ in 0..100 {
                panel.poll(&ctx);
                if panel.pending.is_none() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("preview did not load");
        };
        panel.start_read();
        read(&mut panel);
        assert_eq!(panel.path, source);
        assert_eq!(panel.selected_line, Some(2));
        std::fs::write(&literal, "literal colon filename").unwrap();
        std::fs::remove_file(&source).unwrap();
        panel.start_read();
        assert_eq!(panel.text.as_ref().unwrap().line(1), "second");
        read(&mut panel);
        assert!(panel
            .error
            .as_ref()
            .unwrap()
            .contains("Showing the last preview"));
        assert_eq!(panel.text.as_ref().unwrap().line(1), "second");
        std::fs::write(&source, "first\nupdated").unwrap();
        panel.start_read();
        read(&mut panel);
        assert_eq!(panel.path, source);
        assert_eq!(panel.text.as_ref().unwrap().line(1), "updated");
        panel.open_tree_file("owner".into(), literal.clone());
        read(&mut panel);
        assert_eq!(panel.path, literal);
        assert_eq!(
            panel.text.as_ref().unwrap().line(0),
            "literal colon filename"
        );
        assert!(panel.selected_line.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
