/// 链接检测和交互模块
use regex::Regex;

/// 链接类型
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum LinkType {
    /// URL (http/https)
    Url,
    /// 本地文件路径
    FilePath,
    /// IP 地址
    IpAddress,
}

/// 单个链接的信息
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// 链接所在行
    pub line: usize,
    /// 列起始位置
    pub col_start: usize,
    /// 列结束位置（不含）
    pub col_end: usize,
    /// 链接类型
    pub link_type: LinkType,
    /// 链接文本/URL
    pub text: String,
}

/// 链接检测配置
#[derive(Clone, Debug)]
pub struct LinkDetectionConfig {
    /// 是否检测 URL
    pub detect_urls: bool,
    /// 是否检测文件路径
    pub detect_file_paths: bool,
    /// 是否检测 IP 地址
    pub detect_ip_addresses: bool,
}

impl Default for LinkDetectionConfig {
    fn default() -> Self {
        Self {
            detect_urls: true,
            detect_file_paths: true,
            detect_ip_addresses: true,
        }
    }
}

/// 链接检测引擎
pub struct LinkDetector {
    config: LinkDetectionConfig,
    url_regex: Regex,
    ip_regex: Regex,
    file_path_regex: Regex,
}

impl LinkDetector {
    pub fn new(config: LinkDetectionConfig) -> Self {
        // 先识别一切 URL 形状的文本（任意 scheme）；是否可点击仍由
        // `is_supported_hyperlink_uri` 单独裁决。括号允许出现在 URL 体内
        // （如维基百科的 /wiki/Foo_(bar)），尾部不配对的右括号在下方裁剪。
        let url_regex =
            Regex::new(r"[A-Za-z][A-Za-z0-9+.-]*://[^\s<>\[\]{}|\\^`]*[^\s<>\[\]{}|\\^`.,;:!?\-]")
                .unwrap();

        // IP 地址正则：x.x.x.x 格式
        let ip_regex = Regex::new(
            r"\b(?:(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.){3}(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\b"
        ).unwrap();

        // 文件路径正则：以 /、./、../、~/ 开头(前面是行首或空白)。
        // 用捕获组提取实际路径，避免把前导空白计入列偏移。
        let file_path_regex = Regex::new(r"(?:^|\s)((?:\.{0,2}|~)/[^\s<>\[\]{}|\\^`()]*)").unwrap();

        Self {
            config,
            url_regex,
            ip_regex,
            file_path_regex,
        }
    }

    /// 将字节偏移转换为字符（列）偏移
    fn byte_offset_to_char_offset(line: &str, byte_offset: usize) -> usize {
        line[..byte_offset].chars().count()
    }

    /// 在单行文本中检测所有链接
    pub fn detect_links_in_line(&self, line: &str, line_idx: usize) -> Vec<Link> {
        let mut links = Vec::new();
        let mut url_spans = Vec::new();

        // 无论 URL 激活是否开启，都先圈出全部 URL 形状的区段：被拒 scheme
        // 里的 IP/路径子串绝不允许回落成别的链接类型。
        for mat in self.url_regex.find_iter(line) {
            let mut url = mat.as_str();
            // 裁掉尾部不配对的 )，让 (https://example.com) 不吞掉右括号，
            // 同时保留配对的 /wiki/Foo_(bar)。
            while url.ends_with(')') && url.matches(')').count() > url.matches('(').count() {
                url = &url[..url.len() - 1];
            }
            let col_start = Self::byte_offset_to_char_offset(line, mat.start());
            let col_end = Self::byte_offset_to_char_offset(line, mat.start() + url.len());
            // 即使策略拒绝这个 URL，也保留整个区段：否则
            // https://user@192.0.2.1 的内层 IP 会被单独激活。
            url_spans.push((col_start, col_end));
            if self.config.detect_urls && crate::terminal::is_supported_hyperlink_uri(url) {
                links.push(Link {
                    line: line_idx,
                    col_start,
                    col_end,
                    link_type: LinkType::Url,
                    text: url.to_string(),
                });
            }
        }

        // 检测 IP 地址
        if self.config.detect_ip_addresses {
            let bytes = line.as_bytes();
            for mat in self.ip_regex.find_iter(line) {
                // 排除更长数字序列的子串(如版本号 1.2.3.4.5、x.1.2.3.4):
                // 若紧邻的前一个字符是 '.',或后一个字符是 '.' 且其后仍为数字,则跳过
                if mat.start() > 0 && bytes[mat.start() - 1] == b'.' {
                    continue;
                }
                if bytes.get(mat.end()) == Some(&b'.')
                    && bytes.get(mat.end() + 1).is_some_and(|b| b.is_ascii_digit())
                {
                    continue;
                }

                let col_start = Self::byte_offset_to_char_offset(line, mat.start());
                let col_end = Self::byte_offset_to_char_offset(line, mat.end());
                // 避免与 URL 重复（含被策略拒绝、未成为链接的 URL 区段）
                if !url_spans
                    .iter()
                    .any(|&(start, end)| start <= col_start && col_end <= end)
                    && !links
                        .iter()
                        .any(|l| l.col_start <= col_start && col_end <= l.col_end)
                {
                    links.push(Link {
                        line: line_idx,
                        col_start,
                        col_end,
                        link_type: LinkType::IpAddress,
                        text: mat.as_str().to_string(),
                    });
                }
            }
        }

        // 检测文件路径
        if self.config.detect_file_paths {
            for caps in self.file_path_regex.captures_iter(line) {
                let Some(m) = caps.get(1) else { continue };
                let start_b = m.start();
                // 剥离尾部标点(句末句号、逗号、右括号等不属于路径的一部分)
                let trimmed = m.as_str().trim_end_matches(|c: char| {
                    matches!(
                        c,
                        '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '\'' | '"'
                    )
                });
                let end_b = start_b + trimmed.len();
                let matched_text = &line[start_b..end_b];

                let col_start = Self::byte_offset_to_char_offset(line, start_b);
                let col_end = Self::byte_offset_to_char_offset(line, end_b);

                // 避免与 URL 重复（含被策略拒绝、未成为链接的 URL 区段）
                if !url_spans
                    .iter()
                    .any(|&(start, end)| start <= col_start && col_end <= end)
                    && !links
                        .iter()
                        .any(|l| l.col_start <= col_start && col_end <= l.col_end)
                    && Self::is_valid_file_path(matched_text)
                {
                    links.push(Link {
                        line: line_idx,
                        col_start,
                        col_end,
                        link_type: LinkType::FilePath,
                        text: matched_text.to_string(),
                    });
                }
            }
        }

        links
    }

    /// 判断文本是否为有效的文件路径
    fn is_valid_file_path(text: &str) -> bool {
        let trimmed = text.trim();

        if trimmed.is_empty() {
            return false;
        }

        // 必须以 /、./、../ 或 ~/ 开头
        if !(trimmed.starts_with('/')
            || trimmed.starts_with("./")
            || trimmed.starts_with("../")
            || trimmed.starts_with("~/"))
        {
            return false;
        }

        // 排除 C 风格注释起始(// 行注释、/* 块注释),它们不是路径
        if trimmed.starts_with("//") || trimmed.starts_with("/*") {
            return false;
        }

        // 仅由 / 和 . 组成的不是有效路径("/"、"./"、"../"、"..." 等)
        if trimmed.chars().all(|ch| matches!(ch, '/' | '.')) {
            return false;
        }

        // 路径中必须至少包含一个字母或数字,排除 "/*"、"/-" 之类的纯符号串
        trimmed.chars().any(|ch| ch.is_alphanumeric())
    }

    /// 在当前可视内容中检测链接，支持传入row_wrapped标志以正确处理跨行链接。
    pub fn detect_links_in_visible_cells_with_wrapping(
        &self,
        visible_cells: &[Vec<crate::terminal::TerminalCell>],
        row_wrapped: &[bool],
    ) -> Vec<Link> {
        let mut all_links = Vec::new();

        if row_wrapped.is_empty() || row_wrapped.len() != visible_cells.len() {
            for (line_idx, line) in visible_cells.iter().enumerate() {
                let line_str: String = line.iter().map(|cell| cell.character).collect();
                let links = self.detect_links_in_line(&line_str, line_idx);
                all_links.extend(links);
            }
            return all_links;
        }

        // 将连续的换行行合并为逻辑行，记录每行的列数累积偏移
        let mut logical_lines: Vec<(usize, usize, String, Vec<usize>)> = Vec::new();
        let mut current_start = 0;
        let mut current_text = String::new();
        let mut row_char_offsets: Vec<usize> = Vec::new(); // 每个物理行在逻辑行中的起始字符偏移

        for (line_idx, line) in visible_cells.iter().enumerate() {
            row_char_offsets.push(current_text.chars().count());
            let line_str: String = line.iter().map(|cell| cell.character).collect();
            current_text.push_str(&line_str);

            if line_idx == visible_cells.len() - 1 || !row_wrapped[line_idx] {
                logical_lines.push((
                    current_start,
                    line_idx,
                    current_text.clone(),
                    row_char_offsets.clone(),
                ));
                current_text.clear();
                row_char_offsets.clear();
                current_start = line_idx + 1;
            }
        }

        for (start_row, _end_row, logical_text, char_offsets) in logical_lines {
            let links = self.detect_links_in_line(&logical_text, 0);

            for link in links {
                let full_url = link.text.clone();
                let link_start = link.col_start;
                let link_end = link.col_end;

                // 将逻辑偏移分割到多个物理行
                for (i, &row_offset) in char_offsets.iter().enumerate() {
                    let row_idx = start_row + i;
                    let row_len = visible_cells[row_idx].len();
                    let row_end_offset = row_offset + row_len;

                    // 检查该链接是否与这个物理行重叠
                    if link_start < row_end_offset && link_end > row_offset {
                        let col_start = link_start.saturating_sub(row_offset);
                        let col_end = if link_end < row_end_offset {
                            link_end - row_offset
                        } else {
                            row_len
                        };

                        all_links.push(Link {
                            line: row_idx,
                            col_start,
                            col_end,
                            link_type: link.link_type,
                            text: full_url.clone(),
                        });
                    }
                }
            }
        }

        all_links
    }

    /// Detect both textual links and explicit OSC 8 hyperlinks. Explicit
    /// links take precedence over regex matches and carry the resolved target,
    /// which may intentionally differ from the text displayed in the cells.
    pub fn detect_links_in_visible_cells_with_wrapping_and_hyperlinks<F>(
        &self,
        visible_cells: &[Vec<crate::terminal::TerminalCell>],
        row_wrapped: &[bool],
        mut resolve_hyperlink: F,
    ) -> Vec<Link>
    where
        F: FnMut(crate::terminal::HyperlinkId) -> Option<String>,
    {
        let mut explicit_links = Vec::new();

        for (line_idx, row) in visible_cells.iter().enumerate() {
            let mut col = 0;
            while col < row.len() {
                let id = row[col].hyperlink_id;
                if id.is_none() {
                    col += 1;
                    continue;
                }

                let col_start = col;
                col += 1;
                while col < row.len() && row[col].hyperlink_id == id {
                    col += 1;
                }

                if let Some(target) = resolve_hyperlink(id) {
                    explicit_links.push(Link {
                        line: line_idx,
                        col_start,
                        col_end: col,
                        link_type: LinkType::Url,
                        text: target,
                    });
                }
            }
        }

        let mut detected =
            self.detect_links_in_visible_cells_with_wrapping(visible_cells, row_wrapped);
        detected.retain(|candidate| {
            !explicit_links.iter().any(|explicit| {
                explicit.line == candidate.line
                    && explicit.col_start < candidate.col_end
                    && candidate.col_start < explicit.col_end
            })
        });
        explicit_links.extend(detected);
        explicit_links
    }
}

/// 打开链接
pub fn open_link(link: &Link) -> Result<(), Box<dyn std::error::Error>> {
    match link.link_type {
        LinkType::Url => {
            if !crate::terminal::is_supported_hyperlink_uri(&link.text) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "unsupported or unsafe URL scheme",
                )
                .into());
            }
            open_url(&link.text)?;
        }
        LinkType::FilePath => {
            open_file_path(&link.text)?;
        }
        LinkType::IpAddress => {
            // IP 地址可以用浏览器打开或显示 whois 信息
            open_url(&format!("http://{}", link.text))?;
        }
    }
    Ok(())
}

/// Absolute openers, in preference order, per platform.
///
/// A clicked link is terminal-controlled data, so the program that receives it
/// must not be chosen by a mutable `PATH`: a directory the user happened to
/// open, or a `~/.local/bin` entry an unrelated install dropped there, would
/// otherwise decide what a click runs. Only a non-user-writable absolute path
/// is accepted.
#[cfg(target_os = "linux")]
const OPENER_CANDIDATES: &[&str] = &["/usr/bin/xdg-open", "/bin/xdg-open"];
#[cfg(target_os = "macos")]
const OPENER_CANDIDATES: &[&str] = &["/usr/bin/open"];
#[cfg(target_os = "windows")]
const OPENER_CANDIDATES: &[&str] = &[r"C:\Windows\explorer.exe"];

fn trusted_opener() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    for candidate in OPENER_CANDIDATES {
        let path = std::path::Path::new(candidate);
        let Ok(metadata) = std::fs::metadata(path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let mode = metadata.permissions().mode();
            // Executable, and not writable by this user, its group, or others.
            if mode & 0o111 == 0
                || mode & 0o022 != 0
                || (metadata.uid() == unsafe { libc::geteuid() } && mode & 0o200 != 0)
            {
                continue;
            }
        }
        return Ok(path.to_path_buf());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no trusted system opener is available",
    )
    .into())
}

/// 打开 URL（使用系统默认浏览器）
fn open_url(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Re-check at the boundary: `open_link` validated this target, but this
    // function is the one that actually hands bytes to another program.
    if !crate::terminal::is_supported_hyperlink_uri(url) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unsupported or unsafe URL",
        )
        .into());
    }
    let mut child = std::process::Command::new(trusted_opener()?)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // The opener returns immediately after handing off to the desktop; reap it
    // so a clicked link does not leave a zombie behind.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// 打开文件路径（使用系统默认应用）
fn open_file_path(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let expanded_path = expand_path(path);
    let mut command = std::process::Command::new(trusted_opener()?);
    // `--` first: a detected path beginning with `-` is a file operand, never
    // an option for the opener.
    #[cfg(not(target_os = "windows"))]
    command.arg("--");
    let mut child = command
        .arg(&expanded_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// 扩展路径（~/ 变量替换等）
fn expand_path(path: &str) -> String {
    if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return format!("{}{}", home.display(), &path[1..]);
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{HistoryProjection, TerminalState};

    #[test]
    fn osc8_masked_label_resolves_to_real_target() {
        const TARGET: &str = "https://example.test/real-target";
        let mut terminal = TerminalState::new(24, 2);
        terminal.process_input(
            format!("\x1b]8;id=masked;{TARGET}\x1b\\click here\x1b]8;;\x1b\\").as_bytes(),
        );
        let viewport = terminal.projected_viewport(HistoryProjection::identity(), true);
        let detector = LinkDetector::new(LinkDetectionConfig::default());

        let links = detector.detect_links_in_visible_cells_with_wrapping_and_hyperlinks(
            viewport.cells(),
            viewport.row_wrapped(),
            |id| terminal.hyperlink_uri(id).map(str::to_owned),
        );

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].text, TARGET);
        assert_eq!(
            (links[0].line, links[0].col_start, links[0].col_end),
            (0, 0, 10)
        );
    }

    #[test]
    fn osc8_target_takes_precedence_over_decoy_url_label() {
        const TARGET: &str = "https://real.example/landing";
        let mut terminal = TerminalState::new(32, 2);
        terminal.process_input(
            format!("\x1b]8;;{TARGET}\x1b\\https://decoy.example\x1b]8;;\x1b\\").as_bytes(),
        );
        let viewport = terminal.projected_viewport(HistoryProjection::identity(), true);
        let detector = LinkDetector::new(LinkDetectionConfig::default());

        let links = detector.detect_links_in_visible_cells_with_wrapping_and_hyperlinks(
            viewport.cells(),
            viewport.row_wrapped(),
            |id| terminal.hyperlink_uri(id).map(str::to_owned),
        );

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].text, TARGET);
        assert_eq!(links[0].col_end, "https://decoy.example".len());
    }

    #[test]
    fn open_link_rejects_dangerous_and_unsupported_url_schemes() {
        for target in [
            "javascript:alert(1)",
            "data:text/html,hello",
            "shell:touch-dangerous",
        ] {
            let link = Link {
                line: 0,
                col_start: 0,
                col_end: 1,
                link_type: LinkType::Url,
                text: target.to_string(),
            };
            assert!(open_link(&link).is_err(), "accepted unsafe target {target}");
        }
    }

    #[test]
    fn test_url_detection() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "Visit https://example.com for more info";
        let links = detector.detect_links_in_line(line, 0);

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].link_type, LinkType::Url);
        assert_eq!(links[0].text, "https://example.com");
    }

    #[test]
    fn ftp_urls_are_not_detected() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        // Detection policy is absolute HTTP(S) only; an ftp:// target must not
        // become clickable.
        let links = detector.detect_links_in_line("Mirror at ftp://example.com/pub", 0);
        assert!(!links.iter().any(|l| l.link_type == LinkType::Url));
    }

    #[test]
    fn test_ip_detection() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "Server at 192.168.1.1 is down";
        let links = detector.detect_links_in_line(line, 0);

        assert!(links.iter().any(|l| l.link_type == LinkType::IpAddress));
    }

    #[test]
    fn test_file_path_detection() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "Check /etc/hosts file";
        let links = detector.detect_links_in_line(line, 0);

        assert!(links.iter().any(|l| l.link_type == LinkType::FilePath));
    }

    #[test]
    fn test_home_relative_file_path_detection() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "Open ~/notes/todo.md";
        let links = detector.detect_links_in_line(line, 0);

        let p = links
            .iter()
            .find(|l| l.link_type == LinkType::FilePath)
            .unwrap();
        assert_eq!(p.text, "~/notes/todo.md");
        assert_eq!(p.col_start, 5);
        assert_eq!(p.col_end, 20);
    }

    #[test]
    fn test_comment_slashes_are_not_file_paths() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "// Selection rule:";
        let links = detector.detect_links_in_line(line, 0);

        assert!(!links.iter().any(|l| l.link_type == LinkType::FilePath));
    }

    #[test]
    fn test_block_comment_not_file_path() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "code /* block comment */ end";
        let links = detector.detect_links_in_line(line, 0);
        assert!(!links.iter().any(|l| l.link_type == LinkType::FilePath));
    }

    #[test]
    fn test_punct_only_not_file_path() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        // 数学/分隔符场景不应产生路径
        for line in ["a / b", "1 / 2", "use /* or */"] {
            let links = detector.detect_links_in_line(line, 0);
            assert!(
                !links.iter().any(|l| l.link_type == LinkType::FilePath),
                "误匹配于: {line}"
            );
        }
    }

    #[test]
    fn test_file_path_col_start_excludes_leading_space() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "see /etc/hosts here";
        let links = detector.detect_links_in_line(line, 0);
        let p = links
            .iter()
            .find(|l| l.link_type == LinkType::FilePath)
            .unwrap();
        assert_eq!(p.text, "/etc/hosts");
        assert_eq!(p.col_start, 4); // 指向 '/',不含前导空格
        assert_eq!(p.col_end, 14);
    }

    #[test]
    fn test_file_path_trailing_punctuation_stripped() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "edit /etc/hosts.";
        let links = detector.detect_links_in_line(line, 0);
        let p = links
            .iter()
            .find(|l| l.link_type == LinkType::FilePath)
            .unwrap();
        assert_eq!(p.text, "/etc/hosts");
    }

    #[test]
    fn test_version_number_not_ip() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        // 五段数字(版本号)不应被识别为 IP
        let line = "version 1.2.3.4.5 released";
        let links = detector.detect_links_in_line(line, 0);
        assert!(!links.iter().any(|l| l.link_type == LinkType::IpAddress));
    }

    #[test]
    fn test_real_ip_still_detected() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        // 独立 IP、带端口、句末 IP 仍应识别
        for line in ["ping 192.168.1.1 ok", "host 10.0.0.1:8080", "addr 8.8.8.8."] {
            let links = detector.detect_links_in_line(line, 0);
            assert!(
                links.iter().any(|l| l.link_type == LinkType::IpAddress),
                "未识别 IP: {line}"
            );
        }
    }

    #[test]
    fn test_link_detection_config() {
        let config = LinkDetectionConfig {
            detect_urls: false,
            ..LinkDetectionConfig::default()
        };

        let detector = LinkDetector::new(config);
        let line = "Visit https://192.0.2.1/a for more info";
        let links = detector.detect_links_in_line(line, 0);

        assert!(!links.iter().any(|l| l.link_type == LinkType::Url));
        assert!(links.is_empty(), "inner IP must not bypass URL policy");
    }

    #[test]
    fn unsafe_url_shapes_never_fall_back_to_inner_ip_links() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        for text in [
            "ftp://192.0.2.1/archive",
            "https://user:token@192.0.2.1/private",
            "file://192.0.2.1/etc/passwd",
        ] {
            assert!(
                detector.detect_links_in_line(text, 0).is_empty(),
                "unsafe enclosing URL became actionable: {text}"
            );
        }
    }

    #[test]
    fn unbalanced_trailing_paren_is_trimmed_but_balanced_parens_stay() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());

        let links = detector.detect_links_in_line("(https://example.com)", 0);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].text, "https://example.com");
        assert_eq!(links[0].col_end, "(https://example.com".len());

        let links =
            detector.detect_links_in_line("see https://en.wikipedia.org/wiki/Foo_(bar) now", 0);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].text, "https://en.wikipedia.org/wiki/Foo_(bar)");
    }
}
