# Agent Workspace

The left navigation holds CLI sessions, API chats, native tasks and ordinary terminals. Selecting a conversation opens its real interface on a continuous, left-aligned canvas. There is no horizontal session tab strip. The inspector starts closed; **Changes**, **Context** and **Run** open the evidence relevant to the selected conversation.

## Navigation and narrow windows

**Agent Workspace** collapses the conversation group without stopping hosted work or discarding a draft. **New** contains New terminal, New API chat and New task. **More** contains session details, the workspace overview and a separate remote read-only viewer. The existing Tasks dashboard and its guarded worktree/review flow remain available. An explicit `experimental_task_sidebar = false` setting is respected.

At compact widths the navigation and inspector become drawers. They can be closed to recover the center width; resizing does not replace the underlying native terminal or conversation. Session rows use stable source identities. Frost retains keyed row trees across reordering; Ember uses stable widget IDs. A stale pointer callback still has to pass the current source and selection checks.

Search uses the shared workspace query language for metadata and indexed history. Supported fields include `provider:`, `kind:`, `status:`, `runtime:`, `validation:`, `attention:`, `project:`, `branch:`, `tag:` and `body:`. Examples:

```text
provider:codex status:review
kind:task validation:failed
project:"/home/me/project" -tag:done
body:"missing field"
is:archived
```

Malformed or unknown fields show an explanation and no matches. Provider, project, live and Needs you controls also constrain indexed-history searches. Background results do not select a conversation on the user's behalf.

**Needs you** uses the host's factual projection: a current approval, reply, validation/review action, failure, interrupted ownership or conflict. A historical provider message by itself is not a live permission request. Run details distinguish runtime state, task acceptance, validation, saved provider evidence and the captured host event sequence. Pending native requests navigate to the current task's existing request controls.

## Three real conversation surfaces

| Surface | Center | Input and lifetime |
| --- | --- | --- |
| API chat | Saved messages, live response and earlier-message paging | At least three editable lines and a separate visual blank row; no whitespace is inserted into the sent text. The shared host owns the API request. |
| Native task | Provider events, tool evidence, current requests, review and validation | A three-line follow-up editor appears only when the live adapter and control lease allow another turn. The shared host owns supported native runtimes. |
| CLI conversation | The actual provider TUI in a PTY | The TUI owns stdin, completion and prompts. There is no duplicate outer chat composer. The shared host owns the PTY. |

API and native work can continue after a GUI closes while the owning host remains alive. Another view can observe current state and explicitly acquire the available control lease. A host crash or uncertain process ownership produces Interrupted/Unknown evidence; opening a view does not restart a model, repeat the original goal or replay tools.

API Enter inserts a newline and Ctrl+Enter sends by default. `ai_chat_send_on_enter = true` changes this to Enter-to-send and Shift+Enter for a newline. Native follow-up uses Ctrl+Enter. The focused editor owns its input and IME confirmation does not submit. CLI input remains native. The normal shell composer retains at least three editable rows and visual space below its caret when viewport geometry permits.

## Exact search evidence

Saved API messages, retained native turns and explicitly indexed provider/imported excerpts are searchable through the shared host. Coverage reports state what is indexed and what is pending; they do not claim a complete cloud history mirror.

Selecting a history hit opens a **read-only evidence view** for its conversation ID, event ID and revision. If that revision changed, the view asks for a new search instead of substituting another message. **Open conversation** is a separate explicit action and remains unavailable for imported-only history or a source this frontend cannot safely open. Reading evidence does not add it to model context or resume provider execution.

Local Codex / Claude Code histories are distinct from ChatGPT / Claude consumer accounts. Consumer content requires an explicitly supplied link or imported record. This workspace does not supply automatic cloud-history synchronization.

## Context and cross-provider handoff

The Context inspector edits a revisioned package containing Goal, Completed, Remaining, Constraints and up to 16 selected materials, within a 64 KiB total budget. A material can be a note, file excerpt, diff, log or conversation excerpt. Every material can be previewed, edited and removed. Search excerpts retain exact source references; captured diffs and logs retain their run/event provenance without inventing a search revision.

**Save context** uses compare-and-swap revisions. If another window saved a different package, the local draft is retained and the user explicitly chooses **Keep local for review** or **Use saved context**. A late save acknowledgement cannot erase text typed after the request. Nothing is sent simply because it was read or saved.

After saving, choose a target provider to preview the handoff. **Create task draft** or **Create API draft** creates a new conversation draft with the reviewed package. It does not claim native resume on a different provider and does not send or start the draft. Existing task drafts are retained rather than overwritten. An API prefill is bound to the exact newly created chat acknowledgement, even if another chat is selected while creation is pending. Packages over the 16 KiB handoff limit are rejected explicitly instead of silently shortened.

## Results, validation and queue

Changes uses the task's recorded baseline and an explicit read-only result capture. It shows captured HEAD, fingerprint, untracked count, attribution and a bounded diff preview. The diff includes all changes from the task baseline; it does not attribute every edit to the Agent. A capture's run ID and host event sequence remain visible. Different live and captured event versions require a refresh before relying on the result.

Run shows validation command, directory, fingerprint, retained output and whether the evidence still matches the captured files. A previous pass whose fingerprint or command changed is stale, not a current pass. Refreshing result evidence does not run tests, accept a task or commit changes. Existing guarded validation, diff-review and acceptance controls remain authoritative.

The task queue records explicit local task intent and accepted-task dependencies. Updates use the queue revision and reject invalid or cyclic dependencies. **Ready to start explicitly** never launches a task automatically. Add/remove/dependency controls act on the exact selected task. The overview counts observed CLI/API/native activity, running validations, accepted tasks, unconfirmed outcomes and attention; incomplete catalog coverage is labeled.

Usage is displayed only when a supported provider reports it. Codex thread totals and observed Claude native-run usage have distinct source/scope labels. Missing token counts or costs are unavailable, never inferred as zero or estimated from text.

## Shared host and recovery boundaries

The family-shared host is embedded in each frontend executable and starts on demand for local use. Its SQLite store owns shared metadata, messages, draft revisions, reading positions, search documents, context packages and queue intents. `JTERM_WORKSPACE_STATE_DIR` selects an isolated store for tests. Task execution/approval authority comes from the actual runtime and control lease, never from catalog presentation labels.

Only one writer/control owner is admitted for an execution. Another view may observe it; it cannot inject input by appearing in the same sidebar. CLI **Ctrl+]** detaches the view without stopping the host-owned PTY. External-owner conflicts keep their existing explicit recovery checks. Native task approvals keep the adapter's real capabilities; these frontends retain the current display-and-deny controls and do not invent an Allow action.

Codex native restore requires the exact saved provider identity and verified runtime provenance. Claude print supports sequential turns while its native run remains live; continuation after a stopped run uses the explicit terminal handoff and is not advertised as native restore. Kimi remains one-shot. History paging is read-only, and unsupported approval or continuation controls remain absent or disabled.

This release uses workspace protocol **2**. Upgrade the host and frontends together. A version or ownership mismatch is explained; the UI does not kill an older host, bypass its checks or replay interrupted work to conceal incompatibility.

## Remote workspace: read-only SSH

**More → Remote workspace (read-only)** opens an independent viewer. Configure an SSH alias, an absolute remote frontend executable and the expected remote host ID. On the remote machine, `frontend --jterm-workspace-host-id` prints that identity. The remote host must already be running with a compatible release.

The transport keeps strict known-host verification and noninteractive authentication, then verifies host ID and protocol before reading. The viewer has its own client, catalog, scoped search and exact-revision evidence; it never routes remote IDs into local activation, saves or queue updates. Reconnecting retires old callbacks and responses. It does not start a remote host/provider or automatically retry a failed operation. SSH task execution and DLC execution require adapters that are not supplied by this viewer.

## Verification

Frontend reducer tests cover stale search responses, exact evidence identity, context revisions, in-flight edits, source switches, API draft receipts and the existing submission/ownership guards. The standalone smoke script exercises the real embedded socket/SQLite protocol with a private temporary directory and no provider call:

```sh
python3 scripts/verify_workspace_host.py /path/to/first/frontend /path/to/second/frontend
```

GUI validation should use isolated state/config/provider homes. Check a wide window and 390/320-pixel portrait widths, search and source switching, draft retention, workspace collapse, drawer input ownership and CLI absence of a second composer. Provider/network success, remote SSH server availability and paid-model usage are separate acceptance checks and are not implied by local fixture tests.

The executed gates and native-window observations are recorded in
[Agent Workspace v2 validation](agent-workspace-v2-validation.md).
