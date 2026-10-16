//! Adapter-owned, bounded creation journal. This is NOT task/runtime persistence.
//! Git and this file have separate durability boundaries; see the protocol fixture.
use super::{AgentProvider, ManagedWorktree, NewTask, TaskId, TaskManager, WorktreeService};
use crate::persistence_file::{self, FileRevision};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

pub const MAX_RECORDS: usize = 128;
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Requested,
    Prepared,
    Registered,
}

/// Deliberately excludes title, command, output, environment and provider secrets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub id: uuid::Uuid,
    pub repository: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    pub provider: AgentProvider,
    pub stage: Stage,
    pub base_commit: Option<String>,
    pub directory_identity: Option<DirectoryIdentity>,
    pub registered_task_id: Option<TaskId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

fn directory_identity(path: &Path) -> io::Result<DirectoryIdentity> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Err(invalid("worktree directory was replaced"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(DirectoryIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(not(unix))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "worktree identity requires Unix",
    ))
}

impl Record {
    pub fn requested(root: &Path, repository: PathBuf, provider: AgentProvider) -> Self {
        let id = uuid::Uuid::new_v4();
        let name = format!("task-{}", id.simple());
        Self {
            id,
            repository,
            path: root.join(&name),
            branch: format!("ember/{name}"),
            provider,
            stage: Stage::Requested,
            base_commit: None,
            directory_identity: None,
            registered_task_id: None,
        }
    }

    pub fn task_name(&self) -> String {
        format!("task-{}", self.id.simple())
    }

    pub fn new_task(&self) -> Result<NewTask, String> {
        if self.stage != Stage::Prepared {
            return Err(
                "Only prepared records can be retried; inspect this worktree with Open.".into(),
            );
        }
        Ok(NewTask {
            title: "Recovered worktree (command context was not saved)".into(),
            provider: self.provider,
            repo_root: self.repository.clone(),
            worktree_path: self.path.clone(),
            branch: self.branch.clone(),
            base_commit: self
                .base_commit
                .clone()
                .ok_or("Missing original base commit")?,
            source_context: None,
        })
    }

    /// Must run off the UI thread. This never runs Git creation or cleanup.
    pub fn verify(&self, root: &Path) -> Result<(), String> {
        validate_record(self, root).map_err(|_| "Invalid recovery identity".to_string())?;
        if let Some(expected) = &self.directory_identity {
            if directory_identity(&self.path).ok().as_ref() != Some(expected) {
                return Err(
                    "The original worktree directory was replaced. No registration was attempted."
                        .into(),
                );
            }
        }
        WorktreeService::new(root)
            .and_then(|service| {
                service.verify_active_task_worktree(&self.repository, &self.path, &self.branch)
            })
            .map_err(|_| {
                "Worktree identity could not be verified. No files or branches were changed."
                    .to_string()
            })
    }
}

/// One runtime task per recorded identity, including retries after marker-write errors.
/// Call only after `Record::verify`, on the UI thread that owns TaskManager.
pub fn register(
    manager: &mut TaskManager,
    record: &Record,
    task: NewTask,
) -> Result<TaskId, String> {
    if task.repo_root != record.repository
        || task.worktree_path != record.path
        || task.branch != record.branch
        || Some(&task.base_commit) != record.base_commit.as_ref()
        || task.provider != record.provider
        || record.stage != Stage::Prepared
    {
        return Err("Task does not match the prepared recovery record.".into());
    }
    if let Some(existing) = manager
        .tasks()
        .iter()
        .find(|task| task.worktree_path == record.path)
    {
        if existing.repo_root == record.repository
            && existing.branch == record.branch
            && existing.base_commit == record.base_commit.as_deref().unwrap_or("")
            && existing.provider == record.provider
        {
            return Ok(existing.id);
        }
        return Err("This worktree is already attached to a different task identity.".into());
    }
    manager.create(task).map_err(|_| "Task registration rejected its metadata. The worktree is preserved; Retry uses a generic title without saved command context.".into())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    records: Vec<Record>,
}

/// An interprocess lease spans request → Git creation → prepared → registration.
/// Other windows cannot dismiss a request while a worktree is being created.
/// The file lock is distinct from the persistence writer's parent-directory lock.
pub struct Journal {
    root: PathBuf,
    path: PathBuf,
    revision: FileRevision,
    records: Vec<Record>,
    _lease: File,
    #[cfg(test)]
    write_fault: Option<(bool, i32)>,
}

impl Drop for Journal {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: the lease descriptor is live. Release explicitly: an
            // unrelated concurrently forked child may still hold the same open
            // file description briefly before CLOEXEC, even after our close.
            unsafe {
                libc::flock(self._lease.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub fn error_message(error: &io::Error) -> String {
    // Do not echo external command stderr, file contents or arbitrary OS paths.
    format!("Recovery journal unavailable ({:?}). Worktrees are preserved. Retry Reload after fixing storage/access; at the 128-record limit, dismiss inspected records.", error.kind())
}

impl Journal {
    pub fn open(root: &Path) -> io::Result<Self> {
        let directory = root.join(".creation-recovery");
        persistence_file::ensure_private_directory(&directory)?;
        // Persist the new journal directory entry, and the managed-root entry
        // if this was its first creation. No cross-filesystem atomicity is implied.
        File::open(root)?.sync_all()?;
        if let Some(parent) = root.parent() {
            File::open(parent)?.sync_all()?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let lease = options.open(directory.join("lock"))?;
        let metadata = lease.metadata()?;
        if !metadata.is_file() {
            return Err(invalid("recovery lock must be a regular file"));
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            // SAFETY: geteuid has no preconditions; flock borrows a live descriptor.
            if metadata.nlink() != 1
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err(invalid("recovery lock must be private and owned"));
            }
            if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        #[cfg(not(unix))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "recovery locking requires Unix",
        ));
        let path = directory.join("journal.json");
        let revision = persistence_file::read_revision(&path, MAX_BYTES)?;
        let records = match revision.bytes() {
            None => Vec::new(),
            Some(bytes) => {
                let doc: Document = serde_json::from_slice(bytes)
                    .map_err(|_| invalid("invalid recovery document"))?;
                if doc.version != 1 || doc.records.len() > MAX_RECORDS {
                    return Err(invalid("unsupported or oversized recovery document"));
                }
                let mut ids = HashSet::new();
                for record in &doc.records {
                    validate_record(record, root)?;
                    if !ids.insert(record.id) {
                        return Err(invalid("duplicate recovery identity"));
                    }
                }
                doc.records
            }
        };
        Ok(Self {
            root: root.to_owned(),
            path,
            revision,
            records,
            _lease: lease,
            #[cfg(test)]
            write_fault: None,
        })
    }

    pub fn records(&self) -> &[Record] {
        &self.records
    }

    pub fn request(&mut self, record: Record) -> io::Result<()> {
        if self.records.len() >= MAX_RECORDS {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "recovery journal is full",
            ));
        }
        validate_record(&record, &self.root)?;
        if record.stage != Stage::Requested || self.records.iter().any(|r| r.id == record.id) {
            return Err(invalid("request must have a fresh identity"));
        }
        let mut next = self.records.clone();
        next.push(record);
        self.commit(next)
    }

    pub fn prepared(&mut self, id: uuid::Uuid, worktree: &ManagedWorktree) -> io::Result<Record> {
        let mut next = self.records.clone();
        let record = next
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| invalid("request is missing"))?;
        if record.stage != Stage::Requested
            || record.repository != worktree.repository
            || record.path != worktree.path
            || record.branch != worktree.branch
        {
            return Err(invalid("prepared worktree does not match request"));
        }
        record.stage = Stage::Prepared;
        record.base_commit = Some(worktree.head.clone());
        record.directory_identity = Some(directory_identity(&worktree.path)?);
        validate_record(record, &self.root)?;
        let record = record.clone();
        self.commit(next)?;
        Ok(record)
    }

    pub fn registered(&mut self, id: uuid::Uuid, task_id: TaskId) -> io::Result<()> {
        let mut next = self.records.clone();
        let record = next
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| invalid("prepared record is missing"))?;
        if record.stage != Stage::Prepared {
            return Err(invalid("record is not prepared"));
        }
        record.stage = Stage::Registered;
        record.registered_task_id = Some(task_id);
        self.commit(next)
    }

    /// Explicit user action: forget metadata only, without touching Git or worktrees.
    pub fn dismiss(&mut self, id: uuid::Uuid) -> io::Result<()> {
        let next = self
            .records
            .iter()
            .filter(|r| r.id != id)
            .cloned()
            .collect();
        self.commit(next)
    }

    fn commit(&mut self, next: Vec<Record>) -> io::Result<()> {
        let bytes = serde_json::to_vec(&Document {
            version: 1,
            records: next.clone(),
        })
        .map_err(|_| invalid("could not encode recovery document"))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("recovery document exceeds byte limit"));
        }
        #[cfg(test)]
        if let Some((after_publish, errno)) = self.write_fault.take() {
            if after_publish {
                persistence_file::write_atomic_if_unchanged(
                    &self.path,
                    &bytes,
                    &self.revision,
                    MAX_BYTES,
                )?;
            }
            return Err(io::Error::from_raw_os_error(errno));
        }
        self.revision = persistence_file::write_atomic_if_unchanged(
            &self.path,
            &bytes,
            &self.revision,
            MAX_BYTES,
        )?;
        self.records = next;
        Ok(())
    }
}

fn validate_record(record: &Record, root: &Path) -> io::Result<()> {
    for path in [&record.repository, &record.path] {
        if !path.is_absolute()
            || path.as_os_str().as_encoded_bytes().len() > MAX_PATH_BYTES
            || path.components().any(|p| {
                matches!(
                    p,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
        {
            return Err(invalid("invalid recovery path"));
        }
    }
    if record.path != root.join(record.task_name())
        || record.repository == record.path
        || record.branch != format!("ember/{}", record.task_name())
    {
        return Err(invalid("invalid recovery identity"));
    }
    if (record.stage == Stage::Registered) != record.registered_task_id.is_some() {
        return Err(invalid("invalid registration owner"));
    }
    match (&record.stage, &record.base_commit) {
        (Stage::Requested, None) if record.directory_identity.is_none() => Ok(()),
        (Stage::Prepared | Stage::Registered, Some(commit))
            if record.directory_identity.is_some()
                && matches!(commit.len(), 40 | 64)
                && commit.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            Ok(())
        }
        _ => Err(invalid("invalid recovery stage or base commit")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    struct Fixture {
        directory: PathBuf,
        root: PathBuf,
        repository: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let directory =
                std::env::temp_dir().join(format!("ember-recovery-{}", uuid::Uuid::new_v4()));
            let root = directory.join("tasks");
            let repository = directory.join("repository");
            fs::create_dir_all(&root).unwrap();
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            }
            fs::create_dir(&repository).unwrap();
            let fixture = Self {
                directory,
                root,
                repository,
            };
            fixture.git(&["init", "--quiet"]);
            fs::write(fixture.repository.join("tracked.txt"), b"baseline\n").unwrap();
            fixture.git(&["add", "tracked.txt"]);
            fixture.git(&[
                "-c",
                "user.name=Recovery Test",
                "-c",
                "user.email=recovery@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "baseline",
            ]);
            fixture
        }
        fn git(&self, args: &[&str]) -> String {
            let out = Command::new("/usr/bin/git")
                .current_dir(&self.repository)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &self.directory)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "fixture git failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        }
        fn request(&self, journal: &mut Journal) -> Record {
            let record =
                Record::requested(&self.root, self.repository.clone(), AgentProvider::Codex);
            journal.request(record.clone()).unwrap();
            record
        }
        fn worktree(&self, record: &Record) -> ManagedWorktree {
            let head = self.git(&["rev-parse", "HEAD"]);
            self.git(&[
                "worktree",
                "add",
                "--quiet",
                "-b",
                &record.branch,
                record.path.to_str().unwrap(),
                &head,
            ]);
            ManagedWorktree {
                repository: self.repository.clone(),
                path: record.path.clone(),
                branch: record.branch.clone(),
                head,
            }
        }
        fn prepared(&self, journal: &mut Journal) -> Record {
            let record = self.request(journal);
            journal
                .prepared(record.id, &self.worktree(&record))
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn creation_recovery_registration_write_enospc_restart_and_retry_are_idempotent() {
        let f = Fixture::new();
        let mut journal = Journal::open(&f.root).unwrap();
        let record = f.prepared(&mut journal);
        fs::write(record.path.join("tracked.txt"), b"user edits\n").unwrap();
        fs::write(record.path.join("untracked.txt"), b"do not lose this\n").unwrap();
        let mut manager = TaskManager::new();
        let id = register(&mut manager, &record, record.new_task().unwrap()).unwrap();
        journal.write_fault = Some((false, libc::ENOSPC));
        assert_eq!(
            journal
                .registered(record.id, id)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ENOSPC)
        );
        drop(journal);
        let mut journal = Journal::open(&f.root).unwrap();
        let record = journal.records()[0].clone();
        for _ in 0..3 {
            assert_eq!(
                register(&mut manager, &record, record.new_task().unwrap()).unwrap(),
                id
            );
        }
        assert_eq!(manager.tasks().len(), 1);
        // Restart means a fresh runtime registry; recovery still creates just one task.
        let mut restarted = TaskManager::new();
        let recovered = register(&mut restarted, &record, record.new_task().unwrap()).unwrap();
        assert_eq!(
            register(&mut restarted, &record, record.new_task().unwrap()).unwrap(),
            recovered
        );
        journal.registered(record.id, recovered).unwrap();
        drop(journal);
        let journal = Journal::open(&f.root).unwrap();
        assert_eq!(journal.records()[0].stage, Stage::Registered);
        assert_eq!(journal.records()[0].registered_task_id, Some(recovered));
        assert!(journal.records()[0].new_task().is_err());
        assert_eq!(
            fs::read(record.path.join("tracked.txt")).unwrap(),
            b"user edits\n"
        );
        assert_eq!(
            fs::read(record.path.join("untracked.txt")).unwrap(),
            b"do not lose this\n"
        );
        assert_eq!(
            f.git(&["worktree", "list", "--porcelain"])
                .matches("worktree ")
                .count(),
            2
        );
    }

    #[test]
    fn creation_recovery_permission_failure_uses_real_filesystem_then_restart() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let mut journal = Journal::open(&f.root).unwrap();
        let record = f.prepared(&mut journal);
        let parent = journal.path.parent().unwrap().to_owned();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(
            journal
                .registered(record.id, TaskId::new())
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        drop(journal);
        let mut journal = Journal::open(&f.root).unwrap();
        let record = journal.records()[0].clone();
        assert_eq!(record.stage, Stage::Prepared);
        let mut manager = TaskManager::new();
        let id = register(&mut manager, &record, record.new_task().unwrap()).unwrap();
        assert_eq!(
            register(&mut manager, &record, record.new_task().unwrap()).unwrap(),
            id
        );
        journal.registered(record.id, TaskId::new()).unwrap();
        assert!(record.path.join("tracked.txt").is_file());
    }

    #[test]
    fn creation_recovery_core_validation_failure_remains_recoverable_without_secrets() {
        let f = Fixture::new();
        let mut journal = Journal::open(&f.root).unwrap();
        let record = f.prepared(&mut journal);
        let mut manager = TaskManager::new();
        let mut invalid = record.new_task().unwrap();
        invalid.title = "secret-token\ninvalid title".into();
        assert!(register(&mut manager, &record, invalid).is_err());
        assert!(manager.tasks().is_empty());
        drop(journal);
        let journal = Journal::open(&f.root).unwrap();
        let data = fs::read_to_string(&journal.path).unwrap();
        assert!(
            !data.contains("secret-token")
                && !data.contains("source_context")
                && !data.contains("title")
        );
        let record = &journal.records()[0];
        register(&mut manager, record, record.new_task().unwrap()).unwrap();
        assert_eq!(manager.tasks().len(), 1);
        let mut wrong = record.new_task().unwrap();
        wrong.branch = "unrelated".into();
        assert!(register(&mut manager, record, wrong).is_err());
    }

    #[test]
    fn creation_recovery_requested_failure_aborts_and_prepared_failure_preserves_open_only() {
        let f = Fixture::new();
        let mut journal = Journal::open(&f.root).unwrap();
        let record = Record::requested(&f.root, f.repository.clone(), AgentProvider::Codex);
        journal.write_fault = Some((false, libc::ENOSPC));
        assert!(journal.request(record.clone()).is_err());
        assert!(!record.path.exists());
        assert!(
            Journal::open(&f.root).is_err(),
            "lease must block another process/window"
        );
        journal.request(record.clone()).unwrap();
        let worktree = f.worktree(&record);
        fs::write(record.path.join("user.txt"), "preserve").unwrap();
        journal.write_fault = Some((false, libc::ENOSPC));
        assert!(journal.prepared(record.id, &worktree).is_err());
        drop(journal);
        let mut journal = Journal::open(&f.root).unwrap();
        assert_eq!(journal.records()[0].stage, Stage::Requested);
        assert!(journal.records()[0].new_task().is_err());
        journal.dismiss(record.id).unwrap();
        assert!(journal.records().is_empty());
        assert_eq!(
            fs::read_to_string(record.path.join("user.txt")).unwrap(),
            "preserve"
        );
        assert!(f
            .git(&["branch", "--list", &record.branch])
            .contains(&record.branch));
    }

    #[test]
    fn creation_recovery_post_publish_uncertainty_reloads_actual_stage() {
        let f = Fixture::new();
        let mut journal = Journal::open(&f.root).unwrap();
        let record = f.prepared(&mut journal);
        // Models rename success followed by directory-fsync failure. The error
        // must not claim old state is durable or permit blind stale-revision retry.
        journal.write_fault = Some((true, libc::EIO));
        assert!(journal.registered(record.id, TaskId::new()).is_err());
        assert!(journal.registered(record.id, TaskId::new()).is_err());
        drop(journal);
        let journal = Journal::open(&f.root).unwrap();
        assert_eq!(journal.records()[0].stage, Stage::Registered);
        assert!(journal.records()[0].new_task().is_err());
    }

    #[test]
    fn creation_recovery_bounded_journal_rejects_corruption_and_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let f = Fixture::new();
        let mut journal = Journal::open(&f.root).unwrap();
        for _ in 0..MAX_RECORDS {
            f.request(&mut journal);
        }
        let record = Record::requested(&f.root, f.repository.clone(), AgentProvider::Codex);
        assert!(journal.request(record.clone()).is_err());
        assert!(!record.path.exists());
        let path = journal.path.clone();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(journal);
        for bytes in [b"not json".to_vec(), vec![b'x'; MAX_BYTES as usize + 1]] {
            fs::write(&path, &bytes).unwrap();
            assert!(Journal::open(&f.root).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        fs::remove_file(&path).unwrap();
        let outside = f.directory.join("outside");
        fs::write(&outside, "never replace me").unwrap();
        symlink(&outside, &path).unwrap();
        assert!(Journal::open(&f.root).is_err());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "never replace me");
    }

    #[test]
    fn creation_recovery_production_worktree_identity_verification() {
        let f = Fixture::new();
        let mut journal = Journal::open(&f.root).unwrap();
        let record = f.prepared(&mut journal);
        WorktreeService::new(&f.root).expect("trusted system Git resolver precondition");
        record
            .verify(&f.root)
            .expect("existing worktree must verify");
        f.git(&[
            "-C",
            record.path.to_str().unwrap(),
            "switch",
            "-c",
            "changed-branch",
        ]);
        assert!(record.verify(&f.root).is_err());
    }
    #[test]
    fn creation_recovery_cancel_or_dropped_worker_reply_keeps_prepared_record() {
        let f = Fixture::new();
        let mut journal = Journal::open(&f.root).unwrap();
        let record = f.prepared(&mut journal);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        drop(receiver);
        std::thread::spawn(move || {
            // The real worker transfers its lease in the prepared reply. A
            // closing UI drops that reply; neither it nor cancellation cleans Git.
            let _ = sender.send((journal, record));
        })
        .join()
        .unwrap();
        let journal = Journal::open(&f.root).unwrap();
        assert_eq!(journal.records().len(), 1);
        assert_eq!(journal.records()[0].stage, Stage::Prepared);
        assert!(journal.records()[0].path.join("tracked.txt").is_file());
    }

    #[test]
    fn creation_recovery_replaced_directory_is_rejected_before_git_or_registration() {
        let f = Fixture::new();
        let mut journal = Journal::open(&f.root).unwrap();
        let record = f.prepared(&mut journal);
        // Keep the old inode alive, so the recreated directory cannot reuse it.
        let old = record.path.with_extension("old");
        fs::rename(&record.path, &old).unwrap();
        f.git(&["worktree", "prune"]);
        f.git(&[
            "worktree",
            "add",
            "--quiet",
            record.path.to_str().unwrap(),
            &record.branch,
        ]);
        assert_ne!(
            directory_identity(&record.path).unwrap(),
            record.directory_identity.clone().unwrap()
        );
        assert!(record.verify(&f.root).unwrap_err().contains("replaced"));
        assert!(old.join("tracked.txt").exists());
    }

    #[test]
    fn creation_recovery_worker_panic_keeps_requested_record_and_releases_lease() {
        let f = Fixture::new();
        let mut journal = Journal::open(&f.root).unwrap();
        f.request(&mut journal);
        assert!(std::thread::spawn(move || {
            let _journal = journal;
            panic!("injected worker interruption");
        })
        .join()
        .is_err());
        let journal = Journal::open(&f.root).unwrap();
        assert_eq!(journal.records()[0].stage, Stage::Requested);
        assert!(journal.records()[0].new_task().is_err());
    }
    #[test]
    fn creation_recovery_shared_protocol_fixture_matches_actual_schema() {
        let doc: Document = serde_json::from_str(include_str!(
            "../../tests/fixtures/task-recovery/stages.json"
        ))
        .unwrap();
        assert_eq!(doc.version, 1);
        assert_eq!(doc.records.len(), 3);
        for record in &doc.records {
            validate_record(record, Path::new("/fixture/tasks")).unwrap();
        }
        assert!(doc.records[0].new_task().is_err());
        assert!(doc.records[1].new_task().is_ok());
        assert!(doc.records[2].new_task().is_err());
    }
    #[test]
    fn creation_recovery_drop_releases_lease_even_with_inherited_description() {
        let f = Fixture::new();
        let journal = Journal::open(&f.root).unwrap();
        // dup models the open-file-description sharing of a concurrent fork.
        let inherited = journal._lease.try_clone().unwrap();
        assert!(Journal::open(&f.root).is_err());
        drop(journal);
        let reopened = Journal::open(&f.root).expect("owner Drop explicitly unlocks the lease");
        drop(inherited);
        assert!(
            Journal::open(&f.root).is_err(),
            "closing stale descriptor must not release new owner"
        );
        drop(reopened);
    }
}
