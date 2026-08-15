//! Bounded descriptor-based reader for font files named by fontconfig or the
//! user configuration.
//!
//! Font paths come from `fc-match`/`fc-list` output and `config.toml`, so the
//! object behind a path is not trusted to be a small regular file. Every
//! candidate is opened without following its final path component, validated
//! through the resulting descriptor, and capped before any allocation. This
//! mirrors the no-follow open-flags style of `persistence_file` without its
//! ownership and hard-link contract: system fonts are root-owned and
//! world-readable by design.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

/// Ceiling for one font file. Real desktop fonts are a few MiB at most and
/// even large CJK collections stay well under 32 MiB, so 64 MiB leaves ample
/// headroom while bounding what one malicious or corrupted fontconfig/config
/// path can make the startup path allocate.
pub(crate) const MAX_FONT_BYTES: u64 = 64 * 1024 * 1024;

/// Open a font candidate without following its final path component and
/// validate the object through the resulting descriptor. The descriptor, once
/// returned, is the only thing ever read: the path is never reopened, so a
/// replaced directory entry cannot swap the bytes after validation.
fn open_font_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // O_NOFOLLOW rejects a final symlink instead of silently reading its
        // target. O_NONBLOCK keeps a planted FIFO from hanging startup; it is
        // ignored by regular files. O_CLOEXEC keeps the descriptor out of
        // spawned helpers.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }

    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("font file {} is not a regular file", path.display()),
        ));
    }
    Ok(file)
}

fn oversize_error(path: &Path, actual: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::FileTooLarge,
        format!(
            "font file {} is {actual} bytes, over the {MAX_FONT_BYTES}-byte limit",
            path.display()
        ),
    )
}

/// Read one font file without ever consuming more than [`MAX_FONT_BYTES`]
/// plus the single byte needed to detect growth. Callers treat any error as
/// "candidate unusable" and fall through to the next one.
pub(crate) fn read_font_file(path: &Path) -> io::Result<Vec<u8>> {
    let file = open_font_file(path)?;
    let declared_len = file.metadata()?.len();
    if declared_len > MAX_FONT_BYTES {
        return Err(oversize_error(path, declared_len));
    }

    let mut bytes = Vec::with_capacity(usize::try_from(declared_len).unwrap_or(0));
    file.take(MAX_FONT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FONT_BYTES {
        return Err(oversize_error(path, bytes.len() as u64));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ember-font-file-{label}-{}-{id}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir(&path).unwrap();
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

    #[test]
    fn regular_in_limit_file_loads() {
        let root = TestDir::new("regular");
        let path = root.join("font.ttf");
        fs::write(&path, b"fake-font-bytes").unwrap();

        assert_eq!(read_font_file(&path).unwrap(), b"fake-font-bytes");
    }

    #[test]
    fn missing_file_reports_not_found() {
        let root = TestDir::new("missing");
        assert_eq!(
            read_font_file(&root.join("absent.ttf")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn directory_is_rejected_as_non_regular() {
        let root = TestDir::new("directory");
        // Unix reports InvalidInput from the descriptor check; other targets
        // may fail the open itself. Both reject the candidate.
        assert!(read_font_file(&root.0).is_err());
    }

    #[test]
    fn oversized_sparse_file_is_rejected_and_exact_limit_is_accepted() {
        let root = TestDir::new("oversized");
        let oversized = root.join("huge.ttf");
        let file = File::create(&oversized).unwrap();
        file.set_len(MAX_FONT_BYTES + 1).unwrap();
        drop(file);

        let started = Instant::now();
        assert_eq!(
            read_font_file(&oversized).unwrap_err().kind(),
            io::ErrorKind::FileTooLarge
        );
        assert!(started.elapsed() < Duration::from_secs(1));

        let exact = root.join("exact.ttf");
        let file = File::create(&exact).unwrap();
        file.set_len(MAX_FONT_BYTES).unwrap();
        drop(file);
        assert_eq!(read_font_file(&exact).unwrap().len() as u64, MAX_FONT_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_a_real_font_is_rejected_without_touching_the_target() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("symlink");
        let target = root.join("real.ttf");
        fs::write(&target, b"real-font").unwrap();
        let link = root.join("linked.ttf");
        symlink(&target, &link).unwrap();

        assert!(read_font_file(&link).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"real-font");
    }

    #[cfg(unix)]
    #[test]
    fn fifo_is_rejected_without_blocking() {
        let root = TestDir::new("fifo");
        let fifo = root.join("font.fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `name` is a live NUL-terminated path for the duration of the call.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);

        let started = Instant::now();
        assert_eq!(
            read_font_file(&fifo).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
