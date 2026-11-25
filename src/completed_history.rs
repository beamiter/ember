//! Checkpoint read-only completed cards for panes admitted by the layout file.

use crate::{
    session::{Session, SessionPurpose},
    session_persistence::{InstanceLock, SessionsSnapshot},
};
use jterm_core::completed_history::Checkpoint;
use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

const WRITE_INTERVAL: Duration = Duration::from_secs(1);
const SCAN_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Default)]
struct Progress {
    written: u64,
    due: Option<Instant>,
    blocked: bool,
}

struct Write {
    root: PathBuf,
    pane: String,
    revision: u64,
    result: mpsc::Receiver<io::Result<()>>,
}

#[derive(Default)]
pub(crate) struct PaneHistory {
    root: Option<PathBuf>,
    panes: HashMap<String, Progress>,
    pending: Option<Write>,
    next_scan: Option<Instant>,
}

fn archive_name(pane: &str) -> String {
    format!("{pane}.json")
}

impl PaneHistory {
    /// Bind only after a layout has been successfully restored or published.
    /// This prevents a durable card from naming an uncommitted pane identity.
    pub(crate) fn bind_layout(&mut self, path: &Path, snapshot: &SessionsSnapshot) {
        let Some(filename) = path.file_name() else {
            return;
        };
        let mut directory = filename.to_os_string();
        directory.push(".ember-cards");
        let root = path.with_file_name(directory);
        if self.root.as_ref() != Some(&root) {
            self.panes.clear();
            self.root = Some(root);
        }
        let owners: HashSet<_> = snapshot
            .sessions
            .iter()
            .take(crate::session_persistence::MAX_RESTORED_SESSIONS)
            .filter_map(|session| session.session_id.as_deref())
            .filter(|id| jterm_core::execution_journal::is_valid_jsh_session_id(id))
            .collect();
        self.panes.retain(|pane, _| owners.contains(pane.as_str()));
        for pane in owners {
            if !self.panes.contains_key(pane) {
                self.panes.insert(pane.to_owned(), Progress::default());
            }
        }
    }

    pub(crate) fn restore(&mut self, sessions: &[Session]) {
        let Some(root) = self.root.as_ref() else {
            return;
        };
        for session in sessions {
            let pane = &session.metadata.session_id;
            let Some(progress) = self.panes.get_mut(pane) else {
                continue;
            };
            let result =
                Checkpoint::load(&root.join(archive_name(pane)), pane).and_then(|checkpoint| {
                    if let Some(checkpoint) = checkpoint {
                        session
                            .terminal
                            .lock()
                            .restore_completed_history(checkpoint)?;
                    }
                    Ok(())
                });
            if let Err(error) = result {
                progress.blocked = true;
                log::warn!("Cannot restore completed pane history: {error}");
            }
        }
    }

    pub(crate) fn tick(
        &mut self,
        sessions: &[Session],
        owner: &Arc<InstanceLock>,
        ctx: &eframe::egui::Context,
    ) {
        if let Some(write) = self.pending.as_ref() {
            match write.result.try_recv() {
                Ok(result) => self.finish_write(result),
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.finish_write(Err(io::Error::other("history writer exited")))
                }
                Err(mpsc::TryRecvError::Empty) => return,
            }
        }
        let now = Instant::now();
        if self.next_scan.is_some_and(|deadline| now < deadline) {
            return;
        }
        self.next_scan = Some(now + SCAN_INTERVAL);
        let Some((pane, revision, checkpoint)) = self.next_checkpoint(sessions, now, false) else {
            return;
        };
        self.start_write(pane, revision, checkpoint, owner, Some(ctx.clone()));
    }

    /// Flush the one write in progress, then give any newer retained state a
    /// bounded chance to commit. Each job keeps the instance lock alive until
    /// its atomic replacement has stopped touching the archive.
    pub(crate) fn flush(
        &mut self,
        sessions: &[Session],
        owner: &Arc<InstanceLock>,
        timeout: Duration,
    ) {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(write) = self.pending.as_ref() {
                match write
                    .result
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                {
                    Ok(Ok(())) => self.finish_write(Ok(())),
                    Ok(Err(error)) => {
                        self.finish_write(Err(error));
                        return;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        self.finish_write(Err(io::Error::other("history writer exited")));
                        return;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                }
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let Some((pane, revision, checkpoint)) = self.next_checkpoint(sessions, now, true)
            else {
                return;
            };
            if !self.start_write(pane, revision, checkpoint, owner, None) {
                return;
            }
        }
        log::warn!("Completed pane history did not finish saving before exit");
    }

    fn next_checkpoint(
        &mut self,
        sessions: &[Session],
        now: Instant,
        force: bool,
    ) -> Option<(String, u64, Checkpoint)> {
        for session in sessions
            .iter()
            .filter(|session| session.purpose == SessionPurpose::Interactive)
        {
            let pane = &session.metadata.session_id;
            let Some(progress) = self
                .panes
                .get_mut(pane)
                .filter(|progress| !progress.blocked)
            else {
                continue;
            };
            let terminal = session.terminal.lock();
            let revision = terminal.completed_history_revision();
            if revision == progress.written {
                continue;
            }
            let due = *progress.due.get_or_insert(now + WRITE_INTERVAL);
            if !force && now < due {
                continue;
            }
            match terminal.completed_history_checkpoint(pane) {
                Ok(checkpoint) => return Some((pane.clone(), revision, checkpoint)),
                Err(error) => {
                    progress.blocked = true;
                    log::warn!("Cannot capture completed pane history: {error}");
                }
            }
        }
        None
    }

    fn start_write(
        &mut self,
        pane: String,
        revision: u64,
        checkpoint: Checkpoint,
        owner: &Arc<InstanceLock>,
        wake: Option<eframe::egui::Context>,
    ) -> bool {
        let root = self
            .root
            .as_ref()
            .expect("pane was bound to a layout")
            .clone();
        let path = root.join(archive_name(&pane));
        let owner = owner.clone();
        let (send, result) = mpsc::channel();
        match std::thread::Builder::new()
            .name("ember-pane-history".into())
            .spawn(move || {
                let result = checkpoint.save(&path);
                drop(owner);
                let _ = send.send(result);
                if let Some(ctx) = wake {
                    ctx.request_repaint();
                }
            }) {
            Ok(_) => {
                self.pending = Some(Write {
                    root,
                    pane,
                    revision,
                    result,
                });
                true
            }
            Err(error) => {
                if let Some(progress) = self.panes.get_mut(&pane) {
                    progress.due = Some(Instant::now() + Duration::from_secs(5));
                }
                log::warn!("Cannot start completed pane history writer: {error}");
                false
            }
        }
    }

    fn finish_write(&mut self, result: io::Result<()>) {
        let write = self.pending.take().expect("a history write is pending");
        if self.root.as_ref() != Some(&write.root) {
            return;
        }
        let Some(progress) = self.panes.get_mut(&write.pane) else {
            return;
        };
        match result {
            Ok(()) => {
                progress.written = write.revision;
                progress.due = None;
            }
            Err(error) => {
                progress.blocked = matches!(
                    error.kind(),
                    io::ErrorKind::InvalidData
                        | io::ErrorKind::Unsupported
                        | io::ErrorKind::AlreadyExists
                        | io::ErrorKind::PermissionDenied
                        | io::ErrorKind::InvalidInput
                        | io::ErrorKind::FileTooLarge
                );
                progress.due = Some(Instant::now() + Duration::from_secs(5));
                log::warn!("Cannot save completed pane history: {error}");
            }
        }
    }
}
