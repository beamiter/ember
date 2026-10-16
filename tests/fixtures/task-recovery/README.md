# Worktree-creation recovery protocol v1

This adapter protocol can be reused by Frost R09 without copying toolkit code.
It is creation-failure recovery, **not** task/session persistence. The pinned
jterm_core `TaskManager` is an in-memory registry; it does not write storage and
cannot return ENOSPC. Storage failure tests belong to this journal, while core
registration rejection tests exercise its real input validation.

## Data and limits

`journal.json` is an owner-private UTF-8 JSON document with `version: 1` and a
`records` array. Max 128 records and 2 MiB, max 4096 encoded bytes per absolute
path. Reject unknown fields/version, duplicate UUIDs, invalid stage combinations,
path traversal, or identities whose `task-<uuid>` path/branch do not match.
No eviction: at capacity refuse creation before Git and offer explicit dismissal.
The fixture `stages.json` contains synthetic paths and Unix directory fingerprints.
A different platform must define equivalent stable directory identity rather than
blindly interpreting Unix device/inode numbers. Branch namespace (`ember/` here)
is an adapter parameter; Frost should use its own validated namespace with the
same stage, lease, identity, boundedness and uncertain-write semantics.

Never store command text, output, original title, environment or credentials.
Recovered tasks explicitly say their command context was not saved. Diagnostics
use fixed messages and I/O classifications, not raw Git stderr or journal bytes.

## Commit protocol

1. Acquire a nonblocking interprocess lease. Hold it across the complete creation
   operation, including delivery to the UI and the registered marker write.
   Another instance may not dismiss a request while Git is still creating it.
   Explicit unlock on lease Drop prevents unrelated forked/pre-exec children
   retaining a closed owner's lock through a shared open-file description.
2. Persist `requested` (UUID, canonical repository, canonical managed-root child
   path, branch, provider; null base/fingerprint) **before any Git mutation**.
3. Create once using WorktreeService. On error/cancellation/panic/disconnect leave
   the requested record and any worktree intact. Never retry Git creation.
4. Persist `prepared` only after successful verified creation, including its
   immutable original base commit and directory device/inode fingerprint.
5. Register in the runtime TaskManager. Explicit Retry verifies the existing
   registered Git worktree/common repository/branch and directory fingerprint,
   then deduplicates by full identity in the current runtime registry.
6. Only after successful in-memory registration persist `registered` with that
   exact core `registered_task_id` (null in earlier stages). Reload may unblock
   only this winning runtime TaskId, not another window's unconfirmed task with
   the same worktree identity. A losing window stays blocked and can archive its
   duplicate runtime entry or Open the worktree. This owner ID is not restored
   into TaskManager after restart. Provider
   autostart on the normal creation path is deferred until this write succeeds;
   recovered registrations never autostart. Start actions are blocked while the
   marker is uncertain. A Reload proving a registered marker may unblock that
   same runtime task. Completion must recheck Created status before autostart.

Writes reuse the application's private bounded reader and locked compare-and-swap
atomic writer: same-directory private temporary file, file fsync, rename, parent
fsync. Newly created journal/root directory entries are synced too. These writes
are NOT a transaction with Git, which may update another filesystem. A failure
after rename can mean publication succeeded but durability is uncertain: stop,
retain worktrees, and Reload rather than assuming rollback or blindly overwriting.
I/O and Git verification run off the GUI thread.

## Restart and user actions

- `requested`: Open/Copy path/Dismiss only. Missing original base commit means a
  safe retry cannot guess HEAD after edits or new commits. The user can inspect
  and manage the existing worktree manually. Never recreate branch/worktree.
- `prepared`: explicit Retry registration, Open, Copy path, Dismiss. Retry creates
  at most one task per identity in a runtime. IDs are runtime TaskIds; a fresh
  process gets a fresh TaskId. The journal UUID is the persistent recovery ID.
- `registered`: Open/Copy path/Dismiss only. A previous task might have completed;
  never resurrect or automatically execute it after restart.
- Dismiss requires a separate confirmation and only removes journal metadata.
  It never removes/moves worktrees, branches, ignored/untracked/tracked user files.
  A matching unconfirmed runtime task must first Retry/Reload its marker, so
  dismissal cannot strand that task without its registration-confirmation route.
- Worktree identity mismatch blocks Retry/Open; Copy path allows manual inspection.
- Errors and records stay visible in the Tasks recovery panel, independent of
  transient status messages and task query filters. Reload reconciles uncertain
  outcomes. Local worktrees must not be navigated as remote Files paths. Delayed
  Open results carry the Files intent context/epoch; newer navigation wins.

## Verification matrix

Real temporary Git repositories/worktrees exercise dirty/untracked preservation,
registration validation errors, repeated retry/current TaskId dedup, fresh runtime
recovery, requested-stage interruption, explicit dismissal, and same-path/branch
replacement rejection. The real filesystem produces PermissionDenied. A test-only
writer fault injects ENOSPC before publication and EIO after real publication to
exercise the production state machine without filling the user's disk. Neither
is claimed as a TaskManager I/O error or a real disk-full syscall test.

Strict production WorktreeService verification is tested without a trust bypass.
This sandbox's non-system-owned `/` may block its trusted-Git resolver; record that
local limit and require native CI for the unchanged trust boundary. Corrupt,
oversized and symlink journals must remain untouched; leases, capacity and dropped
worker replies must not lose records. No GUI screenshot or power-cut hardware
simulation is implied by these tests.
