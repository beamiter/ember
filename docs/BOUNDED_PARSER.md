# Bounded parser policy

Ember opts into the shared engine's bounded parser. This is a scanner resource
policy, not a claim that every terminal protocol or host queue has a universal
memory or CPU bound.

## Visible behavior

- Complete and fragmented OSC, DCS, APC, SOS and PM payloads have the same
  4 MiB limit. The introducer and terminator are not payload. One pending ESC
  may be retained to recognize an ST split across reads; diagnostic snapshots
  also account for the introducer. Allocator capacity/bookkeeping is additional.
- After exceeding the limit, the entire remaining string stays opaque until
  ST, CAN or SUB; BEL also terminates OSC, but not other string kinds. Embedded
  escape commands in the rejected tail never become screen commands, clipboard
  requests, titles, notifications, or protocol queries.
- Rejected strings are not partially applied. Oversized or cancelled Kitty APC input no
  longer generates the previous payload-derived error response. Producers
  should use protocol-appropriate small chunks rather than oversized APCs.
- Any scanner rejection clears the current hyperlink conservatively. The
  parser does not inspect rejected payloads to guess whether they were OSC 8.
  This prevents an oversized link command from extending a previous safe link
  onto subsequent text. It does not erase earlier cells or reset other state.
- ESC intermediates are limited to eight bytes. CSI retains its existing fixed
  parameter/intermediate limits and truncation semantics; this change is not
  a complete CSI semantic-hardening claim.

Ordinary valid streams retain their existing semantics. The historical parser
migration oracle explicitly uses the legacy parser to preserve its original
recorded evidence, including former over-limit behavior. Production bounded
behavior instead has explicit malicious-tail/recovery tests and ordinary-stream
legacy comparisons at every split. Other semantic/color/reset frozen tests use
the production parser; no frozen fixture is regenerated for this policy change.

## Performance and validation scope

Complete, fresh strings within the payload limit can borrow the checked input
slice. The fast path stops at the first framing-sensitive byte; cancellation,
fresh ESC restarts, incomplete strings and overflows retain the same state
machine. This removes per-byte state moves/allocations for ordinary short OSCs.
Fragmented payloads still need bounded carry storage.

Release parser microbenchmarks are not complete application throughput or GUI
latency measurements. Native desktop, Vim and IME acceptance must be established
by their real workflow runs for the published commit, not inferred from scanner
tests, a successful build, or these microbenchmarks. The existing host PTY and
protocol-response backpressure remains separate from this scanner policy.

## In-flight Kitty transfers

APC overflow, CAN/SUB cancellation, or a fresh ESC abandoning an APC now
silently clears unfinished graphics assembly and its transfer metadata. A later
`m=0` cannot complete bytes retained before the rejected/cancelled APC. Completed
images and placements remain intact; a fresh valid transfer still works. The
scanner signals this without exposing rejected payload bytes or creating a reply.
