/// 链接检测和交互模块
use regex::Regex;

/// Preserve balanced URL parentheses while dropping only excess trailing closers.
/// Count once: rescanning the whole URL after each ')' made link refresh quadratic
/// for process-controlled output with a long run of closing parentheses.
fn trim_unbalanced_url_parentheses(url: &str) -> &str {
    if !url.ends_with(')') {
        return url;
    }
    let (opens, closes) = url.bytes().fold((0usize, 0usize), |(opens, closes), byte| {
        (
            opens + usize::from(byte == b'('),
            closes + usize::from(byte == b')'),
        )
    });
    let excess = closes.saturating_sub(opens);
    let trim = url
        .bytes()
        .rev()
        .take(excess)
        .take_while(|byte| *byte == b')')
        .count();
    &url[..url.len() - trim]
}

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
    pub(crate) group: Option<(usize, usize)>,
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

impl Link {
    pub(crate) fn same_reference(&self, other: &Self) -> bool {
        if let Some(group) = self.group {
            other.group == Some(group)
                && self.link_type == other.link_type
                && self.text == other.text
        } else {
            self == other
        }
    }

    pub(crate) fn selection_bounds(&self, links: &[Self]) -> ((usize, usize), (usize, usize)) {
        let mut start = (self.line, self.col_start);
        let mut end = (self.line, self.col_end - 1);
        for link in links.iter().filter(|link| self.same_reference(link)) {
            start = start.min((link.line, link.col_start));
            end = end.max((link.line, link.col_end - 1));
        }
        (start, end)
    }
}

/// Spans share a reference even when only one of its fragments was covered.
pub(crate) fn keep_uncovered(candidates: &mut Vec<Link>, authoritative: &[Link]) {
    if authoritative.is_empty() {
        return;
    }
    let mut covered = std::collections::HashSet::new();
    candidates.retain(|link| {
        let overlaps = authoritative.iter().any(|other| {
            other.line == link.line
                && other.col_start < link.col_end
                && link.col_start < other.col_end
        });
        if overlaps {
            covered.extend(link.group);
        }
        !overlaps
    });
    candidates.retain(|link| link.group.is_none_or(|group| !covered.contains(&group)));
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

        // Paths start at a token boundary. Bare paths accept Unicode letters/numbers
        // and combining marks; only the known ASCII extension is case-insensitive.
        // Preserve the exact spelling (no Unicode normalization or case folding).
        // 用捕获组提取实际路径，避免把前导空白计入列偏移。
        let file_path_regex = Regex::new(r#"(?:^|[\s`"'])((?:(?:\.{0,2}|~)/[^\s<>\[\]{}|\\^`()"']*|[\p{L}\p{N}_][\p{L}\p{N}\p{M}_./-]*\.(?i-u:tsx|jsx|md|txt|rs|py|json|toml|yaml|yml|css|html|svg|png|jpg|jpeg|webp|ts|js)(?::[0-9]+){0,2}))"#).unwrap();

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
            let url = trim_unbalanced_url_parentheses(mat.as_str());
            let col_start = Self::byte_offset_to_char_offset(line, mat.start());
            let col_end = Self::byte_offset_to_char_offset(line, mat.start() + url.len());
            // 即使策略拒绝这个 URL，也保留整个区段：否则
            // https://user@192.0.2.1 的内层 IP 会被单独激活。
            url_spans.push((col_start, col_end));
            if self.config.detect_urls && crate::terminal::is_supported_hyperlink_uri(url) {
                links.push(Link {
                    group: None,
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
                        group: None,
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
                // Require an explicit delimiter: combining marks, emoji, zero-width
                // characters and suffix operators must not turn a longer token
                // into a different local filename.
                if line[m.end()..].chars().next().is_some_and(|c| {
                    !c.is_whitespace()
                        && !matches!(
                            c,
                            ',' | ';' | '!' | ')' | ']' | '}' | '\'' | '"' | '`' | '>'
                        )
                }) {
                    continue;
                }
                // Explicit path tokens may name files whose final character is
                // punctuation. Never guess a different target by stripping it.
                // Bare extension candidates remain subject to delimiter policy.
                let end_b = m.end();
                let matched_text = m.as_str();

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
                        group: None,
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
            || trimmed.starts_with("~/")
            || (trimmed.split(':').next().is_some_and(|path| {
                path.rsplit('/')
                    .next()
                    .is_some_and(|name| name.contains('.') && !name.starts_with('.'))
            }) && !trimmed.contains("://")))
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
        let wraps_valid = row_wrapped.len() == visible_cells.len();
        let mut text = String::new();
        // One entry per logical character, not per terminal cell. Its physical
        // extent includes any following wide-character continuation cells.
        let mut columns: Vec<(usize, usize)> = Vec::new();
        let mut rows: Vec<(usize, usize, usize)> = Vec::new();

        for (row, cells) in visible_cells.iter().enumerate() {
            let start = columns.len();
            for (col, cell) in cells.iter().enumerate() {
                if cell.flags.wide_continuation() {
                    if columns.len() > start {
                        columns.last_mut().unwrap().1 = col + 1;
                    }
                    continue;
                }
                text.push(cell.character);
                columns.push((col, col + 1));
            }
            rows.push((row, start, columns.len()));
            // Missing/malformed metadata means separate physical lines. Ordinary
            // spaces, including row-end padding, remain text; never guess a wrap.
            if wraps_valid && row_wrapped[row] && row + 1 < visible_cells.len() {
                continue;
            }
            for link in self.detect_links_in_line(&text, 0) {
                // Start at the first overlapping row instead of rescanning the
                // entire wrapped paragraph for every detected link.
                let first = rows.partition_point(|&(_, _, end)| end <= link.col_start);
                let (line, _, end) = rows[first];
                let group = (link.col_end > end).then(|| (line, columns[link.col_start].0));
                for &(line, start, end) in &rows[first..] {
                    if start >= link.col_end {
                        break;
                    }
                    let from = start.max(link.col_start);
                    let to = end.min(link.col_end);
                    if from < to {
                        all_links.push(Link {
                            group,
                            line,
                            col_start: columns[from].0,
                            col_end: columns[to - 1].1,
                            link_type: link.link_type,
                            text: link.text.clone(),
                        });
                    }
                }
            }
            text.clear();
            columns.clear();
            rows.clear();
        }
        let (delimited, blocked) =
            Self::detect_delimited_links(visible_cells.iter().map(|row| row.as_ref()), row_wrapped);
        keep_uncovered(&mut all_links, &blocked);
        keep_uncovered(&mut all_links, &delimited);
        all_links.extend(delimited.into_iter().filter(|link| match link.link_type {
            LinkType::Url => {
                self.config.detect_urls && jterm_core::link::is_openable_url(&link.text)
            }
            LinkType::FilePath => {
                self.config.detect_file_paths
                    && !jterm_core::review_input::contains_visual_spoofing(&link.text)
            }
            LinkType::IpAddress => false,
        }));
        all_links
    }

    fn detect_delimited_links<'a>(
        rows: impl Iterator<Item = &'a [crate::terminal::TerminalCell]>,
        soft_wraps: &[bool],
    ) -> (Vec<Link>, Vec<Link>) {
        let rows: Vec<_> = rows.collect();
        let mut safe_wraps = soft_wraps.to_vec();
        if safe_wraps.len() == rows.len() {
            for index in 0..rows.len().saturating_sub(1) {
                // A wide glyph that cannot fit can leave an unmarked blank in
                // the previous last cell. The grid cannot prove whether that
                // blank was typed; fail closed instead of inventing a filename.
                if rows[index].last().is_some_and(|cell| cell.character == ' ')
                    && rows[index + 1]
                        .first()
                        .is_some_and(|cell| cell.flags.wide())
                {
                    safe_wraps[index] = false;
                }
            }
        }
        if !rows
            .iter()
            .any(|row| row.iter().any(|cell| cell.character == '('))
        {
            return (Vec::new(), Vec::new());
        }
        let mut text_rows = Vec::with_capacity(rows.len());
        let mut columns = Vec::with_capacity(rows.len());
        for row in rows {
            let mut text = String::new();
            let mut cells: Vec<(usize, usize, usize)> = Vec::new();
            for (col, cell) in row.iter().enumerate() {
                if cell.flags.wide_continuation() {
                    if let Some(last) = cells.last_mut() {
                        last.2 = col + 1;
                    }
                } else {
                    cells.push((text.len(), col, col + 1));
                    text.push(cell.character);
                }
            }
            let fills_width = row
                .iter()
                .rposition(|cell| !cell.flags.wide_continuation() && cell.character != ' ')
                .is_some_and(|column| {
                    column + 1 + usize::from(row[column].flags.wide())
                        >= row.len().saturating_sub(1)
                });
            text_rows.push((text, fills_width));
            columns.push(cells);
        }
        let rows: Vec<_> = text_rows
            .iter()
            .map(|(text, fills)| (text.as_str(), *fills))
            .collect();
        let mut links = Vec::new();
        let mut blocked = Vec::new();
        for reference in crate::delimited_reference::detect(&rows, &safe_wraps) {
            let link_type = if reference.web {
                LinkType::Url
            } else {
                LinkType::FilePath
            };
            let (first, start) = &reference.spans[0];
            let first_cell = columns[*first].partition_point(|&(byte, _, _)| byte < start.start);
            let group =
                (reference.spans.len() > 1).then_some((*first, columns[*first][first_cell].1));
            for (line, span) in reference.spans {
                let cells = &columns[line];
                let start = cells.partition_point(|&(byte, _, _)| byte < span.start);
                let end = cells.partition_point(|&(byte, _, _)| byte < span.end);
                let destination = if reference.ambiguous {
                    &mut blocked
                } else {
                    &mut links
                };
                destination.push(Link {
                    group,
                    line,
                    col_start: cells[start].1,
                    col_end: cells[end - 1].2,
                    link_type,
                    text: reference.target.clone(),
                });
            }
        }
        (links, blocked)
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
                        group: None,
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
        keep_uncovered(&mut detected, &explicit_links);
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
            open_local_path(std::path::Path::new(&link.text))?;
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

/// Open a local filesystem path with the trusted system opener.
pub(crate) fn open_local_path(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Open requires an absolute local path",
        )
        .into());
    }
    open_file_path(path)
}

/// 打开文件路径（使用系统默认应用）
fn open_file_path(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut child = std::process::Command::new(trusted_opener()?)
        .args(file_opener_args(path)?)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// xdg-open has no `--` operand separator. An absolute path is already
/// option-safe and keeps relative names such as `-notes.txt` out of its option
/// parser. macOS `open` does accept `--`; preserve that platform's convention.
fn file_opener_args(path: &std::path::Path) -> std::io::Result<Vec<std::ffi::OsString>> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    #[cfg(target_os = "macos")]
    let args = vec![std::ffi::OsString::from("--"), absolute.into_os_string()];
    #[cfg(not(target_os = "macos"))]
    let args = vec![absolute.into_os_string()];
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed_link_cells(
        text: &str,
        cols: usize,
    ) -> (Vec<Vec<crate::terminal::TerminalCell>>, Vec<bool>) {
        let mut terminal = crate::terminal::TerminalState::new(cols, 3);
        terminal.process_input(text.as_bytes());
        let viewport =
            terminal.projected_viewport(crate::terminal::HistoryProjection::identity(), true);
        (viewport.cells().to_vec(), viewport.row_wrapped().to_vec())
    }

    #[test]
    fn parsed_wide_links_keep_targets_and_physical_columns() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        for (text, target, start, end, kind) in [
            (
                "https://example.com/界x",
                "https://example.com/界x",
                0,
                23,
                LinkType::Url,
            ),
            (
                "https://example.com/界",
                "https://example.com/界",
                0,
                22,
                LinkType::Url,
            ),
            ("/tmp/界x", "/tmp/界x", 0, 8, LinkType::FilePath),
            (
                "界 https://example.com/x",
                "https://example.com/x",
                3,
                24,
                LinkType::Url,
            ),
        ] {
            let (cells, wraps) = parsed_link_cells(text, 80);
            assert!(cells[0].iter().any(|cell| cell.flags.wide_continuation()));
            let expected = vec![Link {
                group: None,
                line: 0,
                col_start: start,
                col_end: end,
                link_type: kind,
                text: target.into(),
            }];
            for metadata in [&wraps[..], &[][..], &[true][..]] {
                assert_eq!(
                    detector.detect_links_in_visible_cells_with_wrapping(&cells, metadata),
                    expected,
                    "input {text:?}, wrap metadata {metadata:?}",
                );
            }
        }
    }

    #[test]
    fn parsed_unicode_file_links_map_wide_cells_and_wraps() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let target = "目录/文件.MD:4:2";
        let (cells, wraps) = parsed_link_cells(target, 7);
        assert!(wraps[0]);
        let links = detector.detect_links_in_visible_cells_with_wrapping(&cells, &wraps);
        assert_eq!(links.len(), 3);
        for (link, (row, start, end)) in links.iter().zip([(0, 0, 7), (1, 0, 7), (2, 0, 2)]) {
            assert_eq!(link.text, target);
            assert_eq!(link.link_type, LinkType::FilePath);
            assert_eq!((link.line, link.col_start, link.col_end), (row, start, end));
        }
        // Both physical halves of each wide glyph are inside the activation span.
        assert!(cells[0][1].flags.wide_continuation());
        assert!(cells[0][3].flags.wide_continuation());
        assert!(cells[0][6].flags.wide_continuation());
    }

    #[test]
    fn parsed_wide_file_path_keeps_trailing_period_cell() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let target = "./界.md。";
        let (cells, wraps) = parsed_link_cells(target, 80);
        let links = detector.detect_links_in_visible_cells_with_wrapping(&cells, &wraps);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].text, target);
        assert_eq!((links[0].col_start, links[0].col_end), (0, 9));
        assert!(cells[0][8].flags.wide_continuation());
    }

    #[test]
    fn hard_wrap_at_a_full_width_cjk_cell_keeps_both_fragments() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let (cells, wraps) = parsed_link_cells("(/tmp/中文\r\n报告.md)", 10);
        let links = detector.detect_links_in_visible_cells_with_wrapping(&cells, &wraps);
        assert!(links.is_empty());
    }

    #[test]
    fn rejected_wrapped_authority_does_not_open_its_first_fragment() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let (cells, wraps) =
            parsed_link_cells("(https://safe.example-\r\n@other.example/path)", 80);
        assert!(detector
            .detect_links_in_visible_cells_with_wrapping(&cells, &wraps)
            .is_empty());
    }

    #[test]
    fn ambiguous_physical_destinations_never_open_fragments() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        for input in [
            "(reports/a-\r\n  sub/file.bin)",
            "(reports/a-\r\n  sub/file.md )",
            "(https://trusted.example-\r\n attacker.test/path)",
            "(https://trusted.example\r\n attacker.test/path)",
        ] {
            let (cells, wraps) = parsed_link_cells(input, 80);
            assert!(
                detector
                    .detect_links_in_visible_cells_with_wrapping(&cells, &wraps)
                    .is_empty(),
                "{input}"
            );
        }
    }

    #[test]
    fn rejected_soft_wrapped_file_blocks_ordinary_fragments() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        for input in ["(reports/a-  sub/file.md )", "(reports/a-  sub/file.bin)"] {
            let (cells, wraps) = parsed_link_cells(input, 12);
            assert!(wraps.iter().any(|wrapped| *wrapped));
            assert!(
                detector
                    .detect_links_in_visible_cells_with_wrapping(&cells, &wraps)
                    .is_empty(),
                "{input}"
            );
        }
    }

    #[test]
    fn wide_wrap_padding_cannot_become_a_different_filename() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        // The opening parenthesis plus reports/a consumes ten columns. A
        // two-cell glyph cannot fit in the eleventh remaining cell.
        let (cells, wraps) = parsed_link_cells("(reports/a界.md)", 11);
        assert!(wraps[0]);
        assert_eq!(cells[0].last().unwrap().character, ' ');
        assert!(cells[1][0].flags.wide());
        assert!(detector
            .detect_links_in_visible_cells_with_wrapping(&cells, &wraps)
            .is_empty());
    }

    #[test]
    fn raw_copy_keeps_wide_wrap_geometry() {
        let mut terminal = crate::terminal::TerminalState::new(5, 3);
        terminal.process_input("abcd界x".as_bytes());
        terminal.start_selection((0, 0));
        terminal.update_selection((1, 2));
        assert_eq!(terminal.copy_selection().as_deref(), Some("abcd界x"));
    }

    #[test]
    fn soft_wrapped_file_preserves_literal_spaces() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let (cells, wraps) = parsed_link_cells("(reports/a-  b.md)", 12);
        let links = detector.detect_links_in_visible_cells_with_wrapping(&cells, &wraps);
        assert!(!links.is_empty());
        assert!(links.iter().all(|link| link.text == "reports/a-  b.md"));
        let mut terminal = crate::terminal::TerminalState::new(12, 3);
        terminal.process_input(b"(reports/a-  b.md)");
        let viewport =
            terminal.projected_viewport(crate::terminal::HistoryProjection::identity(), false);
        terminal.select_link_in_projection(&viewport, &links[0], &links);
        assert_eq!(
            terminal.copy_selection().as_deref(),
            Some("reports/a-  b.md")
        );
        let (start, end) = links[0].selection_bounds(&links);
        terminal.start_selection_projected(&viewport, end);
        terminal.update_selection_projected(&viewport, start);
        terminal.normalize_link_selection(&viewport, &links);
        assert_eq!(
            terminal.copy_selection().as_deref(),
            Some("reports/a-  b.md")
        );
        // Identical non-space bytes are not sufficient identity evidence.
        let mut mismatched = links.clone();
        for link in &mut mismatched {
            link.text = "reports/a-b .md".into();
        }
        terminal.select_link_in_projection(&viewport, &mismatched[0], &mismatched);
        assert_ne!(
            terminal.copy_selection().as_deref(),
            Some("reports/a-b .md")
        );
        assert!(detector
            .detect_links_in_visible_cells_with_wrapping(&cells, &[])
            .is_empty());
    }

    #[test]
    fn parsed_wide_links_split_wrapped_cells_without_joining_real_spaces() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let target = "https://example.com/界x";
        // The wide glyph fills the first row exactly; its continuation is a
        // cell, not a space in the target. Each physical segment keeps the URL.
        let (cells, wraps) = parsed_link_cells(target, 22);
        assert!(wraps[0]);
        assert!(cells[0][21].flags.wide_continuation());
        let links = detector.detect_links_in_visible_cells_with_wrapping(&cells, &wraps);
        assert_eq!(
            links,
            vec![
                Link {
                    group: Some((0, 0)),
                    line: 0,
                    col_start: 0,
                    col_end: 22,
                    link_type: LinkType::Url,
                    text: target.into()
                },
                Link {
                    group: Some((0, 0)),
                    line: 1,
                    col_start: 0,
                    col_end: 1,
                    link_type: LinkType::Url,
                    text: target.into()
                },
            ]
        );
        let fallback = detector.detect_links_in_visible_cells_with_wrapping(&cells, &[true]);
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0].text, "https://example.com/界");
        assert_eq!(fallback[0].col_end, 22);

        // Ordinary internal and trailing spaces are never trimmed away to
        // manufacture a target across a soft wrap or explicit newline.
        for (input, width) in [
            ("https://example.com/界 x", 80),
            ("https://example.com/界 x", 23),
            ("https://example.com/界 \r\nx", 23),
        ] {
            let (cells, wraps) = parsed_link_cells(input, width);
            let links = detector.detect_links_in_visible_cells_with_wrapping(&cells, &wraps);
            assert_eq!(links.len(), 1, "{input:?}");
            assert_eq!(links[0].text, "https://example.com/界");
            assert_eq!(
                (links[0].line, links[0].col_start, links[0].col_end),
                (0, 0, 22)
            );
        }
    }

    #[test]
    fn parenthesis_trimming_matches_legacy_for_all_short_inputs() {
        fn legacy(mut url: &str) -> &str {
            while url.ends_with(')') && url.matches(')').count() > url.matches('(').count() {
                url = &url[..url.len() - 1];
            }
            url
        }
        // Include multi-byte text so byte trimming never splits a codepoint.
        let alphabet = ['(', ')', '雪', 'a'];
        for len in 0..=8 {
            for mut encoded in 0..4usize.pow(len) {
                let mut input = String::from("https://example.com/");
                for _ in 0..len {
                    input.push(alphabet[encoded % 4]);
                    encoded /= 4;
                }
                assert_eq!(trim_unbalanced_url_parentheses(&input), legacy(&input));
            }
        }
    }

    #[test]
    fn long_trailing_parentheses_keep_the_balanced_url_and_columns() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let target = "https://example.com/wiki/Foo_(雪)";
        let line = format!("雪 {target}{}", ")".repeat(100_000));
        let links = detector.detect_links_in_line(&line, 7);
        assert_eq!(
            links,
            vec![Link {
                group: None,
                line: 7,
                col_start: 2,
                col_end: 2 + target.chars().count(),
                link_type: LinkType::Url,
                text: target.into(),
            }]
        );
    }

    #[test]
    fn wrapped_unicode_rows_keep_exact_link_cells_and_logical_boundaries() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let rows = ["雪 https://", "example.com/", "Foo_(雪)))", "192.0.2.1"];
        let cells: Vec<Vec<crate::terminal::TerminalCell>> = rows
            .iter()
            .map(|row| {
                row.chars()
                    .map(|character| crate::terminal::TerminalCell {
                        character,
                        ..Default::default()
                    })
                    .collect()
            })
            .collect();
        let links = detector
            .detect_links_in_visible_cells_with_wrapping(&cells, &[true, true, false, false]);
        let target = "https://example.com/Foo_(雪)";
        assert_eq!(
            links,
            vec![
                Link {
                    group: Some((0, 2)),
                    line: 0,
                    col_start: 2,
                    col_end: 10,
                    link_type: LinkType::Url,
                    text: target.into()
                },
                Link {
                    group: Some((0, 2)),
                    line: 1,
                    col_start: 0,
                    col_end: 12,
                    link_type: LinkType::Url,
                    text: target.into()
                },
                Link {
                    group: Some((0, 2)),
                    line: 2,
                    col_start: 0,
                    col_end: 7,
                    link_type: LinkType::Url,
                    text: target.into()
                },
                Link {
                    group: None,
                    line: 3,
                    col_start: 0,
                    col_end: 9,
                    link_type: LinkType::IpAddress,
                    text: "192.0.2.1".into()
                },
            ]
        );
        // An incomplete wrap map must retain the existing per-row fallback.
        assert_eq!(
            detector.detect_links_in_visible_cells_with_wrapping(&cells, &[true]),
            cells
                .iter()
                .enumerate()
                .flat_map(|(row, cells)| {
                    let text: String = cells.iter().map(|cell| cell.character).collect();
                    detector.detect_links_in_line(&text, row)
                })
                .collect::<Vec<_>>(),
        );
    }

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
    fn osc8_wide_decoy_keeps_explicit_target_and_continuation_extent() {
        let mut terminal = TerminalState::new(80, 2);
        terminal.process_input(b"\x1b]8;;https://real.example/target\x1b\\");
        terminal.process_input("https://decoy.example/界".as_bytes());
        terminal.process_input(b"\x1b]8;;\x1b\\");
        let viewport = terminal.projected_viewport(HistoryProjection::identity(), true);
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let links = detector.detect_links_in_visible_cells_with_wrapping_and_hyperlinks(
            viewport.cells(),
            viewport.row_wrapped(),
            |id| terminal.hyperlink_uri(id).map(str::to_owned),
        );
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].text, "https://real.example/target");
        assert_eq!((links[0].col_start, links[0].col_end), (0, 24));
    }

    #[test]
    fn open_link_rejects_dangerous_and_unsupported_url_schemes() {
        for target in [
            "javascript:alert(1)",
            "data:text/html,hello",
            "shell:touch-dangerous",
        ] {
            let link = Link {
                group: None,
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
    fn file_opener_uses_an_absolute_option_safe_operand() {
        let args = file_opener_args(std::path::Path::new("-notes.txt")).unwrap();
        let expected = std::env::current_dir().unwrap().join("-notes.txt");
        #[cfg(target_os = "macos")]
        assert_eq!(
            args,
            [std::ffi::OsString::from("--"), expected.into_os_string()]
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(args, [expected.into_os_string()]);
    }

    #[test]
    fn file_opener_keeps_absolute_paths_as_one_exact_operand() {
        let path = std::env::temp_dir().join("file with spaces;$(not-a-command).txt");
        let args = file_opener_args(&path).unwrap();
        assert_eq!(args.last().unwrap(), path.as_os_str());
        #[cfg(target_os = "linux")]
        assert_eq!(args.len(), 1, "xdg-open rejects a separate -- operand");
    }

    #[cfg(unix)]
    #[test]
    fn file_opener_preserves_non_utf8_path_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let path =
            std::env::temp_dir().join(std::ffi::OsString::from_vec(b"notes-\xff.txt".to_vec()));
        assert_eq!(
            file_opener_args(&path).unwrap().last().unwrap(),
            path.as_os_str()
        );
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
    fn test_file_path_trailing_punctuation_preserved() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "edit /etc/hosts.";
        let links = detector.detect_links_in_line(line, 0);
        let p = links
            .iter()
            .find(|l| l.link_type == LinkType::FilePath)
            .unwrap();
        assert_eq!(p.text, "/etc/hosts.");
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

#[cfg(test)]
mod local_preview_detection_tests {
    use super::*;
    #[test]
    fn detects_codex_source_references_without_a_dot_slash_prefix() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        for text in [
            "src/App.tsx:12:3",
            "README.md",
            "PHOTO.PNG",
            "src/View.TSX:4:2",
            "目录/文件.md",
            "src/报告.tsx:4:2",
            "cafe\u{301}/re\u{301}sume\u{301}.Md:3",
            "./images/chart.png",
            "../notes.md",
        ] {
            let links = detector.detect_links_in_line(text, 0);
            assert!(
                links
                    .iter()
                    .any(|link| link.link_type == LinkType::FilePath && link.text == text),
                "{text}: {links:?}"
            );
        }
    }
}

/// Keep application mouse reporting ownership for local previews, without
/// changing the existing explicit Ctrl-click URL opener contract.
pub fn host_activation_allowed(kind: LinkType, app_mouse: bool) -> bool {
    kind != LinkType::FilePath || !app_mouse
}

#[cfg(test)]
mod preview_boundary_tests {
    use super::*;
    #[test]
    fn longer_extension_is_never_a_different_file() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        for text in [
            "README.md.bak",
            "src/view.tsx.map",
            "file.tsxx",
            "src/view.tsx-other",
            "PHOTO.PNGx",
            "src/报告.TSX.map",
            "cafe.md\u{301}",
            "src/报告.md\u{200d}",
            "src/报告.md💥",
            "PHOTO.PNG:4:2:3",
            "PHOTO.PNG?query",
        ] {
            assert!(!detector
                .detect_links_in_line(text, 0)
                .iter()
                .any(|link| link.link_type == LinkType::FilePath));
        }
    }
    #[test]
    fn unicode_paths_do_not_escape_url_ownership() {
        for enabled in [true, false] {
            let detector = LinkDetector::new(LinkDetectionConfig {
                detect_urls: enabled,
                ..Default::default()
            });
            for text in [
                "https://example.com/src/报告.TSX:4:2",
                "file:///目录/文件.PNG",
                "evil://host/PHOTO.PNG",
                "https://host/README.md.",
                "file:///README.md?",
            ] {
                assert!(
                    !detector
                        .detect_links_in_line(text, 0)
                        .iter()
                        .any(|l| l.link_type == LinkType::FilePath),
                    "{text}"
                );
            }
        }
    }

    #[test]
    fn explicit_paths_keep_literal_punctuation_and_bare_periods_are_ambiguous() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        for target in [
            "./README.md.",
            "./README.md。",
            "../report.md?",
            "/tmp/report.md!",
            "./report.md,",
            "./report.md;",
            "./view.tsx:4:2.",
            "./view.tsx:",
        ] {
            let links = detector.detect_links_in_line(target, 0);
            assert_eq!(links.len(), 1, "{target}: {links:?}");
            assert_eq!(links[0].text, target);
            assert_eq!(links[0].col_end, target.chars().count());
        }
        for target in [
            "README.md.",
            "README.md。",
            "README.md.bak",
            "README.md?",
            "README.md:4:2.",
        ] {
            assert!(
                !detector
                    .detect_links_in_line(target, 0)
                    .iter()
                    .any(|l| l.link_type == LinkType::FilePath),
                "{target}"
            );
        }
    }

    #[test]
    fn preview_keeps_app_mouse_but_preserves_url_contract() {
        assert!(!host_activation_allowed(LinkType::FilePath, true));
        assert!(host_activation_allowed(LinkType::FilePath, false));
        assert!(host_activation_allowed(LinkType::Url, true));
    }
}
