// Included only by the standalone --allocation-probe harness. Never built into Ember.
mod raw_worker_allocation_probe {
    use crate::terminal::search_worker::*;
    use crate::terminal::TerminalState;
    use parking_lot::Mutex;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    struct CountingAllocator;
    static LIVE: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);
    fn added(bytes: usize) {
        let live = LIVE.fetch_add(bytes, Ordering::SeqCst) + bytes;
        PEAK.fetch_max(live, Ordering::SeqCst);
    }
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let ptr = System.alloc(layout);
            if !ptr.is_null() {
                added(layout.size());
            }
            ptr
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let ptr = System.alloc_zeroed(layout);
            if !ptr.is_null() {
                added(layout.size());
            }
            ptr
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            System.dealloc(ptr, layout);
            LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            let next = System.realloc(ptr, layout, size);
            if !next.is_null() {
                if size >= layout.size() {
                    added(size - layout.size());
                } else {
                    LIVE.fetch_sub(layout.size() - size, Ordering::SeqCst);
                }
            }
            next
        }
    }
    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    #[test]
    fn raw_worker_allocation_probe_bounded_peak_with_old_window_retained() {
        let mut terminal = TerminalState::new(1024, 4);
        terminal.set_max_scrollback(1000);
        terminal.process_input(b"\x1b]133;A\x07\x1b]133;C;id=alloc\x07");
        let line = format!("{}\r\n", "x".repeat(1023));
        for _ in 0..200 {
            terminal.process_input(line.as_bytes());
        }
        terminal.process_input(b"\x1b]133;D;0;id=alloc\x07\r\n\r\n\r\n\r\n\r\n");
        let sequence = terminal.command_records().back().unwrap().sequence;
        let terminal = Arc::new(Mutex::new(terminal));
        // Stand in for an already displayed maximum-size cache. Existing
        // terminal/history and this old window remain live during replacement.
        let old_window = vec![0x41u8; 8 * 1024 * 1024];
        for (query, max_chunks) in [
            ("x".repeat(4096), 128),
            ("x".to_string(), 128),
            ("x".repeat(4096), 1),
            ("x".repeat(500), 1),
        ] {
            let base = LIVE.load(Ordering::SeqCst);
            PEAK.store(base, Ordering::SeqCst);
            let pool = Arc::new(SearchWorkerPermitPool::new(1));
            let limits = SearchWorkerLimits {
                chunk: crate::terminal::search_chunk::SearchChunkBudget {
                    rows: 32,
                    cells: 16_384,
                    bytes: 16_384,
                    encoded_bytes: 65_536,
                },
                max_chunks,
                ..Default::default()
            };
            let worker =
                SearchWorker::new(Arc::downgrade(&terminal), pool.clone(), limits).unwrap();
            let generation = worker
                .submit(sequence, "alloc", &query, SearchWorkerOptions::default())
                .unwrap();
            let started = Instant::now();
            let mut final_result = loop {
                if let Some(SearchUpdate::Finished(result)) = worker.take_update() {
                    break result;
                }
                assert!(started.elapsed() < Duration::from_secs(10));
                std::thread::sleep(Duration::from_millis(1));
            };
            assert_eq!(final_result.generation, generation);
            let mut previous_visible = None;
            if max_chunks == 1 {
                assert_eq!(final_result.status, SearchFinishStatus::WorkLimit);
                let continuation = final_result.continuation.take().unwrap();
                previous_visible = Some(final_result);
                let next_generation = worker.resume(continuation).unwrap();
                final_result = loop {
                    if let Some(SearchUpdate::Finished(result)) = worker.take_update() {
                        break result;
                    }
                    assert!(started.elapsed() < Duration::from_secs(10));
                    std::thread::sleep(Duration::from_millis(1));
                };
                assert_eq!(final_result.generation, next_generation);
            }
            assert!(matches!(
                final_result.status,
                SearchFinishStatus::Complete
                    | SearchFinishStatus::WorkLimit
                    | SearchFinishStatus::HitLimit
            ));
            assert!(final_result.hits.len() <= 500);
            let additional_peak = PEAK.load(Ordering::SeqCst).saturating_sub(base);
            assert!(
                additional_peak < 8 * 1024 * 1024,
                "replacement heap budget exceeded: {additional_peak}"
            );
            println!("worker allocator probe: query_bytes={}, status={:?}, chunks={}, ready_us={}, additional_peak={} bytes, old_window={} bytes, accounting={:?}", query.len(), final_result.status, final_result.progress.chunks, started.elapsed().as_micros(), additional_peak, old_window.len(), limits.payload_accounting());
            worker.close();
            drop(worker);
            while pool.active_workers() != 0 {
                assert!(started.elapsed() < Duration::from_secs(10));
                std::thread::sleep(Duration::from_millis(1));
            }
            drop(final_result);
            std::hint::black_box(previous_visible);
        }
        std::hint::black_box(old_window);
    }
}
