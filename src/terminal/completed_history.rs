use super::{BufferAnchor, CommandRecord, CommandState, ExtractedText, TerminalState};
use jterm_core::completed_history::{
    Card, Checkpoint, Output, MAX_RECORDS, MAX_RETAINED_TEXT_BYTES,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(super) const ARCHIVED_COMMAND_ID_PREFIX: &str = "archive:";

fn epoch_millis(time: Option<SystemTime>) -> Option<u64> {
    u64::try_from(time?.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()
}

fn epoch_time(millis: Option<u64>) -> Option<SystemTime> {
    UNIX_EPOCH.checked_add(Duration::from_millis(millis?))
}

impl TerminalState {
    pub(super) fn mark_completed_history_changed(&mut self) {
        self.completed_history_revision = self.completed_history_revision.saturating_add(1);
    }

    pub fn completed_history_revision(&self) -> u64 {
        self.completed_history_revision
    }

    pub fn completed_history_checkpoint(&self, pane: &str) -> std::io::Result<Checkpoint> {
        let mut remaining = MAX_RETAINED_TEXT_BYTES;
        let mut cards = Vec::new();
        for record in self
            .command_records
            .iter()
            .rev()
            .filter(|record| record.complete)
        {
            let Some(source) = record.output_source.clone() else {
                continue;
            };
            let cwd = |path: &Option<String>| {
                path.as_ref()
                    .filter(|path| jterm_core::execution_journal::is_valid_jsh_cwd(path))
                    .cloned()
            };
            let mut card = Card {
                sequence: record.sequence,
                shell_id: record.observed_execution_id().to_owned(),
                shell_session_id: record.session_id.clone(),
                shell_sequence: record.seq,
                shell_started_at_ms: record.started_at_ms,
                command: record.command.clone(),
                command_exact: record.command_exact,
                command_truncated: record.command_truncated,
                cwd: cwd(&record.cwd),
                cwd_after: cwd(&record.cwd_after),
                exit_code: record.exit_code,
                duration_ms: record.duration_ms,
                started_at_ms: epoch_millis(record.started_at),
                finished_at_ms: epoch_millis(record.finished_at),
                start_mark_seen: record.start_mark_seen,
                completion: record.completion_provenance.into(),
                source,
                output: None,
            };
            let Some(text_budget) = remaining.checked_sub(card.retained_text_bytes()) else {
                break;
            };
            remaining = text_budget;
            if let Some(output) = record
                .captured_output
                .as_ref()
                .filter(|output| output.text.len() <= remaining)
            {
                remaining -= output.text.len();
                card.output = Some(Output {
                    text: output.text.clone(),
                    truncated: output.truncated,
                    total_bytes: output.total_bytes as u64,
                });
            }
            cards.push(card);
            if cards.len() == MAX_RECORDS {
                break;
            }
        }
        cards.reverse();
        Checkpoint::new(pane.to_owned(), self.next_command_sequence, cards)
    }

    /// Startup-only. Import the immutable cards before reading the fresh PTY,
    /// without reinstating raw rows, pending OSC events, or old input routes.
    pub fn restore_completed_history(&mut self, checkpoint: Checkpoint) -> std::io::Result<()> {
        if self.next_command_sequence != 1 || !self.command_records.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "completed history can only be restored into a fresh terminal",
            ));
        }
        let (next_sequence, cards) = checkpoint.into_records();
        self.next_command_sequence = next_sequence;
        for card in cards {
            let captured_output = card.output.map(|output| ExtractedText {
                text: output.text,
                truncated: output.truncated,
                total_bytes: usize::try_from(output.total_bytes).unwrap_or(usize::MAX),
            });
            self.captured_command_output_bytes += captured_output
                .as_ref()
                .map_or(0, |output| output.text.len());
            let output_preview = captured_output.as_ref().map_or_else(String::new, |output| {
                crate::fold_preview::from_plain(&output.text)
            });
            self.command_records.push_back(CommandRecord {
                id: format!("{ARCHIVED_COMMAND_ID_PREFIX}{}", card.source.execution_id()),
                archived_shell_id: Some(card.shell_id),
                sequence: card.sequence,
                command: card.command,
                command_exact: card.command_exact,
                command_truncated: card.command_truncated,
                cwd: card.cwd,
                cwd_after: card.cwd_after,
                session_id: card.shell_session_id,
                seq: card.shell_sequence,
                started_at_ms: card.shell_started_at_ms,
                prompt_start: BufferAnchor {
                    line_id: u64::MAX,
                    column: 0,
                },
                command_start: None,
                output_start: None,
                output_end: None,
                end: None,
                exit_code: card.exit_code,
                duration_ms: card.duration_ms,
                state: CommandState::Complete,
                complete: true,
                start_mark_seen: card.start_mark_seen,
                completion_provenance: card.completion.into(),
                started_at: epoch_time(card.started_at_ms),
                finished_at: epoch_time(card.finished_at_ms),
                agent_generation: None,
                captured_output,
                output_source: Some(card.source),
                running_output_source: None,
                output_preview,
                started_instant: None,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_keeps_exact_source_and_accepts_a_same_named_live_completion() {
        const EXECUTION: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07source proof\r\n\x1b]133;C;id=proof;cmdline_url=source%20proof\x07visible\r\n\x1b]133;D;0;id=proof\x07";
        let mut original = TerminalState::new(80, 24);
        original.process_input(EXECUTION);
        let source = original
            .command_record("proof")
            .unwrap()
            .output_source
            .clone()
            .unwrap();
        let checkpoint = original.completed_history_checkpoint("pane").unwrap();

        let mut restored = TerminalState::new(80, 24);
        restored.restore_completed_history(checkpoint).unwrap();
        assert!(restored.take_completed_command_events().is_empty());
        let old = restored.command_records().front().unwrap();
        assert!(old.is_archived());
        assert_eq!(old.observed_execution_id(), "proof");
        assert_eq!(old.output_source.as_ref(), Some(&source));
        assert!(restored
            .buffer_anchor_to_absolute(old.prompt_start)
            .is_none());
        let lookup = old.id.clone();
        assert_eq!(
            restored
                .command_output_text(&lookup, 128)
                .unwrap()
                .text
                .trim_end(),
            "visible"
        );

        restored.process_input(EXECUTION);
        let live = restored.command_record("proof").unwrap();
        assert!(!live.is_archived());
        assert_ne!(
            live.output_source.as_ref().unwrap().execution_id(),
            source.execution_id()
        );
        assert_eq!(
            restored
                .command_record(&lookup)
                .unwrap()
                .output_source
                .as_ref(),
            Some(&source)
        );
        assert_eq!(restored.take_completed_command_events().len(), 1);
    }
}
