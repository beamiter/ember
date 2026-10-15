use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use ember::terminal::TerminalState;
use std::hint::black_box;

/// Benchmark ANSI escape sequence parsing
fn bench_ansi_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("ansi_parsing");

    // Simple text output
    group.bench_function("simple_text", |b| {
        let mut terminal = TerminalState::new(80, 24);
        let data = b"Hello, World!\n".repeat(100);
        b.iter(|| {
            terminal.process_batch(black_box(&data));
        });
    });

    // Color codes (common in shell prompts)
    group.bench_function("color_codes", |b| {
        let mut terminal = TerminalState::new(80, 24);
        let data = b"\x1b[38;5;214muser\x1b[0m@\x1b[38;5;39mhost\x1b[0m $ ";
        b.iter(|| {
            terminal.process_batch(black_box(data));
        });
    });

    // Cursor movement
    group.bench_function("cursor_movement", |b| {
        let mut terminal = TerminalState::new(80, 24);
        let data = b"\x1b[H\x1b[2J\x1b[10;20H";
        b.iter(|| {
            terminal.process_batch(black_box(data));
        });
    });

    // Large output (like cat large_file)
    group.bench_function("large_output", |b| {
        let mut terminal = TerminalState::new(80, 24);
        let data = vec![b'X'; 32768]; // 32KB
        b.iter(|| {
            terminal.process_batch(black_box(&data));
        });
    });

    group.finish();
}

/// Benchmark grid operations
fn bench_grid_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("grid_operations");

    // Measure an offset update from a known interior history position, not
    // repeated clamping at the top/bottom. Setup and destruction are untimed.
    let scroll_fixture = || {
        let mut terminal = TerminalState::new(80, 24);
        terminal.process_batch(&b"history line\r\n".repeat(128));
        assert!(terminal.scrollback_len() >= 64);
        terminal.scroll_offset = 32;
        terminal
    };
    for (name, delta, expected_offset) in [("scroll_down", -3, 29), ("scroll_up", 3, 35)] {
        let mut probe = scroll_fixture();
        probe.scroll(delta);
        assert_eq!(probe.scroll_offset, expected_offset);

        group.bench_function(name, |b| {
            b.iter_batched_ref(
                scroll_fixture,
                |terminal| {
                    terminal.scroll(black_box(delta));
                    black_box(terminal.scroll_offset)
                },
                BatchSize::PerIteration,
            );
        });
    }

    group.bench_function("get_visible_cells", |b| {
        let mut terminal = TerminalState::new(80, 24);
        terminal.process_batch(&vec![b'X'; 2000]);
        b.iter(|| {
            black_box(terminal.get_visible_cells());
        });
    });

    // Streaming workload on a large grid: one row changes per iter, then read.
    // Exercises the incremental-copy fast path of get_visible_cells.
    group.bench_function("get_visible_cells_streaming", |b| {
        let mut terminal = TerminalState::new(200, 50);
        terminal.process_batch(&vec![b'X'; 8000]);
        b.iter(|| {
            terminal.process_batch(b"streaming line of output\r\n");
            black_box(terminal.get_visible_cells());
        });
    });

    // Cache-miss workload far back in history after a width change. Long
    // logical records create soft-wrapped scrollback rows at the source width;
    // alternating offsets prevents the one-entry visible-cells cache from
    // hiding the reflow cost.
    group.bench_function("get_visible_cells_deep_reflow_cache_miss", |b| {
        let mut terminal = TerminalState::new(80, 24);
        terminal.set_max_scrollback(10_000);
        let mut record = vec![b'X'; 136];
        record.extend_from_slice(b"\r\n");
        terminal.process_batch(&record.repeat(4_000));
        terminal.on_resize(120, 40);

        let deep_offset = terminal.scrollback_len().saturating_sub(100).max(2);
        let mut alternate = false;
        b.iter(|| {
            alternate = !alternate;
            terminal.scroll_offset = deep_offset - usize::from(alternate);
            black_box(terminal.get_visible_cells());
        });
    });

    group.finish();
}

/// Benchmark link detection
fn bench_link_detection(c: &mut Criterion) {
    let mut group = c.benchmark_group("link_detection");

    let text_with_links = [
        "https://github.com/rust-lang/rust",
        "Visit http://example.com for more info",
        "/usr/local/bin/cargo",
        "No links here just plain text",
    ];

    group.bench_function("detect_links", |b| {
        use ember::link::{LinkDetectionConfig, LinkDetector};
        let detector = LinkDetector::new(LinkDetectionConfig::default());

        b.iter(|| {
            for (line_idx, text) in text_with_links.iter().enumerate() {
                black_box(detector.detect_links_in_line(black_box(text), black_box(line_idx)));
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_ansi_parsing,
    bench_grid_operations,
    bench_link_detection
);

criterion_main!(benches);
