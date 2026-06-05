use std::sync::OnceLock;

// Only called from the debug_log! macro, which compiles to a no-op in release builds.
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();

    *ENABLED.get_or_init(|| {
        std::env::var_os("JTERM2_DEBUG")
            .map(|value| value != "0")
            .unwrap_or(false)
    })
}

#[macro_export]
macro_rules! debug_log {
    ($($arg:tt)*) => {
        #[cfg(debug_assertions)]
        {
            if $crate::debug::enabled() {
                eprintln!($($arg)*);
            }
        }
    };
}
