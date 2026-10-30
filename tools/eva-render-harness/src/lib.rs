//! Same production egui rendering module; pure atlas re-export only, no IPC/display substitute.
extern crate self as jterm_core;
pub mod companion_host { pub use eva_character_atlas as atlas; }
#[cfg(test)]
#[path = "../../../src/eva_glyphs.rs"]
mod eva_glyphs;
