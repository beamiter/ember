# Ember clipboard lifecycle patch

Upstream: smithay-clipboard 0.7.3, https://crates.io/crates/smithay-clipboard/0.7.3
Published archive SHA-256: `71704c03f739f7745053bde45fa203a46c58d25bc5c4efba1d9a60e9dba81226`.
Original MIT LICENSE and package sources are retained; upstream CI workflows are omitted.

Only production change: retain data and primary-selection devices when a seat loses
its keyboard capability. Keyboard release remains unchanged. Reconnection reuses
existing devices; removing the seat still drops them. This prevents late offers
from invalidating the shared Wayland display connection during virtual keyboard
hotplug. No clipboard routing or provider changes.

The native socket-pair compositor regression is adapted from Frost commit
132f18446413dbd73ac8ea958b7962d00c368994 (same upstream crate). It sends an in-flight
selection offer across keyboard removal/reconnection and checks the connection and
new selection survive. Run explicitly with:

`cargo test --manifest-path vendor/smithay-clipboard/Cargo.toml --lib native_keyboard_unplug -- --ignored --nocapture`

For local before/after verification, the independent extracted crate harness omits
only the example-only dev dependency's xkbcommon feature (system development files
are absent). Production features, Wayland system backend and wire test are identical.
The vendored manifest retains the original dev dependency and features.
