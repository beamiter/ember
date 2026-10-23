# Agent management

Open **Agent Sessions** with **Ctrl+Shift+Alt+G** or the command palette.
The command center brings local Codex and Claude conversation history into a
searchable project library. The **Task dashboard** button opens worktree tasks
when the experimental Tasks sidebar is enabled. The Tasks sidebar has a
**Conversation library** button for the return trip.

## Organize conversations

Select a conversation and open **Organize this conversation** in its details.
Give it a recognizable local alias and comma-separated tags, then choose
**Save details**. **Reset** reloads its saved details. Clearing the alias restores
the provider's original title; clearing tags removes the local tags.

Aliases, tags, pins, local archive state and successful resume history are shared
by Anvil, Forge, Ember and Frost on the same user account. Changes are saved by
the shared background worker. Editing another conversation keeps the original
conversation's unsaved draft bound to its provider and session ID. If another
terminal changes saved details during editing, the editor preserves your draft
and offers Reset to load the newer saved values.

Aliases allow up to 160 UTF-8 bytes; use at most 12 tags, each up to 32 UTF-8
bytes. Tags are normalized and deduplicated on save. A rejected or failed save
shows an error and keeps the editable draft.

**Archive locally** moves a conversation out of the default Active library view.
Choose the Archived library view to find it, then **Restore to active** to bring
it back. This organization is independent of the provider's own archived state.
Archiving does not delete provider history, terminate an agent or erase a pin.

## Find work across projects

The project selector lists discovered directories with session, observed-live
and waiting counts. Choosing a project includes its subdirectories.
**This folder + subfolders** scopes the library to the terminal folder captured when the
manager opened; changing terminal tabs does not move that scope.

Search includes aliases, tags, original titles, project paths, branches, session
IDs and previews. Terms combine with the provider, state, project, pin and
archive controls. For more precise searches:

| Search | Result |
| --- | --- |
| `provider:codex tag:review` | Codex conversations tagged review |
| `project:core "release plan"` | A phrase within matching projects |
| `is:pinned -tag:blocked` | Pinned conversations excluding a tag |
| `status:waiting` | Last recorded waiting state |
| `is:live` | Exact session-ID match to an observed local CLI process |

Use the Archived or All library view when looking for a locally archived
conversation. Invalid queries show a message beside the result count.
**Clear filters** restores Active browsing across all projects.

The session list is virtualized and uses fixed-height, clipped rows so long
aliases or tags do not shift the scroll geometry. Arrow keys, Page Up/Down and
Home/End browse results; Ctrl+L focuses search. Enter resumes from search or the
session list. Editing alias/tags, choosing filters and saving details do not
resume an agent.

## Manage worktree tasks

Enable the experimental Tasks sidebar in Settings. It manages existing tasks
created from failed semantic command blocks and retains each task's provider,
isolated worktree, lifecycle, approvals, validation and diff review.

The dashboard adds exact-project and provider selectors alongside the status
views. Choose Attention, Recent, Oldest, Title or Project ordering. Cards show
project and branch together, with full text on hover. **Previous**, **Next** and
**Next needs you** select a task and scroll to its card without starting it.
The shared core resolves selection by stable task ID after filtering and sorting.

Status counts describe matches before the selected status view. A running native
stream, waiting approval and task lifecycle remain separate facts. Existing
Start, Stop, Follow-up, Finish, Validation and Review diff controls continue to
use the authoritative task runtime; browsing and filtering do not change tasks.
