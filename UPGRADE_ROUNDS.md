# Ember upgrade rounds

Rounds 1–10 record the preceding pass; this pass's additional twenty rounds
are numbered 11–30.

1. **Prefix validation** — absolute runtime prefixes reject controls and lexical
   parent traversal while allowing spaces, Unicode, repeated separators, and
   harmless dot components.
2. **Binary-directory validation** — install/uninstall share the boundary and
   explicit empty `--bin-dir` no longer falls back silently.
3. **DESTDIR confinement** — unsafe staging roots fail before build or writes,
   preventing concatenated paths from escaping the package tree.
4. **Explicit root staging** — `DESTDIR=/` retains staged cache/PATH behavior
   after normalization.
5. **Dependency preflight** — transactional and desktop helper commands are
   checked before building or changing the destination.
6. **Atomic binary replacement** — a mode-`0755` temporary is created beside
   the target and renamed without following a hostile destination symlink.
7. **Pre-commit cleanup** — the active temporary is registered in an EXIT trap
   and cleared only after rename commits the binary; later resource failures do
   not imply binary rollback.
8. **Atomic desktop replacement** — desktop output uses an unpredictable
   same-directory temporary rather than a predictable `.new` pathname.
9. **Remote-host semantic gate** — one application gate combines visual-text
   safety and byte budgets with the shared argv/session/deploy/path schema;
   app text checks precede shared diagnostics, and picker, connection argv, and
   remote filesystem operations all re-run it.
10. **Non-destructive resource bound** — invalid drafts and entries after the
    first 128 round-trip unchanged for repair, while runtime surfaces explain
    why they are unavailable and Settings disables Add at the active limit.
11. **Install-source preflight** — desktop, metadata, SVG, and both PNG inputs
    are verified as readable non-symlink regular files before build/write.
12. **Non-empty descriptor input** — a zero-byte prebuilt binary is rejected
    before the old executable changes.
13. **Scoped staging ancestry** — non-root DESTDIR spellings are normalized and
    their full existing component chains reject disguised symlink roots before
    install/uninstall; normal host prefix links remain supported. This is a
    point-in-time preflight rather than a concurrent-race claim.
14. **Atomic AppStream/SVG commit** — public assets are staged beside their
    targets with mode 0644 and renamed into place.
15. **Atomic raster icons** — both shipped PNG sizes share that commit boundary.
16. **Desktop command structure gate** — canonical Exec/TryExec cardinality and
    absence of alternate commands are checked before rename.
17. **Unset-PATH resilience** — successful installs no longer trip nounset in
    post-install advice.
18. **Hostile-resource contracts** — tests cover zero-byte input, DESTDIR
    ancestor escape, and destination-link replacement without victim writes.
19. **Index-neutral private errors** — helpers lacking a list index use a safe
    neutral host label instead of inventing “remote host #1”.
20. **Bounded picker rows** — at most 256 drafts render per frame; entry 129 is
    still visible and later drafts are explicitly reported as retained.
21. **Bounded settings rows** — the Remote editor uses the same rendering cap
    without shrinking the underlying draft vector.
22. **Active selector cap** — file-tree and tab-menu execution choices expose
    only the first 128 profiles.
23. **Borrow-only picker input** — rendering no longer clones every full remote
    draft on every frame.
24. **Actionable save summary** — success feedback separately counts invalid
    active drafts and over-limit retained drafts.
25. **Shared problem accounting** — normalization and UI diagnostics use one
    active-invalid/inactive-retained calculation.
26. **Real disk round trip** — an atomic private config save/reload proves an
    invalid draft and the 129th entry both survive exactly.
27. **Neutral-label regression** — the disk fixture also proves an empty
    name/target produces bounded non-indexed runtime text.
28. **No-silent-truncation notice** — lists beyond 256 remain saved byte-for-byte
    unchanged and the picker/editor disclose their omitted count.
29. **Consumer fail-closed evidence** — remote-fs tests reject relative paths,
    missing/high indexes, and invalid runtime hosts before spawn; gate tests
    prove oversized/RLO deploy drafts are rejected without echoing raw bytes;
    Settings preflights deploy/user/argv budgets by reference before cloning a
    bounded draft for shared validation.
30. **Documented repair contract** — README now specifies bounded rendering,
    active limits, retained drafts, and atomic public-resource installation.

Block Mode convergence continues with rounds 31–37:

31. **Stable projected endpoints** — normal text selections retain exact
    raw-cell or empty-row identity and re-resolve after a compatible
    collapsed-plan update; a trimmed trailing-blank cell fails closed.
32. **Selection fail-closed matrix** — column selection, width changes,
    effective-collapse changes, evicted identities and ambiguous reflow still
    clear instead of silently selecting different terminal bytes.
33. **Walkable block search** — `Shift+Enter` reveals a hit, selects the next,
    and keeps the query open; plain Enter remains reveal-and-close.
34. **Virtual result widgets** — fixed-height virtualization renders only the
    visible result rows while keyboard, wheel and scrollbar navigation still
    span all 500 hits; stationary hover cannot steal keyboard selection, and a
    retained background-refresh anchor does not recenter pointer browsing.
35. **Actionable integration diagnosis** — empty block actions and search tell
    a marked-but-empty pane from a shell that never emitted OSC 133, and only
    the latter points to the jsh installer.
36. **Single-pass cold planning** — history/grid layouts now stream directly
    into placements and reuse logical-group scratch without changing the
    incremental projection or cache identity contracts.
37. **Current shared security pin** — the exact `jterm_core` revision advances
    to `0f47569`, adopting AI origin/credential/no-proxy validation while retaining the
    established block lifecycle API.

AI chat store convergence adds rounds 38–48 (2026-08-29):

38. **Store moved into the shared core** — `src/ai_chat_store.rs` is now a
    76-line shim over `jterm_core::ai::chat_store`; ember's ~900-line port of
    anvil's multi-chat store and the tests pinning it are gone, and all four
    terminals run the same 1,888-line union (47 tests).
39. **Compaction before serialising** — `persist_encoded` builds its snapshot
    with `snapshot_for_persistence`, so a library that outgrew the schema's
    envelope compacts on the way out instead of returning `SnapshotInvalid`
    forever and taking every later chat down with it.
40. **Truncation travels back to the live chat** — the durable view is a clone,
    so the written snapshot is folded back with `sync_truncation_markers`; the
    chat row and status line say what the file could not keep instead of a
    short saved copy looking complete.
41. **Busy-chat policy is a construction-time choice** — `new_store` and
    `restore_store` both pin `BusyChatPolicy::Refuse` (ember's panel has no
    cancel-then-mutate step) and a test pins both paths, while the persistence
    clone uses `recover_retry_payload_detaching` so flattening the copy leaves
    the live request and its draft alone.
42. **Save failures are visible** — `PersistOutcome::Failed` carries a sentence
    to the panel instead of only a `log::warn` a GUI launch never shows, and a
    window that will never write — a non-owner instance, or a blocked restore
    refusing to overwrite an unreadable file — says so above the conversation.
43. **The suggestion card can be dismissed** — `SuggestionUiOutcome` gained
    `Dismissed`; Dismiss, Escape and the window ✕ were all no-ops because only
    a successful insert or closing the bound terminal cleared the session and
    `show` re-rendered the card next frame. Dismissing now also cancels the
    in-flight request nothing would otherwise harvest.
44. **Per-session suggestion generation** — `generation` was the constant `1`
    on every session, so the reply guard and `complete_accept` both compared
    `x == x` and every card shared one egui window id; a monotonic counter
    makes a stale reply or accept-effect provable.
45. **Ask-AI palette mode fails closed** — `?` replaces the command list with
    one AI row; an empty request accepts nothing rather than dispatching the
    highlighted command, an oversized one is refused with the reason on screen,
    and the raw query is trimmed first so a leading space cannot silently drop
    back into ordinary command matching.
46. **Family AI chord** — `Ctrl+Shift+Alt+A` opens the read-only chats library
    as `ai_chat:toggle` and `agent:toggle` moves to the family's `Ctrl+Alt+G`;
    ember was the one terminal where that gesture opened the panel that runs
    commands. A test pins the pair, the singular id, and `jterm_core`'s
    canonical storage/display spelling.
47. **History cwd bound matches the shared writer** — `MAX_HISTORY_CWD_BYTES`
    rises from 4 KiB to the 16 KiB `jterm_core::command_history` writes with, so
    a deep directory is no longer silently persisted as `cwd: None` and a
    sibling terminal's cwd is no longer erased on read.
48. **Neighbour-timing spawn retry** — the PTY hangup test retries briefly on
    `ETXTBSY` instead of failing the run when a sibling test forks while its
    script is still open for writing.

Verification: `bash scripts/test-install-paths.sh`, Ember config tests, and the
full workspace formatting/check/Clippy/test gates. Rounds 38–48 were checked
with `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets
--all-features -- -D warnings`, and `cargo test` (1,264 unit tests plus the
native worker end-to-end test, zero failures); `scripts/` is untouched by this
pass.
