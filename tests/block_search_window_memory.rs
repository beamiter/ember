//! Isolated-process real allocation accounting for production snapshot windows.
//! Rust allocator requested bytes, not malloc metadata/RSS or fixed-machine p95.
use ember::block_mode::{
    bounded_block_search_sources, bounded_sequenced_block_search_sources,
    build_block_search_cache_cancellable, finalized_block_search_sequences, search_blocks,
    BlockSearchSource,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
fn add(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::SeqCst) + bytes;
    PEAK.fetch_max(live, Ordering::SeqCst);
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if !p.is_null() {
            add(layout.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(layout);
        if !p.is_null() {
            add(layout.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        System.dealloc(p, layout);
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
    }
    unsafe fn realloc(&self, p: *mut u8, old: Layout, size: usize) -> *mut u8 {
        let new = System.realloc(p, old, size);
        if !new.is_null() {
            if size >= old.size() {
                add(size - old.size());
            } else {
                LIVE.fetch_sub(old.size() - size, Ordering::SeqCst);
            }
        }
        new
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

struct Record {
    id: String,
    sequence: u64,
    output: String,
}
fn fixture(count: usize) -> Vec<Record> {
    (0..count)
        .map(|i| Record {
            id: format!("record-{i:010}"),
            sequence: i as u64,
            output: format!("ROW{i:06} {}\n", "x".repeat(85)),
        })
        .collect()
}
fn sources(records: &[Record]) -> impl Iterator<Item = (u64, BlockSearchSource)> + '_ {
    records.iter().rev().map(|r| {
        (
            r.sequence,
            BlockSearchSource::new(r.id.clone(), None, Some(r.output.clone())),
        )
    })
}
fn measure<T>(f: impl FnOnce() -> T) -> (T, usize, usize) {
    let baseline = LIVE.load(Ordering::SeqCst);
    PEAK.store(baseline, Ordering::SeqCst);
    let value = f();
    let peak = PEAK.load(Ordering::SeqCst).saturating_sub(baseline);
    let live = LIVE.load(Ordering::SeqCst).saturating_sub(baseline);
    (value, live, peak)
}

// Fixed before execution. Bounds the bounded source's transient Vec growth,
// accepted owned payload and one rejected record; sequence Vec geometric growth;
// and std HashMap's power-of-two buckets at <=7/8 load plus control bytes.
// This is requested allocation accounting, not allocator metadata or RSS.
fn window_ceiling(source_budget: usize, largest_source_payload: usize) -> usize {
    let n = source_budget / std::mem::size_of::<BlockSearchSource>();
    let buckets = ((n * 8).div_ceil(7)).next_power_of_two().max(4);
    5 * source_budget
        + largest_source_payload
        + 2 * (n + 1) * std::mem::size_of::<u64>()
        + buckets * (std::mem::size_of::<(String, u64)>() + 1)
        + 16
}

#[test]
fn production_window_allocations_do_not_scale_with_retained_history() {
    // The rejected boundary record and raw sources have the same positional
    // identity rules. A smaller normalized cache must shed its excess map keys.
    let mixed = vec![
        (
            900,
            BlockSearchSource::new("new-raw".into(), Some("needle".into()), None),
        ),
        (
            800,
            BlockSearchSource::new("captured".into(), None, Some("needle".repeat(8))),
        ),
        (
            700,
            BlockSearchSource::new("excluded-raw".into(), None, None),
        ),
    ];
    let exact_two = 2 * std::mem::size_of::<BlockSearchSource>() + 7 + 6 + 8 + 6 * 8;
    let (snapshot, map) = bounded_sequenced_block_search_sources(mixed, exact_two);
    assert_eq!(snapshot.sources.len(), 2);
    assert_eq!(map.get("new-raw"), Some(&900));
    assert_eq!(map.get("captured"), Some(&800));
    assert!(!map.contains_key("excluded-raw"));
    let cache_limit =
        2 * std::mem::size_of::<ember::block_mode::CachedBlockSearchRecord>() + 7 + 6 + 6;
    let build = build_block_search_cache_cancellable(snapshot, cache_limit, || false).unwrap();
    assert_eq!(build.records.len(), 1);
    assert_eq!(build.records[0].record_id, "new-raw");
    let final_map = finalized_block_search_sequences(
        map,
        build.records.iter().map(|record| record.record_id.as_str()),
    );
    assert_eq!(final_map.len(), 1);
    assert_eq!(final_map["new-raw"], 900);
    assert!(!final_map.contains_key("captured"));
    let oversized_map = (0..1024)
        .map(|i| (format!("id-{i}"), i))
        .collect::<HashMap<_, _>>();
    let old_capacity = oversized_map.capacity();
    let final_map = finalized_block_search_sequences(oversized_map, ["id-17"]);
    assert_eq!(final_map["id-17"], 17);
    assert!(
        final_map.capacity() < old_capacity,
        "discarded window bucket storage must be freed"
    );

    // Exactly one test in this binary: no concurrently executing test allocator
    // traffic. Fixture and formatting precede baseline; all search clones count.
    const BUDGET: usize = 4096;
    let ceiling = window_ceiling(BUDGET, 17 + 96);
    let mut previous_old_live = 0;
    let mut previous_new_peak = None;
    for count in [10_000, 100_000] {
        let records = fixture(count);
        assert!(records.iter().map(|r| r.output.len()).sum::<usize>() > BUDGET);
        let ((old_source, old_map), old_live, old_peak) = measure(|| {
            // Exact removed production expression: full complete-history index
            // before the same lazy bounded source builder.
            let map = records
                .iter()
                .map(|r| (r.id.clone(), r.sequence))
                .collect::<HashMap<_, _>>();
            (
                bounded_block_search_sources(sources(&records).map(|(_, s)| s), BUDGET),
                map,
            )
        });
        let accepted: Vec<_> = old_source
            .sources
            .iter()
            .map(|s| s.record_id.clone())
            .collect();
        assert_eq!(old_map.len(), count);
        assert!(old_live > previous_old_live);
        previous_old_live = old_live;
        drop((old_source, old_map));
        let ((snapshot, map), new_live, new_peak) =
            measure(|| bounded_sequenced_block_search_sources(sources(&records), BUDGET));
        assert!(
            new_peak <= ceiling,
            "peak {new_peak} exceeds derived ceiling {ceiling}"
        );
        if let Some(previous) = previous_new_peak {
            assert_eq!(new_peak, previous);
        }
        previous_new_peak = Some(new_peak);
        assert!(snapshot.older_not_indexed);
        assert!(snapshot.resident_bytes <= BUDGET);
        assert_eq!(
            snapshot
                .sources
                .iter()
                .map(|s| &s.record_id)
                .collect::<Vec<_>>(),
            accepted.iter().collect::<Vec<_>>()
        );
        assert_eq!(map.len(), snapshot.sources.len());
        for source in &snapshot.sources {
            let i = source.record_id[7..].parse::<usize>().unwrap();
            assert_eq!(map[&source.record_id], records[i].sequence);
        }
        let build = build_block_search_cache_cancellable(snapshot, BUDGET * 2, || false).unwrap();
        let map = finalized_block_search_sequences(
            map,
            build.records.iter().map(|record| record.record_id.as_str()),
        );
        assert_eq!(map.len(), build.records.len());
        let results = search_blocks(build.records.iter().rev(), "ROW");
        assert!(!results.hits.is_empty());
        for hit in &results.hits {
            assert!(map.contains_key(&hit.record_id));
        }
        let (cancelled, _) = bounded_sequenced_block_search_sources(sources(&records), BUDGET);
        let checks = std::cell::Cell::new(0);
        assert!(
            build_block_search_cache_cancellable(cancelled, BUDGET * 2, || {
                checks.set(checks.get() + 1);
                checks.get() == 3
            })
            .is_none(),
            "cancelled normalization must never return a partial cache"
        );
        println!("source_records={count} old_live={old_live} old_peak={old_peak} new_live={new_live} new_peak={new_peak} derived_ceiling={ceiling}; snapshot path only, not RSS/p95/native navigation");
    }
    let records = fixture(100_000); // 100,000 real source lines, 9.6MB output alone.
    let source_budget = ember::block_mode::BLOCK_SEARCH_SOURCE_MAX_BYTES;
    let cache_budget = ember::block_mode::BLOCK_SEARCH_CACHE_MAX_BYTES;
    assert!(records.iter().map(|r| r.output.len()).sum::<usize>() > source_budget);
    let (snapshot, map) = bounded_sequenced_block_search_sources(sources(&records), source_budget);
    assert!(snapshot.older_not_indexed);
    assert!(snapshot.resident_bytes <= source_budget);
    let build = build_block_search_cache_cancellable(snapshot, cache_budget, || false).unwrap();
    assert!(build.resident_bytes <= cache_budget);
    let map = finalized_block_search_sequences(
        map,
        build.records.iter().map(|record| record.record_id.as_str()),
    );
    assert_eq!(map.len(), build.records.len());
    assert!(!search_blocks(build.records.iter().rev(), "ROW099999")
        .hits
        .is_empty());
    // A second window's cursor comes from the final accepted map, never an
    // incomplete/truncated identifier or an all-history rescan for each hit.
    let first = build.records.first().unwrap();
    let boundary = map[&first.record_id];
    let mut boundary = boundary;
    let mut found = false;
    for _ in 0..4 {
        let (earlier, earlier_map) = bounded_sequenced_block_search_sources(
            sources(&records[..boundary as usize]),
            source_budget,
        );
        assert!(earlier
            .sources
            .iter()
            .all(|record| earlier_map[&record.record_id] < boundary));
        let earlier =
            build_block_search_cache_cancellable(earlier, cache_budget, || false).unwrap();
        if !search_blocks(earlier.records.iter().rev(), "ROW000017")
            .hits
            .is_empty()
        {
            found = true;
            break;
        }
        let next = earlier_map[&earlier
            .records
            .first()
            .expect("window must advance")
            .record_id];
        assert!(next < boundary);
        boundary = next;
    }
    assert!(
        found,
        "early row must remain reachable through bounded windows"
    );
}
