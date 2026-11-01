# Agent Workspace

The window is a session workspace: the left navigation selects the conversation, the center shows its real interface, and the context inspector starts closed. **Context** or the upper-right inspector toggle opens it when needed. There is no second desktop to open and no horizontal session tab strip to scan.

## Navigation and entry points

- **＋ / New API** creates a shared API conversation. Configure the API provider in Settings before sending. Existing saved API chats are imported once; changing frontends does not create a second copy of shared chats.
- **Terminal** opens a normal shell. Its stable terminal identity is used for later navigation.
- **Tasks / Task** opens the existing task creation and review flow. Task conversations appear in the sidebar and open the native event view in the center. The Tasks dashboard defaults to visible; an explicit `experimental_task_sidebar = false` remains respected.
- Local Codex and Claude Code histories appear under **Recent conversations** and **Pinned**. Search and the project selector narrow the list. **Running** uses observed processes/managed runtime information. **Needs you** includes actual task attention states; for imported CLI history it uses the provider's last recorded waiting state, which is historical evidence rather than an approval channel.
- The **⋯** menu retains the detailed Agent Sessions manager, including organization and guarded external-owner conflict handling. CLI metadata writes use a source-qualified shared identity. A legacy native ID that identifies multiple provider homes must be selected by its exact source before editing.

Clicking a CLI conversation checks its native ID, provider home, history source and working directory. The managed terminal then attaches to that exact provider conversation. A pointer gesture from an old catalog cannot silently select a different source after a refresh.

## Three conversation surfaces

| Surface | Center | Input and lifetime |
| --- | --- | --- |
| API chat | Saved messages, current streamed response, earlier-message paging | Three editable lines plus a visual blank row. Enter sends; Shift+Enter adds a line. The HTTP request belongs to the initiating app process. |
| Native task | Real provider events, tool activity, approvals, review and validation controls | Three-line follow-up editor at a supported idle review point. Native task runtime belongs to this app and closes with it. |
| CLI conversation | The real provider TUI in a PTY | The TUI owns stdin and its prompts. No additional chat composer sends duplicate input. The shared host owns the PTY process. |

API conversation IDs occupy a separate numeric range from legacy local caches. Each request carries its original conversation ID and request epoch. Switching to another conversation does not redirect old output, clear the new conversation's draft, or revoke an unrelated stream.

## Shared state and conflicts

The family-shared host is embedded in the app executable and starts on demand. Its SQLite store owns the shared catalog, API messages, draft revisions, pin/archive/name metadata and reading positions. `JTERM_WORKSPACE_STATE_DIR` can select an isolated store for testing. Normal use needs no separate daemon installation. Task rows still come from this frontend's existing task store; this integration does not transfer native task execution to another frontend.

Provider dispatch waits for an acknowledged saved draft and an exclusive submission receipt. Completion and cancellation require that exact receipt. Retired receipts cannot append an answer twice, including after a host restart. Drafts typed after submission have their own revision. A completion reconciles the saved draft before saving local history/context, so another window's newer draft cannot be erased by the owner of the earlier response.

When local and shared drafts diverge, both are retained. **Keep my draft · reload shared history** explicitly chooses the local text against the latest shared revision. **Use shared draft** loads the shared text. Ordinary sidebar navigation does not silently choose a winner. Older API messages are loaded read-only; browsing them does not expand the bounded context sent to the model.

The old per-app JSON file remains a safety snapshot/migration source. Shared API saves do not depend on the legacy single-window writer lock. A legacy cache row is retired only after import acknowledgement and exact equality with the imported snapshot; a later edit or active request prevents eviction.

## CLI attachment and native continuation

The managed CLI PTY stays alive when a view detaches. Press **Ctrl+]** inside the managed terminal to detach; select the same source from another family frontend to attach again. Only one input attachment is admitted. A second attachment receives a conflict instead of starting a duplicate provider process. External provider processes keep the explicit conflict check and delayed **Resume anyway** flow.

**Resume native** is available for a cleanly finished Codex task when the current app runtime can prove the exact saved thread and private profile. It resumes that same task conversation without submitting the original goal again. **Refresh history / Load earlier** reads the provider's saved turns separately from live events; history reads do not replay tool operations or approvals. This capability currently depends on the same app runtime retaining its provenance and is not a promise of native task recovery after app restart.

Claude native tasks support additional turns using the exact provider session ID returned by the preceding turn. Claude permission checks remain enabled. A denied tool is reported; use Terminal for interactive approval handling. Kimi's current native path remains one-shot with its existing automatic-tool-permission behavior. OpenCode remains on its CLI PTY path.

A native task's **Terminal fallback** is an explicit CLI recovery route for the task/worktree and goal. It is not a verified transfer of the native provider thread or private history. Native and API runtimes do not acquire the CLI daemon's survive-app-exit lifetime merely because they appear in the same sidebar.

## Verification

`workspace_chat_bridge::tests` exercises the production UI reducer: save acknowledgement cannot dispatch a provider; an exact submission receipt can; newer and remote drafts survive; divergent drafts block overwrite; Stop before a receipt prevents dispatch; stale selection and widget events cannot retarget a conversation.

The Agent Sessions tests bind clicks to catalog epoch and provider source. The standalone smoke script uses built frontend executables, a private temporary directory and the real Unix-socket/SQLite host protocol:

```sh
python3 scripts/verify_workspace_host.py /path/to/first/frontend /path/to/second/frontend
```

It checks the embedded entry point, shared numeric IDs, single submission ownership, newer draft preservation, stale receipt rejection, pin/read-position persistence and reopening the same database with another frontend executable. It does not contact a provider or require credentials.
