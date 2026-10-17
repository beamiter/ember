#!/usr/bin/env python3
"""Dependency-free allocator probe of exact production source-builder definitions.

Not the full integration test: no terminal worker, query or app adoption runs here.
No source implementation is replaced with a model. Hosted cargo tests run the full
integration binary; this probe only permits auditing allocation arithmetic offline.
"""
import hashlib
import os
from pathlib import Path
import re
import subprocess
import tempfile

root = Path(__file__).resolve().parent.parent
source = (root / 'src/block_mode.rs').read_text()
start = source.index('/// Owned newest-first source moved into the bounded index builder.')
end = source.index("/// [`search_blocks`] result:", start)
normalization_start = source.index('pub(crate) fn block_search_literal_scalar')
normalization_end = source.index('/// Display clip for a hit', normalization_start)
production = source[normalization_start:normalization_end] + source[start:end]
test = (root / 'tests/block_search_window_memory.rs').read_text()
support = test[:test.index('#[test]')]
support = re.sub(r'use ember::block_mode::\{.*?\};\n', '', support, flags=re.S)
boundary_start = test.index('    // The rejected boundary record')
boundary_end = test.index('    // Exactly one test', boundary_start)
boundary_checks = test[boundary_start:boundary_end].replace('ember::block_mode::', '')
main = r'''
fn main() {
    const BUDGET: usize = 4096;
    let ceiling = window_ceiling(BUDGET, 17 + 96);
    let mut prior_new = None;
    for count in [10_000, 100_000] {
        let records = fixture(count);
        let ((old_sources, old_map), old_live, old_peak) = measure(|| {
            let map = records.iter().map(|r| (r.id.clone(), r.sequence)).collect::<HashMap<_, _>>();
            (bounded_block_search_sources(sources(&records).map(|(_, s)| s), BUDGET), map)
        });
        let accepted: Vec<_> = old_sources.sources.iter().map(|s| s.record_id.clone()).collect();
        drop((old_sources, old_map));
        let ((snapshot, map), live, peak) = measure(|| bounded_sequenced_block_search_sources(sources(&records), BUDGET));
        assert!(peak <= ceiling);
        if let Some(prior) = prior_new { assert_eq!(peak, prior); }
        prior_new = Some(peak);
        assert_eq!(map.len(), snapshot.sources.len());
        assert_eq!(snapshot.sources.iter().map(|s| &s.record_id).collect::<Vec<_>>(), accepted.iter().collect::<Vec<_>>());
        println!("records={count} accepted={} old_live={old_live} old_peak={old_peak} new_live={live} new_peak={peak} predeclared_ceiling={ceiling}", map.len());
        let original_capacity = map.capacity();
        let final_id = snapshot.sources[0].record_id.as_str();
        let before = LIVE.load(Ordering::SeqCst);
        let (final_map, _, overlap_peak) = measure(|| finalized_block_search_sequences(map, [final_id]));
        let after = LIVE.load(Ordering::SeqCst);
        assert_eq!(final_map.len(), 1);
        assert!(final_map.capacity() < original_capacity);
        assert!(after < before);
        println!("final-cache identity subset: old_capacity={original_capacity} new_capacity={} requested_live_released={} additional_bucket_overlap_peak={overlap_peak}", final_map.capacity(), before-after);
    }
}
'''
main = main.replace('    const BUDGET: usize = 4096;', boundary_checks + '\n    const BUDGET: usize = 4096;', 1)
print('production_fragment_sha256=' + hashlib.sha256(production.encode()).hexdigest(), flush=True)
print('Scope: exact production snapshot source/map allocation only; not full integration, RSS, worker or p95.', flush=True)
with tempfile.TemporaryDirectory(prefix='ember-window-allocation-') as tmp:
    tmp = Path(tmp)
    (tmp / 'probe.rs').write_text(support + '\n' + production + '\n' + main)
    subprocess.run([os.environ.get('RUSTC', 'rustc'), '--edition=2021', '-O', str(tmp / 'probe.rs'), '-o', str(tmp / 'probe')], check=True)
    subprocess.run([str(tmp / 'probe')], check=True)
