//! Asynchronous bridge from terminal OSC 133 records to rsh's execution log.
//!
//! Terminal parsing runs on the UI thread (and on the bounded background-tab
//! pump), so it must never wait for a filesystem, an advisory lock, or another
//! process.  A small bounded channel hands immutable output snapshots to one
//! writer thread.  rsh owns the rest of the execution lifecycle (`start` and
//! `finish`); jterm contributes the text that was actually rendered by the
//! terminal as an `output` event with the same execution id.

use crate::terminal::CompletedCommandOutput;
use crossbeam::channel::{self, Sender, TrySendError};
use once_cell::sync::OnceCell;
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const EXECUTION_JOURNAL_VERSION: u32 = 1;
const WRITER_QUEUE_CAPACITY: usize = 64;

#[derive(Debug)]
pub(crate) enum SubmitError {
    Full,
    Closed,
}

#[derive(Debug, Serialize)]
struct OutputEvent {
    rsh_execution_version: u32,
    event: &'static str,
    id: String,
    text: String,
    truncated: bool,
    total_bytes: usize,
    captured_at_ms: u64,
}

enum JournalMessage {
    Output(OutputEvent),
    Flush(Sender<()>),
}

impl OutputEvent {
    fn from_completed(completed: CompletedCommandOutput) -> Option<Self> {
        // Bare FinalTerm markers receive terminal-local ids so the timeline
        // still works, but there is no matching rsh start/finish lifecycle to
        // correlate on disk.
        if !completed.output_available
            || completed.id.is_empty()
            || completed.id.starts_with("local:")
        {
            return None;
        }
        Some(Self {
            rsh_execution_version: EXECUTION_JOURNAL_VERSION,
            event: "output",
            id: completed.id,
            text: completed.output,
            truncated: completed.truncated,
            total_bytes: completed.total_bytes,
            captured_at_ms: unix_time_ms(),
        })
    }
}

static WRITER: OnceCell<Option<Sender<JournalMessage>>> = OnceCell::new();

fn writer() -> Option<&'static Sender<JournalMessage>> {
    WRITER
        .get_or_init(|| {
            let (tx, rx) = channel::bounded::<JournalMessage>(WRITER_QUEUE_CAPACITY);
            match std::thread::Builder::new()
                .name("rsh-execution-journal".to_owned())
                .spawn(move || {
                    while let Ok(message) = rx.recv() {
                        match message {
                            JournalMessage::Output(event) => {
                                if let Err(error) = append_event(&event) {
                                    log::warn!("cannot append rsh execution output: {error}");
                                }
                            }
                            JournalMessage::Flush(acknowledge) => {
                                let _ = acknowledge.send(());
                            }
                        }
                    }
                }) {
                Ok(_) => Some(tx),
                Err(error) => {
                    log::warn!("cannot start rsh execution journal writer: {error}");
                    None
                }
            }
        })
        .as_ref()
}

/// Queue one completed output without blocking the terminal/UI thread.
///
/// A saturated queue deliberately rejects the newest item. Each command
/// remains represented by rsh's start/finish events, while memory stays
/// bounded even if the state directory is on a stalled filesystem.
pub(crate) fn submit(completed: CompletedCommandOutput) -> Result<(), SubmitError> {
    if !enabled() {
        return Ok(());
    }
    let Some(event) = OutputEvent::from_completed(completed) else {
        return Ok(());
    };
    let writer = writer().ok_or(SubmitError::Closed)?;
    writer
        .try_send(JournalMessage::Output(event))
        .map_err(|error| match error {
            TrySendError::Full(_) => SubmitError::Full,
            TrySendError::Disconnected(_) => SubmitError::Closed,
        })
}

/// Wait briefly for every output accepted before this call to reach disk.
/// Used during orderly application shutdown; normal terminal frames never
/// block on the journal.
pub(crate) fn flush(timeout: std::time::Duration) -> bool {
    if !enabled() {
        return true;
    }
    let Some(Some(writer)) = WRITER.get() else {
        return true;
    };
    let (ack_tx, ack_rx) = channel::bounded(1);
    let started = std::time::Instant::now();
    if writer
        .send_timeout(JournalMessage::Flush(ack_tx), timeout)
        .is_err()
    {
        return false;
    }
    ack_rx
        .recv_timeout(timeout.saturating_sub(started.elapsed()))
        .is_ok()
}

fn enabled() -> bool {
    std::env::var("RSH_EXECUTION_JOURNAL")
        .ok()
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "" | "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true)
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn journal_path() -> io::Result<(PathBuf, bool)> {
    if let Some(path) = std::env::var_os("RSH_EXECUTION_JOURNAL_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "RSH_EXECUTION_JOURNAL_PATH must be absolute",
            ));
        }
        return Ok((path, true));
    }
    let state_dir = dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/state")))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no user state directory"))?;
    Ok((state_dir.join("rsh/executions.jsonl"), false))
}

fn prepare_journal_path() -> io::Result<PathBuf> {
    let (path, custom_path) = journal_path()?;
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "journal has no parent"))?;
    let dir_already_existed = dir.exists();
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    if !custom_path || !dir_already_existed {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(path)
}

fn append_event(event: &OutputEvent) -> io::Result<()> {
    let journal_path = prepare_journal_path()?;
    let dir = journal_path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "journal has no parent"))?;
    let lock_path = dir.join("executions.lock");

    let mut lock_options = OpenOptions::new();
    lock_options.create(true).read(true).write(true);
    #[cfg(unix)]
    lock_options.mode(0o600);
    let lock = lock_options.open(lock_path)?;
    #[cfg(unix)]
    lock.set_permissions(fs::Permissions::from_mode(0o600))?;
    lock_exclusive(&lock)?;

    let write_result = (|| {
        let mut journal_options = OpenOptions::new();
        journal_options.create(true).append(true);
        #[cfg(unix)]
        journal_options.mode(0o600);
        let mut journal = journal_options.open(journal_path)?;
        #[cfg(unix)]
        journal.set_permissions(fs::Permissions::from_mode(0o600))?;
        serde_json::to_writer(&mut journal, event).map_err(io::Error::other)?;
        journal.write_all(b"\n")?;
        journal.flush()
    })();

    let unlock_result = unlock(&lock);
    write_result.and(unlock_result)
}

#[cfg(unix)]
fn lock_exclusive(file: &std::fs::File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: `file` remains open for the entire flock lifetime and flock does
    // not dereference userspace pointers.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn lock_exclusive(_file: &std::fs::File) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn unlock(file: &std::fs::File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: see `lock_exclusive`; the descriptor is still owned by `file`.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn unlock(_file: &std::fs::File) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_output_is_not_persisted() {
        let completed = CompletedCommandOutput {
            id: "id".to_owned(),
            command: None,
            cwd: None,
            exit_code: Some(0),
            duration_ms: None,
            output: String::new(),
            output_available: false,
            truncated: false,
            total_bytes: 0,
        };
        assert!(OutputEvent::from_completed(completed).is_none());
    }

    #[test]
    fn output_event_matches_rsh_envelope() {
        let completed = CompletedCommandOutput {
            id: "exec-1".to_owned(),
            command: Some("printf hi".to_owned()),
            cwd: Some("/tmp".to_owned()),
            exit_code: Some(0),
            duration_ms: Some(12),
            output: "hi".to_owned(),
            output_available: true,
            truncated: false,
            total_bytes: 2,
        };
        let value = serde_json::to_value(OutputEvent::from_completed(completed).unwrap()).unwrap();
        assert_eq!(value["rsh_execution_version"], 1);
        assert_eq!(value["event"], "output");
        assert_eq!(value["id"], "exec-1");
        assert_eq!(value["text"], "hi");
        assert_eq!(value["total_bytes"], 2);
        assert!(value.get("command").is_none());
    }
}
