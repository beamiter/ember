use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

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

    /// 会话标签栏位置:顶部水平栏(默认)或集成进左侧侧边栏
    #[serde(default)]
    pub tab_bar_position: TabBarPosition,

    /// 侧边栏 tab 模式下记住的视图(会话/文件)。默认显示会话,用户切换后会被持久化。
    #[serde(default = "default_sidebar_view")]
    pub sidebar_view: crate::sidebar::SidebarView,

    /// 允许程序通过 OSC 52 写入系统剪贴板(默认允许)。
    #[serde(default = "default_true")]
    pub osc52_clipboard_write: bool,

    /// 允许程序通过 OSC 52 读取系统剪贴板(默认禁止)。
    /// 读取会把剪贴板内容回传给终端内运行的程序,存在隐私/安全风险,故默认关闭。
    #[serde(default)]
    pub osc52_clipboard_read: bool,

    /// 粘贴多行/大块内容时弹窗确认(默认开启)。用户可在确认对话框里选择不再询问。
    #[serde(default = "default_true")]
    pub paste_confirm: bool,
}

fn default_true() -> bool {
    true
}

fn default_sidebar_view() -> crate::sidebar::SidebarView {
    crate::sidebar::SidebarView::Sessions
}

fn default_font_size() -> f32 {
    14.0
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
            osc52_clipboard_write: true,
            osc52_clipboard_read: false,
            paste_confirm: true,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        if let Ok(config_path) = Self::config_path() {
            if config_path.exists() {
                match std::fs::read_to_string(&config_path) {
                    Ok(content) => match toml::from_str::<Config>(&content) {
                        Ok(mut config) => {
                            for warning in config.normalize() {
                                eprintln!("[Config] WARNING: {}", warning);
                            }
                            eprintln!("[Config] Loaded from {}", config_path.display());
                            eprintln!("[Config] Font: {}", config.font_family);
                            return config;
                        }
                        Err(e) => {
                            // 显示具体的解析错误(含行列/原因),便于用户修正配置;
                            // 否则会静默回退到默认值,用户的设置被忽略却毫不知情。
                            eprintln!(
                                "[Config] WARNING: failed to parse {}: {}",
                                config_path.display(),
                                e
                            );
                            eprintln!(
                                "[Config] WARNING: your settings are ignored, using defaults. \
                                 Fix the file above to apply them."
                            );
                        }
                    },
                    Err(e) => {
                        eprintln!(
                            "[Config] WARNING: failed to read {}: {}",
                            config_path.display(),
                            e
                        );
                    }
                }
            }
        }
        eprintln!("[Config] Using default configuration");
        let config = Self::default();
        eprintln!("[Config] Font: {}", config.font_family);
        config
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let config_path = Self::config_path()?;
        // Persist only values that the runtime can safely consume. This also
        // protects callers outside the settings panel (or future migrations)
        // from writing NaN/zero dimensions that make the next startup fail.
        let mut normalized = self.clone();
        for warning in normalized.normalize() {
            eprintln!("[Config] WARNING while saving: {}", warning);
        }
        let content = toml::to_string_pretty(&normalized)?;
        crate::atomic_file::write_atomic(&config_path, content.as_bytes())?;
        eprintln!("[Config] Saved to {}", config_path.display());
        Ok(())
    }

    pub fn session_history_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir().ok_or("Failed to determine config directory")?;
        Ok(config_dir.join("jterm2").join("session_history.json"))
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
        Ok(config_dir.join("jterm2").join("ui_history.json"))
    }

    pub fn config_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir().ok_or("Failed to determine config directory")?;
        Ok(config_dir.join("jterm2").join("config.toml"))
    }

    pub fn config_mtime() -> Option<std::time::SystemTime> {
        Self::config_path()
            .ok()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
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

        warnings
    }

    pub fn get_monospace_fonts() -> &'static Vec<String> {
        detect_monospace_fonts()
    }

    pub fn get_all_fonts() -> &'static Vec<String> {
        detect_available_fonts()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn resolved_session_history_path_honors_override() {
        let config = Config {
            session_history_file: Some(PathBuf::from("/tmp/jterm2-sessions.json")),
            ..Config::default()
        };

        assert_eq!(
            config.resolved_session_history_path().unwrap(),
            PathBuf::from("/tmp/jterm2-sessions.json")
        );
    }
}
