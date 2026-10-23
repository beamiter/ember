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

### Organize a selection

Use the checkboxes beside conversations to build a selection independently of
the conversation being inspected. **Select visible** includes the current
filtered results; **Clear selection** clears the checkmarks. A batch can contain
up to 256 conversations. A selection that would exceed the limit is rejected
in full so it cannot quietly act on only part of a project.

The toolbar shows both the selected count and the count hidden by filters or a
catalog refresh. **Pin selected**, **Unpin selected**, **Archive selected** and
**Restore selected** apply to every checked identity, including hidden ones.
Enter one tag and choose **Add tag** or **Remove tag** to update it across the
selection. These relative tag changes preserve other tags and aliases, including
changes made by another terminal before the batch is saved.

The shared worker admits and saves each batch atomically. An error keeps the
selection and tag input available for retry. Successful batches also retain the
selection, making it possible to pin, tag and then archive the same group.
Batch controls never resume conversations or stop their processes. Pressing
Enter in the tag editor does not apply a batch or resume a conversation.

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

Enable the experimental Tasks sidebar in Settings. It manages tasks created
from project goals or failed semantic command blocks, retaining each task's
provider, isolated worktree, lifecycle, approvals, validation and diff review.

### Create a project task

Choose **New task** and enter an absolute local project directory, a title,
the provider, and a goal with acceptance criteria. Existing task projects are
available in the project picker. The project directory is verified in the
background and Git creates a separate managed worktree from its repository's
current `HEAD`. The goal can contain multiple lines, up to 16,384 UTF-8 bytes.

Optionally enable **Require a validation command before completion**. Enter one
exact command line and the absolute shell path that should run it. Validation
runs at the isolated worktree root only when you choose **Run validation**.
The task keeps the command and shell you selected at creation time.

**Create isolated task** prepares and registers the workspace without starting
Codex, Claude, OpenCode or Kimi. Review the created task and choose **Start**
separately. Closing the composer preserves unfinished input. Input validation and
worktree preparation errors also keep the draft. Enter
inside the goal editor inserts a line break and does not create or start a task.
Native and terminal starts retain the existing AI sharing and provider approval
controls; an authored task cannot start through a terminal fallback without
successfully preparing its goal.

The recovery journal stores workspace identity, not the goal or captured command
content. If the app restarts during creation, the worktree can be inspected and
recovered, but its goal must be entered again in a new task. A journal-marker
retry within the same running app preserves the already registered task and its
goal. Recovery never automatically starts the provider.

### Inspect and accept work

Select a task to see its **Next step**, its authored goal when present, and the
expandable **Task inspector**. The inspector shows the stable task identity,
provider, origin, repository, isolated worktree, branch, original review commit,
runtime, UTC timestamps and available validation details. These are facts from
the current task; the native activity and turn history remain the actual provider
events.

The next-step message explains pending registration, preparation, approvals,
live sessions, validation and recovery gaps. Disabled validation, acceptance and
hide controls explain their prerequisites. Finish any live native session before
validating or accepting its work.

For a goal with a validation command, **Mark complete** requires a passing
validation result and your explicit acceptance. Command-created tasks keep the
same requirement. For an authored goal without a validation command, review the
diff and choose **Accept reviewed work** once the Agent is ready for review.
This records human acceptance without claiming an automated validation pass.

The dashboard adds exact-project and provider selectors alongside the status
views. Choose Attention, Recent, Oldest, Title or Project ordering. Cards show
project and branch together, with full text on hover. **Previous**, **Next** and
**Next needs you** select a task and scroll to its card without starting it.
The shared core resolves selection by stable task ID after filtering and sorting.

Status counts describe matches before the selected status view. A running native
stream, waiting approval and task lifecycle remain separate facts. Existing
Start, Stop, Follow-up, Finish, Validation and Review diff controls continue to
use the authoritative task runtime; browsing and filtering do not change tasks.
