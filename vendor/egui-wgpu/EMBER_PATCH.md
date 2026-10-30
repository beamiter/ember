# Native presentation evidence hook

Upstream `egui-wgpu` 0.36.2, exact version from Ember's prior lockfile.
Official crate archive SHA-256:
`5cef1f6d1bba40b09a4d0d62cb822b81030922cfbf65c5373398c3a3caafc5bd`.
Upstream source revision `49682f8baa058bf49e011035cfbd6e825f88a5ef`, `crates/egui-wgpu`.
MIT and Apache licenses fetched from this exact upstream revision.

Only runtime patch: the winit Painter clears `last_presented_viewport` before
painting, sets it immediately after `queue.present(output_frame)` and exposes a
read-only getter. All early-return, invisible, surface-lost and failed paths
therefore leave no success evidence. The patched eframe calls its default-no-op
`App::on_native_frame_presented` only when that getter matches the viewport.
Glow calls the same hook only after successful `swap_buffers`.

This proves a native buffer submission boundary, NOT actual compositor scanout,
physical monitor visibility, or frozen-window safety on every WM. The shared
protocol capability is NativePresented, never CompositorPresented. A blocked or
missing renderer must withhold proof. Rebase this narrow patch when upgrading
upstream, preserving the early-return invariant and both renderer hooks.
