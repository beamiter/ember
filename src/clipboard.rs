#[cfg(unix)]
mod unix_clipboard {
    use anyhow::Result;
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::Duration;

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

    /// Execute command with timeout to prevent hanging on slow clipboard operations
    fn command_output_with_timeout(
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Option<Vec<u8>> {
        use std::sync::mpsc;
        use std::thread;

        let (tx, rx) = mpsc::channel();
        let program = program.to_string();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();

        thread::spawn(move || {
            let result = Command::new(&program)
                .args(&args)
                .output()
                .ok()
                .and_then(|output| {
                    if output.status.success() {
                        Some(output.stdout)
                    } else {
                        None
                    }
                });
            let _ = tx.send(result);
        });

        rx.recv_timeout(timeout).ok().flatten()
    }

    fn command_output(program: &str, args: &[&str]) -> Option<Vec<u8>> {
        // Use 2 second timeout for clipboard operations
        command_output_with_timeout(program, args, Duration::from_secs(2))
    }

    fn command_with_stdin(program: &str, args: &[&str], input: &[u8]) -> Option<()> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .spawn()
            .ok()?;

        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input);
        }

        child
            .wait()
            .ok()
            .filter(|status| status.success())
            .map(|_| ())
    }

    fn detect_wayland_clipboard() -> bool {
        std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var_os("XDG_SESSION_TYPE").as_deref()
                == Some(std::ffi::OsStr::new("wayland"))
    }

    fn wl_list_types() -> Option<Vec<String>> {
        let output = command_output("wl-paste", &["--list-types"])?;
        Some(
            String::from_utf8_lossy(&output)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
        )
    }

    fn xclip_list_types() -> Option<Vec<String>> {
        let output = command_output("xclip", &["-selection", "clipboard", "-o", "-t", "TARGETS"])?;
        Some(
            String::from_utf8_lossy(&output)
                .split_whitespace()
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
        )
    }

    fn read_wayland_type(mime_type: &str) -> Option<Vec<u8>> {
        command_output("wl-paste", &["--no-newline", "--type", mime_type])
    }

    fn read_xclip_type(mime_type: &str) -> Option<Vec<u8>> {
        command_output("xclip", &["-selection", "clipboard", "-o", "-t", mime_type])
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
            if detect_wayland_clipboard()
                && command_with_stdin(
                    "wl-copy",
                    &["--type", "text/plain;charset=utf-8"],
                    text.as_bytes(),
                )
                .is_some()
            {
                return Ok(());
            }

            if command_with_stdin("xclip", &["-selection", "clipboard"], text.as_bytes()).is_some()
            {
                return Ok(());
            }

            if command_with_stdin("xsel", &["--clipboard", "--input"], text.as_bytes()).is_some() {
                return Ok(());
            }

            Err(anyhow::anyhow!(
                "复制失败:未找到可用的剪贴板工具 (wl-copy/xclip/xsel)"
            ))
        }

        /// 从系统剪贴板粘贴文本
        pub fn paste(&self) -> Result<String> {
            Ok(match self.paste_contents()? {
                ClipboardContent::Text(text) => text,
                ClipboardContent::Binary(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            })
        }

        pub fn paste_contents(&self) -> Result<ClipboardContent> {
            if detect_wayland_clipboard() {
                if let Some(types) = wl_list_types() {
                    // Find the first available image type and try only that one
                    if let Some(mime_type) = IMAGE_MIME_TYPES
                        .iter()
                        .find(|&&mime| types.iter().any(|entry| entry.eq_ignore_ascii_case(mime)))
                    {
                        if let Some(bytes) =
                            read_wayland_type(mime_type).filter(|bytes| !bytes.is_empty())
                        {
                            return Ok(ClipboardContent::Binary(bytes));
                        }
                    }

                    // Find the first available text type
                    if let Some(mime_type) = TEXT_MIME_TYPES
                        .iter()
                        .find(|&&mime| types.iter().any(|entry| entry.eq_ignore_ascii_case(mime)))
                    {
                        if let Some(bytes) = read_wayland_type(mime_type) {
                            return Ok(read_text_from_bytes(bytes));
                        }
                    }
                }

                if let Some(bytes) = command_output("wl-paste", &["--no-newline"]) {
                    return Ok(read_text_from_bytes(bytes));
                }
            }

            if let Some(types) = xclip_list_types() {
                // Find the first available image type and try only that one
                if let Some(mime_type) = IMAGE_MIME_TYPES
                    .iter()
                    .find(|&&mime| types.iter().any(|entry| entry.eq_ignore_ascii_case(mime)))
                {
                    if let Some(bytes) =
                        read_xclip_type(mime_type).filter(|bytes| !bytes.is_empty())
                    {
                        return Ok(ClipboardContent::Binary(bytes));
                    }
                }

                // Find the first available text type
                if let Some(mime_type) = TEXT_MIME_TYPES
                    .iter()
                    .find(|&&mime| types.iter().any(|entry| entry.eq_ignore_ascii_case(mime)))
                {
                    if let Some(bytes) = read_xclip_type(mime_type) {
                        return Ok(read_text_from_bytes(bytes));
                    }
                }
            }

            if let Some(bytes) = command_output("xclip", &["-selection", "clipboard", "-o"]) {
                return Ok(read_text_from_bytes(bytes));
            }

            if let Some(bytes) = command_output("xsel", &["--clipboard", "--output"]) {
                return Ok(read_text_from_bytes(bytes));
            }

            Ok(ClipboardContent::Text(String::new()))
        }

        pub fn available_mime_types(&self) -> Result<Vec<String>> {
            if detect_wayland_clipboard() {
                if let Some(types) = wl_list_types() {
                    return Ok(types);
                }
            }

            if let Some(types) = xclip_list_types() {
                return Ok(types);
            }

            Ok(vec!["text/plain".to_string()])
        }

        pub fn read_mime(&self, mime_type: &str) -> Result<Vec<u8>> {
            if detect_wayland_clipboard() {
                if let Some(bytes) = read_wayland_type(mime_type) {
                    return Ok(bytes);
                }
            }

            if let Some(bytes) = read_xclip_type(mime_type) {
                return Ok(bytes);
            }

            if TEXT_MIME_TYPES
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(mime_type))
            {
                return Ok(self.paste()?.into_bytes());
            }

            Ok(Vec::new())
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

        pub fn paste(&self) -> Result<String> {
            // Windows 剪贴板实现（需要 winapi）
            // 暂时实现为占位符
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
