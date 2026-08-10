//! Provider adapters for the native Agent runtime.

pub mod fake;

pub use fake::{FakeAgentDriver, FakeAgentEvent, FakeAgentProgress};
