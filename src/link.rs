/// 链接检测和交互模块
use regex::Regex;

/// 链接类型
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum LinkType {
    /// URL (http/https/ftp)
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
        // URL 正则：http(s)?:// 或 ftp://
        let url_regex =
            Regex::new(r"(?:https?|ftp)://[^\s<>\[\]{}|\\^`()]*[^\s<>\[\]{}|\\^`().,;:!?\-]")
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

        // 检测 URL
        if self.config.detect_urls {
            for mat in self.url_regex.find_iter(line) {
                let col_start = Self::byte_offset_to_char_offset(line, mat.start());
                let col_end = Self::byte_offset_to_char_offset(line, mat.end());
                links.push(Link {
                    line: line_idx,
                    col_start,
                    col_end,
                    link_type: LinkType::Url,
                    text: mat.as_str().to_string(),
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
                // 避免与 URL 重复
                if !links
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

                // 避免与 URL 重复
                if !links
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
}

/// 打开链接
pub fn open_link(link: &Link) -> Result<(), Box<dyn std::error::Error>> {
    match link.link_type {
        LinkType::Url => {
            open_url(&link.text)?;
        }
        LinkType::FilePath => {
            open_file_path(&link.text)?;
        }
        LinkType::IpAddress => {
            // IP 地址可以用浏览器打开或显示 whois 信息
            open_url(&format!("http://{}", &link.text))?;
        }
    }
    Ok(())
}

/// 打开 URL（使用系统默认浏览器）
#[cfg(target_os = "linux")]
fn open_url(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    std::process::Command::new("xdg-open").arg(url).spawn()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_url(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    std::process::Command::new("open").arg(url).spawn()?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn open_url(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    std::process::Command::new("cmd")
        .args(&["/C", "start", url])
        .spawn()?;
    Ok(())
}

/// 打开文件路径（使用系统默认应用）
#[cfg(target_os = "linux")]
fn open_file_path(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let expanded_path = expand_path(path);

    std::process::Command::new("xdg-open")
        .arg(&expanded_path)
        .spawn()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_file_path(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let expanded_path = expand_path(path);
    std::process::Command::new("open")
        .arg(&expanded_path)
        .spawn()?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn open_file_path(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let expanded_path = expand_path(path);
    std::process::Command::new("explorer")
        .arg(&expanded_path)
        .spawn()?;
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
        let line = "Visit https://example.com for more info";
        let links = detector.detect_links_in_line(line, 0);

        assert!(!links.iter().any(|l| l.link_type == LinkType::Url));
    }
}
