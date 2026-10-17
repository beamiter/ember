# E03 snapshot pending-window adoption regressions

Base: 4ac9459994502951f665f5c60da29627990e94df. This slice adds deterministic
coverage of snapshot result delivery and adoption. It does not change the pixel
gate, observer pilot, source/cache budgets, raw-worker allocation accounting,
full RSS acceptance or fixed-machine p95 target. No new production correctness
defect was demonstrated; the production change extracts existing logic to make
the actual adoption path directly testable without a GUI.

## Production path preservation

The session check, `BlockSearchPendingWindow::is_current`, cancellation flag,
JoinHandle exit check, channel receive, metadata filtering and all cache/range/
version assignments move from `rebuild_block_search_cache` to
`BlockSearchState::poll_history_window` in their original order. Return values
become Waiting/Adopted/Vacant, so the app still waits, returns success, or falls
through to new source capture exactly as before. The preceding raw-worker
single-flight check and following terminal try_lock/version validation remain
in the app. No query/refresh flags are newly changed.

The extraction receipt compares both bodies after removing whitespace and
substituting only self.block_search -> self and the two return-enum names. Their
normalized SHA256 is identical:
78962858eaff88afdc86cf8dca71d02a11ded3c11bca2beafcd89a501a382c32.

## Real worker/channel cases

`src/block_search/pending_tests.rs` builds real bounded source snapshots and
normalizes them with the production cancellable builder in a real thread. A
zero-capacity handshake proves construction reached the pre-send point. A second
handshake proves the result was enqueued while the thread is deliberately still
alive. Only an explicit exit handshake allows the thread to terminate. A five-
second deadline catches deadlocks; no sleep or elapsed-time guess selects the
race ordering. If assertions unwind, dropped senders release blocked workers.

Three named tests execute:

- snapshot_pending_real_channel_waits_for_exit_then_adopts_once: polls before
  send, then with a queued message and a live thread, then after actual exit;
  checks cache, range, sequence map, raw ID+sequence and metadata, and one-time
  adoption.
- snapshot_pending_query_edit_reuses_source_but_scans_only_latest_query:
  source caches are query-independent, so editing alpha to beta intentionally
  preserves the worker; the production search_blocks scan and adopt_hits select
  only beta after adoption. This does not introduce unnecessary query cancellation.
- snapshot_pending_pane_reset_version_cursor_generation_reject_late_delivery:
  pane switch, close/reopen reset, source version reset, history cursor change,
  and explicit refresh generation change, each before and after channel enqueue.
  Every case calls the production is_current and poll path, retains admission
  while the old worker lives, rejects its result, and accepts a genuinely fresh
  replacement only after the stale thread exits.

These are production state/cache/worker-channel tests, not compositor or UI
navigation acceptance. Existing raw-worker match/exit-barrier tests are not
reimplemented. Full cargo execution is required on exact-head hosted CI; the
local offline dependency cache lacks the pinned jterm_core Git revision, so
format/static equivalence checks alone must not be reported as test passes.
