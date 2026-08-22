# Ember upgrade rounds

Rounds 1–10 record the preceding pass; this pass's additional twenty rounds
are numbered 11–30.

1. **Prefix validation** — absolute runtime prefixes reject controls and lexical
   parent traversal while allowing spaces, Unicode, repeated separators, and
   harmless dot components.
2. **Binary-directory validation** — install/uninstall share the boundary and
   explicit empty `--bin-dir` no longer falls back silently.
3. **DESTDIR confinement** — unsafe staging roots fail before build or writes,
   preventing concatenated paths from escaping the package tree.
4. **Explicit root staging** — `DESTDIR=/` retains staged cache/PATH behavior
   after normalization.
5. **Dependency preflight** — transactional and desktop helper commands are
   checked before building or changing the destination.
6. **Atomic binary replacement** — a mode-`0755` temporary is created beside
   the target and renamed without following a hostile destination symlink.
7. **Pre-commit cleanup** — the active temporary is registered in an EXIT trap
   and cleared only after rename commits the binary; later resource failures do
   not imply binary rollback.
8. **Atomic desktop replacement** — desktop output uses an unpredictable
   same-directory temporary rather than a predictable `.new` pathname.
9. **Remote-host semantic gate** — one application gate combines visual-text
   safety and byte budgets with the shared argv/session/deploy/path schema;
   app text checks precede shared diagnostics, and picker, connection argv, and
   remote filesystem operations all re-run it.
10. **Non-destructive resource bound** — invalid drafts and entries after the
    first 128 round-trip unchanged for repair, while runtime surfaces explain
    why they are unavailable and Settings disables Add at the active limit.
11. **Install-source preflight** — desktop, metadata, SVG, and both PNG inputs
    are verified as readable non-symlink regular files before build/write.
12. **Non-empty descriptor input** — a zero-byte prebuilt binary is rejected
    before the old executable changes.
13. **Scoped staging ancestry** — non-root DESTDIR spellings are normalized and
    their full existing component chains reject disguised symlink roots before
    install/uninstall; normal host prefix links remain supported. This is a
    point-in-time preflight rather than a concurrent-race claim.
14. **Atomic AppStream/SVG commit** — public assets are staged beside their
    targets with mode 0644 and renamed into place.
15. **Atomic raster icons** — both shipped PNG sizes share that commit boundary.
16. **Desktop command structure gate** — canonical Exec/TryExec cardinality and
    absence of alternate commands are checked before rename.
17. **Unset-PATH resilience** — successful installs no longer trip nounset in
    post-install advice.
18. **Hostile-resource contracts** — tests cover zero-byte input, DESTDIR
    ancestor escape, and destination-link replacement without victim writes.
19. **Index-neutral private errors** — helpers lacking a list index use a safe
    neutral host label instead of inventing “remote host #1”.
20. **Bounded picker rows** — at most 256 drafts render per frame; entry 129 is
    still visible and later drafts are explicitly reported as retained.
21. **Bounded settings rows** — the Remote editor uses the same rendering cap
    without shrinking the underlying draft vector.
22. **Active selector cap** — file-tree and tab-menu execution choices expose
    only the first 128 profiles.
23. **Borrow-only picker input** — rendering no longer clones every full remote
    draft on every frame.
24. **Actionable save summary** — success feedback separately counts invalid
    active drafts and over-limit retained drafts.
25. **Shared problem accounting** — normalization and UI diagnostics use one
    active-invalid/inactive-retained calculation.
26. **Real disk round trip** — an atomic private config save/reload proves an
    invalid draft and the 129th entry both survive exactly.
27. **Neutral-label regression** — the disk fixture also proves an empty
    name/target produces bounded non-indexed runtime text.
28. **No-silent-truncation notice** — lists beyond 256 remain saved byte-for-byte
    unchanged and the picker/editor disclose their omitted count.
29. **Consumer fail-closed evidence** — remote-fs tests reject relative paths,
    missing/high indexes, and invalid runtime hosts before spawn; gate tests
    prove oversized/RLO deploy drafts are rejected without echoing raw bytes;
    Settings preflights deploy/user/argv budgets by reference before cloning a
    bounded draft for shared validation.
30. **Documented repair contract** — README now specifies bounded rendering,
    active limits, retained drafts, and atomic public-resource installation.

Verification: `bash scripts/test-install-paths.sh`, Ember config tests, and the
full workspace formatting/check/Clippy/test gates.
