# Independent parser migration oracle

The pre-extraction Ember scanner produced the baseline before the production
adapter was replaced. The initial engine adapter then passed a comparison of
all full snapshots before these data were compacted. This is independent of the
new scanner's self-consistency/fragmentation tests.

The fixture has 916 cases/partitions, each with prefix and final snapshots,
covering the parser corpus, malformed CSI, ESC intermediates, invalid UTF-8,
incomplete controls, and OSC/DCS/APC cap edges. Ordinary snapshots have 69 lanes;
oversized cases omit pending-buffer text to avoid massive records. Application
wall clocks, batch-version counters and global image revisions are excluded by
the existing normalized snapshot oracle.

The original full JSON's SHA-256 was
`ae52ba0cb3b4bdc73f3749913d0282a78c583889cc69153f3f42661cde939a73`.
The 137 MiB JSON was compacted without rerunning either parser: each lane string
became `[UTF-8 byte length, lowercase 16-digit FNV-1a-64]`. The FNV initial value
is 14695981039346656037; each byte is XORed then multiplied by 1099511628211
modulo 2^64. These are deterministic regression fingerprints, not cryptographic
security checks. Case names, cuts and lane names remain readable.

`reference-scanner.rs.txt` and `reference-state.rs.txt` retain the original
implementation from the already-validated screen extraction milestone. They are
reference fixtures, never compiled or used by the production parser. Original
project licenses apply. The latter includes host helpers used by that scanner.

Run the regular `terminal::fragmentation_tests::parser_migration_recorded_oracle`
test to compare. It never updates expected values. Explicit capture requires
`EMBER_CAPTURE_PARSER_ORACLE=1` and a nonexistent expected file (`create_new`);
it must only be used on a deliberately restored original-scanner checkout, not
to bless a failing migrated implementation. Restore the original scanner fields,
watcher predicate and reference sources together when reproducing the historical
capture. The corpus and snapshot/capture procedure are in fragmentation_tests.rs.

## Local semantic dispatcher baseline

`pre-semantic-oracle.json` was captured from the production host dispatcher after
the structural-screen milestone, before the engine semantic dispatcher was
integrated. It is a separate baseline; the scanner and structural expected data
were not regenerated. Its SHA-256 is
`b21076900fb0ad83b76ae80e613150f1a0990dccce254dfb278f7bb40a3c1763`.

`reference-pre-semantic.rs.txt` preserves that host semantic implementation
(SHA-256 `caa4a34fb218eb60a42e4ec9607015c57c92ec6ec23d6cdb4049b4a96a4d1c47`).
The semantic baseline uses the same per-lane fingerprint scheme and records
prefix/final states for each generated case and cut. It covers permissive
private-prefix matching, omitted/zero parameters, malformed/extended colors,
SGR background side effects, command provenance, keyboard state, protected
cells, host-hook mixed mode lists and ordered replies. The test generates cases
explicitly in `semantic_migration_recorded_oracle`.

Run that test normally to compare only. `EMBER_CAPTURE_SEMANTIC_ORACLE=1` is an
explicit original-dispatcher capture path using `create_new`; it cannot overwrite
an existing baseline. Never capture from the new dispatcher to bless differences.
