# Mouse modifier host integration

Ember passes xterm Cb modifier bits (Shift=4, Alt/Meta=8, Ctrl=16) to the
shared engine for application button press, motion and release reports.
Super/Command does not add a protocol bit. The shared core revision is
`80195416ed2a16e4d3f13418b5c05493076839f5`; its legacy release encoder retains
these modifier bits while replacing the button bits with release=3.

The host still decides gesture ownership at button-down. Shift starts a local
selection; releasing Shift during that selection cannot produce application
motion or an orphan release. Adding Shift to an application-owned gesture does
not steal it. Ctrl-link activation retains its existing host override.

Button edges use the matching egui event's modifier snapshot, rather than the
frame-final keyboard state. Motion uses current modifiers. Pointer/focus
cancellation admits no new gesture or motion and uses the captured modifier
snapshot if no explicit release event exists. Captured terminal, writer and
last real cell continue to route the release, including inactive-pane releases.
Cancellation suppresses PRIMARY copying from a local selection. Backpressured
control bytes remain encoded and ordered; subsequent mode changes cannot
rewrite them. If the application disables reporting before a release is
encoded, the existing route-closing policy applies.

Regression commands:

```sh
cargo test --locked --bin ember mouse_
cargo test --locked --lib mouse_
```

The added host regression checks distinct press/release snapshots, Shift
ownership and cancellation fallback. The independent facade byte oracle covers
all eight modifier combinations and three buttons, press/drag/release, legacy,
UTF-8, urxvt and SGR releases, live encoding changes and reporting disable.
These are automated input/protocol regressions, not physical mouse acceptance.
Existing native CI smoke tests provide separate application-level evidence.
Wheel routing and the existing Ctrl-wheel font-zoom policy are unchanged.
