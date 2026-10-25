# Agent management

Open **Agent Sessions** with **Ctrl+Shift+Alt+G** or the command palette.
The command center brings local Codex and Claude conversation history into a
searchable project library. The **Task dashboard** button opens worktree tasks
when the experimental Tasks sidebar is enabled. The Tasks sidebar has a
**Conversation library** button for the return trip; it opens the library or
brings it forward, and never closes it. **Ctrl+Alt+Shift+T** (`task:toggle`,
also in the command palette) shows or hides the Tasks dashboard; while the
dashboard is disabled it explains where to enable it.

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
bytes. The editor shows the alias byte count and applies the shared validation
while you type: an over-long name, too many or too long tags, or a control
character is explained inline and **Save details** stays disabled until it is
fixed. The batch tag field works the same way for **Add tag** and **Remove tag**,
and also rejects a comma. A paste is never silently shortened to fit. Tags are
normalized and deduplicated on save. A failed save shows an error and keeps the
editable draft. The error is reported even if a background reload finishes at
the same moment, and stays until the next successful reload replaces it; the
draft stays either way.

Pin, archive, Save and batch controls pause only while your own change is being
saved (**Saving favorites and organization…**). The periodic read-only reload
that picks up other terminals' changes never disables them.

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

The overview counts sessions, waiting conversations and **live CLI
conversations**: conversations with an exact session-ID match to at least one
local CLI process, so several processes on one conversation count once.

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
conversation. Invalid queries show a message beside the result count and no
results. Searches are limited to 4,096 UTF-8 bytes; a longer paste stays visibly
over the limit with that explanation rather than silently losing its final terms.
**Clear filters** restores Active browsing across all projects.

The session list is virtualized and uses fixed-height, clipped rows so long
aliases or tags do not shift the scroll geometry. Arrow keys, Page Up/Down and
Home/End browse results; Ctrl+L focuses search. Enter resumes from search or the
session list. Editing alias/tags, choosing filters and saving details do not
resume an agent.

Editing a query or filter selects its first match. A background refresh never
moves the selection to a different conversation: when the selected conversation
is archived by another terminal, filtered out or no longer discovered, it stays
selected but detached. The details pane says it is no longer in this view, and
Resume and organization stay unavailable until you select a conversation again.
When your own change moved it (archiving or restoring it, unpinning it under
**Pinned only**, or a name or tag that no longer matches the search), the notice
says so and names the view that finds it. Opening the manager replaces a
selection that is no longer listed with the first match.

While the manager is open, discovery rescans five seconds after the previous scan
finished and reloads the shared library in the same step. Opening the manager,
**F5** and **Refresh** rescan and reload immediately.

### Resume safely

A conversation already open in an Ember tab switches to that tab. Otherwise,
when the latest scan observed a CLI process already running the conversation,
the first **Resume** (button or Enter) does not launch: it shows which process
holds the history. Only **Resume anyway**, or Enter again, for exactly that
conversation starts a second CLI that writes to the same history. Selecting
another conversation, a refresh that removes it, **Cancel** or **Escape**
dismisses the warning. A repeat within 600 ms of the warning, such as the second
click of a double-click, is ignored, so it cannot confirm a warning you have not
read. **Resume anyway** covers only the attempt it starts: if that readiness
check fails or the attempt is dropped, the next Resume warns again. If a scan
reveals such a process while a resume is waiting for its readiness check, the
launch stops and asks first.

The details pane shows the exact resume command, including an
`env CODEX_HOME=…` or `CLAUDE_CONFIG_DIR=…` override when the conversation
belongs to another provider home. The background readiness check computes it,
so it appears a moment after selecting without any file checks on the UI
thread; it stays copyable when only the CLI is missing from this desktop.
**Copy command** copies that command and **Copy path** the project directory;
each confirms with a short “Copied …” note.

**Escape** first leaves a focused search, alias, tag or batch-tag editor (a
pending live-process warning stays), then dismisses that warning, and otherwise
closes the manager.

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
views. Choose one of six orderings: Needs you first, Recently updated, Oldest
update, Task name, Project, or Needs you · oldest update. Cards show
project and branch together, with full text on hover. **Previous**, **Next** and
**Next needs you** select a task and scroll to its card without starting it.
The shared core resolves selection by stable task ID after filtering and sorting.

Status counts describe matches before the selected status view. A running native
stream, waiting approval and task lifecycle remain separate facts. Existing
Start, Stop, Follow-up, Finish, Validation and Review diff controls continue to
use the authoritative task runtime; browsing and filtering do not change tasks.
Native views show readable phases (for example **Waiting for approval**) and
approval kinds. An approval wait or an idle review point polls the provider
slowly, but Approve, Deny, Follow-up, Finish and Stop refresh the view promptly.

### Hide and restore tasks

**Hide task** removes a task from the dashboard and leaves its worktree in
place. It is disabled, with the reason on hover, while the Agent or its
preparation is still running or validation is in progress. Once at least one
task is hidden, **Hidden (N)** switches the list to hidden tasks; choose it again
to return. Opening the dashboard (from the top-bar indicator, `task:toggle` or
the Agent Sessions **Task dashboard** button), creating a task and restoring one
also return to the dashboard, as does restoring the last hidden task. Each
hidden task offers **Restore** and a read-only **Review diff**.
Restore returns the task to the lifecycle state it had when it was hidden, with
its native transcript, approvals and follow-up draft, and reports it (for
example "Task restored · Ready for review"); it never starts an Agent, reruns
validation or accepts work. If its registration was still unconfirmed when it
was hidden, Start stays blocked after Restore until **Retry registration** or
**Reload** confirms it. If its recovery record was dismissed meanwhile, nothing
can confirm it any more: the Restore message and the task card say so, and
dismissing such a record warns first. A task hidden by an older version has no
recorded state, so it cannot be restored.

### See what needs you

The sidebar's **Tasks N** tab counts tasks in all five attention queues; hover
for the breakdown, such as "2 tasks need you: 1 approval, 1 reply". Hidden tasks
never count. While the sidebar is hidden, a compact **Tasks N** indicator in the
top bar opens the dashboard.

### Work through the attention queues

The task dashboard separates work that needs you into **Approvals**, **Needs
reply**, **Validation issues**, **Ready to review** and **Failed tasks**. Counts
describe the current search, provider and project scope before the selected
status view or attention queue. A pending approval or question takes precedence
over an older validation issue; a running check is not a review-ready result.

Choose a queue to narrow the list without replacing your search text. Choosing
a status view clears the queue selection; **Clear filters** clears both. A
selected task stays bound to its identity when it is hidden by either filter.
**Show selected task** restores the full view, while **Next needs you** navigates
only the currently visible tasks. These controls never start or accept work.

Task search now supports field constraints in the same dashboard:

| Search | Result |
| --- | --- |
| `provider:codex attention:approval,review` | Codex tasks awaiting approval or review |
| `project:ember -validation:passed` | Matching projects without a recorded passing check |
| `status:failed,cancelled "release plan"` | Failed or cancelled tasks matching the phrase |
| `is:running -runtime:fallback` | Current activity excluding terminal-fallback tasks |
| `branch:release id:1234` | A branch match and a task-ID fragment |

Supported fields are `title`, `provider`, `status`, `runtime`, `validation`,
`project`, `branch`, `id`, `is` and `attention`. Separate terms combine with AND;
comma-separated alternatives in enumerated fields combine with OR. Prefix a
term with `-` to exclude it and use double quotes for phrases. **Search help**
shows examples in the panel. Queries are limited to 256 UTF-8 bytes. Invalid
fields or values show an error and no results, retaining the selected task and
editor drafts until the query is corrected. Enter selects a result without
starting the Agent. Pasting a query never silently truncates its final terms;
an editor update larger than 4 KiB is rejected in full with a visible notice.

### Read task state without conflating progress and acceptance

Task cards show the provider and lifecycle, validation attempt/result, runtime
attachment, project/branch and last recorded update separately. A live native
session can remain open while its task awaits review. A passing validation is
shown as a passing check; the completion checkmark appears only after the task
has actually been accepted. Relative update times use the task's recorded
timestamp, with unavailable or future timestamps identified explicitly.
Card height follows the configured text sizes, keeping all six information
lines visible when the interface font is enlarged.

The selected task's **Next step** and current blocking reason use the shared
four-terminal guidance. Pending recovery registration takes precedence until
its durable marker is confirmed. These explanations do not enable an action
whose runtime prerequisites are unmet. Action controls appear before the
expandable goal and inspector, keeping long instructions from pushing the
controls out of immediate reach. Follow-up drafts are cleared only after
the runtime accepts the exact submitted text; a later edit or another task's
draft remains available.
