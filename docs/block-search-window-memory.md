# E03 source-window identity allocation correction

Base dcb28569ea7b72b98a0a95c064ada3b5879d2ac8. This change fixes one production
memory defect, not full E03 acceptance. The existing pixel gate and independent
observer pilot remain failed and unchanged. Raw-worker cancellation/adoption,
complete allocator/RSS accounting, and fixed-machine interaction p95 <100ms
remain separate work.

## Defect and scope

`rebuild_block_search_cache` built an ID-to-sequence HashMap for every complete
retained command before applying the 8MiB source budget. Command-history capacity
can reach one million records. This auxiliary search allocation therefore grew
with the whole history even when the indexed window stayed fixed.

The new production source helper records sequence numbers only as the lazy
bounded iterator consumes sources. It pairs them by position with the builder's
actual accepted prefix, including its final capacity correction. Keys are cloned
from accepted complete IDs, not display snippets. One inspected but rejected
boundary record and all older records are absent. The cache normalization phase
may reject more sources; before sending its completed result, the worker moves
only accepted-cache keys into a new HashMap and drops the old bucket allocation.
Raw cursors require both ID and sequence membership; unmapped IDs use membership.
Their containers are shrunk after normalization. UI adoption repeats bounded HashMap membership checks, replacing its previous per-metadata linear cache scans. Hit lookup remains HashMap.get,
not a per-hit scan of all retained history. Cancellation that makes the existing
cache builder return None does not construct or send a partially trimmed window.
Existing generation/session/version adoption checks and UI operation order stay
unchanged.

## Reproducible allocation measurement

`tests/block_search_window_memory.rs` is a dedicated one-test process using a
System GlobalAlloc wrapper. It counts actual allocator requested live and peak
bytes, including all search-required string clones, temporary sequence storage,
and HashMap allocations. Fixture construction is outside the baseline. This
phase is synchronous source capture: no worker thread starts or is prewarmed in
the measured region, so no thread-start cost is silently excluded. The separate
raw-worker phase will need its own explicit thread-start/steady-state baseline.
System/malloc metadata, realloc-internal temporary copies, OS thread stacks and
RSS are NOT measured by requested Layout sizes; this is not a resident-memory
acceptance claim.

The fixed fixture has 10,000 and 100,000 completed-source representations, one
96-byte historical line per record, with fixed-width IDs. It does not pretend to
be a PTY or terminal-decoder benchmark. Both versions use the same actual
production bounded source builder and source-cloning iterator; the old comparator
contains the removed all-record map expression. The source window budget is 4096
bytes, chosen before measurement to isolate the metadata scaling defect.

The peak ceiling is derived before running: five source budgets conservatively
cover owned source payload, Vec growth/compaction and accepted ID copies; add one
largest rejected payload, sequence-Vec geometric growth, and power-of-two std
HashMap buckets at at most 7/8 load plus control bytes. The formula is executable
in window_ceiling and yields 23633 bytes for this fixture. It is a target-specific
requested-allocation bound, not a portable promise about malloc overhead.

A dependency-free offline probe extracts the exact production definitions from
block_mode.rs and the exact allocator/fixture support from the integration test,
prints the production fragment SHA256, compiles with rustc, and measures them:

    python3 scripts/measure-block-search-window-allocations.py

First measured result: 10k/100k records both accept 22 sources. Removed path live
714758/6029462 and peak 715591/6030295 bytes; corrected path live 5516 and peak
5772 bytes in both cases. Final identity-map reduction from 22 records to one reduced capacity 28 to 3 and released 1281 requested live bytes, with 148 bytes of additional bucket-overlap peak. These are source/map-only measurements. The probe does
not execute query matching, terminal decoding, worker races or app adoption.

## Hosted regression requirements

The full integration test additionally uses the actual shipped 8MiB source and
16MiB cache budgets on 100,000 source lines (9.6MB output alone), runs production
cache normalization and search, pages using accepted sequence identities until
ROW000017 is found, and checks cancellation returns no partial cache. It also
covers mixed captured/raw-source boundaries and narrower final-cache membership.
The test shares no copied implementation of the new builders. It is run by the
existing cargo test workflow in its own test binary; no new desktop diagnostic
step is added.

At freeze time full cargo compilation was unavailable locally because the pinned
jterm_core Git dependency was absent from the offline cache. Do not describe the
full integration test as passed until exact-head hosted CI executes it. The
standalone allocation probe, formatting and static checks are independently
reproducible now.
