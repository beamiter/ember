# Winit 0.30.13 X11 wheel correction

This copy is extracted from the crates.io package verified against Cargo.lock:

`a6755fa58a9f8350bd1e472d4c3fcc25f824ec358933bba33306d0b63df5978d`

The wheel patch ignores XI_ButtonRelease for legacy wheel buttons 4–7
in xinput2_button_input. The original emits MouseWheel for both halves of a
single press/release pair.

Upstream source: https://github.com/rust-windowing/winit/blob/v0.30.13/src/platform_impl/linux/x11/event_processor.rs

The native Vim buffer window smoke injects a complete X11 notch, checks exactly
three source rows and traces intermediate GUI pixel positions. This patch can
be removed after upgrading to an upstream release with equivalent behavior.

## Layout and lock state in native keyboard events

`KeyEvent::unmodified_key` borrows the layout key with modifiers removed
on platforms that provide it. XKB snapshots Caps Lock and Num Lock once
per key event. Ember uses those snapshots to encode Kitty input on X11
and Wayland.
