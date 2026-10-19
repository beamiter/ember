//! One bounded background metadata inspection; never wait from the UI callback.
use std::io;
use std::sync::mpsc;

#[derive(Default)]
pub(super) struct AsyncRecheck {
    last_generation: u64,
    active_generation: Option<u64>,
    pending: Option<Pending>,
}

struct Pending {
    generation: u64,
    receiver: mpsc::Receiver<Result<String, ()>>,
}

impl AsyncRecheck {
    pub(super) fn is_busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Returns false for a duplicate request, including a canceled worker that
    /// has not finished. Dropping a receiver cannot cancel filesystem I/O.
    pub(super) fn start(
        &mut self,
        inspect: impl FnOnce() -> String + Send + 'static,
        wake: impl FnOnce() + Send + 'static,
    ) -> io::Result<bool> {
        if self.is_busy() {
            return Ok(false);
        }
        let generation = self
            .last_generation
            .checked_add(1)
            .ok_or_else(|| io::Error::other("recheck generation exhausted"))?;
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("session-recheck".into())
            .spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(inspect)).map_err(|_| ());
                let _ = sender.send(result);
                wake();
            })?;
        self.last_generation = generation;
        self.active_generation = Some(generation);
        self.pending = Some(Pending {
            generation,
            receiver,
        });
        Ok(true)
    }

    /// Invalidate this result, but retain the occupied slot until completion.
    pub(super) fn cancel(&mut self) -> bool {
        self.active_generation.take().is_some()
    }

    pub(super) fn poll(&mut self) -> Option<Result<String, ()>> {
        let result = match self.pending.as_ref()?.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err(()),
        };
        let completed = self.pending.take().expect("pending receiver was just read");
        if self.active_generation != Some(completed.generation) {
            return None;
        }
        self.active_generation = None;
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};

    fn finish(state: &mut AsyncRecheck) -> Option<Result<String, ()>> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while state.is_busy() {
            if let Some(result) = state.poll() {
                return Some(result);
            }
            assert!(
                Instant::now() < deadline,
                "controlled inspector did not finish"
            );
            std::thread::yield_now();
        }
        None
    }

    #[test]
    fn initiation_returns_before_controlled_slow_inspector_is_released() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (returned_tx, returned_rx) = mpsc::channel();
        let invoker = std::thread::spawn(move || {
            let mut state = AsyncRecheck::default();
            let started = state
                .start(
                    move || {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        "inspected".into()
                    },
                    || {},
                )
                .unwrap();
            returned_tx.send((state, started)).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // This is a causal barrier, not a filesystem/UI latency benchmark.
        let returned = returned_rx.recv_timeout(Duration::from_secs(2));
        release_tx.send(()).unwrap();
        invoker.join().unwrap();
        let (mut state, started) = returned.expect("start waited for the inspector");
        assert!(started);
        assert_eq!(finish(&mut state), Some(Ok("inspected".into())));
    }

    #[test]
    fn close_and_repeat_cannot_spawn_more_work_or_accept_an_old_generation() {
        let (release_tx, release_rx) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let ran = calls.clone();
        let mut state = AsyncRecheck::default();
        assert!(state
            .start(
                move || {
                    ran.fetch_add(1, Ordering::SeqCst);
                    release_rx.recv().unwrap();
                    "old".into()
                },
                || {}
            )
            .unwrap());
        let old = state.last_generation;
        assert!(state.cancel());
        assert!(!state.cancel());
        for _ in 0..16 {
            assert!(state.is_busy());
            assert_eq!(state.poll(), None);
            assert!(!state
                .start(
                    || panic!("duplicate inspector"),
                    || panic!("duplicate wake")
                )
                .unwrap());
        }
        release_tx.send(()).unwrap();
        assert_eq!(finish(&mut state), None);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(state.start(|| "new".into(), || {}).unwrap());
        assert!(state.last_generation > old);
        assert_eq!(finish(&mut state), Some(Ok("new".into())));
    }

    #[test]
    fn failed_worker_releases_slot_and_reports_categorical_result() {
        let (wake_tx, wake_rx) = mpsc::channel();
        let mut state = AsyncRecheck::default();
        assert!(state
            .start(
                || panic!("synthetic inspector failure"),
                move || {
                    wake_tx.send(()).unwrap();
                }
            )
            .unwrap());
        wake_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(state.poll(), Some(Err(())));
        assert!(!state.is_busy());
        assert!(state.start(|| "retry".into(), || {}).unwrap());
        assert_eq!(finish(&mut state), Some(Ok("retry".into())));
    }

    #[test]
    fn disconnected_and_exhausted_states_fail_closed() {
        let mut state = AsyncRecheck::default();
        let (sender, receiver) = mpsc::channel();
        drop(sender);
        state.pending = Some(Pending {
            generation: 1,
            receiver,
        });
        state.active_generation = Some(1);
        state.last_generation = 1;
        assert_eq!(state.poll(), Some(Err(())));
        state.last_generation = u64::MAX;
        assert!(state
            .start(|| panic!("exhausted inspector"), || {})
            .is_err());
        assert!(!state.is_busy());
        assert_eq!(state.poll(), None);
    }
}
