//! Read-only preview. One bounded worker; no browser, subprocess or renderer execution.
use crate::glass_shell::BoundedPanel;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

#[derive(Default)]
pub struct LocalPreviewPanel {
    session_id: String,
    path: PathBuf,
    visible: bool,
    pending: Option<Receiver<Result<crate::local_preview::PreviewContent, String>>>,
    text: Option<String>,
    texture: Option<egui::TextureHandle>,
    error: Option<String>,
    candidate: Option<(String, PathBuf)>,
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
                let _ = tx.send(
                    resolve(&target, &cwd).and_then(|source| crate::local_preview::load(&source)),
                );
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
                        Ok(crate::local_preview::PreviewContent::Text(text)) => {
                            self.text = Some(text)
                        }
                        Ok(crate::local_preview::PreviewContent::Image(image)) => {
                            self.texture = Some(ui.ctx().load_texture(
                                "local-file-preview",
                                image,
                                egui::TextureOptions::LINEAR,
                            ));
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
                egui::ScrollArea::both()
                    .id_salt("local_preview_content")
                    .show(ui, |ui| {
                        if let Some(text) = &self.text {
                            ui.add(
                                egui::Label::new(egui::RichText::new(text).monospace())
                                    .selectable(true),
                            );
                        }
                        if let Some(texture) = &self.texture {
                            ui.image((texture.id(), texture.size_vec2()));
                        }
                    });
            });
    }
}

fn resolve(target: &str, cwd: &std::path::Path) -> Result<PathBuf, String> {
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
        Ok(_) => Ok(exact),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => candidate
            .source_location
            .map(|location| join(&location.path))
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
            resolve("view.tsx:12:3", &dir).unwrap(),
            dir.join("view.tsx:12:3")
        );
        std::fs::remove_file(dir.join("view.tsx:12:3")).unwrap();
        assert_eq!(
            resolve("view.tsx:12:3", &dir).unwrap(),
            dir.join("view.tsx")
        );
        assert!(resolve("https://host/x.tsx", &dir).is_err());
        assert!(resolve("view.tsx", std::path::Path::new("relative")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
