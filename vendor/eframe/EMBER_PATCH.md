# Pinned eframe native-key hook

Upstream: emilk/egui, crates/eframe at revision 4c1f2fae95475a40e524884ebb298bcb1714b08e, version 0.36.1.
Official crates.io archive SHA256: 2afc0cbcdb6896b7bfb1dbbebaf7b9af9635ff38fd01e89bdd0174c1717b1857. This matches the original Ember lockfile and verified archive.

All 35 original files (538,493 content bytes) are preserved. Apache-2.0 and MIT licenses were retrieved from the exact upstream revision because the published archive omits the root license files. UPSTREAM_INTEGRITY.json records every original and local file hash; only four upstream files differ.

## Local change

A default-false App::on_native_key_event callback runs at the shared native conversion boundary. Glow and Wgpu pass the application and viewport to that boundary. The callback exposes winit physical identity and synthetic-event metadata before egui-winit merges Enter and NumpadEnter. Non-key events and default implementations retain their existing behavior. No changes were made to egui, winit, platform code, or graphics logic.

The Ember consumer is root-window-only. It filters confirmation-owned Enter presses and forwards every release so egui cannot retain a stale logical pressed state. Overlapping physical Enter keys join the same owned gesture. Synthetic pressed snapshots populate passive held-key identity and join any existing owned gesture, without becoming a fresh action. Synthetic releases cannot clear ownership. If a real key-up is lost while unfocused, that physical key remains conservatively owned until its real release is observed, potentially requiring a harmless press/release cycle of the same key. There is no timing heuristic or global input capture.

## Validation scope and maintenance

Ember adopts this bounded native-key hook for its Linux X11 and Wayland targets. Both native renderers compile with X11-only and Wayland-only feature combinations; actual X11 Glow and Wgpu recorder tests exercise the hook. Real Wayland and native IME validation are unavailable in this cloud because creating AF_UNIX sockets returns EPERM. Actual egui Text/Paste/IME commit regressions are tested separately. Windows/macOS receive source-level compatibility review only, with no compile/runtime claim. The hook is excluded on wasm; the web implementation is unchanged.

Dependency/source/license policy and RustSec checks pass. Both the patched lockfile and the pre-vendor registry lockfile were audited so the path override cannot hide eframe's upstream advisory identity. No advisory exceptions were added; the existing ttf-parser unmaintained-notice exception is unchanged.

Future eframe updates must rebase or remove this documented small patch. Vendoring introduces a maintenance obligation despite the small functional delta. Preserve the checksum, both licenses, integrity manifest, and paired-renderer regression gates during upgrades.
