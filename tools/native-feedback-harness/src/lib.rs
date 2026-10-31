//! Compile the exact production receipt/scope code without a GUI or Vulkan device.
#![allow(dead_code)]

pub mod ffi {
    pub use x11_dl::xlib::XClientMessageEvent;
}
pub struct XConnection(x11rb::rust_connection::RustConnection);
impl XConnection {
    fn watch_frame_authority(
        &self,
        _: u32,
        _: u32,
        _: u32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err("real Xlib authority wiring requires native X11 acceptance".into())
    }
    fn xcb_connection(&self) -> &x11rb::rust_connection::RustConnection {
        &self.0
    }
}

#[path = "../../../vendor/winit/src/platform_impl/linux/x11/frame_feedback.rs"]
mod frame_feedback;
#[path = "../../../vendor/wgpu-hal/src/vulkan/publication.rs"]
mod publication;
