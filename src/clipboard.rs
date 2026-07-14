#[cfg(unix)]
mod unix_clipboard {
    use anyhow::Result;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    pub enum ClipboardContent {
        Text(String),
        Binary(Vec<u8>),
    }

    const IMAGE_MIME_TYPES: &[&str] = &[
        "image/png",
        "image/jpeg",
        "image/webp",
        "image/gif",
        "image/bmp",
    ];

    const TEXT_MIME_TYPES: &[&str] = &[
        "text/plain;charset=utf-8",
        "UTF8_STRING",
        "text/plain",
        "STRING",
    ];

    /// Clipboard owners are external processes and can stream indefinitely.
    /// Keep reads within the same bound enforced by OSC 5522 responses.
    const MAX_CLIPBOARD_READ_BYTES: usize = 32 * 1024 * 1024;
    const MAX_CLIPBOARD_MIME_TYPES: usize = 256;
    const MAX_CLIPBOARD_MIME_LEN: usize = 256;
    /// A user-visible copy/paste action gets one deadline shared by every
    /// backend probe. Without this, three unavailable or hostile helpers can
    /// each consume the full timeout and freeze the UI for many seconds.
    const CLIPBOARD_OPERATION_TIMEOUT: Duration = Duration::from_secs(2);

    fn first_offered_mime<'a>(types: &[String], preferred: &'a [&'a str]) -> Option<&'a str> {
        preferred
            .iter()
            .copied()
            .find(|mime| types.iter().any(|entry| entry.eq_ignore_ascii_case(mime)))
    }

    fn terminate_child_group(child: &mut Child) {
        // Clipboard helpers may fork while retaining our pipe. They are placed
        // in their own process group, so killing only the direct child is not
        // sufficient to unblock reader/writer threads on timeout.
        let pgid = child.id() as i32;
        // SAFETY: negative pgid targets only the process group created for
        // this child via CommandExt::process_group below.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
        let _ = child.kill();

        // `wait()` has no deadline. A helper stuck in uninterruptible kernel
        // I/O could therefore turn our timeout into another unbounded wait.
        let deadline = std::time::Instant::now() + Duration::from_millis(100);
        loop {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Ok(None) => break,
            }
        }
    }

    fn set_nonblocking<T: AsRawFd + ?Sized>(fd: &T) -> Option<()> {
        let fd = fd.as_raw_fd();
        // SAFETY: `fd` belongs to the live ChildStdin/ChildStdout passed by
        // reference. F_GETFL/F_SETFL neither outlive nor take ownership of it.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return None;
        }
        // SAFETY: same live descriptor; only O_NONBLOCK is added.
        (unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0).then_some(())
    }

    /// Execute a clipboard command without any reader thread to join. A
    /// descendant may inherit stdout and even escape the helper's process
    /// group; nonblocking reads let us drop our pipe at the deadline anyway.
    #[cfg(test)]
    fn command_output_with_timeout(
        program: &str,
        args: &[&str],
        timeout: Duration,
        max_bytes: usize,
    ) -> Option<Vec<u8>> {
        command_output_until(program, args, Instant::now() + timeout, max_bytes)
    }

    fn command_output_until(
        program: &str,
        args: &[&str],
        deadline: Instant,
        max_bytes: usize,
    ) -> Option<Vec<u8>> {
        use std::io::Read;

        if Instant::now() >= deadline {
            return None;
        }

        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        let mut child = command.spawn().ok()?;

        let mut stdout = child.stdout.take()?;
        if set_nonblocking(&stdout).is_none() {
            terminate_child_group(&mut child);
            return None;
        }

        let mut output = Vec::new();
        let mut scratch = [0_u8; 8192];
        let mut pipe_eof = false;
        let mut exit_status = None;
        loop {
            if !pipe_eof {
                loop {
                    if Instant::now() >= deadline {
                        terminate_child_group(&mut child);
                        return None;
                    }
                    match stdout.read(&mut scratch) {
                        Ok(0) => {
                            pipe_eof = true;
                            break;
                        }
                        Ok(read) => {
                            output.extend_from_slice(&scratch[..read]);
                            if output.len() > max_bytes {
                                terminate_child_group(&mut child);
                                return None;
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => {
                            terminate_child_group(&mut child);
                            return None;
                        }
                    }
                }
            }

            if exit_status.is_none() {
                match child.try_wait() {
                    Ok(status) => exit_status = status,
                    Err(_) => {
                        terminate_child_group(&mut child);
                        return None;
                    }
                }
            }
            if let Some(status) = exit_status {
                if !status.success() {
                    terminate_child_group(&mut child);
                    return None;
                }
                if pipe_eof {
                    return Some(output);
                }
            }
            if Instant::now() >= deadline {
                terminate_child_group(&mut child);
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn command_output_before(program: &str, args: &[&str], deadline: Instant) -> Option<Vec<u8>> {
        command_output_until(program, args, deadline, MAX_CLIPBOARD_READ_BYTES)
    }

    #[cfg(test)]
    fn command_with_stdin(program: &str, args: &[&str], input: &[u8]) -> Option<()> {
        command_with_stdin_until(
            program,
            args,
            input,
            Instant::now() + CLIPBOARD_OPERATION_TIMEOUT,
        )
    }

    fn command_with_stdin_until(
        program: &str,
        args: &[&str],
        input: &[u8],
        deadline: Instant,
    ) -> Option<()> {
        if Instant::now() >= deadline {
            return None;
        }

        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let mut child = command.spawn().ok()?;
        let mut stdin = child.stdin.take();
        if stdin.as_ref().and_then(set_nonblocking).is_none() {
            terminate_child_group(&mut child);
            return None;
        }

        let mut written = 0;
        loop {
            if let Some(pipe) = stdin.as_mut() {
                while written < input.len() {
                    if Instant::now() >= deadline {
                        terminate_child_group(&mut child);
                        return None;
                    }
                    match pipe.write(&input[written..]) {
                        Ok(0) => break,
                        Ok(count) => written += count,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => {
                            terminate_child_group(&mut child);
                            return None;
                        }
                    }
                }
                if written == input.len() {
                    // EOF tells helpers such as xclip that the complete
                    // selection has arrived.
                    stdin = None;
                }
            }

            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() && written == input.len() {
                        return Some(());
                    }
                    // The direct helper may exit while a descendant retains
                    // stdin. Kill its group and drop our write end; never wait
                    // for a writer thread because there is none.
                    terminate_child_group(&mut child);
                    return None;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(None) | Err(_) => {
                    terminate_child_group(&mut child);
                    return None;
                }
            }
        }
    }

    fn detect_wayland_clipboard() -> bool {
        std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var_os("XDG_SESSION_TYPE").as_deref()
                == Some(std::ffi::OsStr::new("wayland"))
    }

    fn wl_list_types(deadline: Instant) -> Option<Vec<String>> {
        let output = command_output_before("wl-paste", &["--list-types"], deadline)?;
        Some(
            String::from_utf8_lossy(&output)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && line.len() <= MAX_CLIPBOARD_MIME_LEN)
                .take(MAX_CLIPBOARD_MIME_TYPES)
                .map(ToOwned::to_owned)
                .collect(),
        )
    }

    fn xclip_list_types(deadline: Instant) -> Option<Vec<String>> {
        let output = command_output_before(
            "xclip",
            &["-selection", "clipboard", "-o", "-t", "TARGETS"],
            deadline,
        )?;
        Some(
            String::from_utf8_lossy(&output)
                .split_whitespace()
                .map(str::trim)
                .filter(|entry| !entry.is_empty() && entry.len() <= MAX_CLIPBOARD_MIME_LEN)
                .take(MAX_CLIPBOARD_MIME_TYPES)
                .map(ToOwned::to_owned)
                .collect(),
        )
    }

    fn read_wayland_type(mime_type: &str, deadline: Instant) -> Option<Vec<u8>> {
        command_output_before("wl-paste", &["--no-newline", "--type", mime_type], deadline)
    }

    fn read_wayland_primary_text(deadline: Instant) -> Option<Vec<u8>> {
        command_output_before("wl-paste", &["--primary", "--no-newline"], deadline)
    }

    fn read_xclip_type(mime_type: &str, deadline: Instant) -> Option<Vec<u8>> {
        command_output_before(
            "xclip",
            &["-selection", "clipboard", "-o", "-t", mime_type],
            deadline,
        )
    }

    fn read_xclip_primary_text(deadline: Instant) -> Option<Vec<u8>> {
        command_output_before("xclip", &["-selection", "primary", "-o"], deadline)
    }

    fn read_text_from_bytes(bytes: Vec<u8>) -> ClipboardContent {
        ClipboardContent::Text(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub struct ClipboardManager;

    impl ClipboardManager {
        pub fn new() -> Result<Self> {
            Ok(ClipboardManager)
        }

        /// 复制文本到系统剪贴板
        pub fn copy(&self, text: &str) -> Result<()> {
            let deadline = Instant::now() + CLIPBOARD_OPERATION_TIMEOUT;
            if detect_wayland_clipboard()
                && command_with_stdin_until(
                    "wl-copy",
                    &["--type", "text/plain;charset=utf-8"],
                    text.as_bytes(),
                    deadline,
                )
                .is_some()
            {
                return Ok(());
            }

            if command_with_stdin_until(
                "xclip",
                &["-selection", "clipboard"],
                text.as_bytes(),
                deadline,
            )
            .is_some()
            {
                return Ok(());
            }

            if command_with_stdin_until(
                "xsel",
                &["--clipboard", "--input"],
                text.as_bytes(),
                deadline,
            )
            .is_some()
            {
                return Ok(());
            }

            Err(anyhow::anyhow!(
                "复制失败:未找到可用的剪贴板工具 (wl-copy/xclip/xsel)"
            ))
        }

        /// Copy text to the X11/Wayland PRIMARY selection. VTE terminals update
        /// this selection while selecting text so middle-click paste feels native.
        pub fn copy_primary(&self, text: &str) -> Result<()> {
            let deadline = Instant::now() + CLIPBOARD_OPERATION_TIMEOUT;
            if detect_wayland_clipboard()
                && command_with_stdin_until(
                    "wl-copy",
                    &["--primary", "--type", "text/plain;charset=utf-8"],
                    text.as_bytes(),
                    deadline,
                )
                .is_some()
            {
                return Ok(());
            }

            if command_with_stdin_until(
                "xclip",
                &["-selection", "primary"],
                text.as_bytes(),
                deadline,
            )
            .is_some()
            {
                return Ok(());
            }

            if command_with_stdin_until(
                "xsel",
                &["--primary", "--input"],
                text.as_bytes(),
                deadline,
            )
            .is_some()
            {
                return Ok(());
            }

            Err(anyhow::anyhow!(
                "复制 PRIMARY 失败:未找到可用的剪贴板工具 (wl-copy/xclip/xsel)"
            ))
        }

        /// 从系统剪贴板粘贴文本
        pub fn paste(&self) -> Result<String> {
            match self.paste_contents()? {
                ClipboardContent::Text(text) => Ok(text),
                ClipboardContent::Binary(_) => Err(anyhow::anyhow!(
                    "剪贴板内容不是文本，拒绝将二进制数据转换为终端输入"
                )),
            }
        }

        pub fn paste_contents(&self) -> Result<ClipboardContent> {
            let deadline = Instant::now() + CLIPBOARD_OPERATION_TIMEOUT;
            self.paste_contents_until(deadline)
        }

        fn paste_contents_until(&self, deadline: Instant) -> Result<ClipboardContent> {
            if detect_wayland_clipboard() {
                if let Some(types) = wl_list_types(deadline) {
                    // Ordinary terminal paste is text-first. Rich applications
                    // negotiate exact image MIME through OSC 5522 instead.
                    if let Some(mime_type) = first_offered_mime(&types, TEXT_MIME_TYPES) {
                        if let Some(bytes) = read_wayland_type(mime_type, deadline) {
                            return Ok(read_text_from_bytes(bytes));
                        }
                    }

                    if let Some(mime_type) = first_offered_mime(&types, IMAGE_MIME_TYPES) {
                        if let Some(bytes) =
                            read_wayland_type(mime_type, deadline).filter(|bytes| !bytes.is_empty())
                        {
                            return Ok(ClipboardContent::Binary(bytes));
                        }
                    }
                }

                if let Some(bytes) = command_output_before("wl-paste", &["--no-newline"], deadline)
                {
                    return Ok(read_text_from_bytes(bytes));
                }
            }

            if let Some(types) = xclip_list_types(deadline) {
                if let Some(mime_type) = first_offered_mime(&types, TEXT_MIME_TYPES) {
                    if let Some(bytes) = read_xclip_type(mime_type, deadline) {
                        return Ok(read_text_from_bytes(bytes));
                    }
                }

                if let Some(mime_type) = first_offered_mime(&types, IMAGE_MIME_TYPES) {
                    if let Some(bytes) =
                        read_xclip_type(mime_type, deadline).filter(|bytes| !bytes.is_empty())
                    {
                        return Ok(ClipboardContent::Binary(bytes));
                    }
                }
            }

            if let Some(bytes) =
                command_output_before("xclip", &["-selection", "clipboard", "-o"], deadline)
            {
                return Ok(read_text_from_bytes(bytes));
            }

            if let Some(bytes) =
                command_output_before("xsel", &["--clipboard", "--output"], deadline)
            {
                return Ok(read_text_from_bytes(bytes));
            }

            Ok(ClipboardContent::Text(String::new()))
        }

        pub fn paste_primary(&self) -> Result<String> {
            let deadline = Instant::now() + CLIPBOARD_OPERATION_TIMEOUT;
            if detect_wayland_clipboard() {
                if let Some(bytes) = read_wayland_primary_text(deadline) {
                    return Ok(String::from_utf8_lossy(&bytes).into_owned());
                }
            }

            if let Some(bytes) = read_xclip_primary_text(deadline) {
                return Ok(String::from_utf8_lossy(&bytes).into_owned());
            }

            if let Some(bytes) = command_output_before("xsel", &["--primary", "--output"], deadline)
            {
                return Ok(String::from_utf8_lossy(&bytes).into_owned());
            }

            Ok(String::new())
        }

        pub fn available_mime_types(&self) -> Result<Vec<String>> {
            let deadline = Instant::now() + CLIPBOARD_OPERATION_TIMEOUT;
            if detect_wayland_clipboard() {
                if let Some(types) = wl_list_types(deadline) {
                    return Ok(types);
                }
            }

            if let Some(types) = xclip_list_types(deadline) {
                return Ok(types);
            }

            Ok(vec!["text/plain".to_string()])
        }

        pub fn read_mime(&self, mime_type: &str) -> Result<Vec<u8>> {
            let deadline = Instant::now() + CLIPBOARD_OPERATION_TIMEOUT;
            if detect_wayland_clipboard() {
                if let Some(bytes) = read_wayland_type(mime_type, deadline) {
                    return Ok(bytes);
                }
            }

            if let Some(bytes) = read_xclip_type(mime_type, deadline) {
                return Ok(bytes);
            }

            if TEXT_MIME_TYPES
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(mime_type))
            {
                // Do not fall back to the generic `paste_contents` path here:
                // it intentionally prefers images. If the clipboard changes
                // after a text-only capability was minted, returning those
                // image bytes under a text MIME would violate the grant.
                return Ok(Vec::new());
            }

            Ok(Vec::new())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn clipboard_command_output_is_size_bounded() {
            let within_limit = command_output_with_timeout(
                "sh",
                &["-c", "printf 1234"],
                Duration::from_secs(1),
                4,
            );
            assert_eq!(within_limit.as_deref(), Some(b"1234".as_slice()));

            let over_limit = command_output_with_timeout(
                "sh",
                &["-c", "printf 12345"],
                Duration::from_secs(1),
                4,
            );
            assert!(over_limit.is_none());
        }

        #[test]
        fn ordinary_paste_prefers_an_offered_text_representation() {
            let offered = vec!["image/png".to_string(), "TEXT/PLAIN".to_string()];
            assert_eq!(
                first_offered_mime(&offered, TEXT_MIME_TYPES),
                Some("text/plain")
            );
            assert_eq!(
                first_offered_mime(&offered, IMAGE_MIME_TYPES),
                Some("image/png")
            );
        }

        #[test]
        fn inherited_stdout_pipe_cannot_extend_the_deadline() {
            let started = std::time::Instant::now();
            let output = command_output_with_timeout(
                "sh",
                &["-c", "sleep 5 & printf inherited"],
                Duration::from_millis(80),
                64,
            );
            assert!(output.is_none());
            assert!(started.elapsed() < Duration::from_secs(1));
        }

        #[test]
        fn fallback_commands_share_one_absolute_deadline() {
            let started = Instant::now();
            let deadline = started + Duration::from_millis(80);
            let first =
                command_output_until("sh", &["-c", "sleep 5"], deadline, MAX_CLIPBOARD_READ_BYTES);
            assert!(first.is_none());

            // Once the operation deadline is exhausted, a fallback must not
            // receive a fresh timeout or even spawn another helper.
            let fallback = command_output_until(
                "sh",
                &["-c", "printf should-not-run"],
                deadline,
                MAX_CLIPBOARD_READ_BYTES,
            );
            assert!(fallback.is_none());
            assert!(started.elapsed() < Duration::from_secs(1));
        }

        #[test]
        fn early_exit_with_inherited_stdin_never_waits_for_a_writer_thread() {
            let input = vec![b'x'; 1024 * 1024];
            let started = std::time::Instant::now();
            let output = command_with_stdin("sh", &["-c", "sleep 5 & exit 0"], &input);
            assert!(output.is_none());
            assert!(started.elapsed() < Duration::from_secs(1));
        }
    }
}

#[cfg(windows)]
mod windows_clipboard {
    use anyhow::Result;

    pub enum ClipboardContent {
        Text(String),
        Binary(Vec<u8>),
    }

    pub struct ClipboardManager;

    impl ClipboardManager {
        pub fn new() -> Result<Self> {
            Ok(ClipboardManager)
        }

        pub fn copy(&self, _text: &str) -> Result<()> {
            // Windows 剪贴板实现（需要 winapi）
            // 暂时实现为占位符
            Ok(())
        }

        pub fn copy_primary(&self, _text: &str) -> Result<()> {
            Ok(())
        }

        pub fn paste(&self) -> Result<String> {
            // Windows 剪贴板实现（需要 winapi）
            // 暂时实现为占位符
            Ok(String::new())
        }

        pub fn paste_primary(&self) -> Result<String> {
            Ok(String::new())
        }

        pub fn paste_contents(&self) -> Result<ClipboardContent> {
            Ok(ClipboardContent::Text(String::new()))
        }

        pub fn available_mime_types(&self) -> Result<Vec<String>> {
            Ok(vec!["text/plain".to_string()])
        }

        pub fn read_mime(&self, _mime_type: &str) -> Result<Vec<u8>> {
            Ok(Vec::new())
        }
    }
}

#[cfg(unix)]
pub use unix_clipboard::{ClipboardContent, ClipboardManager};

#[cfg(windows)]
pub use windows_clipboard::{ClipboardContent, ClipboardManager};
