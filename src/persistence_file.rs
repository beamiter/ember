//! Hardened I/O boundary for ember's small text persistence files.
//!
//! The pinned `jterm_core` revision bounds reads and rejects non-regular
//! descriptors, but it predates the symlink, hard-link and ownership checks
//! required by ember's configurable snapshot paths. Keep those checks local
//! until the hardened core implementation is part of the pinned contract.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const DIRECTORY_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_API_KEY_FILE_BYTES: u64 = 16 * 1024;
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// Exact byte identity used by optimistic persistence transactions. Debug
/// output intentionally exposes only the size, never user configuration.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum FileRevision {
    Missing,
    Present(Box<[u8]>),
}

impl FileRevision {
    pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
        Self::Present(bytes.to_vec().into_boxed_slice())
    }

    pub(crate) fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Missing => None,
            Self::Present(bytes) => Some(bytes),
        }
    }
}

impl fmt::Debug for FileRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => formatter.write_str("Missing"),
            Self::Present(bytes) => formatter
                .debug_struct("Present")
                .field("bytes", &bytes.len())
                .finish(),
        }
    }
}

fn invalid_path(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Open a persistence file without following its final path component and
/// validate the object through the resulting descriptor.
fn open_owned_regular(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // O_NONBLOCK prevents a planted FIFO from hanging startup. O_NOFOLLOW
        // makes the check about the configured entry itself, not its target.
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }

    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid_path("persistence path is not a regular file"));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        validate_unix_identity(metadata.nlink(), metadata.uid(), unsafe {
            // SAFETY: geteuid has no preconditions and only reads process state.
            libc::geteuid()
        })?;
        // Configuration and restored session state are integrity-sensitive:
        // a different group member must not be able to mutate the inode even
        // when its directory entry itself is protected.
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "persistence file must not be group- or world-writable",
            ));
        }
    }

    Ok(file)
}

#[cfg(unix)]
fn validate_unix_identity(link_count: u64, owner: u32, effective_user: u32) -> io::Result<()> {
    if link_count != 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "persistence file must have exactly one hard link",
        ));
    }
    if owner != effective_user {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "persistence file is not owned by the current user",
        ));
    }
    Ok(())
}

/// Read one owned regular UTF-8 file without ever consuming more than the
/// caller's limit plus the single byte needed to detect growth.
pub(crate) fn read_revision(path: &Path, max_bytes: u64) -> io::Result<FileRevision> {
    let file = match open_owned_regular(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(FileRevision::Missing);
        }
        Err(error) => return Err(error),
    };
    let declared_len = file.metadata()?.len();
    if declared_len > max_bytes {
        return Err(oversize_error(path, declared_len, max_bytes));
    }

    let mut bytes = Vec::with_capacity(usize::try_from(declared_len.min(max_bytes)).unwrap_or(0));
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(oversize_error(path, bytes.len() as u64, max_bytes));
    }

    Ok(FileRevision::from_bytes(&bytes))
}

/// Read one owned regular UTF-8 file without ever consuming more than the
/// caller's limit plus the single byte needed to detect growth.
/// Highest number of same-millisecond claim attempts before giving up. A
/// caller retrying a hundred times inside one millisecond is looping, not
/// making progress.
const MAX_CLAIM_ATTEMPTS: u32 = 100;

/// Atomically take exclusive ownership of `path`, returning the private name
/// the file now lives at.
///
/// This is the one-winner primitive behind a restore: only the caller whose
/// no-clobber link succeeds ever observes the snapshot, so two simultaneous
/// openers cannot both resume the same session — and neither can a read that
/// is later followed by a separate delete. `hard_link` acts on the directory
/// entry rather than its target, so a symlink at `path` is retired without
/// touching what it points to.
pub fn claim_exclusive(path: &Path) -> io::Result<PathBuf> {
    let file_type = fs::symlink_metadata(path)?.file_type();
    if !file_type.is_file() && !file_type.is_symlink() {
        return Err(invalid_path("refusing to claim a non-file snapshot path"));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| invalid_path("snapshot path has no file name"))?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    for attempt in 0..MAX_CLAIM_ATTEMPTS {
        let mut claimed_name = file_name.to_os_string();
        claimed_name.push(format!(
            ".claimed-{timestamp}-{}-{attempt}",
            std::process::id()
        ));
        let claimed = parent.join(claimed_name);
        #[cfg(unix)]
        match fs::hard_link(path, &claimed) {
            Ok(()) => match fs::remove_file(path) {
                Ok(()) => return Ok(claimed),
                Err(error) => {
                    let _ = fs::remove_file(&claimed);
                    return Err(error);
                }
            },
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
        #[cfg(not(unix))]
        {
            // symlink_metadata, not `exists()`: a dangling symlink at this
            // name reports "does not exist" and must not be overwritten.
            if fs::symlink_metadata(&claimed).is_ok() {
                continue;
            }
            fs::rename(path, &claimed)?;
            return Ok(claimed);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique claimed-snapshot name",
    ))
}

pub(crate) fn read_bounded(path: &Path, max_bytes: u64) -> io::Result<String> {
    let revision = read_revision(path, max_bytes)?;
    let bytes = revision.bytes().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("persistence file {} does not exist", path.display()),
        )
    })?;
    String::from_utf8(bytes.to_vec()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("persistence file {} is not valid UTF-8", path.display()),
        )
    })
}

fn expand_private_path(raw_path: &str) -> io::Result<PathBuf> {
    let raw_path = raw_path.trim();
    if raw_path.is_empty() {
        return Err(invalid_path("credential path is empty"));
    }
    if raw_path == "~" || raw_path.starts_with("~/") {
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid_path("HOME is unavailable for ~/ credential path"))?;
        let mut path = PathBuf::from(home);
        if let Some(rest) = raw_path.strip_prefix("~/") {
            path.push(rest);
        }
        return Ok(path);
    }
    let path = PathBuf::from(raw_path);
    if !path.is_absolute() {
        return Err(invalid_path(
            "credential path must be absolute or begin with ~/",
        ));
    }
    Ok(path)
}

fn validate_private_key_metadata(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    if metadata.len() > MAX_API_KEY_FILE_BYTES {
        return Err(oversize_error(path, metadata.len(), MAX_API_KEY_FILE_BYTES));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "credential file {} must not be accessible by group or other users",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

/// Read an API key through the same descriptor-level no-follow, owner and
/// hard-link checks as snapshots. This local boundary avoids the pinned
/// core's blocking `File::open` when a FIFO is planted at a configured path.
pub fn read_api_key_file(raw_path: &str) -> io::Result<String> {
    let path = expand_private_path(raw_path)?;
    let file = open_owned_regular(&path)?;
    let metadata = file.metadata()?;
    validate_private_key_metadata(&path, &metadata)?;

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_API_KEY_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_API_KEY_FILE_BYTES {
        return Err(oversize_error(
            &path,
            bytes.len() as u64,
            MAX_API_KEY_FILE_BYTES,
        ));
    }
    let contents = String::from_utf8(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("credential file {} is not valid UTF-8", path.display()),
        )
    })?;
    let key = contents.trim();
    if key.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("credential file {} is empty", path.display()),
        ));
    }
    if key.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "credential file {} must contain one line without control characters",
                path.display()
            ),
        ));
    }
    Ok(key.to_string())
}

/// Store one settings-entered API key through ember's private atomic writer.
/// Existing symlinks, hard links, non-regular files, foreign owners and loose
/// permissions are rejected before replacement.
pub fn write_api_key_file(raw_path: &str, raw_key: &str) -> io::Result<()> {
    let path = expand_private_path(raw_path)?;
    let key = raw_key.trim();
    if key.is_empty() {
        return Err(invalid_path("API key must not be empty"));
    }
    if key.chars().any(char::is_control) {
        return Err(invalid_path(
            "API key must be one line without control characters",
        ));
    }
    if key.len() as u64 + 1 > MAX_API_KEY_FILE_BYTES {
        return Err(oversize_error(
            &path,
            key.len() as u64 + 1,
            MAX_API_KEY_FILE_BYTES,
        ));
    }

    match open_owned_regular(&path) {
        Ok(file) => validate_private_key_metadata(&path, &file.metadata()?)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut encoded = Vec::with_capacity(key.len() + 1);
    encoded.extend_from_slice(key.as_bytes());
    encoded.push(b'\n');
    write_atomic(&path, &encoded)
}

fn oversize_error(path: &Path, actual: u64, max_bytes: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::FileTooLarge,
        format!(
            "persistence file {} is {actual} bytes, over the {max_bytes}-byte limit",
            path.display()
        ),
    )
}

#[cfg(unix)]
fn open_existing_parent(parent: &Path) -> io::Result<File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    // Validate the directory entry without following a final symlink. Do not
    // chmod it: a configured session path may intentionally use $HOME or
    // another owner-controlled directory shared with unrelated applications.
    // Group/world-writable boundaries such as /tmp are rejected below.
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(parent)?;
    let metadata = directory.metadata()?;
    if !metadata.is_dir() {
        return Err(invalid_path("persistence parent is not a directory"));
    }
    // SAFETY: geteuid has no preconditions and only reads process state.
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "persistence parent is not owned by the current user",
        ));
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "persistence parent must not be group- or world-writable",
        ));
    }
    Ok(directory)
}

#[cfg(not(unix))]
fn open_existing_parent(parent: &Path) -> io::Result<File> {
    if fs::symlink_metadata(parent)?.is_dir() {
        File::open(parent)
    } else {
        Err(invalid_path("persistence parent is not a directory"))
    }
}

fn create_missing_parent(parent: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        // `mode` applies only to directories this call creates. In particular,
        // this never tightens an existing configured/shared parent.
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(parent)?;

    open_existing_parent(parent).map(drop)
}

/// Create or validate the immediate parent used by a persistence path.
/// Existing directories are never chmodded, and a final symlink is rejected.
pub(crate) fn ensure_parent(path: &Path) -> io::Result<()> {
    if path.file_name().is_none() {
        return Err(invalid_path("persistence path has no file name"));
    }

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        match fs::symlink_metadata(parent) {
            Ok(_) => drop(open_existing_parent(parent)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                create_missing_parent(parent)?;
            }
            Err(error) => return Err(error),
        }
    }

    Ok(())
}

/// Atomically replace a persistence file without following or chmodding an
/// existing parent symlink/directory.
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    ensure_parent(path)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = open_existing_parent(parent)?;
    lock_directory(&directory)?;

    atomic_replace_locked(path, contents, parent, &directory)
}

/// Compare the exact current bytes while holding the parent-directory lock,
/// then publish one complete replacement. A stale editor/window can never
/// silently overwrite a newer generation.
pub(crate) fn write_atomic_if_unchanged(
    path: &Path,
    contents: &[u8],
    expected: &FileRevision,
    max_current_bytes: u64,
) -> io::Result<FileRevision> {
    ensure_parent(path)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = open_existing_parent(parent)?;
    lock_directory(&directory)?;
    let current = read_revision(path, max_current_bytes)?;
    if &current != expected {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "{} changed outside this window; reload or reset before saving",
                path.display()
            ),
        ));
    }
    atomic_replace_locked(path, contents, parent, &directory)?;
    Ok(FileRevision::from_bytes(contents))
}

fn atomic_replace_locked(
    path: &Path,
    contents: &[u8],
    parent: &Path,
    directory: &File,
) -> io::Result<()> {
    let (mut file, temp_path) = create_unique_temp(path, parent)?;
    let mut cleanup = TempFileGuard::new(temp_path.clone());
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp_path, path)?;
    cleanup.committed = true;
    directory.sync_all()
}

#[cfg(unix)]
fn lock_directory(directory: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let started = Instant::now();
    loop {
        // SAFETY: directory owns this live descriptor and flock only changes
        // its advisory lock state.
        if unsafe { libc::flock(directory.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::WouldBlock {
            return Err(error);
        }
        if started.elapsed() >= DIRECTORY_LOCK_TIMEOUT {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for persistence directory lock",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(unix))]
fn lock_directory(_directory: &File) -> io::Result<()> {
    Ok(())
}

struct TempFileGuard {
    path: PathBuf,
    committed: bool,
}

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn create_unique_temp(path: &Path, parent: &Path) -> io::Result<(File, PathBuf)> {
    let destination = path
        .file_name()
        .ok_or_else(|| invalid_path("persistence path has no file name"))?;
    for _ in 0..128 {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let mut name = OsString::from(".");
        name.push(destination);
        name.push(format!(".tmp.{}.{id}", std::process::id()));
        let temp_path = parent.join(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        match options.open(&temp_path) {
            Ok(file) => return Ok((file, temp_path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique persistence staging file",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ember-persistence-file-{label}-{}-{id}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir(&path).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_private(path: &Path, contents: impl AsRef<[u8]>) {
        fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn bounded_read_is_inclusive_and_requires_utf8() {
        let root = TestDir::new("bounded");
        let path = root.join("state.json");
        write_private(&path, b"hello");

        assert_eq!(read_bounded(&path, 5).unwrap(), "hello");
        assert_eq!(
            read_bounded(&path, 4).unwrap_err().kind(),
            io::ErrorKind::FileTooLarge
        );

        write_private(&path, [0xff, 0xfe]);
        assert_eq!(
            read_bounded(&path, 2).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[cfg(unix)]
    #[test]
    fn malicious_snapshot_entries_are_rejected_without_blocking() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = TestDir::new("malicious-reads");
        let target = root.join("target.json");
        write_private(&target, b"sentinel");

        let symlink_path = root.join("symlink.json");
        symlink(&target, &symlink_path).unwrap();
        assert!(read_bounded(&symlink_path, 1024).is_err());

        let hardlink_path = root.join("hardlink.json");
        fs::hard_link(&target, &hardlink_path).unwrap();
        assert_eq!(
            read_bounded(&hardlink_path, 1024).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );

        let fifo_path = root.join("fifo.json");
        let name = std::ffi::CString::new(fifo_path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `name` is a live NUL-terminated path for the duration of the call.
        if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } == 0 {
            assert_eq!(
                read_bounded(&fifo_path, 1024).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }

        let writable_path = root.join("writable.json");
        fs::write(&writable_path, b"untrusted").unwrap();
        fs::set_permissions(&writable_path, fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(
            read_bounded(&writable_path, 1024).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(fs::read(target).unwrap(), b"sentinel");
    }

    #[cfg(unix)]
    #[test]
    fn api_key_io_is_private_bounded_and_never_follows_special_entries() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = TestDir::new("api-key");
        let path = root.join("ai.key");
        write_api_key_file(path.to_str().unwrap(), "  sk-secret  ").unwrap();
        assert_eq!(
            read_api_key_file(path.to_str().unwrap()).unwrap(),
            "sk-secret"
        );
        assert_eq!(fs::read(&path).unwrap(), b"sk-secret\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(
            read_api_key_file(path.to_str().unwrap())
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            write_api_key_file(path.to_str().unwrap(), "replacement")
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(fs::read(&path).unwrap(), b"sk-secret\n");

        let victim = root.join("victim.key");
        fs::write(&victim, b"victim\n").unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o600)).unwrap();
        let linked = root.join("linked.key");
        symlink(&victim, &linked).unwrap();
        assert!(read_api_key_file(linked.to_str().unwrap()).is_err());
        assert!(write_api_key_file(linked.to_str().unwrap(), "replacement").is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"victim\n");

        let hard_linked = root.join("hard-linked.key");
        fs::hard_link(&victim, &hard_linked).unwrap();
        assert!(read_api_key_file(hard_linked.to_str().unwrap()).is_err());
        assert!(write_api_key_file(hard_linked.to_str().unwrap(), "replacement").is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"victim\n");

        let fifo = root.join("fifo.key");
        let encoded = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: encoded is a live NUL-terminated path for this call.
        assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);
        let started = Instant::now();
        assert!(read_api_key_file(fifo.to_str().unwrap()).is_err());
        assert!(write_api_key_file(fifo.to_str().unwrap(), "replacement").is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn unix_owner_and_link_contract_is_explicit() {
        assert!(validate_unix_identity(1, 1000, 1000).is_ok());
        assert_eq!(
            validate_unix_identity(2, 1000, 1000).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            validate_unix_identity(1, 1001, 1000).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_an_existing_shared_parent_mode() {
        use std::os::unix::fs::PermissionsExt;

        let root = TestDir::new("shared-parent");
        let shared = root.join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();

        let path = shared.join("sessions.json");
        write_atomic(&path, b"private state").unwrap();

        assert_eq!(
            fs::metadata(&shared).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_never_follows_a_parent_symlink() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("parent-symlink");
        let target_parent = root.join("target-parent");
        let linked_parent = root.join("linked-parent");
        fs::create_dir(&target_parent).unwrap();
        symlink(&target_parent, &linked_parent).unwrap();

        assert!(write_atomic(&linked_parent.join("state.json"), b"do not write").is_err());
        assert!(!target_parent.join("state.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn replacing_a_destination_symlink_does_not_touch_its_target() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("destination-symlink");
        let target = root.join("target.json");
        let destination = root.join("state.json");
        fs::write(&target, b"sentinel").unwrap();
        symlink(&target, &destination).unwrap();

        write_atomic(&destination, b"new state").unwrap();

        assert_eq!(fs::read(target).unwrap(), b"sentinel");
        assert_eq!(fs::read(&destination).unwrap(), b"new state");
        assert!(!fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_rejects_a_writable_or_foreign_parent_boundary() {
        use std::os::unix::fs::PermissionsExt;

        let root = TestDir::new("unsafe-parent");
        let unsafe_parent = root.join("shared");
        fs::create_dir(&unsafe_parent).unwrap();
        fs::set_permissions(&unsafe_parent, fs::Permissions::from_mode(0o777)).unwrap();

        assert!(write_atomic(&unsafe_parent.join("state.json"), b"secret").is_err());
        assert!(!unsafe_parent.join("state.json").exists());
        assert_eq!(
            fs::metadata(&unsafe_parent).unwrap().permissions().mode() & 0o777,
            0o777,
            "a shared directory must never be chmodded as a side effect"
        );
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_atomic_writers_never_publish_mixed_or_partial_bytes() {
        let root = TestDir::new("concurrent");
        let path = root.join("state.json");
        let mut writers = Vec::new();
        for byte in b'a'..=b'h' {
            let path = path.clone();
            writers.push(std::thread::spawn(move || {
                write_atomic(&path, &vec![byte; 64 * 1024]).unwrap();
            }));
        }
        for writer in writers {
            writer.join().unwrap();
        }

        let bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 64 * 1024);
        assert!(bytes.iter().all(|byte| *byte == bytes[0]));
        assert_eq!(
            fs::read_dir(&root.0)
                .unwrap()
                .filter_map(Result::ok)
                .count(),
            1
        );
    }
}
