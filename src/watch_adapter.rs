//! Host eligibility/age wrapper; cadence itself lives only in jterm_core.
use crate::terminal::{WatchCommand, WatchParsed};
use jterm_core::organism::{Behavior, BodyLanguage, RenderContext, WatchRhythm};
#[cfg(test)]
use jterm_core::organism_watch::WatchDeadline;
use jterm_core::organism_watch::{WatchEvent, WatchObservation, WatchOutputDisposition};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    command: WatchCommand,
    observation: u64,
}

pub struct WatchAdapter {
    core: WatchObservation<Identity>,
    identity: Option<Identity>,
    next_observation: Option<u64>,
    observed_since: Option<Duration>,
}

impl Default for WatchAdapter {
    fn default() -> Self {
        Self {
            core: WatchObservation::default(),
            identity: None,
            next_observation: Some(1),
            observed_since: None,
        }
    }
}

impl WatchAdapter {
    pub fn reset(&mut self) {
        self.core.set_identity(None);
        self.identity = None;
        self.observed_since = None;
    }
    fn begin(&mut self, now: Duration, command: WatchCommand, trusted: bool, preserve_age: bool) {
        let age = preserve_age.then_some(self.observed_since).flatten();
        self.reset();
        let Some(observation) = self.next_observation else {
            return;
        };
        self.next_observation = observation.checked_add(1);
        let identity = Identity {
            command,
            observation,
        };
        self.identity = Some(identity);
        self.observed_since = age.or(Some(now));
        self.core.set_identity(Some(identity));
        let event = if trusted {
            WatchEvent::BeginTrustedCommand
        } else {
            WatchEvent::AttachRunningObservation
        };
        if self.core.advance(now, &identity, event).is_err() {
            // The reducer preserves its clock across identities. Never leave
            // an apparently active host identity after a rejected start.
            self.reset();
        }
    }
    pub fn attach(&mut self, now: Duration, command: Option<WatchCommand>) {
        let Some(command) = command else {
            self.reset();
            return;
        };
        if self
            .identity
            .is_none_or(|identity| identity.command != command)
        {
            self.begin(now, command, false, false);
        }
    }
    pub fn parsed(
        &mut self,
        now: Duration,
        read_at: Duration,
        update: WatchParsed,
        quarantine: bool,
    ) {
        let Some(command) = update.command else {
            self.reset();
            return;
        };
        let same = self
            .identity
            .is_some_and(|identity| identity.command == command);
        if !same || update.invalidated {
            // Accepted parser boundaries revoke stale cadence even before a
            // long source batch finishes. Source age is not the 60s host age.
            let trusted = update.trusted_start && !quarantine;
            self.begin(now, command, trusted, same);
        }
        if let (Some(identity), Some(batch)) = (self.identity, update.batch) {
            let disposition = if quarantine || batch.quarantined {
                WatchOutputDisposition::Quarantined
            } else {
                WatchOutputDisposition::Accepted
            };
            let _ = self.core.advance(
                read_at,
                &identity,
                WatchEvent::OutputObserved {
                    batch_seq: batch.origin.sequence,
                    disposition,
                },
            );
        }
    }
    pub fn quarantine(&mut self, now: Duration) {
        if let Some(identity) = self.identity {
            self.begin(now, identity.command, false, true);
        }
    }
    /// Called after source ingestion, never before validating an arriving batch.
    pub fn advance(&mut self, now: Duration) {
        if let Some(identity) = self.identity {
            let _ = self.core.advance(now, &identity, WatchEvent::Advance);
        }
    }
    pub fn rhythm(&self, now: Duration) -> WatchRhythm {
        self.identity
            .and_then(|identity| self.core.snapshot(now, &identity).ok())
            .map_or(WatchRhythm::Steady, |snapshot| snapshot.rhythm)
    }
    pub fn wake_after(&self, now: Duration) -> Option<Duration> {
        let identity = self.identity?;
        self.core
            .snapshot(now, &identity)
            .ok()?
            .next_deadline
            .map(|deadline| deadline.at().saturating_sub(now))
    }
    pub fn behavior(&self, now: Duration) -> Behavior {
        if self
            .observed_since
            .is_some_and(|start| now.saturating_sub(start) >= Duration::from_secs(60))
        {
            Behavior::WatchSettled
        } else {
            Behavior::WatchCommand
        }
    }
    pub fn context(
        &self,
        now: Duration,
        language: BodyLanguage,
        rhythm_enabled: bool,
    ) -> RenderContext {
        RenderContext::new(self.behavior(now), language, false).with_watch_rhythm(
            if rhythm_enabled {
                self.rhythm(now)
            } else {
                WatchRhythm::Steady
            },
        )
    }
    #[cfg(test)]
    fn deadline(&self, now: Duration) -> Option<(Identity, WatchDeadline)> {
        let identity = self.identity?;
        Some((
            identity,
            self.core.snapshot(now, &identity).ok()?.next_deadline?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output_batch::{OutputBuffer, OutputSource};
    use crate::terminal::TerminalState;
    use std::time::Instant;
    struct Fixture {
        terminal: TerminalState,
        source: OutputSource,
        adapter: WatchAdapter,
        epoch: Instant,
    }
    impl Fixture {
        fn new() -> Self {
            let source = OutputSource::default();
            let mut terminal = TerminalState::new(80, 24);
            terminal.bind_watch_source(source.epoch());
            Self {
                terminal,
                source,
                adapter: WatchAdapter::default(),
                epoch: Instant::now(),
            }
        }
        fn parse(&mut self, data: &OutputBuffer, now: Duration, quarantine: bool) {
            self.terminal.process_output_buffer(data, |u| {
                let read_at = u
                    .origin
                    .and_then(|o| o.read_at.checked_duration_since(self.epoch))
                    .unwrap_or(now);
                self.adapter.parsed(now, read_at, u, quarantine);
            });
        }
        fn feed(&mut self, at: Duration, bytes: &[u8]) {
            let data = self.source.batch(bytes.to_vec(), self.epoch + at);
            self.parse(&data, at, false);
        }
        fn running() -> Self {
            let mut f = Self::new();
            f.feed(Duration::ZERO, b"\x1b]133;C\x07");
            // The lifecycle batch was quarantined; the first subsequent batch
            // is the neutral baseline, not a busy or resumed pulse.
            f.feed(Duration::ZERO, b"baseline");
            f
        }
    }
    #[test]
    fn cadence_waits_resumes_and_expires_at_exact_nanoseconds() {
        let mut f = Fixture::running();
        assert_eq!(
            f.adapter.rhythm(Duration::from_secs(3)),
            WatchRhythm::Waiting
        );
        f.feed(Duration::from_secs(3), b"x");
        assert_eq!(
            f.adapter.rhythm(Duration::from_millis(3899)),
            WatchRhythm::Resumed
        );
        assert_eq!(
            f.adapter.rhythm(Duration::from_millis(3900)),
            WatchRhythm::Steady
        );
        let mut f = Fixture::running();
        for millis in [100, 200, 300] {
            f.feed(Duration::from_millis(millis), b"x");
        }
        assert_eq!(
            f.adapter.rhythm(Duration::from_millis(1300)),
            WatchRhythm::Busy
        );
        assert_eq!(
            f.adapter
                .rhythm(Duration::from_millis(1300) + Duration::from_nanos(1)),
            WatchRhythm::Steady
        );
        f.adapter.advance(Duration::from_millis(1300));
        assert_eq!(
            f.adapter.wake_after(Duration::from_millis(1300)),
            Some(Duration::from_nanos(1))
        );
    }
    #[test]
    fn several_reads_one_frame_count_but_one_read_several_frames_does_not() {
        let mut f = Fixture::running();
        let mut combined = OutputBuffer::default();
        for millis in [100, 200, 300] {
            combined.extend(
                f.source
                    .batch(b"x".to_vec(), f.epoch + Duration::from_millis(millis)),
            );
        }
        f.parse(&combined, Duration::from_millis(400), false);
        assert_eq!(
            f.adapter.rhythm(Duration::from_millis(400)),
            WatchRhythm::Busy
        );
        let mut f = Fixture::running();
        let mut data = f
            .source
            .batch(b"abcdef".to_vec(), f.epoch + Duration::from_millis(100));
        let mut middle = data.split_off(2);
        let last = middle.split_off(2);
        for (i, data) in [data, middle, last].iter().enumerate() {
            f.parse(data, Duration::from_millis(100 + i as u64), false);
        }
        assert_eq!(
            f.adapter.rhythm(Duration::from_millis(300)),
            WatchRhythm::Steady
        );
    }
    #[test]
    fn settling_uses_host_observation_age_not_command_age_or_read_time() {
        let mut f = Fixture::new();
        let data = f.source.batch(b"\x1b]133;C\x07".to_vec(), f.epoch);
        f.parse(&data, Duration::from_secs(120), false);
        assert_eq!(
            f.adapter.behavior(Duration::from_secs(179)),
            Behavior::WatchCommand
        );
        assert_eq!(
            f.adapter.behavior(Duration::from_secs(180)),
            Behavior::WatchSettled
        );
        f.adapter.quarantine(Duration::from_secs(190));
        assert_eq!(
            f.adapter.behavior(Duration::from_secs(190)),
            Behavior::WatchSettled
        );
        f.adapter.reset();
        f.adapter
            .attach(Duration::from_secs(200), f.terminal.watch_command());
        assert_eq!(
            f.adapter.behavior(Duration::from_secs(200)),
            Behavior::WatchCommand
        );
    }
    #[test]
    fn first_late_attach_output_stays_neutral_and_does_not_reset_age() {
        let mut f = Fixture::running();
        f.adapter.reset();
        f.adapter.attach(Duration::ZERO, f.terminal.watch_command());
        f.feed(Duration::from_secs(61), b"x");
        assert_eq!(
            f.adapter.rhythm(Duration::from_secs(61)),
            WatchRhythm::Steady
        );
        assert_eq!(
            f.adapter.behavior(Duration::from_secs(61)),
            Behavior::WatchSettled
        );
    }
    #[test]
    fn backlogged_and_parked_batches_quarantine_without_resetting_age() {
        let mut f = Fixture::running();
        for millis in [100, 200, 300] {
            f.feed(Duration::from_millis(millis), b"x");
        }
        let mut parked = f
            .source
            .batch(b"parked".to_vec(), f.epoch + Duration::from_secs(1));
        parked.quarantine();
        f.adapter.quarantine(Duration::from_secs(1));
        f.parse(&parked, Duration::from_secs(1), false);
        assert_eq!(
            f.adapter.rhythm(Duration::from_secs(1)),
            WatchRhythm::Steady
        );
        assert_eq!(
            f.adapter.behavior(Duration::from_secs(60)),
            Behavior::WatchSettled
        );
        f.feed(Duration::from_secs(4), b"neutral");
        assert_eq!(
            f.adapter.rhythm(Duration::from_secs(4)),
            WatchRhythm::Steady
        );
    }
    #[test]
    fn older_and_retimestamped_source_sequences_do_not_advance_new_clock() {
        let mut f = Fixture::running();
        let data = f
            .source
            .batch(b"x".to_vec(), f.epoch + Duration::from_secs(1));
        f.parse(&data, Duration::from_secs(1), false);
        let before = f.adapter.deadline(Duration::from_secs(1));
        let mut origin = data.fragments().next().unwrap().origin.unwrap();
        origin.read_at = f.epoch + Duration::from_secs(100);
        let repeated = OutputBuffer::from_parts(b"x".to_vec(), Some(origin));
        f.parse(&repeated, Duration::from_secs(100), false);
        assert_eq!(before, f.adapter.deadline(Duration::from_secs(1)));
    }
    #[test]
    fn stale_callback_and_owner_round_trip_cannot_reanimate_previous_identity() {
        let mut f = Fixture::running();
        let old = f.adapter.deadline(Duration::ZERO).unwrap();
        f.adapter.reset();
        f.adapter
            .attach(Duration::from_secs(1), f.terminal.watch_command());
        let before = f.adapter.deadline(Duration::from_secs(1));
        assert!(f
            .adapter
            .core
            .advance(
                Duration::from_secs(99),
                &old.0,
                WatchEvent::DeadlineDue { deadline: old.1 }
            )
            .is_err());
        assert_eq!(before, f.adapter.deadline(Duration::from_secs(1)));
    }
    #[test]
    fn backwards_batch_is_consumed_without_false_burst() {
        let mut f = Fixture::running();
        f.adapter.advance(Duration::from_secs(10));
        for millis in [100, 200, 300] {
            f.feed(Duration::from_millis(millis), b"x");
        }
        assert_eq!(
            f.adapter.rhythm(Duration::from_secs(10)),
            WatchRhythm::Waiting
        );
    }
    #[test]
    fn delayed_new_command_after_advance_starts_at_dispatch_and_keeps_working() {
        let mut f = Fixture::running();
        f.adapter.advance(Duration::from_secs(10));
        let data = f.source.batch(
            b"\x1b]133;D;0\x07\x1b]133;A\x07\x1b]133;C\x07".to_vec(),
            f.epoch + Duration::from_secs(9),
        );
        f.parse(&data, Duration::from_secs(11), false);
        assert!(f.adapter.deadline(Duration::from_secs(11)).is_some());
        f.feed(Duration::from_secs(12), b"baseline");
        for millis in [12100, 12200, 12300] {
            f.feed(Duration::from_millis(millis), b"x");
        }
        assert_eq!(
            f.adapter.rhythm(Duration::from_millis(12300)),
            WatchRhythm::Busy
        );
    }

    #[test]
    fn old_source_parser_effects_cannot_leave_a_false_running_owner() {
        let mut f = Fixture::running();
        let old = f
            .source
            .batch(b"\x1bc".to_vec(), f.epoch + Duration::from_secs(1));
        f.source = OutputSource::default();
        f.terminal.bind_watch_source(f.source.epoch());
        f.feed(Duration::from_secs(2), b"\x1b]133;A\x07\x1b]133;C\x07");
        let before = f.adapter.deadline(Duration::from_secs(2));
        f.parse(&old, Duration::from_secs(100), false);
        assert_eq!(before, f.adapter.deadline(Duration::from_secs(2)));
        // The ordinary terminal accepted RIS, so the next host eligibility
        // check retires B instead of trusting a cached Watch running flag.
        f.adapter
            .attach(Duration::from_secs(100), f.terminal.watch_command());
        assert!(f.adapter.identity.is_none());
    }

    #[test]
    fn source_metadata_overflow_preserves_terminal_bytes_and_restores_by_attach() {
        let mut f = Fixture::new();
        f.feed(Duration::ZERO, b"\x1b]133;C\x07");
        let mut combined = OutputBuffer::default();
        for _ in 0..1025 {
            combined.extend(
                f.source
                    .batch(b"x".to_vec(), f.epoch + Duration::from_secs(1)),
            );
        }
        assert_eq!(combined.len(), 1025);
        f.parse(&combined, Duration::from_secs(1), false);
        assert_eq!(f.terminal.get_cursor_pos(), (12, 65));
        assert!(f.adapter.identity.is_none());
        f.feed(Duration::from_secs(2), b"fresh");
        assert!(f.adapter.identity.is_some());
        assert_eq!(
            f.adapter.rhythm(Duration::from_secs(2)),
            WatchRhythm::Steady
        );
        assert_eq!(
            f.adapter.behavior(Duration::from_secs(61)),
            Behavior::WatchCommand
        );
    }

    #[test]
    fn mid_frame_boundary_retires_busy_and_final_tail_cannot_add_a_pulse() {
        let mut f = Fixture::running();
        for millis in [100, 200, 300] {
            f.feed(Duration::from_millis(millis), b"x");
        }
        assert_eq!(
            f.adapter.rhythm(Duration::from_millis(300)),
            WatchRhythm::Busy
        );
        let mut data = f.source.batch(
            b"\x1b]133;D;0\x07\x1b]133;A\x07\x1b]133;C\x07tail".to_vec(),
            f.epoch + Duration::from_secs(1),
        );
        let tail = data.split_off(data.len() - 4);
        f.parse(&data, Duration::from_secs(1), false);
        assert_eq!(
            f.adapter.rhythm(Duration::from_secs(1)),
            WatchRhythm::Steady
        );
        f.parse(&tail, Duration::from_secs(2), false);
        assert_eq!(
            f.adapter.rhythm(Duration::from_secs(2)),
            WatchRhythm::Steady
        );
        f.feed(Duration::from_secs(3), b"neutral");
        assert_eq!(
            f.adapter.rhythm(Duration::from_secs(3)),
            WatchRhythm::Steady
        );
    }

    #[test]
    fn observation_exhaustion_never_reuses_identity_and_keeps_terminal_working() {
        let mut f = Fixture::running();
        f.adapter.next_observation = None;
        f.adapter.reset();
        f.adapter
            .attach(Duration::from_secs(1), f.terminal.watch_command());
        f.feed(Duration::from_secs(1), b"still works");
        assert!(f.adapter.identity.is_none());
        assert!(f.terminal.watch_command().is_some());
    }
    #[test]
    fn static_context_has_steady_rhythm_without_rewriting_age() {
        let f = Fixture::running();
        let context = f
            .adapter
            .context(Duration::from_secs(60), BodyLanguage::default(), false);
        assert_eq!(context.watch_rhythm, WatchRhythm::Steady);
        assert_eq!(context.behavior, Behavior::WatchSettled);
    }
}
