//! Real worker/channel handshakes around the production snapshot adoption path.
use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

const DEADLOCK_LIMIT: Duration = Duration::from_secs(5);
struct Gates {
    allow_send: mpsc::SyncSender<()>,
    sent: mpsc::Receiver<bool>,
    allow_exit: mpsc::SyncSender<()>,
}
fn version() -> BlockSearchRecordVersion {
    BlockSearchRecordVersion {
        len: 2,
        oldest_sequence: Some(1),
        newest_sequence: Some(2),
    }
}
fn pending(state: &mut BlockSearchState) -> Gates {
    let (sender, receiver) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (send_tx, send_rx) = mpsc::sync_channel(0);
    let (sent_tx, sent_rx) = mpsc::sync_channel(0);
    let (exit_tx, exit_rx) = mpsc::sync_channel(0);
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancellation = cancelled.clone();
    let worker = std::thread::spawn(move || {
        let (snapshot, sequences) = crate::block_mode::bounded_sequenced_block_search_sources(
            [
                (
                    2,
                    crate::block_mode::BlockSearchSource::new(
                        "newer".into(),
                        None,
                        Some("beta".into()),
                    ),
                ),
                (
                    1,
                    crate::block_mode::BlockSearchSource::new(
                        "older".into(),
                        None,
                        Some("alpha".into()),
                    ),
                ),
            ],
            4096,
        );
        let result =
            crate::block_mode::build_block_search_cache_cancellable(snapshot, 8192, || {
                cancellation.load(Ordering::Relaxed)
            })
            .map(|build| {
                let sequences = crate::block_mode::finalized_block_search_sequences(
                    sequences,
                    build.records.iter().map(|record| record.record_id.as_str()),
                );
                BlockSearchWindow {
                    build,
                    sequences: Arc::new(sequences),
                    unmapped: HashSet::from(["older".into(), "not-in-window".into()]),
                    unavailable: HashSet::from(["newer".into(), "not-in-window".into()]),
                    boundary_missing: false,
                    raw_records: vec![
                        BlockSearchHistoryCursor {
                            record_id: "older".into(),
                            sequence: 1,
                        },
                        BlockSearchHistoryCursor {
                            record_id: "newer".into(),
                            sequence: 99,
                        },
                    ],
                }
            });
        // Deliberately finish construction before cancellation. This models
        // the final-check/send race, not a fabricated generation-only result.
        if ready_tx.send(()).is_err() || send_rx.recv().is_err() {
            return;
        }
        let delivered = sender.send(result).is_ok();
        if sent_tx.send(delivered).is_err() {
            return;
        }
        let _ = exit_rx.recv();
    });
    state.history_pending = Some(BlockSearchPendingWindow {
        generation: state.history_generation,
        session_id: "pane-a".into(),
        version: version(),
        before: state.history_before.clone(),
        cancelled,
        receiver,
        worker,
    });
    ready_rx
        .recv_timeout(DEADLOCK_LIMIT)
        .expect("builder must reach pre-send gate");
    Gates {
        allow_send: send_tx,
        sent: sent_rx,
        allow_exit: exit_tx,
    }
}
fn send(gates: &Gates) -> bool {
    gates.allow_send.send(()).unwrap();
    gates.sent.recv_timeout(DEADLOCK_LIMIT).unwrap()
}
fn exit(state: &BlockSearchState, gates: &Gates) {
    gates.allow_exit.send(()).unwrap();
    let deadline = Instant::now() + DEADLOCK_LIMIT;
    while !state.history_pending.as_ref().unwrap().worker.is_finished() {
        assert!(
            Instant::now() < deadline,
            "worker must exit after explicit handshake"
        );
        std::thread::yield_now();
    }
}
fn state() -> BlockSearchState {
    let mut state = BlockSearchState::default();
    state.open();
    state.session_id = Some("pane-a".into());
    state.cache = vec![crate::block_mode::CachedBlockSearchRecord::new(
        "retained",
        None,
        Some("unchanged".into()),
    )];
    state
}

#[test]
fn snapshot_pending_real_channel_waits_for_exit_then_adopts_once() {
    let mut state = state();
    let gates = pending(&mut state);
    assert!(state.history_pending.as_ref().unwrap().is_current(
        state.history_generation,
        "pane-a",
        version(),
        &None
    ));
    assert_eq!(
        state.poll_history_window("pane-a", version()),
        BlockSearchWindowPoll::Waiting
    );
    assert!(send(&gates));
    // Message is queued, but the real worker is still held at the exit gate.
    assert_eq!(
        state.poll_history_window("pane-a", version()),
        BlockSearchWindowPoll::Waiting
    );
    assert_eq!(state.cache[0].record_id, "retained");
    exit(&state, &gates);
    assert_eq!(
        state.poll_history_window("pane-a", version()),
        BlockSearchWindowPoll::Adopted
    );
    assert!(state.history_pending.is_none());
    assert_eq!(state.history_range, Some((1, 2)));
    assert_eq!(state.record_sequences.len(), 2);
    assert_eq!(
        state.unmapped_output_records,
        HashSet::from(["older".into()])
    );
    assert_eq!(state.history_unavailable, 1);
    assert_eq!(
        state.raw.records,
        vec![BlockSearchHistoryCursor {
            record_id: "older".into(),
            sequence: 1
        }]
    );
    assert_eq!(
        state.poll_history_window("pane-a", version()),
        BlockSearchWindowPoll::Vacant
    );
}

#[test]
fn snapshot_pending_query_edit_reuses_source_but_scans_only_latest_query() {
    let mut state = state();
    state.query = "alpha".into();
    let gates = pending(&mut state);
    assert!(send(&gates));
    state.query = "beta".into();
    assert_eq!(
        state.poll_history_window("pane-a", version()),
        BlockSearchWindowPoll::Waiting
    );
    assert!(!state
        .history_pending
        .as_ref()
        .unwrap()
        .cancelled
        .load(Ordering::Relaxed));
    exit(&state, &gates);
    assert_eq!(
        state.poll_history_window("pane-a", version()),
        BlockSearchWindowPoll::Adopted
    );
    let results = crate::block_mode::search_blocks(state.cache.iter().rev(), &state.query);
    state.adopt_hits(results.hits, results.capped, "pane-a", None);
    assert_eq!(state.hits.len(), 1);
    assert_eq!(state.hits[0].record_id, "newer");
    assert_eq!(state.hits[0].line_text, "beta");
}

#[test]
fn snapshot_pending_pane_reset_version_cursor_generation_reject_late_delivery() {
    for after_send in [false, true] {
        for mutation in [
            "pane",
            "close-open",
            "version-reset",
            "cursor",
            "manual-refresh",
        ] {
            let mut state = state();
            let gates = pending(&mut state);
            if after_send {
                assert!(send(&gates));
            }
            let mut session = "pane-a";
            let mut current_version = version();
            match mutation {
                "pane" => session = "pane-b",
                "close-open" => {
                    state.close();
                    state.open();
                }
                "version-reset" => current_version = BlockSearchRecordVersion::default(),
                "cursor" => {
                    state.history_before = Some(BlockSearchHistoryCursor {
                        record_id: "older".into(),
                        sequence: 1,
                    })
                }
                "manual-refresh" => assert!(state.request_manual_refresh()),
                _ => unreachable!(),
            }
            assert_eq!(
                state.poll_history_window(session, current_version),
                BlockSearchWindowPoll::Waiting,
                "{mutation}/{after_send}"
            );
            let current_pending = state.history_pending.as_ref().unwrap();
            assert!(current_pending.cancelled.load(Ordering::Relaxed));
            assert!(!current_pending.is_current(
                state.history_generation,
                session,
                current_version,
                &state.history_before
            ));
            // Cancellation cannot release this actual live worker's admission.
            assert!(!current_pending.worker.is_finished());
            if !after_send {
                let _ = send(&gates);
            }
            exit(&state, &gates);
            assert_eq!(
                state.poll_history_window(session, current_version),
                BlockSearchWindowPoll::Vacant,
                "{mutation}/{after_send}"
            );
            assert!(state.history_pending.is_none());
            assert!(state
                .cache
                .iter()
                .all(|record| record.record_id == "retained"));
            assert!(state.record_sequences.is_empty());
            assert!(state.hits.is_empty());
            // A real fresh replacement can be accepted after the stale worker exits.
            state.session_id = Some("pane-a".into());
            state.history_before = None;
            let fresh = pending(&mut state);
            assert!(send(&fresh));
            exit(&state, &fresh);
            assert_eq!(
                state.poll_history_window("pane-a", version()),
                BlockSearchWindowPoll::Adopted
            );
        }
    }
}
