# Asynchronous recovery metadata recheck

The recovery panel previously called directory metadata inspection directly
inside the egui button callback. A bounded snapshot can still contain a slow
filesystem path. This change moves only the user-requested recheck into one
background worker; startup loading and the existing inspection/report content
are unchanged.

One unfinished metadata inspection is allowed per recovery object. Duplicate clicks do not
start work. Closing the summary invalidates that generation's result but keeps
its occupied slot until completion or disconnection. Reopening cannot create
another inspection while the old metadata call is blocked. A completed worker
may still be finishing its repaint notification when the next inspection starts;
this is not a strict bound on live OS thread objects. The UI polls with try_recv, even
while closed, and never joins the worker. Cancellation is result invalidation,
not a promise to interrupt filesystem I/O.

Recheck and export have separate pending/notice fields. Closing still permits
an already-started export to finish and retain its outcome. A late recheck must
not overwrite export success, failure or uncertain-completion advice. Recheck
never changes the captured original bytes, startup summary or live sessions.
Worker panic/disconnection yields a categorical UI notice. The default Rust
panic hook is unchanged and can still log a panic payload; no log-suppression
claim is made. Starting failure does
not occupy a slot. Generations fail closed rather than wrap at u64 exhaustion.

## Pure regression runner

```
python3 scripts/test-session-recheck.py
```

This uses only rustc and the standard library. It compiles the production async
module, extracts the real recheck/poll/cancel callback methods and report
formatter verbatim, and uses a controlled fake inspector. A channel barrier
holds inspection until the caller has returned, proving that the callback does
not wait for the inspection. It also covers duplicate requests, close/reopen,
stale-result rejection, released capacity, panic/disconnect, generation
exhaustion, original bytes/report retention and both orders of recheck/export
notice completion.

Context repaint, inspector data, fixture storage and text-bounding are stubs.
Three additional fake-egui cases execute the real show method, including the
export receiver success/error/disconnected branches, both completion orders,
close/dismiss/reopen, button enablement, missing/invalid original bytes and
failed parse classification. Static checks also verify wiring. This is
not an actual egui click/rendering test, full-crate build or native acceptance.
The export receiver is driven by synthetic channel results; the unchanged
export worker/provider is not executed by this pure runner.
Existing local recovery tests await the new asynchronous completion explicitly.
No slow network directory, parser, shell, PTY, clipboard or socket is used.

The barrier deadlines bound a failed test; they are not a UI performance budget.
No measured native latency or E09 full-startup <50ms acceptance is claimed.
A filesystem call may remain blocked; one worker slot remains occupied until
it returns. The UI can stay responsive and export independently in that state.
