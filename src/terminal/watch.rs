//! Fixed-size facts from accepted native parser transitions, never from history.
use crate::output_batch::{OutputBuffer, OutputFragment, OutputOrigin};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchCommand {
    pub source: u64,
    pub generation: u64,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct WatchBatch {
    pub origin: OutputOrigin,
    pub quarantined: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct WatchParsed {
    pub origin: Option<OutputOrigin>,
    pub command: Option<WatchCommand>,
    pub trusted_start: bool,
    pub batch: Option<WatchBatch>,
    /// Lost provenance and unfinished boundary batches revoke old cadence now.
    pub invalidated: bool,
}

#[derive(Clone, Copy, Debug)]
struct InFlight {
    origin: OutputOrigin,
    next_offset: usize,
    initial_command: Option<u64>,
    boundaries: u64,
    quarantined: bool,
    ambiguous_control: bool,
}

#[derive(Clone, Debug)]
pub(super) struct ParserWatch {
    source: Option<u64>,
    next_generation: Option<u64>,
    running: Option<(u64, u64)>, // local record sequence, independent watch generation
    revision: u64,
    disabled: bool,
    in_flight: Option<InFlight>,
    last_sequence: Option<u64>,
    slice_started: bool,
    slice_boundary: bool,
    valid: bool,
}

impl Default for ParserWatch {
    fn default() -> Self {
        Self {
            source: None,
            next_generation: Some(1),
            running: None,
            revision: 0,
            disabled: false,
            in_flight: None,
            last_sequence: None,
            slice_started: false,
            slice_boundary: false,
            valid: false,
        }
    }
}

impl ParserWatch {
    pub(super) fn start(&mut self, record: u64) {
        if self.disabled {
            return;
        }
        let Some(generation) = self.next_generation else {
            self.disable();
            return;
        };
        self.next_generation = generation.checked_add(1);
        self.boundary();
        if self.disabled {
            return;
        }
        self.running = Some((record, generation));
        self.slice_started = true;
    }
    pub(super) fn finish(&mut self, record: u64) {
        if self.running.is_some_and(|(current, _)| current == record) {
            self.boundary();
            self.running = None;
        }
    }
    pub(super) fn prompt(&mut self) {
        self.boundary();
        self.running = None;
    }
    pub(super) fn boundary(&mut self) {
        self.slice_boundary = true;
        self.slice_started = false;
        match self.revision.checked_add(1) {
            Some(revision) => self.revision = revision,
            None => {
                self.disable();
                return;
            }
        }
        if let Some(batch) = &mut self.in_flight {
            batch.boundaries = batch.boundaries.saturating_add(1);
            batch.quarantined = true;
        }
    }
    fn disable(&mut self) {
        self.disabled = true;
        self.running = None;
        self.in_flight = None;
        self.valid = false;
    }
    fn command(&self) -> Option<WatchCommand> {
        if self.disabled || !self.valid {
            return None;
        }
        Some(WatchCommand {
            source: self.source?,
            generation: self.running?.1,
            revision: self.revision,
        })
    }
    fn begin_fragment(
        &mut self,
        fragment: OutputFragment<'_>,
        pending_control: bool,
    ) -> Option<bool> {
        // Stale or replaced source facts cannot even revoke the current clock.
        if fragment
            .origin
            .is_some_and(|origin| Some(origin.source) != self.source)
        {
            return None;
        }
        if let Some(origin) = fragment.origin {
            let continuing = self.in_flight.is_some_and(|batch| {
                batch.origin == origin && batch.next_offset == fragment.offset
            });
            if !continuing
                && (fragment.offset != 0
                    || self
                        .last_sequence
                        .is_some_and(|last| origin.sequence <= last))
            {
                return None;
            }
        }
        self.slice_started = false;
        self.slice_boundary = false;
        let Some(origin) = fragment
            .origin
            .filter(|origin| Some(origin.source) == self.source)
        else {
            self.in_flight = None;
            self.valid = false;
            return Some(false);
        };
        let continuing = self
            .in_flight
            .is_some_and(|batch| batch.origin == origin && batch.next_offset == fragment.offset);
        if !continuing {
            self.in_flight = None;
            if fragment.offset != 0
                || self
                    .last_sequence
                    .is_some_and(|last| origin.sequence <= last)
            {
                self.valid = false;
                return Some(false);
            }
            self.last_sequence = Some(origin.sequence);
            self.in_flight = Some(InFlight {
                origin,
                next_offset: 0,
                initial_command: self.running.map(|r| r.1),
                boundaries: 0,
                quarantined: fragment.quarantined || pending_control,
                ambiguous_control: pending_control,
            });
        }
        self.valid = !self.disabled;
        if let Some(batch) = &mut self.in_flight {
            batch.quarantined |= fragment.quarantined;
            batch.next_offset = match fragment.offset.checked_add(fragment.bytes.len()) {
                Some(end) if end <= origin.total => end,
                _ => {
                    self.in_flight = None;
                    self.valid = false;
                    return Some(false);
                }
            };
        }
        Some(true)
    }
    fn end_fragment(
        &mut self,
        pending_control: bool,
        alternate: bool,
        accepted: bool,
    ) -> WatchParsed {
        let origin = self.in_flight.map(|batch| batch.origin);
        let trusted = self.in_flight.is_some_and(|batch| !batch.ambiguous_control);
        let batch = if self
            .in_flight
            .is_some_and(|batch| batch.next_offset == batch.origin.total)
        {
            let batch = self.in_flight.take().unwrap();
            Some(WatchBatch {
                origin: batch.origin,
                quarantined: batch.quarantined
                    || pending_control
                    || batch.boundaries != 0
                    || batch.initial_command != self.running.map(|r| r.1),
            })
        } else {
            None
        };
        WatchParsed {
            origin,
            command: (!alternate).then(|| self.command()).flatten(),
            trusted_start: trusted && accepted && self.slice_started && !alternate && self.valid,
            batch,
            invalidated: !accepted
                || alternate
                || self.slice_boundary
                || self.in_flight.is_some_and(|batch| batch.quarantined),
        }
    }
}

impl super::TerminalState {
    pub fn bind_watch_source(&mut self, source: Option<u64>) {
        if self.watch.source != source {
            self.watch.boundary();
            self.watch.source = source;
            self.watch.running = None;
            self.watch.last_sequence = None;
            self.watch.in_flight = None;
            self.watch.valid = false;
        }
    }
    pub fn watch_command(&self) -> Option<WatchCommand> {
        // This is only a validity gate for a parser-issued identity. History
        // can neither create a generation nor manufacture a trusted C.
        let actual_running = self.command_records.back().is_some_and(|record| {
            record.state == super::CommandState::Running
                && record.start_mark_seen
                && self
                    .watch
                    .running
                    .is_some_and(|(sequence, _)| sequence == record.sequence)
        });
        (!self.use_alt_buffer && actual_running)
            .then(|| self.watch.command())
            .flatten()
    }
    fn watch_pending_control(&self) -> bool {
        !self.pending_escape.is_empty()
            || !self.pending_osc.is_empty()
            || !self.pending_apc.is_empty()
            || !self.pending_string.is_empty()
    }
    pub fn process_output_buffer(
        &mut self,
        data: &OutputBuffer,
        mut observe: impl FnMut(WatchParsed),
    ) {
        for fragment in data.fragments() {
            let pending = self.watch_pending_control();
            let accepted = self.watch.begin_fragment(fragment, pending);
            let Some(accepted) = accepted else {
                let retained = self.watch.clone();
                self.process_batch(fragment.bytes);
                self.watch = retained;
                continue;
            };
            self.process_batch(fragment.bytes);
            let pending = self.watch_pending_control();
            let mut update = self
                .watch
                .end_fragment(pending, self.use_alt_buffer, accepted);
            if self.watch_command().is_none() {
                update.command = None;
                update.trusted_start = false;
            }
            observe(update);
        }
    }
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
