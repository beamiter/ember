//! Host-owned inputs captured before terminal parsing begins.

/// Immutable per-terminal host identity. Construct explicitly for embedded or
/// remote hosts; no environment or process identity is consulted while parsing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalHostContext {
    pub(super) local_hostname: Option<String>,
    pub(super) notification_app_name: String,
}

impl TerminalHostContext {
    /// Supply the host name accepted by OSC 7 and the trusted application name
    /// used when OSC 9/777 does not supply a title. PTY-authored fields still go
    /// through the parser's existing validation and notification sanitization.
    pub fn new(local_hostname: Option<String>, notification_app_name: impl Into<String>) -> Self {
        Self {
            local_hostname,
            notification_app_name: notification_app_name.into(),
        }
    }

    /// Compatibility resolver for local application callers. Call after app
    /// identity initialization and before parsing. The hostname retains the
    /// previous process-wide cache, including failed lookups; application
    /// identity is snapshotted for each terminal, not cached before app startup.
    pub fn resolve_local() -> Self {
        static LOCAL_HOSTNAME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        let local_hostname = LOCAL_HOSTNAME.get_or_init(|| {
            std::env::var("HOSTNAME").ok().or_else(|| {
                std::fs::read_to_string("/etc/hostname")
                    .ok()
                    .map(|hostname| hostname.trim().to_string())
            })
        });
        Self::new(local_hostname.clone(), jterm_core::identity::get().app_name)
    }

    pub(super) fn osc7_host_is_local(&self, host: &str) -> bool {
        host.is_empty()
            || host.eq_ignore_ascii_case("localhost")
            || self
                .local_hostname
                .as_deref()
                .is_some_and(|local| host.eq_ignore_ascii_case(local))
    }
}
