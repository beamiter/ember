//! Clock for the DEC private mode 2026 synchronized-output watchdog only.

use std::sync::Arc;
use std::time::Instant;

/// A per-terminal monotonic clock, fixed at construction and retained across RIS.
///
/// Only synchronized-output start and watchdog checks consult this clock.
/// Clipboard capability expiration, command timing and search deadlines always
/// retain their own real clocks. Clones share the supplied clock source.
#[derive(Clone)]
pub struct SyncOutputClock(Arc<dyn Fn() -> Instant + Send + Sync>);

impl SyncOutputClock {
    /// Supply a monotonic source for embedded hosts or deterministic tests.
    /// A source should not move backwards; backwards readings conservatively
    /// count as zero elapsed time for the watchdog.
    pub fn new(now: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        Self(Arc::new(now))
    }

    pub(super) fn now(&self) -> Instant {
        (self.0)()
    }
}

impl Default for SyncOutputClock {
    fn default() -> Self {
        Self::new(Instant::now)
    }
}
