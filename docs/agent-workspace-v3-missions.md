# Ember Mission workbench

The native **Missions** view uses the shared `MissionWorkbench` controller and
existing embedded workspace host. It adds no daemon, scheduler, provider session,
execution reducer, or automatic Git operation.

## User path

1. Enable the existing Agent workspace and open **Missions** in its sidebar.
2. Create a five-repository plan with a title, acceptance goal and absolute parent
   directory. The exact `beamiter/jterm_core`, `anvil`, `forge`, `ember`, and `frost`
   identities are retained. Individual checkout/worktree paths are editable.
3. Save the required environment, target refs and exact required check names.
   Inspect repositories explicitly. Missing repositories, pins, tasks and evidence
   remain visible and do not count as success.
4. Freeze clean observed candidates explicitly. The shared controller allows this
   only for the current, complete inspection; refresh/reopen/stale observations
   cannot be adopted. Freezing does not commit or publish anything.
5. Link the currently selected Task run. This references the existing
   source-qualified conversation, Task ID, run and sequence. It does not resume a
   provider or copy its identity.
6. Review modified/committed/pushed/integrated/validated facts separately. Read the
   exact observed HEAD, dirty fingerprint, manifest/lock pins, dated remote ref,
   and individual passed/failed/blocked/not-run/stale checks and host receipts.
   A current task/Result button checks both Task ID and source-qualified
   conversation. It explicitly opens the current task, not a historical run.

Known incomplete Missions contribute to **Needs you**. Selecting its Mission row
opens the same stable identity. The catalog refresh is explicit, so other
frontends' newly created Missions appear after refresh rather than being inferred.

Drafts are retained independently by Mission ID while selecting other records or
closing the window. An external revision does not silently rebase a dirty draft.
Editing is disabled while a request is pending. An acknowledged own save
reconciles host-owned revision/timestamp fields. Conflicting saved contracts can
be compared, and keeping the draft against a newer revision requires an explicit
gesture before another Save. Unsaved
acceptance edits never display the older saved report as current verification.
Draft retention is in-process; saved Mission records survive host restart. This
is not a promise that unsaved UI drafts survive an application crash.

## Validation scope

Shared core: `c1aebbe7b6ad4a8edb98582ea282d6e4d2ba6816`.
Stability checkpoint: [b600737](https://github.com/beamiter/ember/commit/b600737d37202643f6d74a1bbecafb7ed8622041).

Seven focused tests executed successfully against the final core pin:
acknowledged-save reconciliation, concurrent-edit preservation, explicit conflict
resolution, close/reopen and selection retention, source-qualified navigation,
uninspected attention, and native egui rendering at 320/960-point widths without
starting host operations. Strict all-target Clippy and formatting passed.

The complete locked workspace/all-targets/all-features command executed 1,415
passing library tests and 2,297 passing binary tests. Each target retained the
same three trusted-Git root-ownership failures described in the V2.1 record, with
three and six ignored tests respectively. Counts are per target and overlap;
they are not a combined distinct-test claim. The separate block-search memory
test passed. Ignored native cases and unconfigured-provider fallbacks are not
counted as provider or hardware acceptance.

All 21 `scripts/test-native-*.py` suites passed. These are pure/synthetic script
regressions. The release observer now registers revision3 navigation minima and
52-by-26 default-font mode controls from the actual adapter, with independent
exact geometry assertions and unchanged strict paint negatives. The previous
20-column/10-row improvement assertion described an older design; revision3's
registered unchanged-cell comparison is exactly 10 columns and nine rows.

The stability checkpoint's remote Rustfmt, font groups, receipt state, installer,
Clippy, full Tests and native Chat/composer geometry jobs passed. Its native
release job exposed the old observation fixture and further native gates still
being investigated. No all-green release or V3 acceptance is claimed here.

The existing isolated `verify_workspace_host.py` now also exercises Mission
create/list/read, stale-revision rejection, five missing-checkout errors and
persistence across host replacement in the actual frontend executable. It uses a
temporary state directory and no provider credentials, network requests or user
configuration. Local host creation remains sandbox-blocked; the normal Linux CI
runner must execute this gate.

No real IME candidate window, physical keyboard/GPU, external model service or
SSH host was used for these headless tests. Hardware/native acceptance remains
separate. See [V2.1 validation](agent-workspace-v21-validation.md) for the baseline
failures and cloud security-precondition limits.
