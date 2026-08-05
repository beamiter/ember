use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

use crate::persistence_file::FileRevision;

pub const DEFAULT_FONT_SIZE: f32 = 14.0;

/// Upper bound for `config.toml`. A hand-written terminal config is a few
/// kilobytes; the bound exists so a file that is not really a config (a log
/// rotated onto the path, a deliberately fattened file) is rejected with a
/// size error instead of being read in full and then failing as a TOML parse
/// error pointing at the wrong thing. Rejection sets `load_error`, which is
/// what keeps the file from being overwritten by the next font zoom.
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_CONFIG_NAME_BYTES: usize = 256;
const MAX_CONFIG_VALUE_BYTES: usize = 4 * 1024;

// Nerd Font priority list
const NERD_FONT_CANDIDATES: &[&str] = &[
    "SauceCodePro Nerd Font",
    "SauceCodePro Nerd Font Mono",
    "Monokoi Nerd Font",
    "Monokoi Nerd Font Mono",
    "JetBrains Mono Nerd Font",
    "JetBrains Mono NF",
    "JetBrainsMono Nerd Font",
    "FiraCode Nerd Font",
];

// 延迟加载的字体列表缓存（避免启动时阻塞）
static AVAILABLE_FONTS: Lazy<Vec<String>> = Lazy::new(|| {
    eprintln!("[Config] Scanning system fonts (one-time)...");
    detect_fonts_by_query(&[":"])
});

static MONOSPACE_FONTS: Lazy<Vec<String>> = Lazy::new(|| {
    eprintln!("[Config] Scanning monospace fonts (one-time)...");
    detect_fonts_by_query(&[":spacing=100"])
});

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum FontBackendType {
    #[default]
    Fontdue,
    AbGlyph,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum AppRendererType {
    Glow,
    #[default]
    Wgpu,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ScrollbarVisibility {
    Auto,
    #[default]
    Always,
}

/// 会话标签栏的位置:顶部水平栏 或 集成进左侧侧边栏
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TabBarPosition {
    #[default]
    Top,
    Sidebar,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// AI features master switch. Off by default: nothing leaves the machine
    /// unless the user opts in.
    #[serde(default)]
    pub ai_enabled: bool,

    /// AI provider: "anthropic", "openai-compatible", or "ollama".
    #[serde(default = "default_ai_provider")]
    pub ai_provider: String,

    #[serde(default = "default_ai_base_url")]
    pub ai_base_url: String,

    #[serde(default = "default_ai_model")]
    pub ai_model: String,

    #[serde(default = "default_ai_max_tokens")]
    pub ai_max_tokens: u32,

    /// 采样温度（None 使用 provider 默认；有效范围 0.0..=2.0）
    #[serde(default)]
    pub ai_temperature: Option<f32>,

    /// Scrub high-confidence secrets from AI-bound text (default on).
    #[serde(default = "default_true")]
    pub ai_redact_secrets: bool,

    /// Optional path to a 0600 file holding the provider API key, so the key
    /// never has to live in the process environment or this config file.
    #[serde(default)]
    pub ai_api_key_file: Option<String>,

    /// Turn budget for one Agent-mode session.
    #[serde(default = "default_agent_max_turns")]
    pub agent_max_turns: u32,

    /// Remote destinations for the host picker (Ctrl+Shift+S). Grammar,
    /// validation and the argv a tab runs are the family-shared
    /// `jterm_core::jsh_remote::RemoteHostConfig`. A file with no key at all
    /// gets [`default_remote_hosts`] — two worked entries to copy; an explicit
    /// list, `[]` included, is taken as written.
    #[serde(default = "default_remote_hosts")]
    pub remote_hosts: Vec<jterm_core::jsh_remote::RemoteHostConfig>,

    #[serde(default = "default_font_size")]
    pub font_size: f32,

    #[serde(default = "default_font_family")]
    pub font_family: String,

    #[serde(default = "default_font_weight")]
    pub font_weight: f32,

    #[serde(default = "default_font_sharpness")]
    pub font_sharpness: f32,

    #[serde(default)]
    pub font_backend: FontBackendType,

    #[serde(default = "default_padding")]
    pub padding: f32,

    #[serde(default = "default_line_spacing")]
    pub line_spacing: f32,

    #[serde(default)]
    pub scrollbar_visibility: ScrollbarVisibility,

    #[serde(default = "default_scrollback_lines")]
    pub scrollback_lines: usize,

    #[serde(default = "default_initial_width")]
    pub initial_width: f32,

    #[serde(default = "default_initial_height")]
    pub initial_height: f32,

    #[serde(default = "default_cols")]
    pub cols: usize,

    #[serde(default = "default_rows")]
    pub rows: usize,

    #[serde(default = "default_theme")]
    pub theme: String,

    #[serde(default = "default_restore_session")]
    pub restore_session: bool,

    #[serde(default)]
    pub session_history_file: Option<PathBuf>,

    #[serde(default = "default_opacity")]
    pub opacity: f32,

    #[serde(default = "default_gpu_rendering")]
    pub gpu_rendering: bool,

    #[serde(default)]
    pub app_renderer: AppRendererType,

    #[serde(default = "default_scroll_speed")]
    pub scroll_speed: u32,

    #[serde(default)]
    pub ui_scale: Option<f32>,

    #[serde(default = "default_subpixel_rendering")]
    pub subpixel_rendering: bool,

    #[serde(default = "default_font_ligatures")]
    pub font_ligatures: bool,

    /// Explicit shell path (overrides auto-detection). Useful when PATH is stripped by launchers like wofi.
    #[serde(default)]
    pub shell: Option<String>,

    /// When to look for a newer jsh: "startup", "daily" (default) or "never".
    /// The check only decides whether the offer appears; installing always
    /// stays an explicit choice.
    #[serde(default = "default_jsh_update_check")]
    pub jsh_update_check: String,

    /// 会话标签栏位置:顶部水平栏(默认)或集成进左侧侧边栏
    #[serde(default)]
    pub tab_bar_position: TabBarPosition,

    /// 侧边栏 tab 模式下记住的视图(会话/文件)。默认显示会话,用户切换后会被持久化。
    #[serde(default = "default_sidebar_view")]
    pub sidebar_view: crate::sidebar::SidebarView,

    /// 允许程序通过 OSC 52 写入系统剪贴板(默认禁止)。
    /// 写入会让终端里运行的任意程序(包括 ssh 对端)悄悄改写系统剪贴板,
    /// 下一次粘贴就可能粘出别人准备好的内容,因此新安装和缺省配置一律关闭;
    /// 显式写成 `true` 的配置会原样保留。
    #[serde(default)]
    pub osc52_clipboard_write: bool,

    /// 允许程序通过 OSC 52 读取系统剪贴板(默认禁止)。
    /// 读取会把剪贴板内容回传给终端内运行的程序,存在隐私/安全风险,故默认关闭。
    #[serde(default)]
    pub osc52_clipboard_read: bool,

    /// 粘贴多行/大块内容时弹窗确认(默认开启)。用户可在确认对话框里选择不再询问。
    #[serde(default = "default_true")]
    pub paste_confirm: bool,

    /// 长命令完成后发送桌面通知(默认开启)。仅当命令运行超过
    /// `notify_long_block_threshold_ms` 且完成时用户没有盯着该 pane
    /// (窗口失焦或会话在后台 tab)才触发。与 anvil 的
    /// `notify_long_blocks` 同名,保持家族配置一致。
    #[serde(default = "default_true")]
    pub notify_long_blocks: bool,

    /// `notify_long_blocks` 的时长阈值(毫秒)。
    #[serde(default = "default_notify_long_block_threshold_ms")]
    pub notify_long_block_threshold_ms: u64,

    /// 多 pane 布局的头部里显示 git 分支/脏状态(默认开启)。与 anvil
    /// 的 `show_repo_strip` 同名,保持家族配置一致。
    #[serde(default = "default_true")]
    pub show_repo_strip: bool,

    /// 窗口底部的家族统一状态栏(cwd、git 分支、上一条命令结果、grid 尺寸、
    /// tab 位置;默认开启)。四个 jterm 共用 `bottom_bar` 键与默认值,
    /// 内容由 `jterm_core::bottom_bar` 统一编排。
    #[serde(default = "default_bottom_bar")]
    pub bottom_bar: bool,

    /// 单击终端内容区把 shell 的编辑光标移动到点击处(默认开启)。四个 jterm
    /// 共用 `click_moves_cursor` 键与默认值,移动量由
    /// `jterm_core::click_cursor` 统一计算。
    #[serde(default = "default_click_moves_cursor")]
    pub click_moves_cursor: bool,

    /// 命令块 chrome(默认开启):按 OSC 133 记录绘制每个命令块的侧边条纹、
    /// 分隔线与结果徽章,与 anvil/forge 的 block UI 一致。关闭后
    /// `block:*` 命令(跳转/复制/召回)仍然可用。
    #[serde(default = "default_block_mode")]
    pub block_mode: bool,

    /// Why this run could not use the on-disk config, if it exists but could
    /// not be read or parsed. Never serialized: it describes the load attempt,
    /// not a user setting.
    ///
    /// While it is set, [`Config::save`] refuses to write. Otherwise the
    /// built-in defaults sitting in this struct would be flushed over a
    /// hand-written file by something as incidental as a Ctrl+wheel font zoom
    /// or the save on Drop — one typo would cost the whole file. Recovery is
    /// either fixing the file (hot reload clears this) or an explicit
    /// "Reset to defaults", which replaces the whole struct.
    #[serde(skip)]
    pub load_error: Option<String>,

    /// Exact bytes this in-memory value was loaded or saved against. `None`
    /// is reserved for an explicit default/reset value (force-write) or a
    /// path that could not be inspected at all.
    #[serde(skip)]
    pub(crate) revision: Option<FileRevision>,
}

fn default_true() -> bool {
    true
}

fn default_bottom_bar() -> bool {
    jterm_core::bottom_bar::ENABLED_BY_DEFAULT
}

fn default_click_moves_cursor() -> bool {
    jterm_core::click_cursor::ENABLED_BY_DEFAULT
}

fn default_block_mode() -> bool {
    true
}

fn default_notify_long_block_threshold_ms() -> u64 {
    10_000
}

fn default_jsh_update_check() -> String {
    jterm_core::jsh_install::UpdateCheck::default()
        .as_str()
        .to_string()
}

fn default_ai_provider() -> String {
    "anthropic".to_string()
}

fn default_ai_base_url() -> String {
    "https://api.anthropic.com".to_string()
}

fn default_ai_model() -> String {
    "claude-sonnet-4-6".to_string()
}

fn default_ai_max_tokens() -> u32 {
    1_024
}

fn default_agent_max_turns() -> u32 {
    20
}

/// Two worked entries a new destination can be copied from: one ssh target and
/// one running container. They exist because the two mistakes the grammar
/// cannot forgive are invisible in an empty list — the port belongs in
/// `ssh_args`, never as `host:port`, and the login belongs in `user`, never as
/// a `user@host` string that ssh would take literally as a hostname.
///
/// Only consulted when the file has no `remote_hosts` key at all. An explicit
/// list — including `remote_hosts = []` — always wins, so hosts deleted in the
/// settings panel (which writes the key back) stay deleted.
pub fn default_remote_hosts() -> Vec<jterm_core::jsh_remote::RemoteHostConfig> {
    vec![
        jterm_core::jsh_remote::RemoteHostConfig {
            name: "dev-60".to_string(),
            host: "10.68.18.60".to_string(),
            user: Some("root".to_string()),
            docker: false,
            remote_shell: "jsh".to_string(),
            session: None,
            // 22 is ssh's default and could be omitted; it is spelled out so a
            // copied entry has the flag to change rather than one to remember.
            ssh_args: vec!["-p".to_string(), "22".to_string()],
            deploy: "persist".to_string(),
            deploy_artifact: None,
        },
        jterm_core::jsh_remote::RemoteHostConfig {
            name: "myubuntu".to_string(),
            host: "myubuntu".to_string(),
            // The container user is `docker exec -u`; unset means the image's.
            user: None,
            docker: true,
            remote_shell: "jsh".to_string(),
            session: None,
            // Meaningless for docker, and the launcher ignores them.
            ssh_args: Vec::new(),
            deploy: "persist".to_string(),
            deploy_artifact: None,
        },
    ]
}

fn default_sidebar_view() -> crate::sidebar::SidebarView {
    crate::sidebar::SidebarView::Sessions
}

fn default_font_size() -> f32 {
    DEFAULT_FONT_SIZE
}

fn default_font_weight() -> f32 {
    1.0
}

fn default_font_sharpness() -> f32 {
    1.0
}

fn default_line_spacing() -> f32 {
    1.0
}

fn detect_fonts_by_query(extra_args: &[&str]) -> Vec<String> {
    let mut args = Vec::from(extra_args);
    args.push("family");
    if let Ok(output) = Command::new("fc-list").args(&args).output() {
        if let Ok(stdout) = String::from_utf8(output.stdout) {
            let mut seen = std::collections::HashSet::new();
            let mut families: Vec<String> = stdout
                .lines()
                .filter_map(|line| {
                    let family = line.split(',').next()?.trim();
                    if family.is_empty() {
                        return None;
                    }
                    if seen.insert(family.to_lowercase()) {
                        Some(family.to_string())
                    } else {
                        None
                    }
                })
                .collect();
            families.sort_by_key(|a| a.to_lowercase());
            return families;
        }
    }
    Vec::new()
}

fn detect_available_fonts() -> &'static Vec<String> {
    &AVAILABLE_FONTS
}

fn detect_monospace_fonts() -> &'static Vec<String> {
    &MONOSPACE_FONTS
}

fn default_font_family() -> String {
    // 快速路径：直接使用第一个候选字体，不检测系统字体
    // 这避免了启动时的 fc-list 调用，加快启动速度
    // 字体检测会在用户打开配置面板时延迟进行
    eprintln!(
        "[Config] Using default font (no scan): {}",
        NERD_FONT_CANDIDATES[0]
    );
    NERD_FONT_CANDIDATES[0].to_string()

    // 原有的检测逻辑已移除，避免启动时阻塞
    // 如需验证字体存在性，可在配置面板中按需检测
}

fn default_padding() -> f32 {
    2.0
}

fn default_scrollback_lines() -> usize {
    10000
}

fn default_initial_width() -> f32 {
    1200.0
}

fn default_initial_height() -> f32 {
    600.0
}

fn default_cols() -> usize {
    100
}

fn default_rows() -> usize {
    30
}

fn default_theme() -> String {
    "dark".to_string()
}

fn default_restore_session() -> bool {
    true
}

fn default_opacity() -> f32 {
    1.0
}

fn default_gpu_rendering() -> bool {
    true
}

fn default_scroll_speed() -> u32 {
    3
}

fn default_subpixel_rendering() -> bool {
    false
}

fn default_font_ligatures() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Config {
            jsh_update_check: default_jsh_update_check(),
            ai_enabled: false,
            ai_provider: default_ai_provider(),
            ai_base_url: default_ai_base_url(),
            ai_model: default_ai_model(),
            ai_max_tokens: default_ai_max_tokens(),
            ai_temperature: None,
            ai_redact_secrets: true,
            ai_api_key_file: None,
            agent_max_turns: default_agent_max_turns(),
            remote_hosts: default_remote_hosts(),
            font_size: default_font_size(),
            font_family: default_font_family(),
            font_weight: default_font_weight(),
            font_sharpness: default_font_sharpness(),
            font_backend: FontBackendType::default(),
            padding: default_padding(),
            line_spacing: default_line_spacing(),
            scrollbar_visibility: ScrollbarVisibility::default(),
            scrollback_lines: default_scrollback_lines(),
            initial_width: default_initial_width(),
            initial_height: default_initial_height(),
            cols: default_cols(),
            rows: default_rows(),
            theme: default_theme(),
            restore_session: default_restore_session(),
            session_history_file: None,
            opacity: default_opacity(),
            gpu_rendering: default_gpu_rendering(),
            app_renderer: AppRendererType::default(),
            scroll_speed: default_scroll_speed(),
            subpixel_rendering: default_subpixel_rendering(),
            font_ligatures: default_font_ligatures(),
            ui_scale: None,
            shell: None,
            tab_bar_position: TabBarPosition::default(),
            sidebar_view: default_sidebar_view(),
            osc52_clipboard_write: false,
            osc52_clipboard_read: false,
            paste_confirm: true,
            notify_long_blocks: true,
            notify_long_block_threshold_ms: default_notify_long_block_threshold_ms(),
            show_repo_strip: true,
            bottom_bar: default_bottom_bar(),
            click_moves_cursor: default_click_moves_cursor(),
            block_mode: default_block_mode(),
            load_error: None,
            revision: None,
        }
    }
}

/// Read a config file under [`MAX_CONFIG_BYTES`]. Separate from
/// [`Config::load`] only because `load` resolves the path from the environment
/// and cannot be pointed at a fixture.
#[cfg(test)]
pub(crate) fn read_config_file(path: &std::path::Path) -> std::io::Result<String> {
    crate::persistence_file::read_bounded(path, MAX_CONFIG_BYTES)
}

impl Config {
    pub fn load() -> Self {
        let config_path = match Self::config_path() {
            Ok(path) => path,
            Err(error) => {
                return Self {
                    load_error: Some(format!("cannot locate config: {error}")),
                    ..Self::default()
                };
            }
        };
        let revision = match crate::persistence_file::read_revision(&config_path, MAX_CONFIG_BYTES)
        {
            Ok(revision) => revision,
            Err(error) => {
                eprintln!(
                    "[Config] WARNING: failed to read {}: {}",
                    config_path.display(),
                    error
                );
                return Self {
                    load_error: Some(format!("{}: {error}", config_path.display())),
                    ..Self::default()
                };
            }
        };
        if revision == FileRevision::Missing {
            eprintln!("[Config] Using default configuration");
            let config = Self {
                revision: Some(revision),
                ..Self::default()
            };
            eprintln!("[Config] Font: {}", config.font_family);
            return config;
        }
        match Self::from_revision(&config_path, &revision) {
            Ok(config) => {
                eprintln!("[Config] Loaded from {}", config_path.display());
                eprintln!("[Config] Font: {}", config.font_family);
                config
            }
            Err(error) => {
                eprintln!("[Config] WARNING: {error}");
                eprintln!(
                    "[Config] WARNING: your settings are ignored, using defaults. \
                     Fix the file above to apply them."
                );
                let config = Self {
                    load_error: Some(error),
                    revision: Some(revision),
                    ..Self::default()
                };
                eprintln!("[Config] Using default configuration");
                eprintln!("[Config] Font: {}", config.font_family);
                config
            }
        }
    }

    pub(crate) fn from_revision(
        path: &std::path::Path,
        revision: &FileRevision,
    ) -> Result<Self, String> {
        let bytes = revision
            .bytes()
            .ok_or_else(|| format!("cannot parse {}: file does not exist", path.display()))?;
        let content = std::str::from_utf8(bytes)
            .map_err(|error| format!("cannot read {} as UTF-8: {error}", path.display()))?;
        let mut config = toml::from_str::<Config>(content)
            .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
        for warning in config.normalize() {
            eprintln!("[Config] WARNING: {warning}");
        }
        config.revision = Some(revision.clone());
        config.load_error = None;
        Ok(config)
    }

    #[allow(dead_code)] // used by the binary's hot-reload module, not the lib target
    pub(crate) fn current_revision() -> Result<FileRevision, String> {
        let path = Self::config_path().map_err(|error| error.to_string())?;
        crate::persistence_file::read_revision(&path, MAX_CONFIG_BYTES)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))
    }

    #[allow(dead_code)] // used by the binary's hot-reload module, not the lib target
    pub(crate) fn observed_revision(&self) -> Option<&FileRevision> {
        self.revision.as_ref()
    }

    pub fn save(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let config_path = Self::config_path()?;
        self.save_path(&config_path)
    }

    fn save_path(
        &mut self,
        config_path: &std::path::Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // 拒写保护:`self` 此刻是内建默认值,一次字号缩放或退出时的自动保存
        // 就会把无法解析的用户配置整体覆盖掉。见 `load_error` 的说明。
        if let Some(error) = &self.load_error {
            return Err(format!(
                "refusing to overwrite the unparsable config ({error}); \
                 fix the file or reset to defaults"
            )
            .into());
        }
        // Persist only values that the runtime can safely consume. This also
        // protects callers outside the settings panel (or future migrations)
        // from writing NaN/zero dimensions that make the next startup fail.
        let mut normalized = self.clone();
        for warning in normalized.normalize() {
            eprintln!("[Config] WARNING while saving: {}", warning);
        }
        let content = toml::to_string_pretty(&normalized)?;
        if content.len() as u64 > MAX_CONFIG_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                format!(
                    "serialized config is {} bytes; limit is {MAX_CONFIG_BYTES}",
                    content.len()
                ),
            )
            .into());
        }
        let intended = FileRevision::from_bytes(content.as_bytes());
        let write_result = match self.revision.as_ref() {
            Some(expected) => crate::persistence_file::write_atomic_if_unchanged(
                config_path,
                content.as_bytes(),
                expected,
                MAX_CONFIG_BYTES,
            ),
            None => crate::persistence_file::write_atomic(config_path, content.as_bytes())
                .map(|()| intended.clone()),
        };
        match write_result {
            Ok(revision) => {
                normalized.revision = Some(revision);
                normalized.load_error = None;
                *self = normalized;
            }
            Err(error) => {
                // A rename is visible before the final directory fsync. If
                // that boundary alone failed, adopt the exact published bytes
                // in memory while still reporting the durability error.
                let current =
                    crate::persistence_file::read_revision(config_path, MAX_CONFIG_BYTES).ok();
                if current.as_ref() == Some(&intended) {
                    normalized.revision = Some(intended);
                    normalized.load_error = None;
                    *self = normalized;
                } else if error.kind() == std::io::ErrorKind::AlreadyExists {
                    self.revision = current;
                    self.load_error = Some(format!("{}: {error}", config_path.display()));
                }
                return Err(error.into());
            }
        }
        eprintln!("[Config] Saved to {}", config_path.display());
        Ok(())
    }

    pub fn session_history_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir().ok_or("Failed to determine config directory")?;
        Ok(config_dir.join("ember").join("session_history.json"))
    }

    /// Resolve the session snapshot path, honoring the documented per-user
    /// override instead of always writing to the default config directory.
    pub fn resolved_session_history_path(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
        match &self.session_history_file {
            Some(path) if !path.as_os_str().is_empty() => Ok(path.clone()),
            _ => Self::session_history_path(),
        }
    }

    pub fn ui_history_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir().ok_or("Failed to determine config directory")?;
        Ok(config_dir.join("ember").join("ui_history.json"))
    }

    pub fn config_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir().ok_or("Failed to determine config directory")?;
        Ok(config_dir.join("ember").join("config.toml"))
    }

    pub fn get_font_family(&self) -> &str {
        &self.font_family
    }

    // 配置值约束方法
    pub fn clamp_font_size(size: f32) -> f32 {
        size.clamp(8.0, 72.0)
    }

    /// Normalize values loaded from hand-edited TOML before any renderer,
    /// window or terminal allocation sees them. Returns human-readable notes
    /// for diagnostics; valid configurations produce an empty list.
    pub fn normalize(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();

        fn finite_or(value: f32, fallback: f32) -> f32 {
            if value.is_finite() {
                value
            } else {
                fallback
            }
        }

        macro_rules! normalize_f32 {
            ($field:ident, $min:expr, $max:expr, $fallback:expr) => {{
                let old = self.$field;
                self.$field = finite_or(old, $fallback).clamp($min, $max);
                if self.$field != old {
                    warnings.push(format!(
                        "{}={} is outside the supported range; using {}",
                        stringify!($field),
                        old,
                        self.$field
                    ));
                }
            }};
        }

        normalize_f32!(font_size, 8.0, 72.0, default_font_size());
        normalize_f32!(font_weight, 0.5, 2.0, default_font_weight());
        normalize_f32!(font_sharpness, 0.5, 2.0, default_font_sharpness());
        normalize_f32!(padding, 0.0, 20.0, default_padding());
        normalize_f32!(line_spacing, 0.8, 3.0, default_line_spacing());
        normalize_f32!(initial_width, 320.0, 16_384.0, default_initial_width());
        normalize_f32!(initial_height, 200.0, 16_384.0, default_initial_height());
        normalize_f32!(opacity, 0.05, 1.0, default_opacity());

        let old_cols = self.cols;
        let old_rows = self.rows;
        (self.cols, self.rows) = crate::terminal::clamp_terminal_dimensions(self.cols, self.rows);
        if (self.cols, self.rows) != (old_cols, old_rows) {
            warnings.push(format!(
                "terminal dimensions {}x{} are unsupported; using {}x{}",
                old_cols, old_rows, self.cols, self.rows
            ));
        }

        let old_scrollback = self.scrollback_lines;
        self.scrollback_lines = self.scrollback_lines.clamp(100, 1_000_000);
        if self.scrollback_lines != old_scrollback {
            warnings.push(format!(
                "scrollback_lines={} is outside 100..=1000000; using {}",
                old_scrollback, self.scrollback_lines
            ));
        }

        let old_scroll_speed = self.scroll_speed;
        self.scroll_speed = self.scroll_speed.clamp(1, 50);
        if self.scroll_speed != old_scroll_speed {
            warnings.push(format!(
                "scroll_speed={} is outside 1..=50; using {}",
                old_scroll_speed, self.scroll_speed
            ));
        }

        if self.font_family.trim().is_empty() {
            self.font_family = default_font_family();
            warnings.push("font_family is empty; using the default font".to_string());
        }
        if self.theme.trim().is_empty() {
            self.theme = default_theme();
            warnings.push("theme is empty; using the default theme".to_string());
        }
        if self
            .shell
            .as_ref()
            .is_some_and(|shell| shell.trim().is_empty())
        {
            self.shell = None;
            warnings.push("shell is empty; using automatic shell detection".to_string());
        }
        if let Some(scale) = self.ui_scale {
            let normalized = finite_or(scale, 1.0).clamp(0.5, 3.0);
            if normalized != scale {
                warnings.push(format!(
                    "ui_scale={} is outside 0.5..=3; using {}",
                    scale, normalized
                ));
            }
            self.ui_scale = Some(normalized);
        }

        let old_ai_max_tokens = self.ai_max_tokens;
        self.ai_max_tokens = self.ai_max_tokens.clamp(64, 32_768);
        if self.ai_max_tokens != old_ai_max_tokens {
            warnings.push(format!(
                "ai_max_tokens={old_ai_max_tokens} is outside 64..=32768; using {}",
                self.ai_max_tokens
            ));
        }
        if self.ai_temperature.is_some_and(|temperature| {
            !temperature.is_finite() || !(0.0..=2.0).contains(&temperature)
        }) {
            self.ai_temperature = None;
            warnings.push("ai_temperature is invalid; using the provider default".to_string());
        }
        let old_agent_max_turns = self.agent_max_turns;
        self.agent_max_turns = self.agent_max_turns.clamp(1, 100);
        if self.agent_max_turns != old_agent_max_turns {
            warnings.push(format!(
                "agent_max_turns={old_agent_max_turns} is outside 1..=100; using {}",
                self.agent_max_turns
            ));
        }
        if normalize_required_text(
            &mut self.ai_provider,
            MAX_CONFIG_NAME_BYTES,
            default_ai_provider,
        ) {
            warnings.push(
                "ai_provider is empty, oversized, or contains controls; using default".into(),
            );
        }
        if normalize_required_text(
            &mut self.ai_base_url,
            MAX_CONFIG_VALUE_BYTES,
            default_ai_base_url,
        ) {
            warnings.push(
                "ai_base_url is empty, oversized, or contains controls; using default".into(),
            );
        }
        if normalize_required_text(&mut self.ai_model, MAX_CONFIG_NAME_BYTES, default_ai_model) {
            warnings
                .push("ai_model is empty, oversized, or contains controls; using default".into());
        }
        if normalize_optional_text(&mut self.ai_api_key_file, MAX_CONFIG_VALUE_BYTES) {
            warnings.push(
                "ai_api_key_file is empty, oversized, or contains controls; ignoring it".into(),
            );
        }
        if normalize_required_text(
            &mut self.font_family,
            MAX_CONFIG_NAME_BYTES,
            default_font_family,
        ) {
            warnings.push(
                "font_family is empty, oversized, or contains controls; using default".into(),
            );
        }
        if normalize_required_text(&mut self.theme, MAX_CONFIG_NAME_BYTES, default_theme) {
            warnings.push("theme is empty, oversized, or contains controls; using default".into());
        }
        if normalize_optional_text(&mut self.shell, MAX_CONFIG_VALUE_BYTES) {
            warnings.push(
                "shell is empty, oversized, or contains controls; using automatic detection".into(),
            );
        }
        if self
            .session_history_file
            .as_ref()
            .is_some_and(|path| !valid_config_path(path))
        {
            self.session_history_file = None;
            warnings.push(
                "session_history_file is empty, oversized, or contains controls; using default"
                    .into(),
            );
        }
        let normalized_update_check =
            jterm_core::jsh_install::UpdateCheck::parse(&self.jsh_update_check)
                .as_str()
                .to_string();
        if normalized_update_check != self.jsh_update_check {
            self.jsh_update_check = normalized_update_check;
            warnings.push("jsh_update_check was normalized to a supported value".into());
        }

        warnings
    }

    pub fn get_monospace_fonts() -> &'static Vec<String> {
        detect_monospace_fonts()
    }

    pub fn get_all_fonts() -> &'static Vec<String> {
        detect_available_fonts()
    }
}

fn valid_config_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn normalize_required_text(value: &mut String, max_bytes: usize, fallback: fn() -> String) -> bool {
    let normalized = value.trim().to_string();
    if valid_config_text(&normalized, max_bytes) {
        let changed = *value != normalized;
        *value = normalized;
        changed
    } else {
        *value = fallback();
        true
    }
}

fn normalize_optional_text(value: &mut Option<String>, max_bytes: usize) -> bool {
    let original = value.clone();
    *value = value
        .take()
        .map(|value| value.trim().to_string())
        .filter(|value| valid_config_text(value, max_bytes));
    *value != original
}

fn valid_config_path(path: &std::path::Path) -> bool {
    let value = path.to_string_lossy();
    !value.is_empty()
        && value.len() <= MAX_CONFIG_VALUE_BYTES
        && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_private(path: &std::path::Path, contents: impl AsRef<[u8]>) {
        std::fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn click_moves_cursor_defaults_on_and_can_be_disabled() {
        let config: Config = toml::from_str("").expect("empty config parses");
        assert!(config.click_moves_cursor);

        let config: Config =
            toml::from_str("click_moves_cursor = false\n").expect("override parses");
        assert!(!config.click_moves_cursor);
    }

    #[test]
    fn block_mode_defaults_on_and_can_be_disabled() {
        let config: Config = toml::from_str("").expect("empty config parses");
        assert!(config.block_mode);

        let config: Config = toml::from_str("block_mode = false\n").expect("override parses");
        assert!(!config.block_mode);
    }

    #[test]
    fn normalize_repairs_unsafe_hand_edited_values() {
        let mut config = Config {
            font_size: f32::NAN,
            font_weight: 99.0,
            padding: -5.0,
            line_spacing: 0.0,
            opacity: 2.0,
            cols: 0,
            rows: usize::MAX,
            scrollback_lines: 0,
            scroll_speed: 0,
            ui_scale: Some(f32::INFINITY),
            shell: Some("   ".to_string()),
            ai_max_tokens: u32::MAX,
            ai_temperature: Some(f32::INFINITY),
            agent_max_turns: u32::MAX,
            ..Config::default()
        };

        let warnings = config.normalize();

        assert!(!warnings.is_empty());
        assert_eq!(config.font_size, default_font_size());
        assert_eq!(config.font_weight, 2.0);
        assert_eq!(config.padding, 0.0);
        assert_eq!(config.line_spacing, 0.8);
        assert_eq!(config.opacity, 1.0);
        assert!(config.cols > 0);
        assert!(config.rows < usize::MAX);
        assert_eq!(config.scrollback_lines, 100);
        assert_eq!(config.scroll_speed, 1);
        assert_eq!(config.ui_scale, Some(1.0));
        assert_eq!(config.shell, None);
        assert_eq!(config.ai_max_tokens, 32_768);
        assert_eq!(config.ai_temperature, None);
        assert_eq!(config.agent_max_turns, 100);
    }

    #[test]
    fn normalize_drops_oversized_or_control_bearing_config_strings() {
        let mut config = Config {
            ai_provider: "p".repeat(MAX_CONFIG_NAME_BYTES + 1),
            ai_base_url: format!(
                "https://example.test/{}",
                "x".repeat(MAX_CONFIG_VALUE_BYTES)
            ),
            ai_model: "model\nspoof".to_string(),
            ai_api_key_file: Some("/tmp/key\0suffix".to_string()),
            font_family: "f".repeat(MAX_CONFIG_NAME_BYTES + 1),
            theme: "bad\ntheme".to_string(),
            shell: Some("/bin/sh\0--arg".to_string()),
            session_history_file: Some(PathBuf::from("x".repeat(MAX_CONFIG_VALUE_BYTES + 1))),
            jsh_update_check: "unexpected".to_string(),
            ..Config::default()
        };

        let warnings = config.normalize();

        assert!(!warnings.is_empty());
        assert_eq!(config.ai_provider, default_ai_provider());
        assert_eq!(config.ai_base_url, default_ai_base_url());
        assert_eq!(config.ai_model, default_ai_model());
        assert_eq!(config.ai_api_key_file, None);
        assert_eq!(config.font_family, default_font_family());
        assert_eq!(config.theme, default_theme());
        assert_eq!(config.shell, None);
        assert_eq!(config.session_history_file, None);
        assert_eq!(config.jsh_update_check, "daily");
    }

    #[test]
    fn resolved_session_history_path_honors_override() {
        let config = Config {
            session_history_file: Some(PathBuf::from("/tmp/ember-sessions.json")),
            ..Config::default()
        };

        assert_eq!(
            config.resolved_session_history_path().unwrap(),
            PathBuf::from("/tmp/ember-sessions.json")
        );
    }

    #[test]
    fn config_that_failed_to_load_refuses_to_be_overwritten() {
        let mut broken = Config {
            load_error: Some("/home/u/.config/ember/config.toml: expected `=`".to_string()),
            ..Config::default()
        };

        // Must fail before touching the filesystem: the real user file is the
        // thing being protected.
        let error = broken
            .save()
            .expect_err("defaults must never be flushed over an unparsable config");
        let message = error.to_string();
        assert!(message.contains("refusing to overwrite"), "{message}");
        assert!(message.contains("expected `=`"), "{message}");

        // Both recovery paths hand over a struct that never failed to load:
        // "Reset to defaults" builds one, a successful hot reload parses one.
        assert!(Config::default().load_error.is_none());
        let repaired: Config = toml::from_str("font_size = 15.0").expect("valid config");
        assert!(repaired.load_error.is_none());
    }

    #[test]
    fn exact_revisions_allow_only_one_concurrent_config_generation() {
        let root = std::env::temp_dir().join(format!(
            "ember-config-cas-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = root.join("config.toml");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut writers = Vec::new();
        for font_size in [17.0, 29.0] {
            let path = path.clone();
            let barrier = barrier.clone();
            writers.push(std::thread::spawn(move || {
                let mut config = Config {
                    font_size,
                    revision: Some(FileRevision::Missing),
                    ..Config::default()
                };
                barrier.wait();
                (
                    font_size,
                    config.save_path(&path).map_err(|error| error.to_string()),
                )
            }));
        }
        barrier.wait();
        let outcomes: Vec<_> = writers
            .into_iter()
            .map(|writer| writer.join().unwrap())
            .collect();

        assert_eq!(
            outcomes.iter().filter(|(_, result)| result.is_ok()).count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|(_, result)| result.is_err())
                .count(),
            1
        );
        let winner = outcomes
            .iter()
            .find_map(|(font_size, result)| result.as_ref().ok().map(|()| *font_size))
            .unwrap();
        let revision = crate::persistence_file::read_revision(&path, MAX_CONFIG_BYTES).unwrap();
        assert_eq!(
            Config::from_revision(&path, &revision).unwrap().font_size,
            winner
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// An oversized config is a read error, which sets `load_error` and so
    /// makes `save` refuse: the user's file survives instead of being replaced
    /// by defaults on the next font zoom.
    #[test]
    fn an_oversized_config_is_rejected_rather_than_parsed() {
        let path = std::env::temp_dir().join(format!(
            "ember-config-oversized-{}.toml",
            std::process::id()
        ));
        write_private(&path, vec![b' '; MAX_CONFIG_BYTES as usize + 1]);

        let error = read_config_file(&path).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::FileTooLarge);

        write_private(&path, b"font_size = 15.0");
        assert_eq!(read_config_file(&path).unwrap(), "font_size = 15.0");
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn config_reader_never_follows_a_symlink() {
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("ember-config-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root).unwrap();
        let target = root.join("target.toml");
        let link = root.join("config.toml");
        write_private(&target, b"font_size = 15.0");
        symlink(&target, &link).unwrap();

        assert!(read_config_file(&link).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"font_size = 15.0");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn load_error_never_round_trips_through_the_config_file() {
        let broken = Config {
            load_error: Some("boom".to_string()),
            ..Config::default()
        };

        let serialized = toml::to_string_pretty(&broken).expect("config serializes");
        assert!(!serialized.contains("load_error"), "{serialized}");

        // 用户文件里若残留过这个键(或任何未知键),也不能让加载整体失败。
        let parsed: Config = toml::from_str("load_error = \"stale\"").expect("config parses");
        assert!(parsed.load_error.is_none());
    }

    #[test]
    fn remote_hosts_deserialize_through_the_shared_family_type() {
        let config: Config = toml::from_str(
            r#"
[[remote_hosts]]
name = "build"
host = "myubuntu"
docker = true
deploy = "incognito"

[[remote_hosts]]
host = "dev.example.com"
user = "yj"
ssh_args = ["-p", "2222"]
"#,
        )
        .expect("parse");
        assert_eq!(config.remote_hosts.len(), 2);
        assert!(config.remote_hosts.iter().all(|h| h.validate().is_ok()));
        assert_eq!(config.remote_hosts[0].display_name(), "build");
        // No key at all means the worked examples; an explicit empty list is
        // taken as written, so a host deleted in the panel stays deleted.
        let config: Config = toml::from_str("").expect("parse empty");
        assert_eq!(config.remote_hosts, default_remote_hosts());
        let config: Config = toml::from_str("remote_hosts = []").expect("parse empty list");
        assert!(config.remote_hosts.is_empty());
    }

    /// The defaults are what a user copies, so they have to be spelled the way
    /// the family type accepts: the port as an `ssh_args` flag and the login in
    /// `user`, never folded into `host` as `root@10.68.18.60:22`.
    #[test]
    fn default_remote_hosts_are_valid_and_correctly_shaped() {
        let hosts = default_remote_hosts();
        let names: Vec<&str> = hosts.iter().map(|h| h.display_name()).collect();
        assert_eq!(names, ["dev-60", "myubuntu"]);
        for host in &hosts {
            assert!(host.validate().is_ok(), "{:?}", host.validate());
        }
        assert_eq!(hosts[0].host, "10.68.18.60");
        assert_eq!(hosts[0].user.as_deref(), Some("root"));
        assert_eq!(hosts[0].ssh_args, ["-p", "22"]);
        assert!(!hosts[0].docker);
        assert!(hosts[1].docker);
    }
}
