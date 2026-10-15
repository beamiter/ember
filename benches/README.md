# ember Performance Benchmarks

This directory contains performance benchmarks for critical code paths in ember.

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
- **scroll_down**: Update the offset toward newer history (32 to 29)
- **scroll_up**: Update the offset toward older history (32 to 35)
- **get_visible_cells**: Cell extraction for rendering

Scroll benchmarks start from a fresh populated history on each iteration. Fixture
setup and destruction are outside the measured routine. They measure only an
offset update, not cell projection or rendering; timer overhead can be significant.

### 3. Link Detection (`link_detection`)
- **detect_links**: Production `LinkDetector::detect_links_in_line` on four input
  strings, including allocation and disposal of returned links. Detector creation
  is outside the measured loop.

The former `key_to_string` placeholder was removed because it did not call the
conversion function. Previous placeholder link-detection results are also invalid;
do not compare them with this production-path benchmark.

## Comparing Performance

```bash
# Run baseline
cargo bench -- --save-baseline before

# Make changes...

# Compare
cargo bench -- --baseline before
```

## Interpreting Results

Criterion reports timing estimates and changes against a saved baseline. Throughput
is only available for groups that explicitly configure it; these groups do not.

Use the same source revision, profile, compiler, workload and machine for repeated
runs. Record raw results and variability before choosing a regression threshold.
Shared cloud hosts can add scheduling and frequency noise. There is no universal
percentage threshold established by this suite.

ANSI benchmarks reuse terminal state across iterations and measure steady-state
streaming. The unchanged `get_visible_cells` case can exercise the warm cache;
`get_visible_cells_streaming` updates a row before reading, and
`get_visible_cells_deep_reflow_cache_miss` alternates deep-history offsets after a
resize. These CPU microbenchmarks do not measure input-to-photon latency, GPU
rendering, idle CPU, power usage or overall terminal performance.

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

The current CI workflow has no dedicated benchmark regression gate. Run and save
repeated measurements explicitly before proposing a baseline or threshold. Debug
profile smoke runs validate the harness only; do not compare them with release
performance measurements.
