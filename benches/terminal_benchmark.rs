use criterion::{black_box, criterion_group, criterion_main, Criterion};
use jterm2::terminal::TerminalState;

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

    group.bench_function("scroll_down", |b| {
        let mut terminal = TerminalState::new(80, 24);
        // Fill with data first
        terminal.process_batch(&vec![b'X'; 2000]);
        b.iter(|| {
            terminal.scroll(black_box(-3));
        });
    });

    group.bench_function("scroll_up", |b| {
        let mut terminal = TerminalState::new(80, 24);
        terminal.process_batch(&vec![b'X'; 2000]);
        b.iter(|| {
            terminal.scroll(black_box(3));
        });
    });

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

    group.finish();
}

/// Benchmark string conversion
fn bench_string_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("string_operations");

    // Benchmark the optimized key_to_string (now returns &'static str)
    group.bench_function("key_to_string", |b| {
        use egui::Key;
        let keys = vec![
            Key::A, Key::B, Key::F1, Key::Enter, Key::Escape,
            Key::ArrowUp, Key::Home, Key::Delete
        ];
        b.iter(|| {
            for key in &keys {
                // This would call the internal function if exported
                black_box(key);
            }
        });
    });

    group.finish();
}

/// Benchmark link detection
fn bench_link_detection(c: &mut Criterion) {
    let mut group = c.benchmark_group("link_detection");

    let text_with_links = vec![
        "https://github.com/rust-lang/rust",
        "Visit http://example.com for more info",
        "/usr/local/bin/cargo",
        "No links here just plain text",
    ];

    group.bench_function("detect_links", |b| {
        use jterm2::link::{LinkDetector, LinkDetectionConfig};
        let _detector = LinkDetector::new(LinkDetectionConfig::default());

        b.iter(|| {
            for text in &text_with_links {
                black_box(text);
                // Would call detector.detect_links if the API was exposed
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_ansi_parsing,
    bench_grid_operations,
    bench_string_operations,
    bench_link_detection
);

criterion_main!(benches);
