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

Command-correction convergence adds rounds 49–59 (2026-08-29):

49. **Engine moved into the shared core** — `src/command_correction.rs` is now
    an 889-line shim over `jterm_core::command_correction`. Ember's 2,335-line
    copy of classification, token extraction, ranking, the safety gate, the
    prompt, the reply parser, the helper-trust predicate, the probe layer and
    the request epoch machine is gone; all four terminals ran the same engine
    (7,852 lines between them), all four had drifted, and the core now carries
    their union in 3,937 lines. Only ember's egui card, its focus/arming rules
    and the PTY effect stayed.
50. **One helper-trust predicate for the family** — helper resolution goes
    through `jterm_core::helper::trusted_component`. Ember's own
    `owner == euid || mode & 0o022 != 0` called a binary owned by a *third*
    user at mode 0755 trusted, and resolution reached it by scanning the user's
    `PATH`, so a hostile `bash` earlier on `PATH` was spawned automatically by
    any failed command. Clamping the child's `PATH` was never a defence: the
    helper was itself the hostile binary.
51. **Helpers work again as root** — the same expression made `owner == euid`
    true for every root-owned system binary, so under `sudo ember` or in a
    container every helper was refused and `apt-cache pkgnames` could never
    run, silently. The shared predicate exempts euid 0 and keeps refusing a
    non-root user's own writable file.
52. **Pipe-to-interpreter rule** — `syntax_markers` only asks whether a marker
    is *present*, so against an original that already contained a pipe,
    appending `| sh` introduced no new marker and passed the superset check.
    Ember had no separate check at all. The shared rule splits the pipeline
    quote-aware and compares the set of interpreter stage names, pinned by a
    test against jagent's own lexer, so `|  sh`, `| /bin/sh`, `| zsh`,
    `| busybox sh` and `| xargs -n1 sh -c` are refused while
    `ls | gerp foo` → `ls | grep foo` is still offered.
53. **Consent is a construction-time type** — ember was the only copy that
    honoured `ai_share_command_context` before shipping the failed command, the
    cwd and up to 8 KiB of output, and that behaviour is now the family's
    `ContextSharing`, with no `Default` and a `ConsentProof` the prompt builder
    demands. Ember's observable behaviour is unchanged; the shim no longer
    conditionally builds the client but lets the policy refuse before the
    provider stage.
54. **The card renders only sanitised text** — the provider's `message` used to
    be interpolated raw into `ui.label` one line above the pre-filled,
    auto-focused command field, so a reply carrying U+202E could reverse the
    rendered order of the prose beside it. Every string now arrives through the
    engine's display accessors, collapsed to one line with controls and bidi
    replaced by U+FFFD, and inline feedback is bounded to 200 characters.
55. **Destructive drafts are labelled, and the action label matches the action**
    — `is_dangerous` gates only the direct-run decision, whose verified conjunct
    is false for every AI and target-output proposal, so `rm -rf ~/work` always
    reached the card and ember drew it in the same chrome as `git status`. The
    `⚠ destructive` label is recomputed after each frame's edit, and the primary
    button's label is now computed after that edit too, so it can no longer be
    one frame stale relative to what the button does.
56. **Only a shell-reported completion raises a card** — a `BoundaryInferred`
    block attributes stale scrollback and a guessed status to a command, so the
    classifier could read "command not found" out of the *previous* command's
    output. Ember's execution journal, Agent panel and long-command toast all
    already refused such a completion; this surface was the exception.
57. **The declared 16 KiB budget is enforced at classification** — a longer
    command line is declined rather than classified, ranked, probed and
    prompted about; ember relied on `review_input`'s 256 KiB cap. The provider
    payload's cwd handling changed shape with it: an over-4 KiB cwd is
    sanitised and truncated rather than replaced by `.`, and an absent cwd is
    sent empty rather than as `.`.
58. **A closed helper set** — the programs this surface may execute narrowed
    from the `{apt-cache, bash, sh, sleep, head}` name allow-list to two
    `TrustedHelper` constants. `sh`, `sleep` and `head` were in the production
    allow-list only so one unit test could exercise `run_capture`'s bounds; that
    test now lives in the core behind its own fixtures.
59. **Current shared security pin** — `jterm_core` advances to `badcce2`, the
    revision that introduces `command_correction`; `jagent` is unchanged at
    `f9383ec`, and the only other `Cargo.lock` movement is `fuzzy-matcher`
    appearing under `jterm_core`, a crate ember already depends on directly.

Rounds 49–59 were checked with `cargo fmt --all -- --check`, `cargo clippy
--locked --all-targets --all-features -- -D warnings`, and `cargo test --locked`
(947 library tests, 1,248 binary tests and the native worker end-to-end test —
2,196 total — with zero failures) against the published core revision. The
module's own suite went from 23 tests to 7 as twenty engine duplicates moved to
the core; `scripts/` is untouched by this pass.
