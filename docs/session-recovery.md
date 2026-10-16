# Session recovery summary

Startup retains the existing bounded, no-follow, owner-checked snapshot reader,
64-session limit, sanitizer, corrupt-file quarantine, and save blocking when
quarantine fails. A normal snapshot produces no recovery UI.

When metadata needs repair, a nonmodal window reports retained/skipped session
counts, changed layout/selection categories, and affected original session
positions for bounded fields and invalid or unavailable directories. Layout
repair is a category, not a fabricated count of corrupted panes; unreadable
snapshots explicitly have unknown session counts. Names, IDs and paths are not
shown, including for private-title tabs. Closing the window retains its report
and original bytes; the bottom-left details button reopens it during this run.

The original is the exact byte sequence obtained by the single safe startup
read, capped at 4 MiB. It is retained only when recovery is needed. It is not
reread by pathname later, so autosaves or a pathname replacement cannot change
what is exported. Unsafe/unreadable/oversized sources have no export option.
The existing quarantine flow remains unchanged and is not used to bypass a
failed source read.

“Recheck original metadata” repeats bounded validation against those captured
bytes, including directory availability. It reports a preview, preserves the
startup report, and neither starts shells nor changes current sessions or the
saved snapshot. It cannot apply recovery commands.

“Export original locally” requires an explicit new absolute pathname and click.
The existing private-file writer stages and fsyncs the content, then publishes
with a no-replace link; existing files/symlinks are not overwritten. Files are
0600. Export runs off the UI thread. A private-data warning accompanies the
choice. Nothing is uploaded, and dismissing the summary does not export.

## Verification

Unit coverage includes normal and 67-session snapshots, corrupt layouts,
unsafe and missing directories, unknown counts for corrupt UTF-8, exact-byte
retention through dismissal/recheck and source replacement, exclusive private
exports and symlink rejection. An egui frame smoke checks render/dismiss state.

Run the installed-app native acceptance separately from the main desktop gate:

```sh
for case in normal corrupt-layout over-limit invalid-cwd; do
  python3 scripts/native-session-recovery.py --binary /path/to/installed/ember \
    --backend x11 --case "$case" --output "/tmp/recovery-$case"
done
```

Repeat with Wayland where the established native desktop dependencies are
available. The fixture imports the general smoke driver's compositor, OCR and
real keyboard/pointer transport. It checks rendered reasons/counts, terminal
input while the summary is open, dismiss/reopen, recheck and exact-byte export.
It does not inject application-private events. Preserve screenshots and logs.

The fixed 64-session loader/report microbenchmark uses 20 warmups and 200
samples on the same 4,614-byte input with existing local cwd paths, comparing
the immutable baseline loader and the new loader in one process. This measures
loader/report overhead only; it is not the 64-shell full-app startup metric.
Full restored-app startup acceptance remains a native measurement requirement.
