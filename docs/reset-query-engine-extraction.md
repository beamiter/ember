# Ember reset/query engine extraction

Production DECSTR and DECRQSS execute in `jterm_engine::semantics::dispatch_local`
before host fallback. Ember no longer defines production `soft_reset`,
`handle_dcs_payload`, `handle_decrqss`, `decrqss_sgr_payload` or `sgr_color_params`.
It consumes the existing ordered `GlobalBackground` and `Reply` effects.

The matching is deliberately legacy-compatible: no-prefix CSI with exactly `!`
and final `p` (parameters ignored), or a completed DCS beginning with `$q`.
Unknown selectors return the unchanged failure bytes; unrelated events remain
unhandled without mutation. RIS, mode hooks, OSC, graphics, clipboard, identity,
command lifecycle and synchronized-output host lifecycle remain in Ember.
Frost's distinct soft reset is not migrated.

## Independent evidence

`reset-query-oracle-checkpoint.md` describes the immutable pre-extraction oracle.
Its test file, original frozen helper bodies, corpus and expected bytes are
unchanged, including every per-partition search revision. The separate
`reset_query_engine_test_adapter.rs` exists only under `cfg(test)`: it bridges
old helper-level invocations to the actual engine dispatcher and existing host
effect consumer. Its string accessors decode actual engine Reply framing, not
reimplement color/reset semantics. Production stream tests do not use this bridge.

The engine adds independently frozen reference tests covering rich state, all
u16 modes, both Kitty stacks, exact matching, every color variant/indexed value,
all underline styles and deterministic rendition combinations. Actual engine
reset/reply source mutations are checked against both standalone tests and the
unchanged Ember oracle. This is not a claim of native GUI or release acceptance.

Local candidate validation uses private Cargo manifests/locks and explicit
command-scoped core/engine overrides. Publication must remain core-first, followed
by exact consumer pin updates and separate published-pin validation.

## Published dependency integration

The production and font-group harness manifests/locks resolve core and engine
from published core `4b189cb7d55fe00d4ee39e74a143fc09d044c772`. The consumer
starts from Ember `dbfb6de080ca968c4a29adf6572f212610f54d40`, retaining its
observer recapture fix without edits. Standalone published-pin builds use no
local core overrides. Other terminal consumers need no pin change: the dispatcher
signature and effect variants are unchanged, and Frost does not call this
Ember-specific semantic dispatcher.
