# Structural operation migration oracle

`structural_before_engine.json` contains 106 exact production snapshots captured
before Ember started calling the engine's region operations. It was captured and
then independently replayed against the unchanged production implementation before
integration. It is not a second implementation of the expected behavior.

Original source SHA-256:

- `src/terminal/state.rs`: `1a20820df60f8c26fbe337f7f3db0cda0228426cc2dbbab9a39e4950256bcebc`
- `src/terminal/parser.rs`: `c1212dfb885c7f1e6f007740ba4de4214fc078bacbeb47c4fb5694d450b18f5f`

The JSON interns identical lane values: `cases[case][lane]` indexes `values`.
Every expected value is retained, not replaced with a checksum. Source hashes
are provenance, not the regression comparison.

Coverage includes all valid regions on a five-row grid, SU/SD and IL/DL counts,
out-of-margin line edits, the legacy IL/DL wrap-flag distinction, BCE and wide
cells, original/fresh/exhausted row identities, primary/alternate/synchronized/CLI
archive policy, copied snapshots versus actual transfers, capped-history selection
rebasing, finished and active command ownership, Kitty placement motion, and
LF/IND/RI/deferred-wrap entry points. Snapshots include cache presence and exact
revision counters, including the two identity updates per real history append.
Clocks, process-global image revisions and source epochs are intentionally absent.

`structural_operations_match_pre_extraction_production` compares each lane with
these fixed values. The ignored `print_structural_baseline` helper only prints
snapshots for an explicitly selected build; it never rewrites the fixture and
must not be used to bless a regression by replacing these original captures.

No IL/DL wrap-flag correction is included in this extraction.
