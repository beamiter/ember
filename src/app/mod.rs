// Application core module - main app state and coordination

pub(crate) mod block_review;
pub mod commands;
pub mod events;
pub mod input;
pub mod rendering;
pub mod state;
pub mod tabs;
pub mod tasks;
pub mod window;

#[cfg(test)]
mod visual_test_support;
