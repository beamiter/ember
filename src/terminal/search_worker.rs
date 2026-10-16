//! Search-only bounded worker. This module has no UI toolkit dependency.
//!
//! A controller must keep one shared [`SearchWorkerPermitPool`] across picker
//! close/reopen and session changes. Cancellation invalidates delivery, but
//! never releases admission: only `JoinHandle::is_finished` does that. Dropping
//! a worker is nonblocking. Requests coalesce into one latest slot; progress and
//! results replace one slot. Retain the generation and validate the final seal
//! under the terminal lock before adopting/navigating any result.
//!
//! A request searches one sealed completed output within a finite work window.
//! `Complete` is the only exhaustive result; hit/work/time limits are explicit
//! partial outcomes. Work/time limits expose a move-only bounded continuation;
//! only an explicit resume advances another window, preserving literal carry.
use super::search_chunk::{
    RawLiteralHit, RawLiteralOverlap, RawSearchCursor, RawSearchSeal, RawSearchSpan,
    SearchChunkBudget, SearchChunkOutcome, SearchChunkWork, RAW_LITERAL_MAX_HITS,
    RAW_LITERAL_QUERY_MAX_BYTES,
};
use super::{TerminalState, MAX_OSC_133_ID_BYTES};
use parking_lot::{Condvar, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const MAX_SEARCH_WORKERS: usize = 1;
pub const MAX_SEARCH_WINDOW_CHUNKS: usize = 1024;
pub const MAX_SEARCH_WINDOW_CELLS: usize = 1_048_576;
pub const MAX_SEARCH_WINDOW_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SEARCH_WINDOW_ENCODED_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_SEARCH_WINDOW_DURATION: Duration = Duration::from_secs(2);
const MAX_RETRY_BACKOFF: Duration = Duration::from_millis(16);
// Global uniqueness prevents a taken old result from matching generation 1
// after close/reopen on the same still-valid source. Zero is never allocated.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
fn next_generation(counter: &AtomicU64) -> Result<u64, SearchSubmitError> {
    counter
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| SearchSubmitError::GenerationExhausted)
}

/// Retain this pool across controller/session replacements. Slots own handles,
/// so even an already-cancelled or dropped worker occupies its permit until the
/// actual thread has exited. No UI-facing operation joins a thread.
pub struct SearchWorkerPermitPool {
    limit: usize,
    threads: Mutex<Vec<JoinHandle<()>>>,
}
impl SearchWorkerPermitPool {
    pub fn new(limit: usize) -> Self {
        Self {
            limit: limit.clamp(1, MAX_SEARCH_WORKERS),
            threads: Mutex::new(Vec::with_capacity(limit.clamp(1, MAX_SEARCH_WORKERS))),
        }
    }
    pub fn active_workers(&self) -> usize {
        let mut threads = self.threads.lock();
        threads.retain(|thread| !thread.is_finished());
        threads.len()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SearchWorkerLimits {
    pub chunk: SearchChunkBudget,
    pub max_hits: usize,
    pub max_chunks: usize,
    pub max_cells: usize,
    pub max_bytes: usize,
    pub max_encoded_bytes: usize,
    pub max_duration: Duration,
}
impl Default for SearchWorkerLimits {
    fn default() -> Self {
        Self {
            chunk: SearchChunkBudget {
                rows: 8,
                cells: 8192,
                bytes: 8192,
                encoded_bytes: 65_536,
            },
            max_hits: RAW_LITERAL_MAX_HITS,
            max_chunks: 128,
            max_cells: 524_288,
            max_bytes: 2 * 1024 * 1024,
            max_encoded_bytes: 4 * 1024 * 1024,
            max_duration: Duration::from_millis(500),
        }
    }
}
impl SearchWorkerLimits {
    /// Upper bounds apply to every caller. Never silently increase a caller's
    /// budget; admission rejects values below the atomic minimum.
    pub fn bounded(self) -> Self {
        Self {
            chunk: SearchChunkBudget {
                rows: self.chunk.rows.min(32),
                cells: self.chunk.cells.min(16_384),
                bytes: self.chunk.bytes.min(16_384),
                encoded_bytes: self.chunk.encoded_bytes.min(65_536),
            },
            max_hits: self.max_hits.min(RAW_LITERAL_MAX_HITS),
            max_chunks: self.max_chunks.min(MAX_SEARCH_WINDOW_CHUNKS),
            max_cells: self.max_cells.min(MAX_SEARCH_WINDOW_CELLS),
            max_bytes: self.max_bytes.min(MAX_SEARCH_WINDOW_BYTES),
            max_encoded_bytes: self.max_encoded_bytes.min(MAX_SEARCH_WINDOW_ENCODED_BYTES),
            max_duration: self.max_duration.min(MAX_SEARCH_WINDOW_DURATION),
        }
    }

    /// Conservative payload accounting, not allocator/RSS or native performance
    /// acceptance. Includes raw text+provenance, Unicode normalization+provenance,
    /// carry+needle, one transient 512-capacity matcher-hit vector, one worker
    /// aggregate, one result slot and the continuation's duplicate hit storage. Query/record/cursor/lock/thread metadata,
    /// allocator headers, terminal's existing scrollback and bounded single-row
    /// decoder scratch are separate. No term scales with retained history.
    pub fn payload_accounting(self) -> SearchWorkerPayloadAccounting {
        let limits = self.bounded();
        let span = std::mem::size_of::<RawSearchSpan>();
        let hit = std::mem::size_of::<RawLiteralHit>();
        let raw_spans = (limits.chunk.cells + limits.chunk.rows).min(limits.chunk.bytes);
        // Unicode lowercase yields <= 3 scalars, each <= 4 UTF-8 bytes.
        let carry = 3 * RAW_LITERAL_QUERY_MAX_BYTES;
        SearchWorkerPayloadAccounting {
            raw_chunk_bytes: limits.chunk.bytes + raw_spans * span,
            normalized_chunk_bytes: 12 * limits.chunk.bytes + 3 * limits.chunk.bytes * span,
            // Normalization temporarily includes old carry; retain new carry
            // and the needle, plus a pending continuation's query/carry.
            // Scalar provenance <= byte count.
            carry_and_query_bytes: 5 * carry + 3 * carry * span + 2 * RAW_LITERAL_QUERY_MAX_BYTES,
            hit_storage_bytes: (512 + 3 * RAW_LITERAL_MAX_HITS) * hit,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchWorkerPayloadAccounting {
    pub raw_chunk_bytes: usize,
    pub normalized_chunk_bytes: usize,
    pub carry_and_query_bytes: usize,
    pub hit_storage_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SearchWorkerOptions {
    pub case_sensitive: bool,
    pub regex: bool,
    pub whole_word: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub enum SearchWorkerAdmission {
    InvalidBudget,
    Busy,
    SpawnFailed,
}
#[derive(Debug, PartialEq, Eq)]
pub enum SearchSubmitError {
    Closed,
    GenerationExhausted,
    InvalidRecord,
    DifferentSource,
    Unsupported(&'static str),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchFinishStatus {
    Complete,
    HitLimit,
    WorkLimit,
    TimeLimit,
    Stale,
    Unavailable,
    SessionClosed,
    BudgetTooSmall,
    Unsupported(&'static str),
}
#[derive(Clone, Debug)]
pub struct SearchProgress {
    pub generation: u64,
    pub chunks: usize,
    pub work: SearchChunkWork,
    pub hits: usize,
    pub lock_misses: usize,
    pub lock_wait: Duration,
    pub capture_time: Duration,
    pub match_time: Duration,
    /// Submission to result construction, including contention/queue delay.
    /// External ready latency also includes single-slot publication/observation.
    pub elapsed: Duration,
}
#[derive(Debug)]
pub struct SearchFinished {
    pub generation: u64,
    pub record_sequence: u64,
    pub record_id: String,
    pub seal: Option<RawSearchSeal>,
    pub status: SearchFinishStatus,
    pub hits: Vec<RawLiteralHit>,
    pub progress: SearchProgress,
    pub continuation: Option<SearchContinuation>,
}
impl SearchFinished {
    /// Caller must also ask `worker.is_current(generation)` immediately before
    /// adoption. This validates source retention/mutation, not query generation.
    pub fn validate_source(&self, terminal: &TerminalState) -> Result<(), SearchChunkOutcome> {
        let Some(seal) = self.seal.as_ref() else {
            return Err(SearchChunkOutcome::Unavailable);
        };
        terminal.validate_search_seal(seal)
    }
}
#[derive(Debug)]
pub enum SearchUpdate {
    Progress(SearchProgress),
    Finished(SearchFinished),
}
impl SearchUpdate {
    pub fn generation(&self) -> u64 {
        match self {
            Self::Progress(progress) => progress.generation,
            Self::Finished(result) => result.generation,
        }
    }
}
/// Move-only continuation. Private query/record/options and matcher state cannot
/// be mixed with another request. Includes capped cumulative hits so subsequent
/// windows preserve the same global cap and overlap/nonoverlap semantics.
pub struct SearchContinuation {
    source: Weak<Mutex<TerminalState>>,
    sequence: u64,
    record_id: String,
    query: String,
    options: SearchWorkerOptions,
    resume: ResumeState,
}
impl std::fmt::Debug for SearchContinuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchContinuation")
            .field("record_sequence", &self.sequence)
            .field("record_id", &self.record_id)
            .field("retained_hits", &self.resume.hits.len())
            .finish_non_exhaustive()
    }
}
struct ResumeState {
    seal: Option<RawSearchSeal>,
    cursor: Option<RawSearchCursor>,
    matcher: RawLiteralOverlap,
    hits: Vec<RawLiteralHit>,
}
struct Request {
    generation: u64,
    sequence: u64,
    record_id: String,
    query: String,
    options: SearchWorkerOptions,
    submitted: Instant,
    resume: Option<ResumeState>,
}
struct SharedState {
    generation: u64,
    closed: bool,
    pending: Option<Request>,
    update: Option<SearchUpdate>,
}
struct Shared {
    state: Mutex<SharedState>,
    wake: Condvar,
    #[cfg(test)]
    hooks: TestHooks,
}
/// Owns no strong reference to the session. The thread upgrades the weak
/// reference only during a nonblocking terminal-lock attempt and bounded raw
/// capture, then drops it before matching, progress delivery, parking or sleep.
pub struct SearchWorker {
    terminal: Weak<Mutex<TerminalState>>,
    shared: Arc<Shared>,
    // Keeps admission coordination alive even if a controller drops its copy.
    _pool: Arc<SearchWorkerPermitPool>,
}
impl SearchWorker {
    pub fn new(
        terminal: Weak<Mutex<TerminalState>>,
        pool: Arc<SearchWorkerPermitPool>,
        limits: SearchWorkerLimits,
    ) -> Result<Self, SearchWorkerAdmission> {
        Self::new_inner(
            terminal,
            pool,
            limits,
            #[cfg(test)]
            TestHooks::default(),
        )
    }
    fn new_inner(
        terminal: Weak<Mutex<TerminalState>>,
        pool: Arc<SearchWorkerPermitPool>,
        limits: SearchWorkerLimits,
        #[cfg(test)] hooks: TestHooks,
    ) -> Result<Self, SearchWorkerAdmission> {
        if limits.chunk.rows == 0
            || limits.chunk.cells == 0
            || limits.chunk.bytes < 4
            || limits.chunk.encoded_bytes == 0
            || limits.max_hits == 0
            || limits.max_chunks == 0
            || limits.max_cells == 0
            || limits.max_bytes < 4
            || limits.max_encoded_bytes == 0
            || limits.max_duration < Duration::from_millis(1)
        {
            return Err(SearchWorkerAdmission::InvalidBudget);
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(SharedState {
                generation: 0,
                closed: false,
                pending: None,
                update: None,
            }),
            wake: Condvar::new(),
            #[cfg(test)]
            hooks,
        });
        {
            let mut threads = pool.threads.lock();
            threads.retain(|thread| !thread.is_finished());
            if threads.len() >= pool.limit {
                return Err(SearchWorkerAdmission::Busy);
            }
            let worker_shared = Arc::clone(&shared);
            let worker_terminal = terminal.clone();
            let thread = std::thread::Builder::new()
                .name("ember-raw-search".into())
                .spawn(move || {
                    run_worker(worker_terminal, worker_shared.clone(), limits.bounded());
                    #[cfg(test)]
                    if let Some(gate) = &worker_shared.hooks.before_exit {
                        gate.enter();
                    }
                })
                .map_err(|_| SearchWorkerAdmission::SpawnFailed)?;
            threads.push(thread);
        }
        Ok(Self {
            terminal,
            shared,
            _pool: pool,
        })
    }
    /// The latest valid request replaces pending work and invalidates old
    /// progress/results. Invalid options leave the current request unchanged.
    pub fn submit(
        &self,
        record_sequence: u64,
        record_id: &str,
        query: &str,
        options: SearchWorkerOptions,
    ) -> Result<u64, SearchSubmitError> {
        if record_id.is_empty() || record_id.len() > MAX_OSC_133_ID_BYTES {
            return Err(SearchSubmitError::InvalidRecord);
        }
        if options.regex || options.whole_word {
            return Err(SearchSubmitError::Unsupported(
                "only literal substring search is supported",
            ));
        }
        // Bounded validation/normalization outside the terminal lock. Reject
        // oversized inputs before cloning anything into the pending slot.
        RawLiteralOverlap::new(query, options.case_sensitive)
            .map_err(SearchSubmitError::Unsupported)?;
        let mut state = self.shared.state.lock();
        if state.closed {
            return Err(SearchSubmitError::Closed);
        }
        let generation = next_generation(&NEXT_GENERATION)?;
        state.generation = generation;
        state.pending = Some(Request {
            generation,
            sequence: record_sequence,
            record_id: record_id.to_owned(),
            query: query.to_owned(),
            options,
            submitted: Instant::now(),
            resume: None,
        });
        state.update = None;
        self.shared.wake.notify_all();
        Ok(generation)
    }
    /// Resume exactly the opaque record/query/options in this token. Tokens
    /// from a different terminal are rejected by seal validation at capture.
    /// Query changes must use `submit`, which starts a new matcher.
    pub fn resume(&self, continuation: SearchContinuation) -> Result<u64, SearchSubmitError> {
        if !Weak::ptr_eq(&self.terminal, &continuation.source) {
            return Err(SearchSubmitError::DifferentSource);
        }
        let mut state = self.shared.state.lock();
        if state.closed {
            return Err(SearchSubmitError::Closed);
        }
        let generation = next_generation(&NEXT_GENERATION)?;
        state.generation = generation;
        state.pending = Some(Request {
            generation,
            sequence: continuation.sequence,
            record_id: continuation.record_id,
            query: continuation.query,
            options: continuation.options,
            submitted: Instant::now(),
            resume: Some(continuation.resume),
        });
        state.update = None;
        self.shared.wake.notify_all();
        Ok(generation)
    }
    pub fn cancel(&self) {
        let mut state = self.shared.state.lock();
        // Zero invalidates all allocated generations without consuming an ID.
        state.generation = 0;
        state.pending = None;
        state.update = None;
        self.shared.wake.notify_all();
    }
    /// Nonblocking: wake the worker and retain its pool slot until actual exit.
    pub fn close(&self) {
        let mut state = self.shared.state.lock();
        state.closed = true;
        state.pending = None;
        state.update = None;
        self.shared.wake.notify_all();
    }
    pub fn is_current(&self, generation: u64) -> bool {
        let state = self.shared.state.lock();
        generation != 0 && !state.closed && state.generation == generation
    }
    pub fn take_update(&self) -> Option<SearchUpdate> {
        let mut state = self.shared.state.lock();
        if state.closed {
            return None;
        }
        state
            .update
            .take()
            .filter(|update| update.generation() == state.generation)
    }
    /// Capacity invariant: each component is always 0 or 1.
    pub fn buffered_slots(&self) -> (usize, usize) {
        let state = self.shared.state.lock();
        (
            usize::from(state.pending.is_some()),
            usize::from(state.update.is_some()),
        )
    }
}
impl Drop for SearchWorker {
    fn drop(&mut self) {
        self.close();
    }
}
fn cancelled(shared: &Shared, generation: u64) -> bool {
    let state = shared.state.lock();
    state.closed || state.generation != generation
}
fn publish(shared: &Shared, update: SearchUpdate) {
    let mut state = shared.state.lock();
    if !state.closed && state.generation == update.generation() {
        state.update = Some(update);
    }
}
fn wait_retry(shared: &Shared, generation: u64, delay: Duration) {
    let mut state = shared.state.lock();
    if !state.closed && state.generation == generation {
        // Mutex+condition variable avoid lost cancellation/request wakeups.
        shared.wake.wait_for(&mut state, delay);
    }
}
fn run_worker(
    terminal: Weak<Mutex<TerminalState>>,
    shared: Arc<Shared>,
    limits: SearchWorkerLimits,
) {
    loop {
        let request = {
            let mut state = shared.state.lock();
            while state.pending.is_none() && !state.closed {
                shared.wake.wait(&mut state);
            }
            if state.closed {
                return;
            }
            state.pending.take().expect("pending checked under lock")
        };
        if let Some(result) = run_request(&terminal, &shared, request, limits) {
            publish(&shared, SearchUpdate::Finished(result));
        }
    }
}
fn status_for(outcome: SearchChunkOutcome) -> SearchFinishStatus {
    match outcome {
        SearchChunkOutcome::Stale => SearchFinishStatus::Stale,
        SearchChunkOutcome::Unavailable => SearchFinishStatus::Unavailable,
        SearchChunkOutcome::BudgetTooSmall => SearchFinishStatus::BudgetTooSmall,
        SearchChunkOutcome::Unsupported(reason) => SearchFinishStatus::Unsupported(reason),
        SearchChunkOutcome::Chunk(_) => unreachable!("capture chunk handled separately"),
    }
}
fn run_request(
    terminal: &Weak<Mutex<TerminalState>>,
    shared: &Shared,
    mut request: Request,
    limits: SearchWorkerLimits,
) -> Option<SearchFinished> {
    let ResumeState {
        mut matcher,
        mut seal,
        mut cursor,
        mut hits,
    } = request.resume.take().unwrap_or_else(|| ResumeState {
        matcher: RawLiteralOverlap::new(&request.query, request.options.case_sensitive)
            .expect("submission validated literal query"),
        seal: None,
        cursor: None,
        hits: Vec::with_capacity(limits.max_hits),
    });
    // A continuation may be resumed through a worker with a smaller configured
    // hit limit. Preserve the stronger bound without admitting an oversized hit
    // aggregate; this remains an explicit capped, nonresumable outcome.
    let prior_hit_limit = hits.len() >= limits.max_hits;
    hits.truncate(limits.max_hits);
    let mut progress = SearchProgress {
        generation: request.generation,
        chunks: 0,
        work: SearchChunkWork::default(),
        hits: hits.len(),
        lock_misses: 0,
        lock_wait: Duration::ZERO,
        capture_time: Duration::ZERO,
        match_time: Duration::ZERO,
        elapsed: Duration::ZERO,
    };
    let mut backoff = Duration::from_millis(1);
    let status = loop {
        if cancelled(shared, request.generation) {
            return None;
        }
        if prior_hit_limit {
            break SearchFinishStatus::HitLimit;
        }
        let elapsed = request.submitted.elapsed();
        if elapsed >= limits.max_duration {
            break SearchFinishStatus::TimeLimit;
        }
        let remaining_cells = limits.max_cells - progress.work.cells_charged;
        let remaining_bytes = limits.max_bytes - progress.work.bytes_emitted;
        let remaining_encoded = limits.max_encoded_bytes - progress.work.encoded_bytes_charged;
        if progress.chunks >= limits.max_chunks
            || remaining_cells == 0
            || remaining_bytes < 4
            || remaining_encoded == 0
        {
            break SearchFinishStatus::WorkLimit;
        }
        let budget = SearchChunkBudget {
            rows: limits.chunk.rows,
            cells: limits.chunk.cells.min(remaining_cells),
            bytes: limits.chunk.bytes.min(remaining_bytes),
            encoded_bytes: limits.chunk.encoded_bytes.min(remaining_encoded),
        };
        // This lexical block is the only strong session reference. A failed
        // try_lock drops it before cancellable backoff; successful capture drops
        // guard and Arc before normalization/matching or test gates.
        let capture = {
            let Some(session) = terminal.upgrade() else {
                break SearchFinishStatus::SessionClosed;
            };
            let captured = if let Some(guard) = session.try_lock() {
                if cancelled(shared, request.generation) {
                    return None;
                }
                let started = Instant::now();
                let outcome = if let Some(existing) = seal.as_ref() {
                    guard.search_sealed_output_chunk(existing, cursor.as_ref(), budget)
                } else {
                    match guard.seal_search_output_with_work(request.sequence, &request.record_id) {
                        Ok((created, records_examined)) => {
                            progress.work.records_examined += records_examined;
                            let outcome = guard.search_sealed_output_chunk(&created, None, budget);
                            seal = Some(created);
                            outcome
                        }
                        Err(outcome) => outcome,
                    }
                };
                progress.capture_time += started.elapsed();
                Some(outcome)
            } else {
                None
            };
            captured
        };
        let Some(capture) = capture else {
            progress.lock_misses += 1;
            let started = Instant::now();
            wait_retry(
                shared,
                request.generation,
                backoff.min(
                    limits
                        .max_duration
                        .saturating_sub(request.submitted.elapsed()),
                ),
            );
            progress.lock_wait += started.elapsed();
            backoff = (backoff * 2).min(MAX_RETRY_BACKOFF);
            continue;
        };
        backoff = Duration::from_millis(1);
        let SearchChunkOutcome::Chunk(chunk) = capture else {
            // Only a remainder after actual progress is resumable. If a fresh
            // window cannot admit one row, returning the same cursor would
            // loop forever under identical limits (including on resume).
            if matches!(capture, SearchChunkOutcome::BudgetTooSmall)
                && progress.chunks > 0
                && (budget.cells < limits.chunk.cells
                    || budget.bytes < limits.chunk.bytes
                    || budget.encoded_bytes < limits.chunk.encoded_bytes)
            {
                break SearchFinishStatus::WorkLimit;
            }
            break status_for(capture);
        };
        if cancelled(shared, request.generation) {
            return None;
        }
        #[cfg(test)]
        if let Some(gate) = &shared.hooks.before_match {
            gate.enter();
        }
        let started = Instant::now();
        let matched = matcher.consume(&chunk);
        progress.match_time += started.elapsed();
        if cancelled(shared, request.generation) {
            return None;
        }
        let matched = match matched {
            Ok(matched) => matched,
            Err(reason) => break SearchFinishStatus::Unsupported(reason),
        };
        progress.chunks += 1;
        progress.work.records_examined += chunk.work.records_examined;
        progress.work.rows_decoded += chunk.work.rows_decoded;
        progress.work.cells_charged += chunk.work.cells_charged;
        progress.work.encoded_bytes_charged += chunk.work.encoded_bytes_charged;
        progress.work.bytes_emitted += chunk.work.bytes_emitted;
        let available = limits.max_hits - hits.len();
        let overflow = matched.hits.len() > available || matched.capped;
        hits.extend(matched.hits.into_iter().take(available));
        progress.hits = hits.len();
        cursor = chunk.continuation;
        if terminal.strong_count() == 0 {
            break SearchFinishStatus::SessionClosed;
        }
        // A full cap with a remaining cursor is conservatively partial, even
        // if the unvisited suffix ultimately contains no additional matches.
        if overflow || (hits.len() == limits.max_hits && cursor.is_some()) {
            break SearchFinishStatus::HitLimit;
        }
        if cursor.is_none() {
            break SearchFinishStatus::Complete;
        }
        progress.elapsed = request.submitted.elapsed();
        publish(shared, SearchUpdate::Progress(progress.clone()));
        // Fairness even when try_lock always succeeds: give PTY/input a
        // cancellable scheduling boundary at most every four captures.
        if progress.chunks % 4 == 0 {
            wait_retry(
                shared,
                request.generation,
                Duration::from_millis(1).min(
                    limits
                        .max_duration
                        .saturating_sub(request.submitted.elapsed()),
                ),
            );
        }
    };
    if cancelled(shared, request.generation) {
        return None;
    }
    progress.elapsed = request.submitted.elapsed();
    let continuation = matches!(
        status,
        SearchFinishStatus::WorkLimit | SearchFinishStatus::TimeLimit
    )
    .then(|| SearchContinuation {
        source: terminal.clone(),
        sequence: request.sequence,
        record_id: request.record_id.clone(),
        query: request.query,
        options: request.options,
        resume: ResumeState {
            seal: seal.clone(),
            cursor,
            matcher,
            hits: hits.clone(),
        },
    });
    Some(SearchFinished {
        generation: request.generation,
        record_sequence: request.sequence,
        record_id: request.record_id.clone(),
        seal,
        status,
        hits,
        progress,
        continuation,
    })
}

#[cfg(test)]
#[derive(Clone, Default)]
struct TestHooks {
    before_match: Option<Arc<TestGate>>,
    before_exit: Option<Arc<TestGate>>,
}
#[cfg(test)]
#[derive(Default)]
struct TestGate {
    state: Mutex<(bool, bool)>,
    wake: Condvar,
}
#[cfg(test)]
impl TestGate {
    fn enter(&self) {
        let mut state = self.state.lock();
        state.0 = true;
        self.wake.notify_all();
        while !state.1 {
            self.wake.wait(&mut state);
        }
    }
    fn wait_entered(&self) {
        let mut state = self.state.lock();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !state.0 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "worker did not reach test gate");
            self.wake.wait_for(&mut state, remaining);
        }
    }
    fn release(&self) {
        self.state.lock().1 = true;
        self.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn fixture(lines: usize, content: &str) -> (Arc<Mutex<TerminalState>>, u64, String) {
        let mut terminal = TerminalState::new(64, 4);
        terminal.set_max_scrollback(lines * 4 + 100);
        terminal.process_input(b"\x1b]133;A\x07\x1b]133;C;id=worker\x07");
        for _ in 0..lines {
            terminal.process_input(content.as_bytes());
            terminal.process_input(b"\r\n");
        }
        terminal.process_input(b"\x1b]133;D;0;id=worker\x07");
        let record = terminal.command_records().back().unwrap();
        let identity = (record.sequence, record.id.clone());
        // Move both output endpoints off the mutable grid before sealing.
        terminal.process_input(b"\r\n\r\n\r\n\r\n");
        terminal
            .seal_search_output(identity.0, &identity.1)
            .unwrap();
        (Arc::new(Mutex::new(terminal)), identity.0, identity.1)
    }
    fn pool() -> Arc<SearchWorkerPermitPool> {
        Arc::new(SearchWorkerPermitPool::new(1))
    }
    fn worker(terminal: &Arc<Mutex<TerminalState>>, limits: SearchWorkerLimits) -> SearchWorker {
        SearchWorker::new(Arc::downgrade(terminal), pool(), limits).unwrap()
    }
    fn finish(worker: &SearchWorker) -> SearchFinished {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(SearchUpdate::Finished(result)) = worker.take_update() {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "worker did not publish final status"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn await_exit(pool: &SearchWorkerPermitPool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while pool.active_workers() > 0 {
            assert!(Instant::now() < deadline, "worker did not exit");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn options() -> SearchWorkerOptions {
        SearchWorkerOptions::default()
    }
    #[test]
    fn search_worker_literal_result_requires_generation_and_source_validation() {
        let (terminal, seq, id) = fixture(4, "xxNeedlexx");
        let worker = worker(&terminal, SearchWorkerLimits::default());
        let generation = worker.submit(seq, &id, "needle", options()).unwrap();
        let result = finish(&worker);
        assert_eq!(result.generation, generation);
        assert_eq!(result.status, SearchFinishStatus::Complete);
        assert_eq!(result.hits.len(), 4);
        assert!(worker.is_current(result.generation));
        result.validate_source(&terminal.lock()).unwrap();
        // Append-only PTY output preserves this result's immutable seal.
        terminal.lock().process_input(b"new prompt text\r\n");
        result.validate_source(&terminal.lock()).unwrap();
        // Source mutation invalidates already-queued results at adoption.
        terminal.lock().on_resize(65, 4);
        assert!(matches!(
            result.validate_source(&terminal.lock()),
            Err(SearchChunkOutcome::Stale)
        ));
        worker.cancel();
        assert!(!worker.is_current(result.generation));
        assert!(worker.take_update().is_none());
    }
    #[test]
    fn search_worker_latest_request_coalesces_and_cancelled_match_cannot_publish() {
        let (terminal, seq, id) = fixture(2, "alpha beta final");
        let gate = Arc::new(TestGate::default());
        let worker = SearchWorker::new_inner(
            Arc::downgrade(&terminal),
            pool(),
            SearchWorkerLimits::default(),
            TestHooks {
                before_match: Some(gate.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let old = worker.submit(seq, &id, "alpha", options()).unwrap();
        gate.wait_entered();
        // Capture released the terminal lock before the matching phase.
        assert!(terminal.try_lock().is_some());
        let mut latest = old;
        for query in ["beta", "missing", "alpha", "beta", "final"] {
            latest = worker.submit(seq, &id, query, options()).unwrap();
            assert_eq!(worker.buffered_slots(), (1, 0));
        }
        assert!(!worker.is_current(old));
        gate.release();
        let result = finish(&worker);
        assert_eq!(result.generation, latest);
        assert_eq!(result.hits.len(), 2);
        assert_eq!(result.hits[0].start.anchor.column, 11);
        assert!(worker.take_update().is_none());
    }
    #[test]
    fn search_worker_cancel_keeps_permit_and_close_waits_for_actual_thread_exit() {
        let (terminal, seq, id) = fixture(1, "needle");
        let shared_pool = pool();
        let match_gate = Arc::new(TestGate::default());
        let exit_gate = Arc::new(TestGate::default());
        let worker = SearchWorker::new_inner(
            Arc::downgrade(&terminal),
            shared_pool.clone(),
            SearchWorkerLimits::default(),
            TestHooks {
                before_match: Some(match_gate.clone()),
                before_exit: Some(exit_gate.clone()),
            },
        )
        .unwrap();
        worker.submit(seq, &id, "needle", options()).unwrap();
        match_gate.wait_entered();
        worker.cancel();
        assert_eq!(shared_pool.active_workers(), 1);
        assert!(matches!(
            SearchWorker::new(
                Arc::downgrade(&terminal),
                shared_pool.clone(),
                SearchWorkerLimits::default()
            ),
            Err(SearchWorkerAdmission::Busy)
        ));
        worker.close();
        match_gate.release();
        exit_gate.wait_entered();
        assert_eq!(shared_pool.active_workers(), 1);
        assert!(matches!(
            SearchWorker::new(
                Arc::downgrade(&terminal),
                shared_pool.clone(),
                SearchWorkerLimits::default()
            ),
            Err(SearchWorkerAdmission::Busy)
        ));
        assert!(worker.take_update().is_none());
        exit_gate.release();
        await_exit(&shared_pool);
        let replacement = SearchWorker::new(
            Arc::downgrade(&terminal),
            shared_pool.clone(),
            SearchWorkerLimits::default(),
        )
        .unwrap();
        replacement.close();
        await_exit(&shared_pool);
    }
    #[test]
    fn search_worker_contended_input_lock_uses_bounded_wait_and_finite_window() {
        let (terminal, seq, id) = fixture(2, "needle");
        let guard = terminal.lock();
        let worker = worker(
            &terminal,
            SearchWorkerLimits {
                max_duration: Duration::from_millis(70),
                ..Default::default()
            },
        );
        let start = Instant::now();
        worker.submit(seq, &id, "needle", options()).unwrap();
        let result = finish(&worker);
        assert_eq!(result.status, SearchFinishStatus::TimeLimit);
        assert_eq!(result.progress.chunks, 0);
        assert!(result.progress.lock_misses > 1);
        // Exponential waits reach 16ms rather than polling in a busy loop.
        assert!(result.progress.lock_misses < 40);
        assert!(result.progress.lock_wait >= Duration::from_millis(20));
        assert!(result.progress.elapsed >= Duration::from_millis(70));
        println!("contended raw-search ready={:?}, lock wait={:?}, lock misses={}; observation only, not a native acceptance threshold",
            start.elapsed(), result.progress.lock_wait, result.progress.lock_misses);
        drop(guard);
    }
    #[test]
    fn search_worker_cancellation_wakes_contended_retry_and_never_delivers_old_result() {
        let (terminal, seq, id) = fixture(1, "needle");
        let shared_pool = pool();
        let worker = SearchWorker::new(
            Arc::downgrade(&terminal),
            shared_pool.clone(),
            SearchWorkerLimits {
                max_duration: MAX_SEARCH_WINDOW_DURATION,
                ..Default::default()
            },
        )
        .unwrap();
        let guard = terminal.lock();
        worker.submit(seq, &id, "needle", options()).unwrap();
        std::thread::sleep(Duration::from_millis(35));
        worker.cancel();
        assert!(worker.take_update().is_none());
        let start = Instant::now();
        worker.close();
        await_exit(&shared_pool);
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(worker.take_update().is_none());
        drop(guard);
    }
    #[test]
    fn search_worker_chunk_work_and_global_hit_cap_are_finite() {
        let (terminal, seq, id) = fixture(200, "x x x x x x x x");
        let worker = worker(
            &terminal,
            SearchWorkerLimits {
                max_hits: 7,
                chunk: SearchChunkBudget {
                    rows: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        worker.submit(seq, &id, "x", options()).unwrap();
        let result = finish(&worker);
        assert_eq!(result.status, SearchFinishStatus::HitLimit);
        assert_eq!(result.hits.len(), 7);
        assert!(result.progress.chunks <= 2);
        let worker = super::SearchWorker::new(
            Arc::downgrade(&terminal),
            pool(),
            SearchWorkerLimits {
                max_chunks: 3,
                chunk: SearchChunkBudget {
                    rows: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        worker.submit(seq, &id, "no-match", options()).unwrap();
        let result = finish(&worker);
        assert_eq!(result.status, SearchFinishStatus::WorkLimit);
        assert_eq!(result.progress.chunks, 3);
        assert_eq!(result.progress.work.rows_decoded, 3);
        assert_eq!(result.progress.work.cells_charged, 192);
        // Sealed capture charges both bounded record validation and extraction.
        assert_eq!(result.progress.work.records_examined, 7);
    }
    #[test]
    fn search_worker_reduced_total_budget_is_partial_and_config_is_hard_clamped() {
        let (terminal, seq, id) = fixture(20, "abcdefgh");
        let worker = worker(
            &terminal,
            SearchWorkerLimits {
                max_cells: 100,
                chunk: SearchChunkBudget {
                    rows: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        worker.submit(seq, &id, "no-match", options()).unwrap();
        let result = finish(&worker);
        assert_eq!(result.status, SearchFinishStatus::WorkLimit);
        assert_eq!(result.progress.work.cells_charged, 64);
        let limits = SearchWorkerLimits {
            chunk: SearchChunkBudget {
                rows: usize::MAX,
                cells: usize::MAX,
                bytes: usize::MAX,
                encoded_bytes: usize::MAX,
            },
            max_hits: usize::MAX,
            max_chunks: usize::MAX,
            max_cells: usize::MAX,
            max_bytes: usize::MAX,
            max_encoded_bytes: usize::MAX,
            max_duration: Duration::MAX,
        }
        .bounded();
        assert_eq!(limits.chunk.rows, 32);
        assert_eq!(limits.chunk.cells, 16_384);
        assert_eq!(limits.chunk.bytes, 16_384);
        assert_eq!(limits.max_hits, RAW_LITERAL_MAX_HITS);
        assert_eq!(limits.max_chunks, MAX_SEARCH_WINDOW_CHUNKS);
        assert_eq!(limits.max_cells, MAX_SEARCH_WINDOW_CELLS);
        assert_eq!(limits.max_bytes, MAX_SEARCH_WINDOW_BYTES);
        assert_eq!(limits.max_encoded_bytes, MAX_SEARCH_WINDOW_ENCODED_BYTES);
        assert_eq!(limits.max_duration, MAX_SEARCH_WINDOW_DURATION);
        println!(
            "bounded search payload accounting: {:?}",
            limits.payload_accounting()
        );
    }
    #[test]
    fn search_worker_progress_slot_stays_single_without_a_consumer() {
        let (terminal, seq, id) = fixture(300, "abcdefgh");
        let worker = worker(
            &terminal,
            SearchWorkerLimits {
                max_chunks: 300,
                max_duration: MAX_SEARCH_WINDOW_DURATION,
                chunk: SearchChunkBudget {
                    rows: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        worker.submit(seq, &id, "no-match", options()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = worker.shared.state.lock();
            assert!(usize::from(state.pending.is_some()) <= 1);
            assert!(usize::from(state.update.is_some()) <= 1);
            if matches!(state.update, Some(SearchUpdate::Finished(_))) {
                break;
            }
            drop(state);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(worker.buffered_slots(), (0, 1));
        let result = finish(&worker);
        assert_eq!(result.progress.chunks, 300);
        assert_eq!(result.status, SearchFinishStatus::WorkLimit);
        assert_eq!(worker.buffered_slots(), (0, 0));
    }
    #[test]
    fn search_worker_session_weak_lifetime_excludes_matching_and_idle_waits() {
        let (terminal, seq, id) = fixture(1, "needle");
        let weak = Arc::downgrade(&terminal);
        let gate = Arc::new(TestGate::default());
        let worker = SearchWorker::new_inner(
            weak.clone(),
            pool(),
            SearchWorkerLimits::default(),
            TestHooks {
                before_match: Some(gate.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        worker.submit(seq, &id, "needle", options()).unwrap();
        gate.wait_entered();
        assert_eq!(Arc::strong_count(&terminal), 1);
        drop(terminal);
        assert!(weak.upgrade().is_none());
        gate.release();
        assert_eq!(finish(&worker).status, SearchFinishStatus::SessionClosed);
        assert!(weak.upgrade().is_none());
        worker.submit(seq, &id, "needle", options()).unwrap();
        assert_eq!(finish(&worker).status, SearchFinishStatus::SessionClosed);
    }
    #[test]
    fn search_worker_session_weak_lifetime_excludes_contention_backoff() {
        let (terminal, seq, id) = fixture(1, "needle");
        let weak = Arc::downgrade(&terminal);
        let guard = terminal.lock();
        let worker = worker(&terminal, SearchWorkerLimits::default());
        worker.submit(seq, &id, "needle", options()).unwrap();
        std::thread::sleep(Duration::from_millis(40));
        // During retry sleeps, the worker owns no strong session reference.
        // Retry attempts are short, so find a quiescent sample with a bounded
        // wait rather than assert at one racy instant.
        let deadline = Instant::now() + Duration::from_secs(1);
        while Arc::strong_count(&terminal) != 1 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        worker.cancel();
        drop(guard);
        drop(terminal);
        let deadline = Instant::now() + Duration::from_secs(1);
        while weak.upgrade().is_some() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn search_worker_ready_latency_with_real_input_append_and_bounded_progress() {
        let (terminal, seq, id) = fixture(2000, "abcdefghijklmnopqrstuvwxyz");
        let mut config = SearchWorkerLimits::default();
        config.max_chunks = 1024;
        config.max_duration = MAX_SEARCH_WINDOW_DURATION;
        let worker = worker(&terminal, config);
        let stop = Arc::new(AtomicBool::new(false));
        let writer_terminal = terminal.clone();
        let writer_stop = stop.clone();
        let writer = std::thread::spawn(move || {
            let mut lock_waits = Vec::new();
            while !writer_stop.load(Ordering::Relaxed) {
                let started = Instant::now();
                let mut guard = writer_terminal.lock();
                lock_waits.push(started.elapsed());
                guard.process_input(b"pty append\r\n");
                drop(guard);
                std::thread::sleep(Duration::from_millis(1));
            }
            lock_waits
        });
        let start = Instant::now();
        worker.submit(seq, &id, "no-match", options()).unwrap();
        let result = finish(&worker);
        let ready = start.elapsed();
        stop.store(true, Ordering::Relaxed);
        let mut input_wait = writer.join().unwrap();
        input_wait.sort_unstable();
        assert_eq!(result.status, SearchFinishStatus::Complete);
        assert!(result.progress.chunks > 100);
        assert!(result.progress.work.cells_charged <= config.max_cells);
        assert!(result.progress.elapsed <= ready);
        assert!(worker.buffered_slots().1 <= 1);
        result.validate_source(&terminal.lock()).unwrap();
        println!("real worker ready={ready:?}, worker elapsed={:?}, capture={:?}, match={:?}, lock wait={:?}, chunks={}, PTY lock p95={:?} max={:?}; not native/performance acceptance",
            result.progress.elapsed, result.progress.capture_time, result.progress.match_time,
            result.progress.lock_wait, result.progress.chunks,
            input_wait.get(input_wait.len() * 95 / 100), input_wait.last());
    }
    #[test]
    fn search_worker_rejects_unsupported_options_and_oversized_input() {
        let (terminal, seq, id) = fixture(1, "needle");
        let worker = worker(&terminal, SearchWorkerLimits::default());
        for unsupported in [
            SearchWorkerOptions {
                regex: true,
                ..Default::default()
            },
            SearchWorkerOptions {
                whole_word: true,
                ..Default::default()
            },
        ] {
            assert!(matches!(
                worker.submit(seq, &id, "needle", unsupported),
                Err(SearchSubmitError::Unsupported(_))
            ));
        }
        assert!(matches!(
            worker.submit(seq, &id, &"x".repeat(4097), options()),
            Err(SearchSubmitError::Unsupported(_))
        ));
        assert!(matches!(
            worker.submit(seq, &"x".repeat(MAX_OSC_133_ID_BYTES + 1), "x", options()),
            Err(SearchSubmitError::InvalidRecord)
        ));
        assert_eq!(worker.buffered_slots(), (0, 0));
        worker.close();
        assert_eq!(
            worker.submit(seq, &id, "needle", options()),
            Err(SearchSubmitError::Closed)
        );
    }

    #[test]
    fn search_worker_resume_preserves_cross_window_unicode_and_nonoverlap() {
        for (content, query, expected) in [
            ("xxNeedleNeedlezz", "needle", vec![2, 8]),
            ("aaaaaaaa", "aa", vec![0, 2, 4, 6]),
            ("xİZz", "i\u{307}z", vec![1]),
        ] {
            let (terminal, seq, id) = fixture(1, content);
            let worker = worker(
                &terminal,
                SearchWorkerLimits {
                    max_chunks: 1,
                    chunk: SearchChunkBudget {
                        bytes: 4,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            worker.submit(seq, &id, query, options()).unwrap();
            let mut windows = 0;
            loop {
                let mut result = finish(&worker);
                windows += 1;
                assert!(windows < 32);
                assert_eq!(result.progress.chunks, 1);
                if result.status == SearchFinishStatus::Complete {
                    assert_eq!(
                        result
                            .hits
                            .iter()
                            .map(|hit| hit.start.anchor.column)
                            .collect::<Vec<_>>(),
                        expected
                    );
                    assert!(result.continuation.is_none());
                    break;
                }
                assert_eq!(result.status, SearchFinishStatus::WorkLimit);
                let old = result.generation;
                worker.resume(result.continuation.take().unwrap()).unwrap();
                assert!(!worker.is_current(old));
            }
            assert!(windows >= 2);
        }
    }
    #[test]
    fn search_worker_resume_retains_global_hit_cap_and_rejects_other_pane() {
        let (terminal, seq, id) = fixture(10, "x");
        let worker = worker(
            &terminal,
            SearchWorkerLimits {
                max_chunks: 1,
                max_hits: 3,
                chunk: SearchChunkBudget {
                    rows: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        worker.submit(seq, &id, "x", options()).unwrap();
        let first = finish(&worker);
        assert_eq!(first.hits.len(), 1);
        worker.resume(first.continuation.unwrap()).unwrap();
        let second = finish(&worker);
        assert_eq!(second.hits.len(), 2);
        worker.resume(second.continuation.unwrap()).unwrap();
        let third = finish(&worker);
        assert_eq!(third.hits.len(), 3);
        assert_eq!(third.status, SearchFinishStatus::HitLimit);
        assert!(third.continuation.is_none());

        worker.submit(seq, &id, "absent", options()).unwrap();
        let result = finish(&worker);
        let (other, _, _) = fixture(10, "x");
        let other_worker = super::SearchWorker::new(
            Arc::downgrade(&other),
            pool(),
            SearchWorkerLimits::default(),
        )
        .unwrap();
        assert_eq!(
            other_worker.resume(result.continuation.unwrap()),
            Err(SearchSubmitError::DifferentSource)
        );
    }
    #[test]
    fn search_worker_resume_validates_mutated_source_before_using_carry() {
        let (terminal, seq, id) = fixture(5, "needle");
        let worker = worker(
            &terminal,
            SearchWorkerLimits {
                max_chunks: 1,
                chunk: SearchChunkBudget {
                    bytes: 4,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        worker.submit(seq, &id, "needle", options()).unwrap();
        let first = finish(&worker);
        assert!(first.hits.is_empty());
        terminal.lock().on_resize(65, 4);
        worker.resume(first.continuation.unwrap()).unwrap();
        let second = finish(&worker);
        assert_eq!(second.status, SearchFinishStatus::Stale);
        assert_eq!(second.progress.chunks, 0);
        assert!(second.hits.is_empty());
        assert!(second.continuation.is_none());
    }
    #[test]
    fn search_worker_time_limited_uncaptured_resume_is_source_bound() {
        let (terminal, seq, id) = fixture(1, "needle");
        let worker = worker(
            &terminal,
            SearchWorkerLimits {
                max_duration: Duration::from_millis(5),
                ..Default::default()
            },
        );
        let guard = terminal.lock();
        worker.submit(seq, &id, "needle", options()).unwrap();
        let first = finish(&worker);
        assert_eq!(first.status, SearchFinishStatus::TimeLimit);
        assert!(first.seal.is_none());
        drop(guard);
        worker.resume(first.continuation.unwrap()).unwrap();
        let second = finish(&worker);
        assert_eq!(second.status, SearchFinishStatus::Complete);
        assert_eq!(second.hits.len(), 1);
    }

    #[test]
    fn search_worker_replacement_cannot_adopt_prior_worker_generation() {
        let (terminal, seq, id) = fixture(1, "needle");
        let shared_pool = pool();
        let first = SearchWorker::new(
            Arc::downgrade(&terminal),
            shared_pool.clone(),
            SearchWorkerLimits::default(),
        )
        .unwrap();
        first.submit(seq, &id, "needle", options()).unwrap();
        let old_result = finish(&first);
        first.close();
        await_exit(&shared_pool);
        let second = SearchWorker::new(
            Arc::downgrade(&terminal),
            shared_pool.clone(),
            SearchWorkerLimits::default(),
        )
        .unwrap();
        let new_generation = second.submit(seq, &id, "different", options()).unwrap();
        assert_ne!(old_result.generation, new_generation);
        // Source remains valid, so only unique request identity prevents ABA.
        old_result.validate_source(&terminal.lock()).unwrap();
        assert!(!second.is_current(old_result.generation));
        let new_result = finish(&second);
        assert_eq!(new_result.generation, new_generation);
        assert!(new_result.hits.is_empty());
        second.close();
        await_exit(&shared_pool);
    }
    #[test]
    fn search_worker_generation_exhaustion_is_sticky_and_never_wraps() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(next_generation(&counter), Ok(u64::MAX - 1));
        assert_eq!(
            next_generation(&counter),
            Err(SearchSubmitError::GenerationExhausted)
        );
        assert_eq!(
            next_generation(&counter),
            Err(SearchSubmitError::GenerationExhausted)
        );
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }
    #[test]
    fn search_worker_atomic_cell_and_encoded_budget_fail_without_continuation() {
        let (terminal, seq, id) = fixture(4, "abcdefgh");
        for limits in [
            SearchWorkerLimits {
                max_cells: 1,
                ..Default::default()
            },
            SearchWorkerLimits {
                max_encoded_bytes: 1,
                ..Default::default()
            },
            SearchWorkerLimits {
                chunk: SearchChunkBudget {
                    cells: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
            SearchWorkerLimits {
                chunk: SearchChunkBudget {
                    encoded_bytes: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        ] {
            let worker = worker(&terminal, limits);
            worker.submit(seq, &id, "needle", options()).unwrap();
            let result = finish(&worker);
            assert_eq!(result.status, SearchFinishStatus::BudgetTooSmall);
            assert_eq!(result.progress.chunks, 0);
            assert_eq!(result.progress.work.cells_charged, 0);
            assert_eq!(result.progress.work.encoded_bytes_charged, 0);
            assert!(result.continuation.is_none());
        }
    }

    #[test]
    fn search_worker_rejects_sub_atomic_configuration_without_budget_promotion() {
        let (terminal, _, _) = fixture(1, "🦀");
        let invalid = [
            SearchWorkerLimits {
                max_cells: 0,
                ..Default::default()
            },
            SearchWorkerLimits {
                max_encoded_bytes: 0,
                ..Default::default()
            },
            SearchWorkerLimits {
                max_bytes: 3,
                ..Default::default()
            },
            SearchWorkerLimits {
                max_chunks: 0,
                ..Default::default()
            },
            SearchWorkerLimits {
                chunk: SearchChunkBudget {
                    bytes: 3,
                    ..Default::default()
                },
                ..Default::default()
            },
            SearchWorkerLimits {
                chunk: SearchChunkBudget {
                    rows: 0,
                    ..Default::default()
                },
                ..Default::default()
            },
            SearchWorkerLimits {
                max_duration: Duration::from_nanos(1),
                ..Default::default()
            },
        ];
        for limits in invalid {
            let bounded = limits.bounded();
            assert!(bounded.max_cells <= limits.max_cells);
            assert!(bounded.max_bytes <= limits.max_bytes);
            assert!(bounded.max_duration <= limits.max_duration);
            let pool = pool();
            assert!(matches!(
                SearchWorker::new(Arc::downgrade(&terminal), pool.clone(), limits),
                Err(SearchWorkerAdmission::InvalidBudget)
            ));
            assert_eq!(pool.active_workers(), 0);
        }
    }

    #[test]
    fn search_worker_utf8_four_byte_unit_progresses_with_exact_budget() {
        let (terminal, seq, id) = fixture(1, "🦀x");
        let worker = worker(
            &terminal,
            SearchWorkerLimits {
                chunk: SearchChunkBudget {
                    rows: 1,
                    cells: 64,
                    bytes: 4,
                    encoded_bytes: 1024,
                },
                max_cells: 64,
                max_bytes: 4,
                ..Default::default()
            },
        );
        worker.submit(seq, &id, "🦀x", options()).unwrap();
        for pass in 0..10 {
            let mut result = finish(&worker);
            assert!(result.progress.work.bytes_emitted <= 4);
            assert!(result.progress.work.cells_charged <= 64);
            if result.status == SearchFinishStatus::Complete {
                assert!(pass > 0);
                assert_eq!(result.hits.len(), 1);
                return;
            }
            assert_eq!(result.status, SearchFinishStatus::WorkLimit);
            assert!(result.progress.chunks > 0);
            worker.resume(result.continuation.take().unwrap()).unwrap();
        }
        panic!("four-byte scalar failed to make finite progress");
    }

    #[test]
    fn search_worker_resumed_window_rejects_later_wider_atomic_row() {
        let mut t = TerminalState::new(16, 4);
        t.set_max_scrollback(200);
        t.process_input(b"\x1b]133;A\x07\x1b]133;C;id=mixed\x07");
        t.process_input("small\r\n".repeat(8).as_bytes());
        t.on_resize(64, 4);
        t.process_input("wide\r\n".repeat(8).as_bytes());
        t.process_input(b"\x1b]133;D;0;id=mixed\x07\r\n\r\n\r\n\r\n\r\n");
        let seq = t.command_records().back().unwrap().sequence;
        let terminal = Arc::new(Mutex::new(t));
        let worker = worker(
            &terminal,
            SearchWorkerLimits {
                max_cells: 32,
                chunk: SearchChunkBudget {
                    rows: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        worker.submit(seq, "mixed", "needle", options()).unwrap();
        let mut prior_progress = false;
        for _ in 0..20 {
            let mut result = finish(&worker);
            if result.status == SearchFinishStatus::BudgetTooSmall {
                assert!(prior_progress);
                assert_eq!(result.progress.chunks, 0);
                assert!(result.continuation.is_none());
                return;
            }
            assert_eq!(result.status, SearchFinishStatus::WorkLimit);
            assert!(result.progress.chunks > 0);
            prior_progress = true;
            worker.resume(result.continuation.take().unwrap()).unwrap();
        }
        panic!("wider row kept returning zero-progress resumable windows");
    }
    #[test]
    fn search_worker_resumed_window_rejects_later_larger_encoded_row() {
        let (terminal, seq, id) = fixture(1, "a\r\nabcdefgh");
        let worker = worker(
            &terminal,
            SearchWorkerLimits {
                max_encoded_bytes: 4,
                chunk: SearchChunkBudget {
                    rows: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        worker.submit(seq, &id, "needle", options()).unwrap();
        let mut first = finish(&worker);
        assert_eq!(first.status, SearchFinishStatus::WorkLimit);
        assert!(first.progress.chunks > 0);
        assert!(first.progress.work.encoded_bytes_charged <= 4);
        worker.resume(first.continuation.take().unwrap()).unwrap();
        let next = finish(&worker);
        assert_eq!(next.status, SearchFinishStatus::BudgetTooSmall);
        assert_eq!(next.progress.chunks, 0);
        assert!(next.continuation.is_none());
    }
}
