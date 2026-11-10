# ED2 and DECALN physical fill extraction

This changes only physical active-grid filling. `ScreenState::erase_screen`
owns ED2's BCE blank fill; `ScreenState::screen_alignment_test` owns DECALN's
default-style E fill and margin/cursor adjustment. Both clear pending wrap and
preserve row identities, row-wrap metadata and the inactive grid.

The host still invalidates provenance before the fill, then performs the exact
original dirty/revision bookkeeping, cursor-reposition notification and graphics
hooks. ED2 retains its original pre-invalidation latch clear so host hooks see
the original state; the engine also independently guarantees clearing its latch.
The parser's archive → inline CLI → erase ordering is unchanged. DECALN does not
archive or clear Kitty graphics. No row/output effect descriptor is fabricated.

The baseline golden, corpus and copied original fill bodies remain frozen.
The baseline checkpoint describes its original capture, not candidate status.
Direct differential and partition-sensitive production replay remain independent
of the new engine implementation. Engine tests add exhaustive small geometry,
margin, cursor, style, origin, pending-wrap and active-buffer combinations and
compare all ScreenState fields and both grids after repeated operations.

Limitations retained from baseline review: command records, projected selection,
source epochs, actual cache contents and hidden Kitty placement state are not
directly snapshotted. Therefore every existing host hook and its ordering remain
unchanged. The baseline's eight production seeds are sparse, not Cartesian.
This is not a claim of complete terminal architecture migration or GUI parity.
