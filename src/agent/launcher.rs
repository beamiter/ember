//! Compatibility launcher for opaque Agent CLIs hosted in ordinary PTYs.
//!
//! Native provider protocols will eventually emit structured `AgentEvent`
//! values.  This launcher is the P0 bridge: it resolves a provider executable
//! before PTY creation, passes an exact argv (never a shell command string),
//! and relies on `SessionManager` to start it in the task worktree.

use super::AgentProvider;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLaunchSpec {
    pub provider: AgentProvider,
    pub executable: PathBuf,
    pub argv: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentLaunchError {
    RepositoryMustBeAbsolute,
    RepositoryUnavailable(PathBuf),
    WorktreeMustBeAbsolute,
    WorktreeUnavailable(PathBuf),
    WorktreeNotUtf8,
    ExecutableInsideRepository(PathBuf),
    ExecutablePathNotUtf8(PathBuf),
    ExecutableUnavailable {
        provider: AgentProvider,
        detail: String,
    },
}

impl fmt::Display for AgentLaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RepositoryMustBeAbsolute => {
                formatter.write_str("task repository must be absolute")
            }
            Self::RepositoryUnavailable(path) => write!(
                formatter,
                "task repository is unavailable or not canonical: {}",
                path.display()
            ),
            Self::WorktreeMustBeAbsolute => formatter.write_str("task worktree must be absolute"),
            Self::WorktreeUnavailable(path) => {
                write!(
                    formatter,
                    "task worktree is not a directory: {}",
                    path.display()
                )
            }
            Self::WorktreeNotUtf8 => formatter.write_str("task worktree path is not valid UTF-8"),
            Self::ExecutableInsideRepository(path) => write!(
                formatter,
                "refusing to run a repository-controlled Agent executable: {}",
                path.display()
            ),
            Self::ExecutablePathNotUtf8(path) => write!(
                formatter,
                "Agent executable path is not valid UTF-8: {}",
                path.display()
            ),
            Self::ExecutableUnavailable { provider, detail } => write!(
                formatter,
                "{} is not available: {detail}",
                provider.display_name()
            ),
        }
    }
}

impl std::error::Error for AgentLaunchError {}

impl AgentLaunchSpec {
    /// Resolve the selected provider using the process PATH and pin the exact
    /// absolute executable path into argv before any PTY is created.
    pub fn resolve(
        provider: AgentProvider,
        repository: &Path,
        worktree: &Path,
    ) -> Result<Self, AgentLaunchError> {
        Self::resolve_with_path(
            provider,
            repository,
            worktree,
            std::env::var_os("PATH").as_deref(),
        )
    }

    fn resolve_with_path(
        provider: AgentProvider,
        repository: &Path,
        worktree: &Path,
        path: Option<&OsStr>,
    ) -> Result<Self, AgentLaunchError> {
        if !repository.is_absolute() {
            return Err(AgentLaunchError::RepositoryMustBeAbsolute);
        }
        let repository = std::fs::canonicalize(repository)
            .ok()
            .filter(|resolved| resolved == repository && resolved.is_dir())
            .ok_or_else(|| AgentLaunchError::RepositoryUnavailable(repository.to_path_buf()))?;
        if !worktree.is_absolute() {
            return Err(AgentLaunchError::WorktreeMustBeAbsolute);
        }
        let worktree = std::fs::canonicalize(worktree)
            .ok()
            .filter(|resolved| resolved == worktree && resolved.is_dir())
            .ok_or_else(|| AgentLaunchError::WorktreeUnavailable(worktree.to_path_buf()))?;
        worktree.to_str().ok_or(AgentLaunchError::WorktreeNotUtf8)?;
        let program = provider.executable_name();
        // A task worktree is repository-controlled. Never apply execvp's
        // relative/empty PATH semantics against it: PATH=".:..." must not let
        // a checkout replace the Agent binary Ember launches. The shared host
        // helper searches absolute PATH entries only and returns an absolute,
        // executable file.
        let executable = jterm_core::host::find_executable_in(program, path).ok_or_else(|| {
            AgentLaunchError::ExecutableUnavailable {
                provider,
                detail: "executable was not found in an absolute PATH directory".to_string(),
            }
        })?;
        let executable = std::fs::canonicalize(&executable).map_err(|error| {
            AgentLaunchError::ExecutableUnavailable {
                provider,
                detail: format!("cannot resolve executable: {error}"),
            }
        })?;
        if executable.starts_with(&repository) || executable.starts_with(&worktree) {
            return Err(AgentLaunchError::ExecutableInsideRepository(executable));
        }
        let executable_arg = executable
            .to_str()
            .ok_or_else(|| AgentLaunchError::ExecutablePathNotUtf8(executable.clone()))?
            .to_string();
        Ok(Self {
            provider,
            executable,
            argv: vec![executable_arg],
        })
    }
}

impl AgentProvider {
    pub fn executable_name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::OpenCode => "opencode",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ember-agent-launcher-{label}-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn provider_names_are_explicit_and_stable() {
        assert_eq!(AgentProvider::Codex.executable_name(), "codex");
        assert_eq!(AgentProvider::Claude.executable_name(), "claude");
        assert_eq!(AgentProvider::OpenCode.executable_name(), "opencode");
    }

    #[test]
    fn resolves_exact_executable_without_building_a_shell_command() {
        let root = TempDir::new("resolve");
        let bin = root.0.join("bin");
        let repository = root.0.join("repository");
        let worktree = root.0.join("worktree");
        fs::create_dir(&bin).unwrap();
        fs::create_dir(&repository).unwrap();
        fs::create_dir(&worktree).unwrap();
        let codex = bin.join("codex");
        fs::write(&codex, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();

        let spec = AgentLaunchSpec::resolve_with_path(
            AgentProvider::Codex,
            &repository,
            &worktree,
            Some(bin.as_os_str()),
        )
        .unwrap();

        assert!(spec.executable.is_absolute());
        assert_eq!(spec.executable, codex);
        assert_eq!(spec.argv, vec![codex.to_string_lossy().into_owned()]);
    }

    #[test]
    fn missing_provider_and_invalid_worktree_fail_before_pty_spawn() {
        let root = TempDir::new("missing");
        let repository = root.0.join("repository");
        fs::create_dir(&repository).unwrap();
        let missing_worktree = root.0.join("missing-worktree");
        assert!(matches!(
            AgentLaunchSpec::resolve_with_path(
                AgentProvider::Claude,
                &repository,
                &missing_worktree,
                Some(root.0.as_os_str())
            ),
            Err(AgentLaunchError::WorktreeUnavailable(_))
        ));

        assert!(matches!(
            AgentLaunchSpec::resolve_with_path(
                AgentProvider::Claude,
                &repository,
                &root.0,
                Some(root.0.as_os_str())
            ),
            Err(AgentLaunchError::ExecutableUnavailable { .. })
        ));
    }

    #[test]
    fn repository_cannot_hijack_agent_through_relative_path_entries() {
        let root = TempDir::new("path-hijack");
        let trusted_bin = root.0.join("bin");
        let repository = root.0.join("repository");
        let worktree = root.0.join("worktree");
        fs::create_dir(&trusted_bin).unwrap();
        fs::create_dir(&repository).unwrap();
        fs::create_dir(&worktree).unwrap();

        let trusted_codex = trusted_bin.join("codex");
        let repository_codex = worktree.join("codex");
        for executable in [&trusted_codex, &repository_codex] {
            fs::write(executable, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(executable, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = std::ffi::OsString::from(format!(":.:{}", trusted_bin.display()));

        let spec = AgentLaunchSpec::resolve_with_path(
            AgentProvider::Codex,
            &repository,
            &worktree,
            Some(path.as_os_str()),
        )
        .unwrap();

        assert_eq!(spec.executable, trusted_codex);
        assert_ne!(spec.executable, repository_codex);
    }

    #[test]
    fn absolute_or_symlinked_repository_executable_is_rejected() {
        let root = TempDir::new("absolute-repository-path");
        let repository = root.0.join("repository");
        let repository_bin = repository.join("bin");
        let link_bin = root.0.join("link-bin");
        let worktree = root.0.join("worktree");
        fs::create_dir(&repository).unwrap();
        fs::create_dir(&repository_bin).unwrap();
        fs::create_dir(&link_bin).unwrap();
        fs::create_dir(&worktree).unwrap();
        let repository_codex = repository_bin.join("codex");
        fs::write(&repository_codex, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&repository_codex, fs::Permissions::from_mode(0o700)).unwrap();

        for candidate_dir in [&repository_bin, &link_bin] {
            let linked = candidate_dir == &link_bin;
            if linked {
                symlink(&repository_codex, link_bin.join("codex")).unwrap();
            }
            let error = AgentLaunchSpec::resolve_with_path(
                AgentProvider::Codex,
                &repository,
                &worktree,
                Some(candidate_dir.as_os_str()),
            )
            .unwrap_err();
            assert_eq!(
                error,
                AgentLaunchError::ExecutableInsideRepository(repository_codex.clone())
            );
        }
    }
}
