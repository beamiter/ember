# jterm2 Performance Benchmarks

This directory contains performance benchmarks for critical code paths in jterm2.

## Running Benchmarks

```bash
# Run all benchmarks
cargo bench

# Run specific benchmark
cargo bench ansi_parsing

# Generate HTML report (in target/criterion/)
cargo bench -- --save-baseline main
```

## Benchmark Categories

### 1. ANSI Parsing (`ansi_parsing`)
- **simple_text**: Plain text output parsing
- **color_codes**: Color escape sequences (shell prompts)
- **cursor_movement**: Cursor positioning codes
- **large_output**: High-volume data processing (32KB)

### 2. Grid Operations (`grid_operations`)
- **scroll_down**: Scrolling backward in history
- **scroll_up**: Scrolling forward
- **get_visible_cells**: Cell extraction for rendering

### 3. String Operations (`string_operations`)
- **key_to_string**: Keyboard input conversion

### 4. Link Detection (`link_detection`)
- **detect_links**: URL and path detection in terminal output

## Comparing Performance

```bash
# Run baseline
cargo bench -- --save-baseline before

# Make changes...

# Compare
cargo bench -- --baseline before
```

## Interpreting Results

Criterion outputs:
- **time**: Average execution time
- **thrpt**: Throughput (operations/second)
- **change**: Performance delta from previous run

Look for:
- Regressions > 5% (investigate)
- Improvements > 10% (celebrate!)
- Variance > 20% (environment noise, re-run)

## Adding New Benchmarks

1. Add to `terminal_benchmark.rs`:
   ```rust
   fn bench_my_feature(c: &mut Criterion) {
       c.bench_function("my_test", |b| {
           b.iter(|| {
               black_box(my_function());
           });
       });
   }
   ```

2. Register in `criterion_group!`:
   ```rust
   criterion_group!(benches, ..., bench_my_feature);
   ```

## CI Integration

Benchmarks run on every PR to detect regressions:
- Fails if >10% slower
- Warns if >5% slower
- Reports improvements

## Performance Targets

| Benchmark | Target | Current |
|-----------|--------|---------|
| simple_text | <100 µs | TBD |
| color_codes | <50 µs | TBD |
| large_output | <5 ms | TBD |
| scroll | <1 ms | TBD |

Run `cargo bench` to populate current values.
