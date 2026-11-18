//! Read-only preview. One bounded worker; no browser, subprocess or renderer execution.
use crate::glass_shell::BoundedPanel;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

struct LoadedPreview {
    path: PathBuf,
    location: Option<crate::local_reference::SourceLocationCandidate>,
    content: crate::local_preview::PreviewContent,
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

#[derive(Default)]
pub struct LocalPreviewPanel {
    session_id: String,
    path: PathBuf,
    visible: bool,
    pending: Option<Receiver<Result<LoadedPreview, String>>>,
    text: Option<PreviewText>,
    texture: Option<egui::TextureHandle>,
    error: Option<String>,
    candidate: Option<(String, PathBuf)>,
    location: Option<crate::local_reference::SourceLocationCandidate>,
    seek_pending: bool,
    content_revision: u64,
}
impl LocalPreviewPanel {
    pub fn open(&mut self, session_id: String, target: String, cwd: PathBuf) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("A file preview is still loading".into());
        }
        self.session_id = session_id;
        self.path = PathBuf::from(&target);
        self.visible = true;
        self.text = None;
        self.texture = None;
        self.error = None;
        self.location = None;
        self.seek_pending = false;
        self.content_revision = self.content_revision.wrapping_add(1);
        self.candidate = Some((target, cwd));
        Ok(())
    }
    fn load_confirmed(&mut self, target: String, cwd: PathBuf) -> Result<(), String> {
        // A blocked filesystem cannot cause unbounded worker creation on repeated clicks.
        if self.pending.is_some() {
            return Err("A file preview is still loading".into());
        }
        let (tx, rx) = mpsc::sync_channel(1);
        let path = PathBuf::from(&target);
        std::thread::Builder::new()
            .name("local-file-preview".into())
            .spawn(move || {
                let _ = tx.send(resolve(&target, &cwd).and_then(|(path, location)| {
                    crate::local_preview::load(&path).map(|content| LoadedPreview {
                        path,
                        location,
                        content,
                    })
                }));
            })
            .map_err(|e| e.to_string())?;
        self.path = path;
        self.visible = true;
        self.pending = Some(rx);
        self.text = None;
        self.texture = None;
        self.error = None;
        Ok(())
    }
    pub fn show(&mut self, ui: &mut egui::Ui, active_session_id: &str) {
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(result) => {
                    self.pending = None;
                    match result {
                        Ok(loaded) => {
                            self.path = loaded.path;
                            self.location = loaded.location;
                            self.seek_pending = true;
                            match loaded.content {
                                crate::local_preview::PreviewContent::Text(text) => {
                                    self.text = Some(PreviewText::new(text))
                                }
                                crate::local_preview::PreviewContent::Image(image) => {
                                    self.texture = Some(ui.ctx().load_texture(
                                        "local-file-preview",
                                        image,
                                        egui::TextureOptions::LINEAR,
                                    ))
                                }
                            }
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.error = Some("Preview worker stopped".into());
                }
                Err(mpsc::TryRecvError::Empty) => ui
                    .ctx()
                    .request_repaint_after(std::time::Duration::from_millis(50)),
            }
        }
        // A result belongs only to the originating session, never to another tab.
        if !self.visible || self.session_id != active_session_id {
            return;
        }
        egui::Panel::right("local_read_only_preview")
            .default_size(340.0)
            .resizable(true)
            .show_bounded(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("File preview · read only");
                    if ui.button("Close").clicked() {
                        self.visible = false;
                    }
                });
                ui.label(crate::sidebar::bound_sidebar_path_label(&self.path));
                ui.separator();
                if let Some((target, cwd)) = self.candidate.clone() {
                    ui.label("Terminal output does not prove a path belongs to this computer.");
                    ui.label(format!(
                        "Local base: {}",
                        crate::sidebar::bound_sidebar_path_label(&cwd)
                    ));
                    if ui.button("Preview local file").clicked() {
                        self.candidate = None;
                        if let Err(error) = self.load_confirmed(target, cwd) {
                            self.error = Some(error);
                        }
                    }
                }
                if self.pending.is_some() {
                    ui.spinner();
                }
                if let Some(error) = &self.error {
                    ui.label(error);
                }
                if let Some(text) = &self.text {
                    let target = self
                        .location
                        .as_ref()
                        .map(|location| location.line.get() as usize - 1);
                    if let Some(location) = &self.location {
                        let column = location
                            .column
                            .map(|value| format!(", column {}", value))
                            .unwrap_or_default();
                        ui.label(format!("Line {}{}", location.line, column));
                        if target.is_some_and(|index| index >= text.starts.len()) {
                            ui.label("Requested line is outside this file; showing the beginning.");
                        }
                    }
                    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
                    let mut scroll = egui::ScrollArea::both()
                        .id_salt(("local_preview_content", self.content_revision));
                    if self.seek_pending {
                        let index = target
                            .filter(|&index| index < text.starts.len())
                            .unwrap_or(0);
                        scroll = scroll.vertical_scroll_offset(
                            index as f32 * (row_height + ui.spacing().item_spacing.y),
                        );
                        self.seek_pending = false;
                    }
                    scroll.show_rows(ui, row_height, text.starts.len(), |ui, range| {
                        for index in range {
                            let mut line = egui::RichText::new(format!(
                                "{:>6}  {}",
                                index + 1,
                                text.line(index)
                            ))
                            .monospace();
                            if target == Some(index) {
                                line = line.background_color(ui.visuals().selection.bg_fill);
                            }
                            ui.add(
                                egui::Label::new(line)
                                    .selectable(true)
                                    .wrap_mode(egui::TextWrapMode::Extend),
                            );
                        }
                    });
                }
                if let Some(texture) = &self.texture {
                    egui::ScrollArea::both()
                        .id_salt(("local_preview_image", self.content_revision))
                        .show(ui, |ui| {
                            ui.image((texture.id(), texture.size_vec2()));
                        });
                }
            });
    }
}

fn resolve(
    target: &str,
    cwd: &std::path::Path,
) -> Result<
    (
        PathBuf,
        Option<crate::local_reference::SourceLocationCandidate>,
    ),
    String,
> {
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
    let exact = join(&candidate.exact_path)?;
    match std::fs::symlink_metadata(&exact) {
        Ok(_) => Ok((exact, None)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => candidate
            .source_location
            .map(|location| join(&location.path).map(|path| (path, Some(location))))
            .unwrap_or_else(|| Err("Local file not found".into())),
        Err(_) => Err("Cannot inspect local file".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
                egui::CentralPanel::default().show(root_ui, |ui| panel.show(ui, session));
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
        panel.pending = Some(rx);
        tx.send(Ok(LoadedPreview {
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
}
