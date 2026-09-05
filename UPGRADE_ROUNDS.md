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

Workflow convergence adds rounds 60–69 (2026-08-29):

60. **An undefaulted argument may no longer be left blank** — the family-wide
    defect this round exists for. `render` was always supposed to refuse a
    declared argument that has no default and no value, and ember, anvil and
    frost each unit-tested that guard; every UI in the family then pre-seeded
    each declared argument with `""`, so it never fired. `kill -9 {{pid}}` with
    an untouched Pid field inserted `kill -9 ` at the prompt. The contract is
    now stated once — an empty value is meaningful only if the file declares it,
    `default = ""` included — and enforced twice: in `render`, against the
    values map itself, so a caller that pre-seeds cannot seed past it, and in
    `ArgsForm`, which keeps Unset and Supplied apart in the type system so the
    dialog can mark outstanding rows with `*` and print `* needs a value` before
    Enter is pressed. Emptying a *defaulted* field stays a deliberate empty
    value; emptying an undefaulted one is a missing value. Whitespace-only
    counts as blank.
61. **The bundled example the rule would have missed** — `docker-tail-logs.yaml`
    declared `default: ""` for its required `container` argument, which under
    the new contract is an explicit empty value: round 60 would not have fired
    on the library ember ships, and the palette would have inserted
    `docker logs -f --tail 100 `. The empty default is removed and `container`
    is a required argument. Every other bundled argument declares a real
    default, so it is the only shipped file the rule touches.
62. **One bundled library across the family** — `diff -rq scripts/workflows` is
    now clean between anvil, ember, forge and frost. Ember's copy is unchanged
    apart from round 61; forge's had diverged in five of six files, and
    substantively in `find-large-files.yaml`, so name-keyed first-wins dedup
    resolved "Find large files" to a different command there than here.
63. **Engine moved into the shared core** — `src/workflows.rs` is now a
    241-line shim (127 before its tests) over `jterm_core::workflows`, and
    `src/workflow_picker.rs` an egui shell over the core's `WorkflowPicker` and
    `ArgsForm`; ember's workflow surface goes 1,151 → 590 lines. The five-tier
    search path, the bounded reader, both serde parsers, the eleven budgets,
    validation and the template engine leave the repo. All four terminals read
    the same files from the same directories out of four separately drifted
    copies — anvil 1,164 lines, forge 801, ember 1,151, frost 1,143 — so a
    divergence in one app was a divergence in what a user's file *meant*.
64. **Discovery policy is injected, not assumed** — the XDG backend (ember and
    frost ask the `dirs` crate, anvil and forge ask glib, and the fallback
    chains differ), the app segment, `LoadOrder` and the dev-tree root are all
    required arguments with no `Default`. `env!("CARGO_MANIFEST_DIR")` resolves
    against the compiling crate, so moving it into the core would have pointed
    all four apps at a directory that does not exist while their bundled-library
    tests kept passing. `SearchPathSpec::for_current_app` returns `Option`
    rather than silently resolving to the neutral `jterm` identity when
    `identity::init` has not run — which is every unit test.
65. **Load order is stated once** — the picker no longer re-sorts the entries it
    is handed. `workflows::LOAD_ORDER` is `ByName`, so the visible ordering is
    byte-for-byte what it was, but changing the loader now actually changes the
    overlay instead of being overwritten by a second `sort_by`.
66. **A padded argument name is rejected at load** — `name = "pid "` used to
    load clean and bind nothing, because placeholder names were trimmed and
    declared names were not: `{{ pid }}` rendered as the literal `{ pid }`, the
    missing-value check called the form complete, and the typed value was
    dropped between the dialog and the prompt. Both sides of that lookup are now
    held to the same spelling.
67. **`{{` and `}}` nest** — the close scan used to run to the end of the
    template, so an unterminated `{{` claimed a later placeholder's `}}`.
    `awk '{{print $1}' {{log}} | sort -u` rendered as
    `awk '{print $1}' access.log | sort -u`: a different, executable awk
    program. Nested JSON bodies such as `-d '{{"a":{{"b":1}}}}'` are unaffected.
68. **A skipped file is logged safely and visibly** — ember wrote
    `path.display()` and the parser's message raw into `log::warn!`, and a TOML
    error quotes the offending source line back verbatim, so a workflow file
    chose the ESC/BEL/bidi bytes that reached whatever tty was tailing the log.
    Both halves are now sanitised and bounded. The line itself stays: a workflow
    that vanishes from the palette without one is indistinguishable from one
    that was never installed.
69. **Current shared pin** — `jterm_core` advances to `790d06a`, the revision
    that introduces `workflows`; `jagent` is unchanged. `serde_yaml_ng` leaves
    ember's manifest and reappears under `jterm_core` in the lock, which is the
    lock's only other movement. No new crate enters the build, and
    `fuzzy-matcher` stays a direct dependency for the history picker and command
    palette.

Rounds 60–69 were checked with `cargo fmt --all -- --check`, `cargo clippy
--locked --all-targets --all-features -- -D warnings`, and `cargo test --locked`
(931 library tests, 1,234 binary tests and the native worker end-to-end test —
2,166 total — with zero failures) against the published `790d06a` core. The
workflow module's own suite went from 21 tests to 5 as sixteen engine duplicates
moved to the core, and the picker's from 6 to 8. `scripts/` is otherwise
untouched by this pass; `scripts/workflows/docker-tail-logs.yaml` is the one
data file it changes (round 61), and no GUI session was run — the dialog changes
are covered at the state layer only.

Workflow observability adds round 70 (2026-08-30):

70. **A refused file is visible without a log terminal** — the picker refresh
    now carries a bounded list of workflow-looking files the shared loader
    rejected and raises a one-line status toast when that path set changes.
    Paths and parser reasons are attacker-controlled display text, so both are
    escaped and capped before egui receives them. Reopening over the same broken
    set is silent; fixing it clears the snapshot so a later regression is
    announced again. This brings ember's synchronous picker to the refusal UX
    anvil already exposes without changing discovery or rendering semantics.

Toolchain gate maintenance adds round 71 (2026-08-30):

71. **Rust 1.96 keeps the strict Clippy gate green** — the stale SSH Files
    result guard now spells “missing pending request or mismatched token” with
    `Option::is_none_or`. This is the exact inverse of the former
    `!is_some_and` predicate, but avoids the new `nonminimal_bool` warning that
    made the repository's `-D warnings` release check fail before reaching this
    round's code.

Desktop-install parity with Anvil adds round 72 (2026-08-30):

72. **XDG data-directory symmetry** — install and uninstall now honor
    `XDG_DATA_HOME` and the explicit `--data-dir` override for launcher,
    AppStream, and icon paths; the same validated runtime base is prepended by
    `DESTDIR`, so packaging metadata never leaks the staging root.

Remote-probe target safety adds round 73 (2026-08-30):

73. **Dangling links still occupy remote names** — every remote creation,
    rename, copy, upload, and archive-extract probe now treats `-L` as occupied
    alongside `-e`. A dangling destination link can no longer make `mkfile`
    create its target outside the directory the user selected, or be silently
    replaced by another operation that promises no overwrite.

Nonblocking remote type discovery adds round 74 (2026-08-30):

74. **`stat` never reads a special leaf** — the probe now classifies links
    before directories and runs `wc` only for a non-link regular file. A FIFO,
    socket, or device still counts as an occupied destination (`f 0`) but can no
    longer stall a paste preflight waiting for content; a link to a directory
    also remains `l`, matching the listing protocol.

Root-directory transfer parity adds round 75 (2026-08-30):

75. **Root children have a real tar parent** — packing `/name` now normalizes
    the empty result of `${p%/*}` back to `/`, matching Anvil. Remote folders
    directly below the filesystem root therefore use `tar -C / name` instead
    of failing with an empty `-C` operand.

Local no-overwrite commits add round 76 (2026-08-30):

76. **The final rename is the existence check** — local Rename and downloaded
    file publication now commit with Linux `renameat2(RENAME_NOREPLACE)`.
    Creating the destination between the earlier friendly check and the commit
    can no longer be overwritten; unsupported kernels/filesystems fail closed.

Exclusive transfer staging adds round 77 (2026-08-30):

77. **Reserve before spawn** — every local transfer staging file now opens with
    owner-only `create_new` before its producer process starts. A preplanted
    hidden symlink is rejected rather than followed and cannot truncate its
    target; failure to reserve the name also cannot leave an unobserved child
    process behind, a permissive umask cannot expose partial content, and the
    published regular file retains mode 0600.

Transactional directory downloads add round 78 (2026-08-30):

78. **Extract privately, publish once** — downloaded tar streams now unpack in
    a fresh mode-0700 same-parent directory, require exactly one matching
    directory root, and commit it with `RENAME_NOREPLACE`. A destination that
    appears during the stream is neither merged with archive content nor
    recursively removed by error cleanup; only private staging is cleaned.

Bounded transfer staging names add round 79 (2026-08-30):

79. **Keep private names short and collision-safe** — file and tar staging now
    uses a fixed-size, process-unique basename instead of appending a suffix to
    the user-controlled entry name, so a valid 255-byte component no longer
    fails with `ENAMETOOLONG`. Each candidate is reserved owner-only with
    exclusive create; an occupied path is retried without unlinking it or
    starting a producer, and an internal candidate can never alias the source
    or final destination it protects. Cleanup is bound to the reserved inode,
    so a path replaced after reservation is left intact.

Exclusive relay scratch adds round 80 (2026-08-30):

80. **Reserve once across both relay legs** — remote-to-remote file and tar
    relays now hold one owner-only `StagedFile` from the source stream through
    the destination upload. The relay no longer derives and unconditionally
    removes a guessed `/tmp/ember-fs-relay-*` path before opening it; occupied
    candidates are skipped and RAII removes only the inode actually reserved.

Identity-bound extraction cleanup adds round 81 (2026-08-30):

81. **Pin the private extraction inode** — every directory-download staging
    root now retains an `O_DIRECTORY|O_NOFOLLOW` descriptor. Drop compares its
    device and inode with the current path before recursive removal, so a path
    moved away and replaced during extraction cannot redirect cleanup into the
    replacement tree; collision retry and mode-0700 isolation remain intact.

Atomic remote file publication adds round 82 (2026-08-30):

82. **Receive privately, hard-link once** — probe v5 `put` now reserves a short
    mode-0700 directory beside the destination, writes stdin to its mode-0600
    payload, and publishes with `ln -T`, whose hard-link creation atomically
    refuses an existing name. The old predictable `"$p.fspart.$$"` redirection
    could follow a planted symlink, exceed `NAME_MAX`, and the final check plus
    `mv` could still overwrite a destination created in between; all three
    paths now fail closed and private staging is cleaned without touching a
    colliding candidate.

Shared-core repin to `9f94f77` (jagent `bdc8023`) adds rounds 83–87
(2026-09-05):

83. **Durable output is bound to the Start the terminal saw** — the shared
    journal's `CompletedExecution` no longer carries a bare `id`; it carries an
    `ExecutionLifecycle` whose only constructor demands `id`, `session_id`,
    `seq` and `started_at_ms` from one OSC 133 `C` packet. Ember's decoder now
    reads those three Start-identity slots, honours them on `C` only — jsh
    emits none of them on `D`, and a token assembled at completion would name a
    generation nobody observed — and carries them on `CommandRecord` and
    `CompletedCommandOutput`. The identity is captured on the first `C` for a
    record and never rebound by a second. Ember's own `local:{sequence}` ids
    fail `is_valid_jsh_execution_id`, so a shell that reports no identity
    produces no journal row rather than a mis-keyed one.
84. **Every OSC 133 slot is single-assignment** — aliases name one semantic
    slot, and a repeated slot now degrades to absent instead of last-wins, so a
    second spelling of `id`, the command, the cwd or the duration cannot
    overwrite the honest first one. The truncation disclosure fails closed the
    other way: a repeat means truncated, because "absent" would re-enable
    replay of a partial command through the block menu's re-run gate. An
    unrecognised `cmd_truncated` value is inexact, not complete, and the
    disclosure is now honoured on `A` and `B` as well as `C` and `D`. A second
    exit slot reports no status at all, so `D;1;exit=0` can no longer turn a
    reported failure into a reported success.
85. **One set of OSC 133 budgets and one cwd rule** — per-field byte caps are
    `jterm_core::parser`'s constants rather than a fourth per-app set, so
    ember's decode and `CommandMeta` cannot disagree about whether a packet
    carried a field. Recorded cwds — from OSC 133 `C`/`D` and from OSC 7 — go
    through `is_valid_jsh_cwd`: 4 KiB, non-empty, no controls, no visual
    spoofing. That value is drawn in the pane header, cloned onto every command
    record and handed to a new session as its working directory, and it was
    stored with no length bound and no character rule. Execution ids are held
    to the shared parser's visual-spoofing rule too.
86. **PTY-authored text is bounded and sanitised where it leaves the terminal**
    — OSC 9/777 notification title and body are sanitised with the shared
    review-input class before they reach `notify-send` and a desktop
    notification server that applies none of this terminal's display rules; an
    OSC 0/2 window title is bounded where it enters terminal state rather than
    only where it is drawn, and is no longer deep-cloned under the terminal
    mutex once a frame. `/etc/hostname` is resolved once per process instead of
    on every OSC 7 naming a non-local host from the PTY parse loop, and the
    Block Search overlay reads its id-to-sequence join from the cache built for
    it instead of rebuilding it, with up to 1024 `String` clones, every frame.
87. **A supply-chain gate ember did not have** — `deny.toml` and
    `.cargo/audit.toml` state the licence, ban, source and advisory policy;
    `scripts/security-check.sh` runs the locked-graph, cargo-deny, RustSec
    (`--deny warnings`) and shellcheck passes that frost and forge already had,
    and CI's audit job runs through it. The installer, uninstaller and their
    path test are now shellchecked by the same entry point that ships them.
