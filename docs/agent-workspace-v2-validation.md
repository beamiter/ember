# Agent Workspace v2 validation

Verified on 2026-11-02 on Linux with Rust 1.99.1. These checks use synthetic
workspace state, empty application/provider homes, and an ordinary test UID.
No real provider transcripts, credentials, models, or remote SSH server were
used. Development builds used the adjacent shared-core source. The final locked
Git-source type check is recorded separately below; it does not establish that
the proposed commit is available from remote GitHub master.

## Compiled and executed checks

| Gate | Result |
| --- | --- |
| `cargo check --tests` | Passed |
| Debug app build, including the final portrait-header and evidence-reader changes | Passed |
| Search request/evidence reducers | 4 passed |
| Revisioned Context/handoff reducers | 4 passed |
| Current-task metadata/result reducers | 6 passed |
| Read-only remote response/connection reducers | 2 passed |
| API chat receipt, draft, dispatch and source-switch guards | 14 passed |
| Composer visual padding and native terminal row tests | 3 passed |
| Review-text rendering and input sanitation tests | 6 passed |

These are **39 different passing tests**, not a claim that the complete app
suite was executed. The final six-test group includes a new regression proving
that evidence retains real paragraph breaks, escapes directional/control
characters and respects its byte budget, while single-line metadata continues
to escape line breaks.

The named test groups can be exercised from the binary test target:

```sh
cargo test --locked --bin ember -- \
  workspace_tools::tests:: \
  workspace_context::tests:: \
  workspace_review::tests:: \
  workspace_remote::tests:: \
  workspace_chat_bridge::tests:: \
  terminal::state::chat::workspace_composer_tests:: \
  terminal::tests::chat_docked_input_places_footer_directly_after_native_rows \
  terminal::tests::chat_presentation_rows_do_not_resize_native_cells_or_duplicate_prompt_origins \
  review_text::tests:: \
  --test-threads=1
```

## Actual embedded host smoke

`verify_workspace_host.py` passed against the built Ember and Frost programs.
It exercises the real embedded socket/SQLite host, revisioned drafts, exclusive
submission admission, a newer unsent draft surviving completion, old receipt
rejection, pinned organization and reading-position persistence. The same
stored history and reading state survive replacing the Ember host with the
Frost host. This protocol fixture does not dispatch an HTTP/model request.

```sh
python3 scripts/verify_workspace_host.py /path/to/ember /path/to/frost
```

## Native UI observations

Actual X11 windows were inspected in Xvfb using the Glow software rendering
path, separate temporary HOME/XDG directories and a synthetic shared host.
The actual X geometry was measured at 1280×900, 540×960 and 360×640.

- A three-line API draft remains visible inside both portrait windows, with a
  separate visual breathing row. The saved draft contains only its original
  text.
- A long conversation title is clipped within its allocated heading region;
  New and Context remain visible and separate at 360 pixels wide.
- Collapsing and restoring Agent Workspace retains the active draft and lets
  the terminal list reclaim the sidebar space.
- The Chinese query `松林验收` matches a synthetic body excerpt with no metadata
  match. Opening it shows an explicitly read-only source, event and exact
  revision. The full evidence keeps paragraph breaks.
- Opening an evidence hit can preserve the current terminal canvas. It does
  not resume the source conversation or create a provider execution.
- The Context drawer displays saved revision 1, goal/completed/remaining/
  constraint fields and selected material provenance at 360×640.
- A read-only audit after these flows found zero `api_jobs` and zero
  `native_jobs` in the isolated fixture database.

A visual inspection found and fixed the portrait heading overlap before the
final header screenshot. The generic metadata renderer also escaped document
paragraph breaks as literal backslash-n; the evidence views now use the
separately bounded multiline renderer. These changes do not alter stored text
or submission behavior.

## Final locked Git-source check

Production and development manifests and `Cargo.lock` now pin
`jterm_core` to `d7387fce27bf2b1e60ff2652ab04239a0b809dd3`.
`cargo metadata --offline --format-version 1` regenerated the lockfile. Relative
to the preceding validation lockfile, only the `jterm_core` package source
record changed; no other dependency package or version changed.

The following final check passed with exit code 0 in 405.49 seconds:

```sh
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 \
  cargo check --offline --locked --tests \
  --config 'profile.dev.package."*".opt-level=0'
```

The dependency optimization setting matches the isolated QA build profile;
repository build profiles were not changed. The command used the manifest's
Git source and lockfile with no local-path override for `jterm_core`. Its Git
objects were primed from the exact local commit, not downloaded from remote
GitHub. The checkout HEAD and tracked core sources were verified against that
commit. Remote commit availability and pushing master remain separate from
this local release check.

This gate type-checks the app and test targets. It does not rerun the 39 tests
or native UI flows recorded above, and it adds no new passing-test count.

## Coverage limits

Xvfb and direct XTest keyboard input do not establish Wayland, real IME
composition/candidate handling, screen-reader behavior or GPU-driver coverage.
This run did not test real model success, billing, a real SSH host, DLC task
execution or GUI restart during an in-flight model request. The SSH viewer
remains read-only; remote task execution is not supplied by this UI.
