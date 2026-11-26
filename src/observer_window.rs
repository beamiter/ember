use crate::observer::Frame;
use crate::terminal::TerminalState;
use parking_lot::Mutex;
use std::sync::Arc;
struct State {
    frame: Option<Arc<Frame>>,
    closed: bool,
    open: bool,
}
pub(crate) struct View {
    id: egui::ViewportId,
    target: crate::observer_identity::Target<Mutex<TerminalState>>,
    label: String,
    state: Arc<Mutex<State>>,
}
impl View {
    fn new(session: &crate::session::Session, serial: u64) -> Self {
        Self {
            id: egui::ViewportId::from_hash_of(("read-only-observer", serial)),
            target: crate::observer_identity::Target::new(
                session.metadata.session_id.clone(),
                &session.terminal,
            ),
            label: format!(
                "PTY owner {} · Read only · owner controls input and terminal size",
                session.get_shell_pid()
            ),
            state: Arc::new(Mutex::new(State {
                frame: None,
                closed: false,
                open: true,
            })),
        }
    }
}
impl crate::TerminalApp {
    pub(crate) fn open_read_only_observer(&mut self, index: usize) {
        if self.observers.len() >= 8 {
            return;
        }
        if let Some(session) = self.session_manager.sessions().get(index) {
            self.observer_serial = self.observer_serial.wrapping_add(1);
            self.observers
                .push(View::new(session, self.observer_serial));
        }
    }
    pub(crate) fn render_read_only_observers(&mut self, ctx: &egui::Context) {
        self.observers.retain(|view| view.state.lock().open);
        for view in &self.observers {
            let session = self
                .session_manager
                .sessions()
                .iter()
                .find(|s| view.target.matches(&s.metadata.session_id, &s.terminal));
            {
                let mut state = view.state.lock();
                state.closed = session
                    .is_none_or(|s| s.purpose == crate::session::SessionPurpose::RetainedCommand);
                if let Some(frame) = session
                    .and_then(|s| s.terminal_presentation.last_frame.as_ref())
                    .map(|f| Arc::clone(&f.observer))
                {
                    state.frame = Some(frame);
                }
            }
            let shared = Arc::clone(&view.state);
            let label = view.label.clone();
            ctx.show_viewport_deferred(view.id,egui::ViewportBuilder::default().with_title("Read-only Ember observer").with_inner_size([850.0,550.0]),move|ui,class| {
                if class==egui::ViewportClass::EmbeddedWindow {ui.label("Native observer windows are unavailable on this backend");return;}
                let (frame,closed)={let s=shared.lock();(s.frame.clone(),s.closed)};
                let close=ui.input(|i|i.viewport().close_requested());
                ui.label(if closed{"Owner closed · last published viewport"}else{&label});
                ui.label("Published terminal cells · image and native editor overlays are not mirrored");
                if ui.button("Close observer").clicked() || close {shared.lock().open=false;ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);ui.ctx().request_repaint_of(egui::ViewportId::ROOT);return;}
                ui.separator();
                if let Some(frame)=frame {frame.paint(ui);} else {ui.label("Waiting for owner publication");}
            });
            ctx.request_repaint_of(view.id);
        }
    }
}
