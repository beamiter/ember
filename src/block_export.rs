//! Whole-session command-block export.
//!
//! Ported from frost's `block_export.rs` onto ember's semantic record model:
//! the UI snapshots one terminal's bounded OSC 133 records, then hands the
//! owned snapshot to a worker thread. Serialization and durable file I/O
//! therefore never hold the terminal lock or block the renderer.

use crate::block_mode;
use crate::persistence_file;
use crate::terminal::{TerminalState, MAX_COMPLETED_COMMAND_OUTPUT_BYTES};
use serde::Serialize;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Maximum command/output text cloned out of one terminal for an export.
/// Captured output is already held to a 16 MiB aggregate budget; this also
/// bounds live-row fallbacks whose snapshots were evicted.
const MAX_SESSION_EXPORT_SOURCE_BYTES: usize = 16 * 1024 * 1024;
/// JSON escaping can expand source text; keep the final file bounded too.
pub const MAX_SESSION_EXPORT_BYTES: usize = 64 * 1024 * 1024;
pub const SESSION_EXPORT_SCHEMA: &str = "ember.block-session";
pub const SESSION_EXPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionExportFormat {
    Markdown,
    Json,
}

impl SessionExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Json => "json",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Markdown => "Markdown",
            Self::Json => "JSON",
        }
    }
}

/// Stable, renderer-independent representation of one exported ember block.
/// Mirrors frost's export shape, plus `command_exact` so a screen-reconstructed
/// command can never read like shell-supplied metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionExportBlock {
    pub id: String,
    pub sequence: u64,
    pub command: Option<String>,
    /// True only when the shell supplied exact command metadata (OSC 133
    /// `cmdline_url`). A screen-reconstructed command is useful for display
    /// but must not be mistaken for an authoritative command line.
    pub command_exact: bool,
    pub output: String,
    pub output_truncated: bool,
    /// Both the retained output snapshot and its original buffer rows are
    /// gone. Kept distinct from a command that genuinely printed nothing.
    pub output_unavailable: bool,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    pub tz_offset_secs: i32,
    pub cwd: Option<String>,
    pub command_truncated: bool,
    /// Whether the OSC 133 `C` command-start mark was observed. Kept even
    /// after row eviction so lifecycle health remains stable in exports.
    pub start_mark_seen: bool,
    #[serde(serialize_with = "serialize_completion_provenance")]
    pub completion_provenance: block_mode::CompletionProvenance,
    #[serde(serialize_with = "serialize_lifecycle_health")]
    pub lifecycle_health: block_mode::BlockLifecycleHealth,
    /// `false` for a lifecycle recovered at the next prompt after OSC 133 `D`
    /// was lost. Retained for compatibility with the v1 boolean field; new
    /// consumers should use `completion_provenance`.
    pub completion_observed: bool,
}

fn serialize_completion_provenance<S>(
    value: &block_mode::CompletionProvenance,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(block_mode::completion_provenance_schema_name(*value))
}

fn serialize_lifecycle_health<S>(
    value: &block_mode::BlockLifecycleHealth,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(block_mode::lifecycle_health_schema_name(*value))
}

/// One immutable pane snapshot handed to the export worker. Keeping identity
/// and capture time beside the blocks makes JSON exports self-describing even
/// after their originating Ember process has exited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionExportSnapshot {
    pub pane_session_id: String,
    pub captured_at_ms: u64,
    pub captured_tz_offset_secs: i32,
    pub blocks: Vec<SessionExportBlock>,
}

#[derive(Serialize)]
struct SessionExportRetention {
    retained_blocks: usize,
    command_truncated: usize,
    output_truncated: usize,
    output_unavailable: usize,
    completion_unobserved: usize,
    lifecycle_degraded: usize,
    lifecycle_recovered: usize,
    lifecycle_incomplete: usize,
}

#[derive(Serialize)]
struct JsonSessionExport<'a> {
    schema: &'static str,
    version: u32,
    pane_session_id: &'a str,
    captured_at_ms: u64,
    captured_tz_offset_secs: i32,
    block_order: &'static str,
    retention: SessionExportRetention,
    blocks: &'a [SessionExportBlock],
}

impl SessionExportSnapshot {
    fn retention(&self) -> SessionExportRetention {
        let is_command_block = |block: &&SessionExportBlock| {
            block.command_truncated
                || block
                    .command
                    .as_deref()
                    .is_some_and(|command| !command.trim().is_empty())
        };
        SessionExportRetention {
            retained_blocks: self.blocks.len(),
            command_truncated: self
                .blocks
                .iter()
                .filter(|block| block.command_truncated)
                .count(),
            output_truncated: self
                .blocks
                .iter()
                .filter(|block| block.output_truncated)
                .count(),
            output_unavailable: self
                .blocks
                .iter()
                .filter(|block| block.output_unavailable)
                .count(),
            completion_unobserved: self
                .blocks
                .iter()
                .filter(|block| !block.completion_observed)
                .count(),
            lifecycle_degraded: self
                .blocks
                .iter()
                .filter(is_command_block)
                .filter(|block| {
                    block.lifecycle_health == block_mode::BlockLifecycleHealth::Degraded
                })
                .count(),
            lifecycle_recovered: self
                .blocks
                .iter()
                .filter(is_command_block)
                .filter(|block| {
                    block.lifecycle_health == block_mode::BlockLifecycleHealth::Recovered
                })
                .count(),
            lifecycle_incomplete: self
                .blocks
                .iter()
                .filter(is_command_block)
                .filter(|block| {
                    block.lifecycle_health == block_mode::BlockLifecycleHealth::Incomplete
                })
                .count(),
        }
    }
}

impl SessionExportBlock {
    fn markdown(&self) -> String {
        let present = self
            .command
            .as_deref()
            .is_some_and(|command| !command.trim().is_empty());
        let background = !present && !self.command_truncated;
        // Same rule as the copy path: a producer-truncated command is omitted
        // rather than exported as if it were the complete line.
        let command = (!self.command_truncated)
            .then_some(self.command.as_deref())
            .flatten();
        let finished = self
            .finished_at_ms
            .map(|ms| block_mode::format_local_datetime(ms / 1000, self.tz_offset_secs));
        block_mode::block_markdown_with_lifecycle(
            &block_mode::MarkdownBlock {
                command,
                command_exact: self.command_exact,
                command_omitted: !background && command.is_none(),
                command_truncated: self.command_truncated,
                output: &self.output,
                output_truncated: self.output_truncated,
                exit_code: self.exit_code,
                duration_ms: self.duration_ms,
                finished: finished.as_deref(),
                cwd: self.cwd.as_deref(),
            },
            self.start_mark_seen,
            self.completion_provenance,
        )
    }
}

fn too_large() -> io::Error {
    io::Error::new(
        io::ErrorKind::FileTooLarge,
        format!(
            "session block text exceeds {} MiB",
            MAX_SESSION_EXPORT_SOURCE_BYTES / (1024 * 1024)
        ),
    )
}

/// Clone the active terminal's retained finalized records oldest-first. Each
/// record's captured snapshot wins over live row extraction through the
/// terminal's existing accessor, so records already outside scrollback still
/// export; a record whose snapshot and rows are both gone is disclosed as
/// `output_unavailable` instead of failing the whole export.
pub fn snapshot_session(
    terminal: &TerminalState,
    pane_session_id: &str,
) -> io::Result<SessionExportSnapshot> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let mut source_bytes = 0usize;
    let records = terminal.command_records();
    let mut blocks = Vec::with_capacity(records.len());

    for record in records.iter().filter(|record| record.complete) {
        let (output, output_truncated, output_unavailable) =
            match terminal.command_output_text(&record.id, MAX_COMPLETED_COMMAND_OUTPUT_BYTES) {
                Some(text) => (text.text, text.truncated, false),
                None => (String::new(), false, true),
            };
        let record_bytes = output
            .len()
            .saturating_add(record.command.as_ref().map_or(0, String::len))
            .saturating_add(record.cwd.as_ref().map_or(0, String::len))
            .saturating_add(128);
        source_bytes = source_bytes
            .checked_add(record_bytes)
            .ok_or_else(too_large)?;
        if source_bytes > MAX_SESSION_EXPORT_SOURCE_BYTES {
            return Err(too_large());
        }
        let finished_secs = record
            .finished_at
            .and_then(block_mode::epoch_secs)
            .unwrap_or(now_ms / 1000);
        blocks.push(SessionExportBlock {
            id: record.id.clone(),
            sequence: record.sequence,
            command: record.command.clone(),
            command_exact: record.command_exact,
            output,
            output_truncated,
            output_unavailable,
            exit_code: record.exit_code,
            duration_ms: record.duration_ms,
            finished_at_ms: record
                .finished_at
                .and_then(block_mode::epoch_secs)
                .map(|secs| secs.saturating_mul(1_000)),
            tz_offset_secs: block_mode::local_utc_offset_secs(finished_secs),
            cwd: record.cwd.clone(),
            command_truncated: record.command_truncated,
            start_mark_seen: record.start_mark_seen,
            completion_provenance: record.completion_provenance,
            lifecycle_health: block_mode::assess_lifecycle(
                record.start_mark_seen,
                record.completion_provenance,
            ),
            completion_observed: record.completion_provenance
                == block_mode::CompletionProvenance::ShellReported,
        });
    }
    Ok(SessionExportSnapshot {
        pane_session_id: pane_session_id.to_string(),
        captured_at_ms: now_ms,
        captured_tz_offset_secs: block_mode::local_utc_offset_secs(now_ms / 1000),
        blocks,
    })
}

struct BoundedBuffer {
    bytes: Vec<u8>,
}

impl BoundedBuffer {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > MAX_SESSION_EXPORT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!(
                    "serialized session export exceeds {} MiB",
                    MAX_SESSION_EXPORT_BYTES / (1024 * 1024)
                ),
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialize_session(
    snapshot: &SessionExportSnapshot,
    format: SessionExportFormat,
) -> io::Result<Vec<u8>> {
    let mut writer = BoundedBuffer::new();
    match format {
        SessionExportFormat::Json => {
            let document = JsonSessionExport {
                schema: SESSION_EXPORT_SCHEMA,
                version: SESSION_EXPORT_SCHEMA_VERSION,
                pane_session_id: &snapshot.pane_session_id,
                captured_at_ms: snapshot.captured_at_ms,
                captured_tz_offset_secs: snapshot.captured_tz_offset_secs,
                block_order: "oldest_first",
                retention: snapshot.retention(),
                blocks: &snapshot.blocks,
            };
            serde_json::to_writer_pretty(&mut writer, &document)
                .map_err(|error| io::Error::other(error.to_string()))?;
            writer.write_all(b"\n")?;
        }
        SessionExportFormat::Markdown => {
            writeln!(writer, "# Terminal Session Export\n")?;
            writeln!(writer, "Pane session: {}", snapshot.pane_session_id)?;
            writeln!(
                writer,
                "Captured: {}",
                block_mode::format_local_datetime(
                    snapshot.captured_at_ms / 1000,
                    snapshot.captured_tz_offset_secs
                )
            )?;
            writeln!(writer, "Total retained blocks: {}\n", snapshot.blocks.len())?;
            writeln!(writer, "---\n")?;
            for (index, block) in snapshot.blocks.iter().enumerate() {
                writeln!(writer, "## Block #{}\n", index + 1)?;
                writer.write_all(block.markdown().as_bytes())?;
                writeln!(writer, "\n---\n")?;
            }
        }
    }
    Ok(writer.finish())
}

fn export_file_name(stamp: &str, extension: &str, attempt: u32) -> String {
    if attempt == 0 {
        format!("session-{stamp}.{extension}")
    } else {
        format!("session-{stamp}-{attempt}.{extension}")
    }
}

fn write_session_export(
    directory: &Path,
    stamp: &str,
    format: SessionExportFormat,
    contents: &[u8],
) -> io::Result<PathBuf> {
    persistence_file::ensure_private_directory(directory)?;
    for attempt in 0..100u32 {
        let path = directory.join(export_file_name(stamp, format.extension(), attempt));
        match persistence_file::write_new_private_file(
            &path,
            contents,
            MAX_SESSION_EXPORT_BYTES as u64,
        ) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many session exports share this timestamp",
    ))
}

fn export_stamp() -> String {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let offset = block_mode::local_utc_offset_secs(now_secs);
    block_mode::compact_local_datetime(now_secs, offset)
}

/// Serialize and durably write one owned session snapshot. Intended for a
/// blocking worker; no live terminal or renderer state is touched here.
pub fn export_session_to_file(
    snapshot: &SessionExportSnapshot,
    format: SessionExportFormat,
) -> io::Result<PathBuf> {
    let contents = serialize_session(snapshot, format)?;
    let directory = dirs::data_local_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no user data directory"))?
        .join("ember")
        .join("exports");
    write_session_export(&directory, &export_stamp(), format, &contents)
}

/// In-flight export worker polled by the app each frame. The owned snapshot
/// means later output/session churn cannot change the document halfway
/// through an export.
pub struct PendingSessionExport {
    pub format: SessionExportFormat,
    receiver: std::sync::mpsc::Receiver<io::Result<PathBuf>>,
}

impl PendingSessionExport {
    pub fn try_recv(&self) -> Result<io::Result<PathBuf>, std::sync::mpsc::TryRecvError> {
        self.receiver.try_recv()
    }
}

/// Spawn the blocking serializer/writer off the UI thread (frost 的
/// `start_block_export` 对应物;ember 用命名线程 + 有界 channel,与
/// `begin_command_worktree_task` 同一模式).
pub fn start_session_export(
    snapshot: SessionExportSnapshot,
    format: SessionExportFormat,
) -> io::Result<PendingSessionExport> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("ember-session-export".to_string())
        .spawn(move || {
            let _ = sender.send(export_session_to_file(&snapshot, format));
        })?;
    Ok(PendingSessionExport { format, receiver })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "ember-block-export-{name}-{}-{unique}",
                std::process::id()
            )))
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn block(sequence: u64, command: &str, output: &str) -> SessionExportBlock {
        SessionExportBlock {
            id: format!("local-{sequence}"),
            sequence,
            command: Some(command.to_string()),
            command_exact: true,
            output: output.to_string(),
            output_truncated: false,
            output_unavailable: false,
            exit_code: Some(0),
            duration_ms: Some(1250),
            finished_at_ms: Some(0),
            tz_offset_secs: 0,
            cwd: Some("/tmp/project".to_string()),
            command_truncated: false,
            start_mark_seen: true,
            completion_provenance: block_mode::CompletionProvenance::ShellReported,
            lifecycle_health: block_mode::BlockLifecycleHealth::Healthy,
            completion_observed: true,
        }
    }

    fn snapshot(blocks: Vec<SessionExportBlock>) -> SessionExportSnapshot {
        SessionExportSnapshot {
            pane_session_id: "pane-42".to_string(),
            captured_at_ms: 0,
            captured_tz_offset_secs: 0,
            blocks,
        }
    }

    #[test]
    fn snapshot_session_bridges_real_osc_133_block_lifecycles() {
        let mut terminal = TerminalState::new(80, 12);

        // An ordinary observed completion keeps the shell-provided command,
        // status, duration, and cwd alongside the captured output snapshot.
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07printf done\r\n");
        terminal.process_input(
            b"\x1b]133;C;cmdline_url=printf%20done;cwd_url=%2Ftmp%2Fjob\x07done\r\n\x1b]133;D;exit_code=7;duration_ms=1250\x07",
        );

        // A prompt cycle that never reaches C finalizes commandless at the
        // next prompt. ember's record model anchors output only at C, so this
        // block discloses `output_unavailable` instead of inventing text.
        terminal.process_input(
            b"\x1b]133;A;cwd_url=%2Ftmp%2Fbackground\x07$ \x1b]133;B\x07background notice",
        );
        terminal.process_input(b"\r\n\x1b]133;A\x07$ ");

        // A command that reached C but lost D is retained at the following A.
        // The producer-truncated command is dropped from the record, and the
        // truncation evidence must survive that recovery path.
        terminal.process_input(b"\x1b]133;B\x07long-running-command\r\n");
        terminal.process_input(
            b"\x1b]133;C;cmdline_url=long-running-command;cmd_truncated=true;cwd_url=%2Fsrv%2Fwork\x07partial output\r\n",
        );
        terminal.process_input(b"\x1b]133;A\x07$ ");

        let snapshot =
            snapshot_session(&terminal, "pane-17").expect("snapshot real block lifecycles");
        assert_eq!(snapshot.pane_session_id, "pane-17");
        assert_eq!(snapshot.blocks.len(), 3);

        let completed = &snapshot.blocks[0];
        assert_eq!(completed.command.as_deref(), Some("printf done"));
        assert!(completed.command_exact);
        assert_eq!(completed.output, "done\n");
        assert!(!completed.output_truncated);
        assert!(!completed.output_unavailable);
        assert_eq!(completed.exit_code, Some(7));
        assert_eq!(completed.duration_ms, Some(1_250));
        assert!(completed.finished_at_ms.is_some());
        assert_eq!(completed.cwd.as_deref(), Some("/tmp/job"));
        assert!(!completed.command_truncated);
        assert!(completed.completion_observed);

        let background = &snapshot.blocks[1];
        assert_eq!(background.command, None);
        assert_eq!(background.output, "");
        assert!(!background.output_truncated);
        assert!(background.output_unavailable);
        assert_eq!(background.exit_code, None);
        assert_eq!(background.duration_ms, None);
        // Only shell-reported completions carry a finish timestamp.
        assert_eq!(background.finished_at_ms, None);
        assert_eq!(background.cwd.as_deref(), Some("/tmp/background"));
        assert!(!background.command_truncated);
        assert!(!background.start_mark_seen);
        assert_eq!(
            background.completion_provenance,
            block_mode::CompletionProvenance::BoundaryInferred
        );
        assert!(!background.completion_observed);

        let recovered = &snapshot.blocks[2];
        // ember never exports a producer-truncated command as if complete.
        assert_eq!(recovered.command, None);
        assert!(!recovered.command_exact);
        assert_eq!(recovered.output, "partial output\n");
        assert!(!recovered.output_truncated);
        assert!(!recovered.output_unavailable);
        assert_eq!(recovered.exit_code, None);
        assert_eq!(recovered.duration_ms, None);
        assert_eq!(recovered.finished_at_ms, None);
        assert_eq!(recovered.cwd.as_deref(), Some("/srv/work"));
        assert!(recovered.command_truncated);
        assert!(recovered.start_mark_seen);
        assert_eq!(
            recovered.lifecycle_health,
            block_mode::BlockLifecycleHealth::Degraded
        );
        assert!(!recovered.completion_observed);

        let json: serde_json::Value = serde_json::from_slice(
            &serialize_session(&snapshot, SessionExportFormat::Json).unwrap(),
        )
        .unwrap();
        assert_eq!(json["retention"]["lifecycle_degraded"], 1);
        assert_eq!(json["retention"]["lifecycle_recovered"], 0);
        assert_eq!(json["retention"]["lifecycle_incomplete"], 0);
    }

    #[test]
    fn snapshot_session_skips_the_live_prompt_record() {
        let mut terminal = TerminalState::new(80, 12);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07echo hi\r\n");
        terminal.process_input(b"\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07");
        // The still-editing prompt is PTY-owned and never exported.
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07draft");

        let snapshot = snapshot_session(&terminal, "pane-1").unwrap();
        assert_eq!(snapshot.blocks.len(), 1);
        assert_eq!(snapshot.blocks[0].command.as_deref(), Some("echo hi"));
    }

    #[test]
    fn session_documents_keep_order_and_shared_block_metadata() {
        let snapshot = snapshot(vec![block(7, "cargo test", "ok"), block(9, "pwd", "/tmp")]);
        let markdown =
            String::from_utf8(serialize_session(&snapshot, SessionExportFormat::Markdown).unwrap())
                .unwrap();
        assert!(markdown.starts_with(
            "# Terminal Session Export\n\nPane session: pane-42\nCaptured: 1970-01-01 00:00:00 +00:00\nTotal retained blocks: 2\n"
        ));
        assert!(markdown.find("cargo test").unwrap() < markdown.find("pwd").unwrap());
        assert!(markdown.contains("- Finished: 1970-01-01 00:00:00 +00:00"));

        let json: serde_json::Value = serde_json::from_slice(
            &serialize_session(&snapshot, SessionExportFormat::Json).unwrap(),
        )
        .unwrap();
        assert_eq!(json["schema"], SESSION_EXPORT_SCHEMA);
        assert_eq!(json["version"], SESSION_EXPORT_SCHEMA_VERSION);
        assert_eq!(json["pane_session_id"], "pane-42");
        assert_eq!(json["captured_at_ms"], 0);
        assert_eq!(json["captured_tz_offset_secs"], 0);
        assert_eq!(json["block_order"], "oldest_first");
        assert_eq!(json["retention"]["retained_blocks"], 2);
        assert_eq!(json["blocks"][0]["sequence"], 7);
        assert_eq!(json["blocks"][1]["command"], "pwd");
        assert_eq!(json["blocks"][0]["output"], "ok");
        assert_eq!(json["blocks"][0]["command_exact"], true);
        assert_eq!(json["blocks"][0]["output_unavailable"], false);
        assert_eq!(json["blocks"][0]["completion_observed"], true);
        assert_eq!(json["blocks"][0]["completion_provenance"], "shell_reported");
        assert_eq!(json["blocks"][0]["lifecycle_health"], "healthy");
    }

    #[test]
    fn exports_disclose_truncated_commands_and_unavailable_output() {
        let mut retained = block(7, "partial-command", "");
        retained.command_truncated = true;
        retained.command_exact = false;
        retained.output_unavailable = true;
        retained.completion_observed = false;
        retained.completion_provenance = block_mode::CompletionProvenance::BoundaryInferred;
        retained.lifecycle_health = block_mode::BlockLifecycleHealth::Degraded;
        let mut clipped = block(8, "large-output", "retained prefix");
        clipped.output_truncated = true;

        let snapshot = snapshot(vec![retained.clone(), clipped]);
        let markdown =
            String::from_utf8(serialize_session(&snapshot, SessionExportFormat::Markdown).unwrap())
                .unwrap();
        assert!(markdown.contains("- Note: command omitted because source text was truncated"));
        assert!(markdown.contains("- Completion: inferred at next shell boundary"));
        // The unavailable marker lives in the JSON; Markdown shows empty output.
        assert!(!markdown.contains("partial-command"));

        let json: serde_json::Value = serde_json::from_slice(
            &serialize_session(&snapshot, SessionExportFormat::Json).unwrap(),
        )
        .unwrap();
        assert_eq!(json["retention"]["command_truncated"], 1);
        assert_eq!(json["retention"]["output_truncated"], 1);
        assert_eq!(json["retention"]["output_unavailable"], 1);
        assert_eq!(json["retention"]["completion_unobserved"], 1);
        assert_eq!(json["retention"]["lifecycle_degraded"], 1);
        assert_eq!(json["blocks"][0]["command_truncated"], true);
        assert_eq!(json["blocks"][0]["output_unavailable"], true);
        assert_eq!(json["blocks"][0]["completion_observed"], false);
        assert_eq!(
            json["blocks"][0]["completion_provenance"],
            "boundary_inferred"
        );
    }

    #[test]
    fn exports_are_private_and_same_second_writes_never_replace() {
        let dir = TestDir::new("collision");
        let first = write_session_export(
            &dir.0,
            "20260807-101112",
            SessionExportFormat::Markdown,
            b"first",
        )
        .unwrap();
        let second = write_session_export(
            &dir.0,
            "20260807-101112",
            SessionExportFormat::Markdown,
            b"second",
        )
        .unwrap();

        assert_eq!(first.file_name().unwrap(), "session-20260807-101112.md");
        assert_eq!(second.file_name().unwrap(), "session-20260807-101112-1.md");
        assert_eq!(fs::read(&first).unwrap(), b"first");
        assert_eq!(fs::read(&second).unwrap(), b"second");
        assert!(fs::read_dir(&dir.0).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".tmp.")));
        assert_eq!(
            fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&dir.0).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn concurrent_same_second_exports_publish_distinct_complete_files() {
        use std::collections::HashSet;
        use std::sync::{Arc, Barrier};

        let dir = TestDir::new("concurrent");
        let barrier = Arc::new(Barrier::new(2));
        let workers: Vec<_> = [b"left".as_slice(), b"right".as_slice()]
            .into_iter()
            .map(|contents| {
                let directory = dir.0.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    write_session_export(
                        &directory,
                        "20260807-101112",
                        SessionExportFormat::Json,
                        contents,
                    )
                    .unwrap()
                })
            })
            .collect();
        let paths: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();

        assert_ne!(paths[0], paths[1]);
        let contents: HashSet<Vec<u8>> = paths.iter().map(|path| fs::read(path).unwrap()).collect();
        assert_eq!(
            contents,
            HashSet::from([b"left".to_vec(), b"right".to_vec()])
        );
    }

    #[test]
    fn export_file_names_are_family_shaped() {
        assert_eq!(
            export_file_name("20260807-101112", "json", 0),
            "session-20260807-101112.json"
        );
        assert_eq!(
            export_file_name("20260807-101112", "json", 3),
            "session-20260807-101112-3.json"
        );
    }

    #[test]
    fn an_existing_symlink_is_a_collision_and_its_target_is_untouched() {
        use std::os::unix::fs::symlink;

        let dir = TestDir::new("symlink");
        let victim = dir.0.join("victim");
        fs::create_dir_all(&dir.0).unwrap();
        fs::write(&victim, b"keep").unwrap();
        let first_name = dir.0.join("session-20260807-101112.json");
        symlink(&victim, &first_name).unwrap();

        let exported =
            write_session_export(&dir.0, "20260807-101112", SessionExportFormat::Json, b"new")
                .unwrap();

        assert_eq!(
            exported.file_name().unwrap(),
            "session-20260807-101112-1.json"
        );
        assert_eq!(fs::read(&victim).unwrap(), b"keep");
        assert!(fs::symlink_metadata(&first_name)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn export_directory_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("directory-symlink");
        fs::create_dir_all(&root.0).unwrap();
        let victim = root.0.join("victim");
        fs::create_dir(&victim).unwrap();
        let linked = root.0.join("exports");
        symlink(&victim, &linked).unwrap();

        assert!(write_session_export(
            &linked,
            "20260807-101112",
            SessionExportFormat::Markdown,
            b"private",
        )
        .is_err());
        assert_eq!(fs::read_dir(&victim).unwrap().count(), 0);
    }
}
