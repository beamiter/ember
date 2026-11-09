//! Frontend adapter. Runtime workers only receive the shared wake handle.
use jterm_core::runtime_wake::WakeSink;

pub fn egui_wake(context: eframe::egui::Context) -> WakeSink {
    WakeSink::new(move || context.request_repaint())
}
