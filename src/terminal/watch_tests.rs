use super::*;
use crate::output_batch::{OutputBuffer, OutputSource};
use crate::terminal::TerminalState;
use std::time::Instant;

fn fixture() -> (TerminalState, OutputSource) {
    let mut terminal = TerminalState::new(80, 24);
    let source = OutputSource::default();
    terminal.bind_watch_source(source.epoch());
    (terminal, source)
}
fn feed(terminal: &mut TerminalState, source: &mut OutputSource, bytes: &[u8]) -> Vec<WatchParsed> {
    let data = source.batch(bytes.to_vec(), Instant::now());
    let mut updates = Vec::new();
    terminal.process_output_buffer(&data, |update| updates.push(update));
    updates
}
#[test]
fn source_batch_identity_survives_multiple_frame_slices() {
    let (mut terminal, mut source) = fixture();
    feed(&mut terminal, &mut source, b"\x1b]133;C\x07");
    let mut first = source.batch(b"abcdef".to_vec(), Instant::now());
    let mut tail = first.split_off(2);
    let last = tail.split_off(2);
    let mut updates = Vec::new();
    for piece in [first, tail, last] {
        terminal.process_output_buffer(&piece, |update| updates.push(update));
    }
    assert!(updates[0].batch.is_none());
    assert!(updates[1].batch.is_none());
    assert!(updates[2]
        .batch
        .is_some_and(|b| b.origin.sequence == 2 && !b.quarantined));
}
#[test]
fn merged_source_batches_are_not_a_single_parser_frame_batch() {
    let (mut terminal, mut source) = fixture();
    feed(&mut terminal, &mut source, b"\x1b]133;C\x07");
    let mut data = OutputBuffer::default();
    for _ in 0..3 {
        data.extend(source.batch(b"x".to_vec(), Instant::now()));
    }
    let mut updates = Vec::new();
    terminal.process_output_buffer(&data, |u| updates.push(u));
    assert_eq!(updates.len(), 3);
    assert_eq!(
        updates
            .iter()
            .map(|u| u.batch.unwrap().origin.sequence)
            .collect::<Vec<_>>(),
        [2, 3, 4]
    );
}
#[test]
fn accepted_boundaries_retire_before_source_tail_and_mixed_commands_quarantine() {
    let (mut terminal, mut source) = fixture();
    feed(&mut terminal, &mut source, b"\x1b]133;C\x07");
    let mut data = source.batch(b"old\x1b]133;D;0\x07new-tail".to_vec(), Instant::now());
    let tail = data.split_off(13);
    let mut updates = Vec::new();
    terminal.process_output_buffer(&data, |u| updates.push(u));
    assert!(updates[0].invalidated);
    assert!(updates[0].command.is_none());
    assert!(updates[0].batch.is_none());
    terminal.process_output_buffer(&tail, |u| updates.push(u));
    assert!(updates[1].batch.unwrap().quarantined);
    let updates = feed(&mut terminal, &mut source, b"\x1b]133;A\x07\x1b]133;C\x07one\x1b]133;D;0\x07\x1b]133;A\x07\x1b]133;C\x07two\x1b]133;D;0\x07\x1b]133;A\x07\x1b]133;C\x07three");
    assert!(updates[0].trusted_start);
    assert!(updates[0].command.is_some());
    assert!(updates[0].batch.unwrap().quarantined);
}
#[test]
fn fragmented_marker_same_source_is_trusted_cross_source_is_quarantined() {
    let (mut terminal, mut source) = fixture();
    let mut data = source.batch(b"\x1b]133;C\x07".to_vec(), Instant::now());
    let tail = data.split_off(4);
    terminal.process_output_buffer(&data, |_| {});
    let mut updates = Vec::new();
    terminal.process_output_buffer(&tail, |u| updates.push(u));
    assert!(updates[0].trusted_start);
    feed(
        &mut terminal,
        &mut source,
        b"\x1b]133;D;0\x07\x1b]133;A\x07",
    );
    feed(&mut terminal, &mut source, b"\x1b]133;");
    let updates = feed(&mut terminal, &mut source, b"C\x07");
    assert!(!updates[0].trusted_start);
    assert!(updates[0].batch.unwrap().quarantined);
}
#[test]
fn duplicate_c_and_rejected_d_do_not_mint_lifecycle_facts() {
    let (mut terminal, mut source) = fixture();
    feed(&mut terminal, &mut source, b"\x1b]133;C;id=first\x07");
    let command = terminal.watch_command();
    let updates = feed(
        &mut terminal,
        &mut source,
        b"\x1b]133;C;id=first\x07\x1b]133;D;0;id=second\x07",
    );
    assert_eq!(terminal.watch_command(), command);
    assert!(!updates[0].trusted_start);
    assert!(!updates[0].invalidated);
    assert!(!updates[0].batch.unwrap().quarantined);
}
#[test]
fn prompt_inferred_finish_ris_and_alt_boundaries_are_native_facts() {
    let (mut terminal, mut source) = fixture();
    feed(&mut terminal, &mut source, b"\x1b]133;C\x07");
    let old = terminal.watch_command().unwrap();
    assert!(feed(&mut terminal, &mut source, b"\x1b[?1049h")[0]
        .command
        .is_none());
    let restored = feed(&mut terminal, &mut source, b"\x1b[?1049l")[0];
    assert!(restored.invalidated);
    assert_eq!(restored.command.unwrap().generation, old.generation);
    assert_ne!(restored.command.unwrap().revision, old.revision);
    assert!(!restored.trusted_start);
    assert!(feed(&mut terminal, &mut source, b"\x1b]133;A\x07")[0]
        .command
        .is_none());
    feed(&mut terminal, &mut source, b"\x1b]133;C\x07");
    assert!(feed(&mut terminal, &mut source, b"\x1bc")[0]
        .command
        .is_none());
    let after = feed(&mut terminal, &mut source, b"\x1b]133;C\x07")[0]
        .command
        .unwrap();
    assert!(after.generation > old.generation);
    assert_eq!(after.source, old.source);
}
#[test]
fn background_then_attach_has_facts_but_never_replays_a_trusted_start() {
    let (mut terminal, mut source) = fixture();
    let data = source.batch(b"\x1b]133;C\x07".to_vec(), Instant::now());
    terminal.process_output_buffer(&data, |_| {});
    let command = terminal.watch_command().unwrap();
    let mut updates = Vec::new();
    terminal.process_output_buffer(&data, |u| updates.push(u));
    assert!(updates.is_empty());
    assert_eq!(terminal.watch_command(), Some(command));
}
#[test]
fn old_source_and_retimestamped_duplicate_cannot_touch_current_watch() {
    let (mut terminal, mut source) = fixture();
    feed(&mut terminal, &mut source, b"\x1b]133;C\x07");
    let old = source.batch(b"\x1bc".to_vec(), Instant::now());
    let mut replacement = OutputSource::default();
    terminal.bind_watch_source(replacement.epoch());
    feed(
        &mut terminal,
        &mut replacement,
        b"\x1b]133;A\x07\x1b]133;C\x07",
    );
    let command = terminal.watch_command();
    let mut updates = Vec::new();
    terminal.process_output_buffer(&old, |u| updates.push(u));
    assert!(updates.is_empty());
    // Bytes retain ordinary terminal semantics, but cannot directly inject a
    // Watch event. The next real eligibility check sees that RIS ended B.
    assert!(command.is_some());
    assert!(terminal.watch_command().is_none());
}
#[test]
fn protocol_park_and_resume_cannot_make_a_fresh_activity_batch() {
    let (mut terminal, mut source) = fixture();
    feed(&mut terminal, &mut source, b"\x1b]133;C\x07");
    let mut data = source.batch(b"ordinary".to_vec(), Instant::now());
    let mut tail = data.split_off(3);
    data.append(&mut tail);
    data.quarantine();
    let mut updates = Vec::new();
    terminal.process_output_buffer(&data, |u| updates.push(u));
    assert!(updates[0].batch.unwrap().quarantined);
    assert!(feed(&mut terminal, &mut source, b"\x1b[2K\r")[0]
        .batch
        .is_some_and(|b| !b.quarantined));
}
#[test]
fn command_generation_exhaustion_disables_only_watch() {
    let (mut terminal, mut source) = fixture();
    terminal.watch.next_generation = None;
    feed(&mut terminal, &mut source, b"\x1b]133;C\x07hello");
    assert!(terminal.watch_command().is_none());
    assert!(terminal.command_records().back().unwrap().start_mark_seen);
}
