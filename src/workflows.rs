//! Thin shim over the shared `jterm_core::workflows`.
//!
//! Ember used to carry its own port of anvil's workflow subsystem — directory
//! discovery, the bounded reader, both parsers, validation and the template
//! engine, ~870 lines with the tests that pinned them, and not one line of
//! egui in any of it. All four family terminals carried that same code against
//! the same on-disk format, and they had drifted in both directions, so what a
//! workflow file *meant* depended on which terminal opened it. Their union now
//! lives in the core; ember keeps the policy below and nothing else.
//!
//! The core is stricter than ember's port was, and adopting it is the point of
//! the migration:
//!
//! - **An argument that declares no default is no longer filled by a blank
//!   string.** Ember's dialog pre-seeded every declared argument with `""`, so
//!   `render`'s missing-value guard could never fire — ember implemented it,
//!   unit-tested it, and defeated it from its own UI. `kill -9 {pid}` with an
//!   untouched Pid field rendered `kill -9 ` and was filled at the prompt. The
//!   rule now lives in `render`, applied to the values map itself, so no
//!   dialog can seed its way past it.
//! - **`{{` and `}}` nest.** Ember's close scan ran to the end of the
//!   template, so `awk '{{print $1}' file` round-tripped while
//!   `awk '{{print $1}' {{log}}` let the first `{{` claim the placeholder's
//!   `}}`, fall into the literal-brace branch, and hand the user a different,
//!   executable awk program.
//! - **A declared argument name must equal its own trim.** Placeholder names
//!   were trimmed and declared names were not, so a quoted `name = "pid "`
//!   loaded clean, bound nothing, rendered the literal `{ pid }`, and threw
//!   away what the user typed while the missing-value guard called the form
//!   complete.
//! - **Both halves of a log line are sanitised.** Ember logged
//!   `path.display()` and the parser's message raw; a TOML error quotes the
//!   offending source line back verbatim, so a workflow file chose the bytes
//!   that reached whatever tty was tailing ember's log.
//!
//! What ember contributed to the union is the `dirs`-crate discovery backend,
//! now the core's [`XdgEnvDirs`] and the default for an app with no GTK
//! dependency. Forge originated the stricter `O_NOFOLLOW` reader; ember had
//! already adopted it before the union, while anvil still followed a planted
//! symlink out of the workflow directory.
//!
//! `welcome_notebook_path` is still not ported: ember has no notebook surface,
//! and the core deliberately left that lookup in the two apps that have one.
//!
//! Refreshing stays synchronous-on-open (`workflow_picker_toggle`), matching
//! the history picker: egui is single-threaded immediate-mode and the read
//! caps keep the worst case small. The core exports anvil's `RefreshLatch` for
//! the toolkit that needs it; ember does not.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use jterm_core::workflows::{
    load_one, search_path, workflow_files_in, DirSources as _, SearchPathSpec, XdgEnvDirs,
    MAX_WORKFLOW_DIRECTORIES,
};

pub use jterm_core::workflows::{render, ArgsForm, LoadOrder, Workflow, WorkflowArg};

/// The directory segment ember looks under, and — derived from it by
/// [`SearchPathSpec::for_app`] — the `EMBER_WORKFLOW_DIR` override it honours.
/// Deriving both from one name is what stops an app from reading one
/// directory while obeying another app's variable.
///
/// Spelled out rather than taken from `jterm_core::identity`:
/// `SearchPathSpec::for_current_app` is `None` until `identity::init` runs,
/// and this module's own tests run in a binary that never calls it.
const APP: &str = "ember";

/// XDG lookups through the `dirs` crate plus `XDG_DATA_DIRS` — ember's own
/// backend, now the core's default. It is injected rather than assumed because
/// anvil and forge ask glib instead, and the two answers differ exactly where
/// it matters: `glib::user_config_dir()` never fails, `dirs::config_dir()`
/// returns `None` with `HOME` unset and the tier is then skipped rather than
/// resolved against the process's working directory.
const DIR_SOURCES: XdgEnvDirs = XdgEnvDirs;

/// Ember's picker lists alphabetically, so the library is sorted by workflow
/// name across every directory.
///
/// [`LoadOrder`] has no `Default` on purpose — anvil and frost list in
/// directory-precedence order instead, and in the four copies this was the
/// presence or absence of a single `sort_by` line. Ember states it once, here:
/// the picker no longer re-sorts what it is handed.
pub const LOAD_ORDER: LoadOrder = LoadOrder::ByName;

/// The source-tree examples, ember's lowest-precedence tier.
///
/// `env!("CARGO_MANIFEST_DIR")` is resolved at compile time against the crate
/// being compiled, which is why the core takes this as a parameter: evaluated
/// there it would point every app at `jterm_core/scripts/workflows`, and the
/// bundled-library contract tests below would keep passing while asserting
/// about a directory that does not exist.
fn dev_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("workflows")
}

/// Every per-app choice discovery makes, in one place.
fn spec() -> SearchPathSpec {
    SearchPathSpec::for_app(APP, Some(dev_root()))
}

/// Workflow search path in precedence order: `~/.config/ember/workflows`,
/// `$EMBER_WORKFLOW_DIR` (additive, never replacing the standard locations),
/// the user data directory, each system data directory, then the source tree.
pub fn workflow_dirs() -> Vec<PathBuf> {
    search_path(&spec(), &DIR_SOURCES)
}

/// The first tier of [`workflow_dirs`], for the picker's empty state — the one
/// place ember has to *name* a directory rather than read it.
///
/// Repeating the `<config>/<app>/workflows` shape is the only duplication left
/// in this file, and `the_directory_the_empty_picker_names_is_the_one_read_first`
/// pins it against the search path so the two cannot drift.
pub fn user_workflow_dir() -> Option<PathBuf> {
    DIR_SOURCES
        .user_config_dir()
        .map(|base| base.join(APP).join("workflows"))
}

/// Load every workflow file under `dirs` in ember's pinned [`LOAD_ORDER`].
///
/// One broken file never disables the rest: it is skipped and logged with its
/// path and the reason, both sanitised.
pub fn load_all(dirs: &[PathBuf]) -> Vec<Workflow> {
    jterm_core::workflows::load_all(dirs, LOAD_ORDER)
}

/// One completed picker refresh: accepted entries plus bounded refusal details.
#[derive(Clone, Debug, Default)]
pub struct LibraryScan {
    pub workflows: Vec<Workflow>,
    pub refused: Vec<(PathBuf, String)>,
}

/// Load the picker library and identify workflow-looking files the shared
/// loader rejected. Healthy files are not opened twice.
pub fn scan(dirs: &[PathBuf]) -> LibraryScan {
    let workflows = load_all(dirs);
    let refused = refused_files(dirs, &workflows);
    LibraryScan { workflows, refused }
}

/// Keep UI diagnostics bounded independently from the library's much larger
/// admitted-workflow budget. One example is enough for the toast; retaining a
/// small set lets the app suppress repeats and notice which refusal changed.
const MAX_REFUSALS_REPORTED: usize = 64;

fn refused_files(dirs: &[PathBuf], loaded: &[Workflow]) -> Vec<(PathBuf, String)> {
    let accepted: HashSet<&Path> = loaded
        .iter()
        .filter_map(|workflow| workflow.source_path.as_deref())
        .collect();
    let mut refused = Vec::new();
    for dir in dirs.iter().take(MAX_WORKFLOW_DIRECTORIES) {
        if !dir.is_dir() {
            continue;
        }
        for path in workflow_files_in(dir) {
            if accepted.contains(path.as_path()) {
                continue;
            }
            if let Err(reason) = load_one(&path) {
                refused.push((path, reason));
                if refused.len() >= MAX_REFUSALS_REPORTED {
                    return refused;
                }
            }
        }
    }
    refused
}

#[cfg(test)]
mod tests {
    use super::*;
    use jterm_core::workflows::MAX_WORKFLOW_DIRECTORIES;
    use std::collections::HashSet;

    /// Everything this app decides on a surface whose engine it no longer
    /// owns. Each of these can be spelled wrong and still compile, and each
    /// changes which files a user's palette shows.
    #[test]
    fn discovery_and_load_order_state_every_policy_this_app_owns() {
        let spec = spec();
        assert_eq!(spec.app(), "ember");
        assert_eq!(
            spec.env_var(),
            "EMBER_WORKFLOW_DIR",
            "the override variable is derived from the segment; a hand-typed \
             name is how an app comes to honour another app's variable"
        );
        assert_eq!(
            spec.dev_root(),
            Some(dev_root().as_path()),
            "the dev tier must be ember's manifest directory, not jterm_core's"
        );
        assert_eq!(LOAD_ORDER, LoadOrder::ByName);
    }

    /// The order pinned above, observed through the function the picker calls
    /// — the constant alone would still be true if the shim forgot to pass it.
    /// Filename order and name order disagree here on purpose.
    #[test]
    fn load_all_applies_this_app_s_pinned_name_order() {
        let dir = tempdir();
        std::fs::write(dir.join("a.yaml"), "name: Zeta\ncommand: echo z\n").unwrap();
        std::fs::write(dir.join("b.yaml"), "name: Alpha\ncommand: echo a\n").unwrap();
        let names: Vec<String> = load_all(std::slice::from_ref(&dir))
            .into_iter()
            .map(|workflow| workflow.name)
            .collect();
        assert_eq!(names, ["Alpha", "Zeta"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn scan_keeps_healthy_entries_and_reports_the_broken_candidate() {
        let dir = tempdir();
        let good = dir.join("good.yaml");
        let broken = dir.join("broken.toml");
        std::fs::write(&good, "name: Healthy\ncommand: echo ok\n").unwrap();
        std::fs::write(&broken, "name = \"Broken\"\ncommand = [\n").unwrap();

        let scan = scan(std::slice::from_ref(&dir));
        assert_eq!(scan.workflows.len(), 1);
        assert_eq!(scan.workflows[0].name, "Healthy");
        assert_eq!(scan.refused.len(), 1);
        assert_eq!(scan.refused[0].0, broken);
        assert!(scan.refused[0].1.starts_with("parse TOML:"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn workflow_dirs_are_unique_bounded_and_end_at_the_source_tree() {
        let dirs = workflow_dirs();
        let mut seen = HashSet::new();
        assert!(dirs.iter().all(|dir| seen.insert(dir)));
        assert!(dirs.len() <= MAX_WORKFLOW_DIRECTORIES);
        assert_eq!(dirs.last(), Some(&dev_root()));
    }

    #[test]
    fn the_directory_the_empty_picker_names_is_the_one_read_first() {
        // Skipped rather than asserted when there is no home directory: that
        // is the case where ember has no user tier at all, and the picker's
        // hint falls back to a literal path.
        let Some(user_dir) = user_workflow_dir() else {
            return;
        };
        assert_eq!(workflow_dirs().first(), Some(&user_dir));
    }

    /// The shipped library must never rot against the loader's validation
    /// rules — ember is one of the three apps whose `scripts/workflows` files
    /// are byte-identical, and a file that stops parsing must break a build
    /// rather than vanish from a palette.
    ///
    /// The candidate count is taken with an extension test written out here
    /// rather than with `jterm_core::workflows::is_workflow_file`: an oracle
    /// that shares the loader's own predicate cannot notice the predicate
    /// changing under it.
    #[test]
    fn every_bundled_workflow_is_parseable_and_review_only() {
        let dir = dev_root();
        let candidate_count = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        matches!(
                            extension.to_ascii_lowercase().as_str(),
                            "toml" | "yaml" | "yml"
                        )
                    })
            })
            .count();
        let workflows = load_all(std::slice::from_ref(&dir));
        assert_eq!(workflows.len(), candidate_count);
        assert!(workflows.len() >= 6);
        assert!(workflows
            .iter()
            .all(|workflow| jterm_core::review_input::validate(&workflow.command).is_ok()));
    }

    fn tempdir() -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ember-workflows-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
