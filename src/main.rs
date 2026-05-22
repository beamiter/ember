mod app;
mod char_width;
mod clipboard;
mod color;
mod command_palette;
mod config;
mod config_panel;
mod debug;
mod debug_panel;
mod gpu;
mod help;
#[allow(dead_code)]
mod keybindings;
mod kitty_graphics;
#[allow(dead_code)]
mod layout;
mod link;
mod pty;
#[allow(dead_code)]
mod scripting;
mod search;
#[allow(dead_code)]
mod search_replace;
#[allow(dead_code)]
mod session;
mod session_manager;
mod session_persistence;
mod shell;
#[allow(dead_code)]
mod sidebar;
mod terminal;
mod theme;
mod ui;
#[allow(dead_code)]
mod windows_compat;

use app::events::{build_keybinding_string, normalize_terminal_shortcut_events,
                  should_restore_terminal_shortcut_event};
use base64::Engine;
use clipboard::{ClipboardContent, ClipboardManager};
use eframe::egui;
use parking_lot::Mutex as ParkingMutex;
use session::Session;
use session_manager::SessionManager;
use shell::{ShellEvent, ShellSession};
use smallvec::SmallVec;
use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use terminal::{clamp_terminal_dimensions, TerminalState};
use ui::TerminalRenderer;

// 全局标志，用于信号处理
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// 设置信号处理器，确保收到SIGINT/SIGTERM时能正常退出
/// 这允许Drop逻辑执行，从而清理所有rsh子进程
#[cfg(unix)]
fn setup_signal_handlers() {
    extern "C" fn handle_signal(_: libc::c_int) {
        SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
        // 注意：在信号处理器中只能做最少的工作
        // 主程序会检查SHUTDOWN_REQUESTED并正常退出
    }

    // 注册SIGINT (Ctrl+C) 和 SIGTERM (kill) 处理器
    // SAFETY: signal 注册信号处理器。handle_signal 是 extern "C" 函数，
    // 符合 sighandler_t 的签名要求。处理器只做最小工作（设置原子标志），是信号安全的。
    unsafe {
        libc::signal(libc::SIGINT, handle_signal as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handle_signal as libc::sighandler_t);
    }
}

#[cfg(not(unix))]
fn setup_signal_handlers() {
    // Windows平台暂不支持
}

fn detect_image_mime_type(data: &[u8]) -> Option<&'static str> {
    if data.len() < 4 {
        crate::debug_log!("[MIME] data too short: {} bytes", data.len());
        return None;
    }

    // PNG: 89 50 4E 47
    if data.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        crate::debug_log!("[MIME] detected PNG");
        return Some("image/png");
    }

    // JPEG: FF D8
    if data.len() >= 2 && data[0] == 0xFF && data[1] == 0xD8 {
        crate::debug_log!("[MIME] detected JPEG");
        return Some("image/jpeg");
    }

    // GIF: 47 49 46 (GIF)
    if data.len() >= 3 && &data[0..3] == b"GIF" {
        crate::debug_log!("[MIME] detected GIF");
        return Some("image/gif");
    }

    // WebP: RIFF...WEBP
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        crate::debug_log!("[MIME] detected WebP");
        return Some("image/webp");
    }

    // BMP: 42 4D (BM)
    if data.len() >= 2 && data[0] == 0x42 && data[1] == 0x4D {
        crate::debug_log!("[MIME] detected BMP");
        return Some("image/bmp");
    }

    // 未识别的格式，显示前几个字节
    let hex_preview = if data.len() >= 8 {
        format!(
            "{:02X} {:02X} {:02X} {:02X} ...",
            data[0], data[1], data[2], data[3]
        )
    } else {
        data.iter()
                .map(|b| format!("{:02X}", b))
                .collect::<Vec<_>>()
                .join(" ").to_string()
    };
    crate::debug_log!(
        "[MIME] unknown format ({}bytes): {}",
        data.len(),
        hex_preview
    );
    None
}

fn register_font_family(
    fonts: &mut egui::FontDefinitions,
    family: egui::FontFamily,
    font_name: &str,
    prepend: bool,
) {
    if let Some(entries) = fonts.families.get_mut(&family) {
        if entries.iter().any(|entry| entry == font_name) {
            return;
        }

        if prepend {
            entries.insert(0, font_name.to_owned());
        } else {
            entries.push(font_name.to_owned());
        }
    }
}

fn load_font_from_path(
    fonts: &mut egui::FontDefinitions,
    loaded_paths: &mut HashMap<String, String>,
    path: &str,
    font_name: &str,
    families: &[egui::FontFamily],
    prepend: bool,
) -> bool {
    let registered_name = if let Some(existing_name) = loaded_paths.get(path) {
        existing_name.clone()
    } else {
        let Ok(font_data) = std::fs::read(path) else {
            return false;
        };

        fonts.font_data.insert(
            font_name.to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(font_data)),
        );
        loaded_paths.insert(path.to_owned(), font_name.to_owned());
        font_name.to_owned()
    };

    for family in families {
        register_font_family(fonts, family.clone(), &registered_name, prepend);
    }

    eprintln!("[Fonts] Loaded {} from {}", registered_name, path);
    true
}

#[cfg(target_os = "linux")]
fn fontconfig_match_file(family: &str) -> Option<String> {
    let output = Command::new("fc-match")
        .args(["-f", "%{file}\n", family])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .map(str::trim)
        .find(|path| !path.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(target_os = "linux")]
fn fontconfig_match_bold_file(family: &str) -> Option<String> {
    let query = format!("{}:style=Bold", family);
    let output = Command::new("fc-match")
        .args(["-f", "%{file}\n", &query])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let path = String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .map(str::trim)
        .find(|path| !path.is_empty())
        .map(ToOwned::to_owned)?;

    // Verify it's actually a bold variant (not the same as regular)
    if path.to_lowercase().contains("bold") {
        Some(path)
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
fn fontconfig_match_file(_family: &str) -> Option<String> {
    None
}

#[cfg(not(target_os = "linux"))]
fn fontconfig_match_bold_file(_family: &str) -> Option<String> {
    None
}

fn create_font_backend(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    cfg: &config::Config,
    font_bytes: &[u8],
    bold_font_data: Option<&[u8]>,
    fallback_font_data: &[Vec<u8>],
    font_size_px: f32,
) -> Box<dyn gpu::font_backend::FontBackend> {
    match cfg.font_backend {
        config::FontBackendType::Fontdue => {
            Box::new(gpu::fontdue_backend::FontdueAtlas::new(
                device,
                queue,
                font_bytes,
                bold_font_data,
                fallback_font_data,
                font_size_px,
                cfg.font_weight,
            ))
        }
        config::FontBackendType::AbGlyph => {
            Box::new(gpu::ab_glyph_backend::AbGlyphAtlas::new(
                device,
                queue,
                font_bytes,
                bold_font_data,
                fallback_font_data,
                font_size_px,
                cfg.font_weight,
            ))
        }
    }
}

fn load_first_matching_font(
    fonts: &mut egui::FontDefinitions,
    loaded_paths: &mut HashMap<String, String>,
    family_candidates: &[&str],
    path_candidates: &[&str],
    font_name: &str,
    families: &[egui::FontFamily],
    prepend: bool,
) -> bool {
    let mut seen_paths = HashSet::new();
    let mut resolved_paths = Vec::new();

    for family in family_candidates {
        if let Some(path) = fontconfig_match_file(family) {
            if seen_paths.insert(path.clone()) {
                resolved_paths.push(path);
            }
        }
    }

    for path in path_candidates {
        let path = (*path).to_owned();
        if seen_paths.insert(path.clone()) {
            resolved_paths.push(path);
        }
    }

    for path in resolved_paths {
        if load_font_from_path(fonts, loaded_paths, &path, font_name, families, prepend) {
            return true;
        }
    }

    false
}

/// 从 PNG 数据中提取宽度和高度
fn extract_png_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 24 {
        return None;
    }

    // PNG 宽度在偏移 16-19，高度在 20-23
    let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);

    crate::debug_log!("[KITTY] PNG dimensions: {}x{}", width, height);
    Some((width, height))
}

/// 生成 Kitty 图像协议数据包
fn kitty_graphics_payload(mime_type: &str, data: &[u8]) -> Vec<u8> {
    crate::debug_log!(
        "[KITTY] generating payload: mime_type={}, data_size={}",
        mime_type,
        data.len()
    );

    let encoded = base64::engine::general_purpose::STANDARD_NO_PAD.encode(data);
    crate::debug_log!(
        "[KITTY] encoded data size (base64): {} bytes",
        encoded.len()
    );

    let mut output = Vec::new();

    // 获取尺寸（如果是 PNG）
    let (width, height) = if mime_type == "image/png" {
        extract_png_dimensions(data).unwrap_or((0, 0))
    } else {
        (0, 0)
    };

    // Kitty 图像协议：ESC _ G id=1,s=WIDTH,v=HEIGHT,mime=image/png;BASE64_DATA ESC \
    output.extend_from_slice(b"\x1b_G");

    if width > 0 && height > 0 {
        output.extend_from_slice(format!("s={},v={},", width, height).as_bytes());
    }

    output.extend_from_slice(b"m=1,"); // m=1: more data coming (or action)

    // 添加 mime 类型（可选，但有助于解析）
    let mime_encoded =
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(mime_type.as_bytes());
    output.extend_from_slice(format!("m={};", mime_encoded).as_bytes());

    // 添加 base64 编码的数据
    output.extend_from_slice(encoded.as_bytes());

    // 结束符
    output.extend_from_slice(b"\x1b\\");

    crate::debug_log!("[KITTY] final packet size: {} bytes", output.len());
    output
}

fn configure_fonts_and_gpu(
    ctx: &egui::Context,
    wgpu_render_state: Option<&egui_wgpu::RenderState>,
    cfg: &config::Config,
) {
    let mut fonts = egui::FontDefinitions::default();
    let mut loaded_font_paths = HashMap::new();

    let configured_mono_family = cfg.get_font_family();
    let mono_loaded = load_first_matching_font(
        &mut fonts,
        &mut loaded_font_paths,
        &[
            configured_mono_family,
            "DejaVu Sans Mono",
            "Liberation Mono",
            "Noto Sans Mono",
            "Noto Mono",
        ],
        &[
            "/usr/share/fonts/opentype/noto/NotoMono-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/opentype/dejavu/DejaVuSansMono.otf",
            "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
            "/usr/share/fonts/liberation-mono-fonts/LiberationMono-Regular.ttf",
        ],
        "monospace_unicode",
        &[egui::FontFamily::Monospace],
        true,
    );

    if !mono_loaded {
        eprintln!(
            "[Fonts] Warning: no monospace font file could be loaded for {}",
            configured_mono_family
        );
    }

    let cjk_loaded = load_first_matching_font(
        &mut fonts,
        &mut loaded_font_paths,
        &[
            "Noto Sans CJK SC",
            "Noto Sans CJK",
            "Source Han Sans SC",
            "WenQuanYi Zen Hei",
            "AR PL UMing CN",
        ],
        &[
            "/usr/share/fonts/google-noto-sans-cjk-fonts/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-sans-cjk-vf-fonts/NotoSansCJK-VF.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJKsc-Regular.otf",
            "/usr/share/fonts/wenquanyi/wqy-zenhei.ttc",
        ],
        "cjk",
        &[egui::FontFamily::Monospace, egui::FontFamily::Proportional],
        false,
    );

    if !cjk_loaded {
        eprintln!("[Fonts] Warning: no CJK fallback font file could be loaded");
    }

    let mono_font_data: Option<Vec<u8>> = fonts
        .font_data
        .get("monospace_unicode")
        .map(|fd| fd.font.to_vec());
    let fallback_font_data: Vec<Vec<u8>> = fonts
        .font_data
        .get("cjk")
        .map(|fd| vec![fd.font.to_vec()])
        .unwrap_or_default();

    // Try to load bold variant of the configured monospace font
    let bold_font_data: Option<Vec<u8>> = {
        let family = cfg.get_font_family();
        let bold_candidates = [
            family,
            "DejaVu Sans Mono",
            "Liberation Mono",
            "Noto Sans Mono",
            "Noto Mono",
        ];
        let mut data = None;
        for candidate in &bold_candidates {
            if let Some(path) = fontconfig_match_bold_file(candidate) {
                if let Ok(bytes) = std::fs::read(&path) {
                    eprintln!("[Fonts] Loaded bold font: {}", path);
                    data = Some(bytes);
                    break;
                }
            }
        }
        if data.is_none() {
            eprintln!(
                "[Fonts] Warning: no bold font variant found, bold text will use regular weight"
            );
        }
        data
    };

    ctx.set_fonts(fonts);

    if let (Some(render_state), Some(font_bytes)) = (wgpu_render_state, mono_font_data) {
        let font_size_px = cfg.font_size * ctx.pixels_per_point();
        let atlas = create_font_backend(
            &render_state.device,
            &render_state.queue,
            cfg,
            &font_bytes,
            bold_font_data.as_deref(),
            &fallback_font_data,
            font_size_px,
        );
        let pipeline = gpu::pipeline::GridPipeline::new(
            &render_state.device,
            render_state.target_format,
            atlas.gpu_resources().0,
            atlas.gpu_resources().1,
        );

        let mut renderer = render_state.renderer.write();
        if let Some(gpu_res) = renderer
            .callback_resources
            .get_mut::<gpu::callback::GpuResources>()
        {
            gpu_res.atlas = atlas;
            gpu_res.pipeline = pipeline;
        } else {
            let gpu_resources = gpu::callback::GpuResources::new(atlas, pipeline);
            renderer.callback_resources.insert(gpu_resources);
        }

        eprintln!(
            "[GPU] Configured GPU terminal renderer (font_size_px={:.1})",
            font_size_px
        );
    } else {
        eprintln!("[Renderer] Using low-memory Glow renderer; terminal GPU callbacks disabled");
    }
}

fn apply_theme_visuals(ctx: &egui::Context, theme: &theme::Theme) {
    let ui = &theme.ui;
    let brightness =
        u16::from(ui.window_bg[0]) + u16::from(ui.window_bg[1]) + u16::from(ui.window_bg[2]);
    let mut visuals = if brightness > 382 {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };

    visuals.window_fill = theme::Theme::rgb_to_color32(ui.window_bg);
    visuals.panel_fill = theme::Theme::rgb_to_color32(ui.panel_bg);
    visuals.extreme_bg_color = theme::Theme::rgb_to_color32(ui.panel_bg);
    visuals.override_text_color = Some(theme::Theme::rgb_to_color32(ui.text));
    visuals.widgets.noninteractive.bg_stroke.color = theme::Theme::rgb_to_color32(ui.border);
    visuals.widgets.inactive.bg_stroke.color = theme::Theme::rgb_to_color32(ui.border);
    visuals.widgets.active.bg_stroke.color = theme::Theme::rgb_to_color32(ui.border);
    visuals.widgets.hovered.bg_stroke.color = theme::Theme::rgb_to_color32(ui.border);

    ctx.set_visuals(visuals);
}

fn main() -> Result<(), eframe::Error> {
    // 设置panic hook，记录panic信息
    // 注意：panic时Drop可能不会被调用，但我们依赖PR_SET_PDEATHSIG确保子进程退出
    std::panic::set_hook(Box::new(|panic_info| {
        eprintln!("[PANIC] jterm2 panicked: {}", panic_info);
        eprintln!("[PANIC] Child rsh processes should exit due to PR_SET_PDEATHSIG");
    }));

    // 设置信号处理，确保收到SIGINT/SIGTERM时能正常清理
    setup_signal_handlers();

    // Load configuration
    let cfg = config::Config::load();

    let renderer = match cfg.app_renderer {
        config::AppRendererType::Glow => eframe::Renderer::Glow,
        config::AppRendererType::Wgpu => eframe::Renderer::Wgpu,
    };
    let transparent = !matches!(renderer, eframe::Renderer::Glow);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([cfg.initial_width, cfg.initial_height])
            .with_transparent(transparent),
        renderer,
        ..Default::default()
    };

    let cfg = std::sync::Arc::new(cfg);

    eframe::run_native(
        "JTerm2",
        options,
        Box::new(move |cc| {
            let cfg_clone = cfg.clone();
            // Set UI scale: use config value if provided, otherwise use native DPI
            let scale = cfg_clone.ui_scale.unwrap_or_else(|| {
                cc.egui_ctx.native_pixels_per_point().unwrap_or(1.0)
            });
            cc.egui_ctx.set_pixels_per_point(scale);
            configure_fonts_and_gpu(&cc.egui_ctx, cc.wgpu_render_state.as_ref(), &cfg_clone);
            let initial_theme = theme::Theme::get_theme(&cfg_clone.theme).unwrap_or_default();
            apply_theme_visuals(&cc.egui_ctx, &initial_theme);

            Ok(Box::new(TerminalApp::new(
                &cfg_clone,
                cc.egui_ctx.clone(),
                cc.wgpu_render_state.clone(),
            )))
        }),
    )
}

// TerminalApp struct definition moved to app::state module
use app::state::TerminalApp;

// normalize_terminal_shortcut_events moved to app::events module

fn wrap_bracketed_paste(mut payload: Vec<u8>) -> Vec<u8> {
    let mut wrapped = Vec::with_capacity(payload.len() + 12);
    wrapped.extend_from_slice(b"\x1b[200~");
    wrapped.append(&mut payload);
    wrapped.extend_from_slice(b"\x1b[201~");
    wrapped
}

fn osc_5522_packet(metadata: &str, payload: Option<&str>) -> Vec<u8> {
    let mut packet = Vec::new();
    packet.extend_from_slice(b"\x1b]5522;");
    packet.extend_from_slice(metadata.as_bytes());
    if let Some(payload) = payload {
        packet.extend_from_slice(b";");
        packet.extend_from_slice(payload.as_bytes());
    }
    packet.extend_from_slice(b"\x1b\\");
    packet
}

fn clipboard_5522_response_for_mime(mime_type: &str, data: &[u8]) -> Vec<u8> {
    let encoded_mime = base64::engine::general_purpose::STANDARD.encode(mime_type.as_bytes());
    let encoded_data = base64::engine::general_purpose::STANDARD.encode(data);
    let mut output = Vec::new();
    output.extend_from_slice(&osc_5522_packet("type=read:status=OK", None));
    output.extend_from_slice(&osc_5522_packet(
        &format!("type=read:status=DATA:mime={}", encoded_mime),
        Some(&encoded_data),
    ));
    output.extend_from_slice(&osc_5522_packet("type=read:status=DONE", None));
    output
}

// key_to_string and build_keybinding_string moved to app::events module

impl TerminalApp {
    fn new(
        cfg: &config::Config,
        repaint_ctx: egui::Context,
        wgpu_render_state: Option<egui_wgpu::RenderState>,
    ) -> Self {
        let (cols, rows) = clamp_terminal_dimensions(cfg.cols, cfg.rows);
        crate::debug_log!(
            "[INIT] terminal dimensions cfg=({}, {}) clamped=({}, {})",
            cfg.cols,
            cfg.rows,
            cols,
            rows
        );

        // 尝试获取实例锁，成功表示没有其他实例在运行
        let lock_file = session_persistence::try_acquire_instance_lock();
        let is_first_instance = lock_file.is_some();

        // 仅在首个实例且配置允许时恢复会话
        let saved_snapshot = if cfg.restore_session && is_first_instance {
            config::Config::session_history_path()
                .ok()
                .and_then(|path| session_persistence::SessionsSnapshot::load(&path).ok())
                .filter(|s| !s.sessions.is_empty())
        } else {
            if !is_first_instance {
                eprintln!("[SessionPersistence] Another instance is running, starting fresh");
            }
            None
        };

        // 创建首个会话，使用保存的 cwd 和 session_id（如果有）
        let first_cwd = saved_snapshot
            .as_ref()
            .and_then(|s| s.sessions.first()?.cwd.as_deref().map(String::from));
        let first_session_id = saved_snapshot
            .as_ref()
            .and_then(|s| s.sessions.first()?.session_id.as_deref().map(String::from));
        let saved_active_index = saved_snapshot.as_ref().and_then(|s| s.active_index);
        let terminal = TerminalState::new(cols, rows);

        let configured_shell = std::env::var("JTERM2_SHELL").ok().or(cfg.shell.clone());

        let shell = match ShellSession::new_with_cwd(
            cols,
            rows,
            first_cwd.as_deref(),
            first_session_id.as_deref(),
            configured_shell.as_deref(),
            repaint_ctx.clone(),
        ) {
            Ok(session) => {
                eprintln!("✓ Shell session started successfully");
                session
            }
            Err(e) => {
                eprintln!(
                    "✗ Failed to start shell with saved cwd, falling back: {}",
                    e
                );
                ShellSession::new(cols, rows, configured_shell.as_deref(), repaint_ctx.clone())
                    .unwrap_or_else(|e| panic!("Cannot create shell session: {}", e))
            }
        };

        let session = Session::with_default_name(0, Arc::new(ParkingMutex::new(terminal)), shell);
        let mut session_manager = SessionManager::new(session, repaint_ctx, configured_shell);

        // 恢复额外的会话（包括 restorable commands 回放）
        if let Some(snap) = saved_snapshot {
            session_manager.restore_from_snapshots(snap.sessions, saved_active_index);
            eprintln!(
                "[SessionPersistence] Restored {} sessions",
                session_manager.len()
            );
        }

        for session in session_manager.sessions_mut() {
            let mut terminal = session.terminal.lock();
            terminal.set_max_scrollback(cfg.scrollback_lines);
        }

        let clipboard = ClipboardManager::new().ok();

        let keybindings = keybindings::KeyBindings::load().unwrap_or_default();

        // Load theme
        let current_theme = theme::Theme::get_theme(&cfg.theme).unwrap_or_default();

        let mut renderer = TerminalRenderer::new(
            cfg.font_size,
            cfg.padding,
            cfg.line_spacing,
            cfg.scrollbar_visibility.clone(),
            current_theme.clone(),
        );
        renderer.opacity = cfg.opacity;
        renderer.gpu_rendering = cfg.gpu_rendering;
        renderer.wgpu_render_state = wgpu_render_state.clone();

        // Initialize layout manager with first session
        let layout_manager = layout::LayoutManager::new(0);

        // Create additional renderers for multi-pane support (start with empty)
        let mut pane_renderers = Vec::new();
        for _ in 0..4 {
            let mut pr = TerminalRenderer::new(
                cfg.font_size,
                cfg.padding,
                cfg.line_spacing,
                cfg.scrollbar_visibility.clone(),
                current_theme.clone(),
            );
            pr.opacity = cfg.opacity;
            pr.gpu_rendering = cfg.gpu_rendering;
            pr.wgpu_render_state = wgpu_render_state.clone();
            pane_renderers.push(pr);
        }

        TerminalApp {
            session_manager,
            input_queue: Arc::new(ParkingMutex::new(Vec::new())),
            renderer,
            clipboard,
            cols,
            rows,
            next_cursor_blink_time: std::time::Instant::now() + Duration::from_millis(1000),
            cursor_visible: true,
            last_activity_time: std::time::Instant::now(),
            status_message: String::new(),
            last_window_title: String::new(),
            hovered_tab_index: None,
            dragging_tab: None,
            drag_start_pos: None,
            current_mouse_x: 0.0,
            tab_scroll_offset: 0.0,
            search_state: search::SearchState::new(),
            link_detector: link::LinkDetector::new(link::LinkDetectionConfig::default()),
            hovered_link: None,
            cached_links: Vec::new(),
            cached_links_grid_version: 0,
            cached_links_scroll_offset: 0,
            keybindings,
            command_palette: command_palette::CommandPalette::new(),
            force_resize_session: false,
            current_theme,
            layout_manager,
            pane_renderers,
            dragging_divider: false,
            help_panel: help::HelpPanel::new(),
            config_panel: config_panel::ConfigPanel::new(),
            debug_panel: debug_panel::DebugPanel::new(),
            config: cfg.clone(),
            config_save_pending: false,
            config_save_deadline: std::time::Instant::now(),
            session_save_pending: true, // 启动后立即保存一次（确保首次运行就有记录）
            session_save_deadline: std::time::Instant::now() + std::time::Duration::from_secs(1),
            _lock_file: lock_file,
            pending_output: Vec::new(),
            scroll_accumulator: 0.0,
            mouse_scroll_accumulator: 0.0,
            font_size_accumulator: 0.0,
            had_ctrl_scroll_last_frame: false,
            frame_events: Vec::new(),
            keyboard_input_buffer: Vec::new(),
            adaptive_frame_budget: 32768, // 初始值 32KB
        }
    }

    fn apply_runtime_config(&mut self, ctx: &egui::Context) {
        // Apply UI scale: use config value if provided, otherwise use native DPI
        let scale = self.config.ui_scale.unwrap_or_else(|| {
            ctx.native_pixels_per_point().unwrap_or(1.0)
        });
        ctx.set_pixels_per_point(scale);

        configure_fonts_and_gpu(ctx, self.renderer.wgpu_render_state.as_ref(), &self.config);
        apply_theme_visuals(ctx, &self.current_theme);

        self.renderer.font_size = self.config.font_size;
        self.renderer.padding = self.config.padding;
        self.renderer.line_spacing = self.config.line_spacing;
        self.renderer.scrollbar_visibility = self.config.scrollbar_visibility.clone();
        self.renderer.theme = self.current_theme.clone();
        self.renderer.opacity = self.config.opacity;
        self.renderer.gpu_rendering = matches!(self.config.app_renderer, config::AppRendererType::Wgpu)
            && self.config.gpu_rendering;
        self.renderer.sync_font_metrics(ctx);

        for renderer in &mut self.pane_renderers {
            renderer.font_size = self.config.font_size;
            renderer.padding = self.config.padding;
            renderer.line_spacing = self.config.line_spacing;
            renderer.scrollbar_visibility = self.config.scrollbar_visibility.clone();
            renderer.theme = self.current_theme.clone();
            renderer.opacity = self.config.opacity;
            renderer.gpu_rendering = matches!(self.config.app_renderer, config::AppRendererType::Wgpu)
                && self.config.gpu_rendering;
            renderer.sync_font_metrics(ctx);
        }

        for session in self.session_manager.sessions_mut() {
            let mut terminal = session.terminal.lock();
            terminal.set_max_scrollback(self.config.scrollback_lines);
        }

        ctx.request_repaint();
    }

    fn create_session_with_current_config(
        &mut self,
        name: Option<String>,
        tags: Option<Vec<String>>,
    ) -> usize {
        let (cols, rows) = clamp_terminal_dimensions(self.cols, self.rows);
        self.session_manager
            .new_session(name, tags, cols, rows, self.config.scrollback_lines)
    }

    #[allow(deprecated)]
    fn render_ui(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::NONE.inner_margin(0.0);

        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            // 消除 tab 栏与终端之间的间距
            ui.spacing_mut().item_spacing.y = 0.0;

            // 渲染会话标签栏（仅在多会话时显示，单会话进入 zen 模式）
            let show_tab_bar = self.session_manager.sessions().len() > 1;

            // Tab 栏 - 绘制标签和按钮
            if show_tab_bar {
                let tab_height = 30.0;
                let close_btn_size = 14.0;
                let tab_rect = egui::Rect::from_min_size(
                    ui.cursor().left_top(),
                    egui::vec2(ui.available_width(), tab_height),
                );

                let painter = ui.painter();

                // 背景
                let tab_alpha = (self.renderer.opacity * 255.0) as u8;
                painter.rect_filled(
                    tab_rect,
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(40, 40, 40, tab_alpha),
                );

                // === Tab 布局常量 ===
                let tab_padding = 20.0 + close_btn_size + 4.0; // 文本左右 padding + 关闭按钮
                let min_tab_width: f32 = 60.0;
                let max_tab_width: f32 = 200.0;
                let active_tab_extra: f32 = 60.0;
                let active_min_width: f32 = min_tab_width * 2.0; // 当前 session 最小宽度，更突出
                let tab_spacing: f32 = 1.0;
                let left_margin: f32 = 5.0;
                let reserved_right: f32 = 80.0; // "+"按钮 + 关闭窗口按钮 + margin

                let active_idx_for_layout = self.session_manager.active_index();

                // 文本测量闭包
                let measure = |text: &str| -> f32 {
                    painter
                        .layout_no_wrap(
                            text.to_string(),
                            egui::FontId::monospace(12.0),
                            egui::Color32::WHITE,
                        )
                        .rect
                        .width()
                };

                // 路径缩略闭包：将 CWD 路径缩略到 max_text_w 像素以内
                let abbreviate_path = |title: &str, max_text_w: f32| -> String {
                    if measure(title) <= max_text_w {
                        return title.to_string();
                    }
                    let (prefix, path_part) = if let Some(rest) = title.strip_prefix("~/") {
                        ("~/", rest)
                    } else if let Some(rest) = title.strip_prefix('/') {
                        ("/", rest)
                    } else {
                        ("", title)
                    };
                    let parts: Vec<&str> = path_part.split('/').collect();
                    if parts.len() <= 1 {
                        let ellipsis = "...";
                        let mut truncated = String::new();
                        for ch in title.chars() {
                            let test = format!("{}{}{}", truncated, ch, ellipsis);
                            if measure(&test) > max_text_w {
                                break;
                            }
                            truncated.push(ch);
                        }
                        return format!("{}{}", truncated, ellipsis);
                    }
                    let last = parts[parts.len() - 1];
                    let abbreviated_middle: Vec<String> = parts[..parts.len() - 1]
                        .iter()
                        .map(|p| p.chars().next().map(|c| c.to_string()).unwrap_or_default())
                        .collect();
                    let short_path = format!("{}{}/{}", prefix, abbreviated_middle.join("/"), last);
                    if measure(&short_path) <= max_text_w {
                        return short_path;
                    }
                    let short_prefix = format!("{}{}/", prefix, abbreviated_middle.join("/"));
                    let ellipsis = "...";
                    let mut truncated = short_prefix.clone();
                    for ch in last.chars() {
                        let test = format!("{}{}{}", truncated, ch, ellipsis);
                        if measure(&test) > max_text_w {
                            break;
                        }
                        truncated.push(ch);
                    }
                    format!("{}{}", truncated, ellipsis)
                };

                // === 第一阶段：收集原始路径 + 为每个 tab 生成 display_text ===
                // 活跃 tab 允许更大的文本宽度
                let active_max_text = max_tab_width + active_tab_extra - tab_padding;
                let inactive_max_text = max_tab_width - tab_padding;

                let tab_infos: Vec<(usize, String, f32)> = self
                    .session_manager
                    .sessions()
                    .iter()
                    .enumerate()
                    .map(|(idx, session)| {
                        let pid = session.get_shell_pid();
                        let tab_title = crate::session_manager::get_process_cwd(pid)
                            .map(|cwd| {
                                if let Ok(home) = std::env::var("HOME") {
                                    if cwd == home {
                                        "~".to_string()
                                    } else if let Some(rest) = cwd.strip_prefix(&home) {
                                        format!("~{}", rest)
                                    } else {
                                        cwd
                                    }
                                } else {
                                    cwd
                                }
                            })
                            .unwrap_or_else(|| session.metadata.name.clone());

                        let max_text_w = if idx == active_idx_for_layout {
                            active_max_text
                        } else {
                            inactive_max_text
                        };
                        let display_text = abbreviate_path(&tab_title, max_text_w);
                        let ideal_width = if idx == active_idx_for_layout {
                            (measure(&display_text) + tab_padding).max(active_min_width)
                        } else {
                            (measure(&display_text) + tab_padding)
                                .clamp(min_tab_width, max_tab_width)
                        };
                        (idx, display_text, ideal_width)
                    })
                    .collect();

                // === 第二阶段：布局分配 —— 计算每个 tab 的最终宽度 ===
                let available_width = tab_rect.width() - left_margin - reserved_right;
                let n = tab_infos.len();
                let total_spacing = if n > 1 {
                    (n - 1) as f32 * tab_spacing
                } else {
                    0.0
                };

                let tab_widths: Vec<f32> = {
                    let total_ideal: f32 =
                        tab_infos.iter().map(|(_, _, w)| w).sum::<f32>() + total_spacing;

                    if total_ideal <= available_width {
                        // 空间足够，各自用理想宽度
                        tab_infos.iter().map(|(_, _, w)| *w).collect()
                    } else {
                        // 空间不足：先保障活跃 tab，压缩非活跃 tab
                        let active_w = tab_infos
                            .iter()
                            .find(|(idx, _, _)| *idx == active_idx_for_layout)
                            .map(|(_, _, w)| *w)
                            .unwrap_or(min_tab_width);
                        let remaining = (available_width - active_w - total_spacing).max(0.0);
                        let inactive_count = n.saturating_sub(1);

                        if inactive_count == 0 {
                            // 只有一个 tab
                            vec![available_width.max(min_tab_width)]
                        } else {
                            let per_inactive =
                                (remaining / inactive_count as f32).max(min_tab_width);
                            tab_infos
                                .iter()
                                .map(|(idx, _, w)| {
                                    if *idx == active_idx_for_layout {
                                        // 活跃 tab 也不能超过可用空间
                                        active_w.min(available_width - total_spacing)
                                    } else {
                                        (*w).min(per_inactive).max(min_tab_width)
                                    }
                                })
                                .collect()
                        }
                    }
                };

                // === 第三阶段：滚动偏移 —— 保证活跃 tab 可见 ===
                {
                    let total_width: f32 = tab_widths.iter().sum::<f32>() + total_spacing;
                    let max_scroll = (total_width - available_width).max(0.0);

                    if total_width <= available_width {
                        self.tab_scroll_offset = 0.0;
                    } else {
                        // 计算活跃 tab 的位置
                        let mut active_left: f32 = 0.0;
                        for (i, tw) in tab_widths.iter().enumerate() {
                            if i == active_idx_for_layout {
                                break;
                            }
                            active_left += tw + tab_spacing;
                        }
                        let active_right = active_left
                            + tab_widths
                                .get(active_idx_for_layout)
                                .copied()
                                .unwrap_or(0.0);

                        // 如果活跃 tab 左边超出可视区
                        if active_left < self.tab_scroll_offset {
                            self.tab_scroll_offset = active_left;
                        }
                        // 如果活跃 tab 右边超出可视区
                        if active_right > self.tab_scroll_offset + available_width {
                            self.tab_scroll_offset = active_right - available_width;
                        }
                        self.tab_scroll_offset = self.tab_scroll_offset.clamp(0.0, max_scroll);
                    }
                }

                // 检测悬停位置（在绘制之前）
                let hover_pos = ctx.input(|i| i.pointer.hover_pos());
                self.hovered_tab_index = None;

                // 更新当前鼠标x位置（用于拖拽动画）
                if let Some(pos) = hover_pos {
                    self.current_mouse_x = pos.x;
                }

                // 检测鼠标释放（点击完成或拖拽结束）
                let mouse_released =
                    ctx.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
                let mouse_pressed =
                    ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));

                // 检查是否发生了实际的拖拽（超过阈值距离）
                let is_actually_dragging =
                    if let (Some(_), Some(start_x)) = (self.dragging_tab, self.drag_start_pos) {
                        if let Some(current_pos) = ctx.input(|i| i.pointer.latest_pos()) {
                            let distance = (current_pos.x - start_x).abs();
                            distance > 5.0 // 5px拖拽阈值
                        } else {
                            false
                        }
                    } else {
                        false
                    };

                // === 交互辅助：用 tab_widths 计算 tab 位置的宏 ===
                // scroll_base: 绝对坐标 x 基准（减去滚动偏移）
                let scroll_base = tab_rect.left() + left_margin - self.tab_scroll_offset;

                // 处理拖拽结束或点击
                if mouse_released {
                    if is_actually_dragging {
                        // 实际拖拽结束 - 计算drop目标并执行重排
                        if let Some(from_idx) = self.dragging_tab {
                            if let Some(hover_pos) = hover_pos {
                                if tab_rect.contains(hover_pos) {
                                    let mut x_off = scroll_base;
                                    let mut target_idx = from_idx;

                                    for (i, &tw) in tab_widths.iter().enumerate() {
                                        if hover_pos.x >= x_off && hover_pos.x < x_off + tw {
                                            target_idx = i;
                                            break;
                                        }
                                        x_off += tw + tab_spacing;
                                    }

                                    // 执行重排
                                    if target_idx != from_idx {
                                        self.session_manager.reorder_sessions(from_idx, target_idx);
                                    }
                                }
                            }
                        }
                        self.dragging_tab = None;
                        self.drag_start_pos = None;
                    } else {
                        // 简单点击（没有发生实际拖拽）
                        if let Some(click_pos) =
                            hover_pos.or_else(|| ctx.input(|i| i.pointer.latest_pos()))
                        {
                            if tab_rect.contains(click_pos) {
                                let mut x_off = scroll_base;
                                for (i, &tw) in tab_widths.iter().enumerate() {
                                    let tab_rect_item = egui::Rect::from_min_size(
                                        egui::pos2(x_off, tab_rect.top() + 5.0),
                                        egui::vec2(tw, tab_height - 10.0),
                                    );

                                    let close_btn_rect = egui::Rect::from_min_size(
                                        egui::pos2(
                                            tab_rect_item.right() - close_btn_size - 3.0,
                                            tab_rect_item.center().y - close_btn_size / 2.0,
                                        ),
                                        egui::vec2(close_btn_size, close_btn_size),
                                    );

                                    if close_btn_rect.contains(click_pos) {
                                        if self.session_manager.len() > 1 {
                                            self.session_manager.close_session(i);
                                            self.schedule_session_save();
                                        } else {
                                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                            return;
                                        }
                                        self.dragging_tab = None;
                                        self.drag_start_pos = None;
                                        break;
                                    } else if tab_rect_item.contains(click_pos) {
                                        self.session_manager.switch_session(i);
                                        self.force_resize_session = true;
                                        self.dragging_tab = None;
                                        self.drag_start_pos = None;
                                        break;
                                    }

                                    x_off += tw + tab_spacing;
                                }
                            }
                        }
                        // 清除拖拽状态（即使没有找到点击的tab）
                        if self.dragging_tab.is_some() {
                            self.dragging_tab = None;
                            self.drag_start_pos = None;
                        }
                    }
                }

                // 检测拖拽开始（鼠标按下且移动）
                if mouse_pressed {
                    if let Some(press_pos) = ctx.input(|i| i.pointer.press_origin()) {
                        if self.dragging_tab.is_none() {
                            let mut x_off = scroll_base;
                            for (i, &tw) in tab_widths.iter().enumerate() {
                                let tab_rect_item = egui::Rect::from_min_size(
                                    egui::pos2(x_off, tab_rect.top() + 5.0),
                                    egui::vec2(tw, tab_height - 10.0),
                                );

                                let close_btn_rect = egui::Rect::from_min_size(
                                    egui::pos2(
                                        tab_rect_item.right() - close_btn_size - 3.0,
                                        tab_rect_item.center().y - close_btn_size / 2.0,
                                    ),
                                    egui::vec2(close_btn_size, close_btn_size),
                                );

                                if tab_rect_item.contains(press_pos)
                                    && !close_btn_rect.contains(press_pos)
                                {
                                    self.dragging_tab = Some(i);
                                    self.drag_start_pos = Some(press_pos.x);
                                    break;
                                }

                                x_off += tw + tab_spacing;
                            }
                        }
                    }
                }

                // 计算拖拽过程中的动画效果
                let mut drag_target_idx: Option<usize> = None;
                if is_actually_dragging {
                    if let Some(hover_pos) = hover_pos {
                        if let Some(_from_idx) = self.dragging_tab {
                            let mut x_off = scroll_base;
                            for (i, &tw) in tab_widths.iter().enumerate() {
                                if hover_pos.x >= x_off && hover_pos.x < x_off + tw {
                                    drag_target_idx = Some(i);
                                    break;
                                }
                                x_off += tw + tab_spacing;
                            }
                        }
                    }
                    // 请求持续重绘以显示动画
                    ctx.request_repaint();
                }

                // === 渲染 Tab 栏（使用 clip rect 裁剪溢出内容）===
                let tab_clip_rect = egui::Rect::from_min_max(
                    egui::pos2(tab_rect.left() + left_margin, tab_rect.top()),
                    egui::pos2(tab_rect.right() - reserved_right, tab_rect.bottom()),
                );
                let clipped_painter = painter.with_clip_rect(tab_clip_rect);

                let mut x_offset = scroll_base;
                let active_idx = self.session_manager.active_index();

                // 绘制每个标签
                for (i, (_, display_text, _)) in tab_infos.iter().enumerate() {
                    let tab_width = tab_widths[i];
                    let mut tab_rect_item = egui::Rect::from_min_size(
                        egui::pos2(x_offset, tab_rect.top() + 5.0),
                        egui::vec2(tab_width, tab_height - 10.0),
                    );

                    let is_active = i == active_idx;
                    let is_dragging = self.dragging_tab == Some(i);
                    let is_drag_target = drag_target_idx == Some(i);

                    // 计算拖拽过程中的动画位移
                    if is_actually_dragging {
                        if is_dragging {
                            // 被拖拽的Tab跟随鼠标移动
                            if let Some(start_x) = self.drag_start_pos {
                                let offset = self.current_mouse_x - start_x;
                                tab_rect_item = tab_rect_item.translate(egui::vec2(offset, 0.0));
                            }
                        } else if let Some(from_idx) = self.dragging_tab {
                            // 其他Tabs根据拖拽目标位置进行动画插入
                            let drag_to_left = is_drag_target
                                && drag_target_idx.map(|t| t < from_idx).unwrap_or(false);
                            let drag_to_right = is_drag_target
                                && drag_target_idx.map(|t| t > from_idx).unwrap_or(false);

                            if drag_to_left {
                                if i > from_idx {
                                    let push_offset = tab_width + tab_spacing;
                                    tab_rect_item =
                                        tab_rect_item.translate(egui::vec2(push_offset, 0.0));
                                }
                            } else if drag_to_right
                                && i < from_idx {
                                    let push_offset = -(tab_width + tab_spacing);
                                    tab_rect_item =
                                        tab_rect_item.translate(egui::vec2(push_offset, 0.0));
                                }
                        }
                    }

                    // 检测悬停
                    let is_hovered = if let Some(hover_pos) = hover_pos {
                        tab_rect_item.contains(hover_pos) && tab_clip_rect.contains(hover_pos)
                    } else {
                        false
                    };

                    if is_hovered && !is_actually_dragging {
                        self.hovered_tab_index = Some(i);
                    }

                    // 背景色：无边框风格，通过背景色差异区分状态
                    let bg_color = if is_active {
                        egui::Color32::from_rgb(50, 50, 60)
                    } else if is_hovered || is_dragging {
                        egui::Color32::from_rgb(55, 55, 65)
                    } else {
                        egui::Color32::TRANSPARENT
                    };

                    // 绘制Tab背景（无边框）
                    if is_dragging && is_actually_dragging {
                        let drag_bg = if is_active {
                            egui::Color32::from_rgba_premultiplied(50, 50, 60, 140)
                        } else {
                            egui::Color32::from_rgba_premultiplied(55, 55, 65, 140)
                        };
                        clipped_painter.rect_filled(tab_rect_item, 4.0, drag_bg);
                    } else {
                        clipped_painter.rect_filled(tab_rect_item, 4.0, bg_color);

                        // Active Tab 底部高亮指示线
                        if is_active {
                            clipped_painter.hline(
                                (tab_rect_item.left() + 4.0)..=(tab_rect_item.right() - 4.0),
                                tab_rect_item.bottom() - 1.0,
                                egui::Stroke::new(2.0, egui::Color32::from_rgb(100, 160, 255)),
                            );
                        }

                        // 拖拽过程中，在目标Tab位置显示插入指示线
                        if is_drag_target && is_actually_dragging {
                            let insert_line_x =
                                if self.current_mouse_x - tab_rect_item.center().x < 0.0 {
                                    tab_rect_item.left()
                                } else {
                                    tab_rect_item.right()
                                };
                            clipped_painter.vline(
                                insert_line_x,
                                tab_rect_item.top()..=tab_rect_item.bottom(),
                                egui::Stroke::new(2.0, egui::Color32::from_rgb(100, 200, 255)),
                            );
                        }
                    }

                    // 绘制文本（使用 tab 内部 clip 防止文本溢出 tab 边界）
                    let text_clip = egui::Rect::from_min_max(
                        tab_rect_item.left_top(),
                        egui::pos2(
                            tab_rect_item.right() - close_btn_size - 6.0,
                            tab_rect_item.bottom(),
                        ),
                    );
                    let text_painter = painter.with_clip_rect(text_clip.intersect(tab_clip_rect));
                    text_painter.text(
                        egui::pos2(tab_rect_item.left() + 10.0, tab_rect_item.center().y),
                        egui::Align2::LEFT_CENTER,
                        display_text,
                        egui::FontId::monospace(12.0),
                        if is_active {
                            egui::Color32::WHITE
                        } else {
                            egui::Color32::from_rgb(180, 180, 190)
                        },
                    );

                    // 绘制关闭按钮（仅在悬停Tab时显示）
                    let close_btn_rect = egui::Rect::from_min_size(
                        egui::pos2(
                            tab_rect_item.right() - close_btn_size - 3.0,
                            tab_rect_item.center().y - close_btn_size / 2.0,
                        ),
                        egui::vec2(close_btn_size, close_btn_size),
                    );

                    if is_hovered && !is_dragging {
                        let close_btn_hovered = if let Some(hover_pos) = hover_pos {
                            close_btn_rect.contains(hover_pos)
                        } else {
                            false
                        };

                        if close_btn_hovered {
                            clipped_painter.circle_filled(
                                close_btn_rect.center(),
                                close_btn_size / 2.0 + 2.0,
                                egui::Color32::from_rgb(100, 50, 50),
                            );
                        }

                        let close_x_color = if close_btn_hovered {
                            egui::Color32::from_rgb(255, 150, 150)
                        } else {
                            egui::Color32::from_rgb(150, 150, 150)
                        };

                        let cross_offset = close_btn_size / 3.0;
                        clipped_painter.line_segment(
                            [
                                egui::pos2(
                                    close_btn_rect.center().x - cross_offset,
                                    close_btn_rect.center().y - cross_offset,
                                ),
                                egui::pos2(
                                    close_btn_rect.center().x + cross_offset,
                                    close_btn_rect.center().y + cross_offset,
                                ),
                            ],
                            egui::Stroke::new(1.5, close_x_color),
                        );
                        clipped_painter.line_segment(
                            [
                                egui::pos2(
                                    close_btn_rect.center().x + cross_offset,
                                    close_btn_rect.center().y - cross_offset,
                                ),
                                egui::pos2(
                                    close_btn_rect.center().x - cross_offset,
                                    close_btn_rect.center().y + cross_offset,
                                ),
                            ],
                            egui::Stroke::new(1.5, close_x_color),
                        );
                    }

                    x_offset += tab_width + tab_spacing;
                }

                // "+" 按钮 - 新建会话（紧跟最后一个 Tab，但不超过 clip 区域）
                let plus_btn_x = x_offset
                    .max(tab_rect.left() + left_margin)
                    .min(tab_clip_rect.right());
                let plus_btn_rect = egui::Rect::from_min_size(
                    egui::pos2(plus_btn_x + 4.0, tab_rect.top() + 5.0),
                    egui::vec2(25.0, tab_height - 10.0),
                );

                // 检测"+"按钮悬停
                let plus_btn_hovered = if let Some(hover_pos) = hover_pos {
                    plus_btn_rect.contains(hover_pos)
                } else {
                    false
                };

                let plus_btn_color = if plus_btn_hovered {
                    egui::Color32::from_rgb(55, 55, 65)
                } else {
                    egui::Color32::TRANSPARENT
                };

                painter.rect_filled(plus_btn_rect, 4.0, plus_btn_color);

                let plus_text_color = if plus_btn_hovered {
                    egui::Color32::from_rgb(220, 220, 220)
                } else {
                    egui::Color32::from_rgb(180, 180, 190)
                };

                painter.text(
                    plus_btn_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "+",
                    egui::FontId::monospace(14.0),
                    plus_text_color,
                );

                // 检测 "+" 按钮点击（在鼠标释放时）
                if mouse_released {
                    if let Some(click_pos) = ctx.input(|i| i.pointer.latest_pos()) {
                        if plus_btn_rect.contains(click_pos) {
                            let new_idx = self.create_session_with_current_config(None, None);
                            self.session_manager.switch_session(new_idx);
                            self.force_resize_session = true;
                            self.schedule_session_save();
                        }
                    }
                }

                // 关闭窗口按钮（最右侧）
                let close_win_size = 25.0;
                let close_win_rect = egui::Rect::from_min_size(
                    egui::pos2(
                        tab_rect.right() - close_win_size - 5.0,
                        tab_rect.top() + 5.0,
                    ),
                    egui::vec2(close_win_size, tab_height - 10.0),
                );

                let close_win_hovered = if let Some(hover_pos) = hover_pos {
                    close_win_rect.contains(hover_pos)
                } else {
                    false
                };

                let close_win_bg = if close_win_hovered {
                    egui::Color32::from_rgb(180, 50, 50)
                } else {
                    egui::Color32::TRANSPARENT
                };

                painter.rect_filled(close_win_rect, 4.0, close_win_bg);

                // 绘制 X 符号
                let cw_cross = 5.0;
                let cw_center = close_win_rect.center();
                let cw_x_color = if close_win_hovered {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::from_rgb(180, 180, 190)
                };
                painter.line_segment(
                    [
                        egui::pos2(cw_center.x - cw_cross, cw_center.y - cw_cross),
                        egui::pos2(cw_center.x + cw_cross, cw_center.y + cw_cross),
                    ],
                    egui::Stroke::new(1.5, cw_x_color),
                );
                painter.line_segment(
                    [
                        egui::pos2(cw_center.x + cw_cross, cw_center.y - cw_cross),
                        egui::pos2(cw_center.x - cw_cross, cw_center.y + cw_cross),
                    ],
                    egui::Stroke::new(1.5, cw_x_color),
                );

                // 检测关闭窗口按钮点击
                if mouse_released {
                    if let Some(click_pos) = ctx.input(|i| i.pointer.latest_pos()) {
                        if close_win_rect.contains(click_pos) {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                }

                // 向下移动光标
                ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), tab_height),
                    egui::Sense::hover(),
                );
            }

            // 终端显示区域
            self.renderer.sync_font_metrics(ctx);
            let (cols, rows) = self.renderer.grid_dimensions(ui.available_size());
            crate::debug_log!("[RESIZE] grid_dimensions => {}x{}", cols, rows);

            if cols != self.cols || rows != self.rows || self.force_resize_session {
                let session = self.session_manager.get_active_session_mut();
                let _ = session.shell.resize(cols, rows);
                let mut terminal = session.terminal.lock();
                terminal.on_resize(cols, rows);
                self.cols = cols;
                self.rows = rows;
                if self.force_resize_session {
                    // Session 切换时重置 renderer 的 IME 状态缓存
                    // 这样下一帧会重新发送 IMEAllowed(true)，确保 IME 不会丢失
                    self.renderer.reset_ime_state();
                }
                self.force_resize_session = false;
            }

            // 多窗格支持：如果有多于一个窗格，则进行分屏渲染
            if self.layout_manager.panes().len() > 1 {
                let available_rect = ui.available_rect_before_wrap();

                // 计算窗格矩形
                self.layout_manager.compute_pane_rects(available_rect);

                // 获取所有窗格信息
                let panes = self.layout_manager.panes().to_vec();
                let divider_rect = self.layout_manager.get_divider_rect();

                // 为每个窗格渲染
                for (pane_idx, pane) in panes.iter().enumerate() {
                    if pane_idx >= self.pane_renderers.len() {
                        break;
                    }

                    let session_idx = pane.session_idx;
                    if let Some(session) = self.session_manager.get_session_mut(session_idx) {
                        let mut terminal_guard = session.terminal.lock();
                        let visible_cells = terminal_guard.get_visible_cells();
                        let row_wrapped = terminal_guard.get_visible_row_wrapped();
                        let links = self
                            .link_detector
                            .detect_links_in_visible_cells_with_wrapping(&visible_cells, &row_wrapped);

                        // 获取当前窗格的渲染器
                        let renderer = &mut self.pane_renderers[pane_idx];

                        // 在指定矩形内渲染（多窗格模式专用方法）
                        renderer.render_in_rect(
                            ui,
                            &mut terminal_guard,
                            self.cursor_visible,
                            &self.search_state,
                            &links,
                            &self.hovered_link,
                            pane.rect,
                        );
                    }
                }

                // 绘制分隔线
                if let Some(divider) = divider_rect {
                    let painter = ui.painter();
                    let divider_color = if self.dragging_divider {
                        egui::Color32::from_rgb(100, 150, 200)
                    } else {
                        egui::Color32::from_rgb(80, 80, 80)
                    };

                    painter.rect_filled(divider, 0.0, divider_color);

                    // 处理分隔线拖拽
                    if ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary)) {
                        if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                            if divider.contains(pos) {
                                self.dragging_divider = true;
                            }
                        }
                    }

                    if self.dragging_divider {
                        if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                            // 计算新的分割比例
                            match self.layout_manager.mode {
                                layout::SplitMode::VerticalSplit { .. } => {
                                    let delta = pos.x - divider.center().x;
                                    let total_width = available_rect.width();
                                    let ratio_delta = delta / total_width * 0.1; // 降低灵敏度
                                    self.layout_manager.adjust_split_ratio(ratio_delta);
                                }
                                layout::SplitMode::HorizontalSplit { .. } => {
                                    let delta = pos.y - divider.center().y;
                                    let total_height = available_rect.height();
                                    let ratio_delta = delta / total_height * 0.1;
                                    self.layout_manager.adjust_split_ratio(ratio_delta);
                                }
                                _ => {}
                            }
                        }

                        if ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary)) {
                            self.dragging_divider = false;
                        }
                    }
                }
            } else {
                // 单窗格渲染（原有逻辑）
                {
                    let session = self.session_manager.get_active_session_mut();
                    let mut terminal_guard = session.terminal.lock();

                    // 获取链接列表用于渲染（使用缓存）
                    let grid_version = terminal_guard.get_grid_version();
                    let scroll_offset = terminal_guard.scroll_offset;

                    if grid_version != self.cached_links_grid_version
                        || scroll_offset != self.cached_links_scroll_offset
                    {
                        let visible_cells = terminal_guard.get_visible_cells();
                        let row_wrapped = terminal_guard.get_visible_row_wrapped();
                        self.cached_links = self.link_detector.detect_links_in_visible_cells_with_wrapping(&visible_cells, &row_wrapped);
                        self.cached_links_grid_version = grid_version;
                        self.cached_links_scroll_offset = scroll_offset;
                    }
                    let links = self.cached_links.clone();

                    // 在渲染终端之前读取滚轮值和 Ctrl 键状态
                    let ctrl_pressed_render = ui.input(|i| i.modifiers.ctrl);

                    // 从原始 MouseWheel 事件中提取 delta（因为 smooth_scroll_delta 被 egui 消费了）
                    let mut scroll_delta_from_event = 0.0;
                    if ctrl_pressed_render {
                        let all_events = ui.input(|i| i.events.clone());
                        for evt in &all_events {
                            if let egui::Event::MouseWheel {
                                delta, modifiers, ..
                            } = evt
                            {
                                if modifiers.ctrl {
                                    scroll_delta_from_event += delta.y;
                                }
                            }
                        }
                    }

                    // Ctrl+滚轮字体缩放（积累事件而不是立即应用）
                    if scroll_delta_from_event != 0.0 && ctrl_pressed_render {
                        let font_size_delta = if scroll_delta_from_event > 0.0 {
                            1.0
                        } else {
                            -1.0
                        };
                        // 积累字体大小变化
                        self.font_size_accumulator += font_size_delta;
                        self.had_ctrl_scroll_last_frame = true;
                    }

                    self.renderer.render(
                        ui,
                        &mut terminal_guard,
                        self.cursor_visible,
                        &self.search_state,
                        &links,
                        &self.hovered_link,
                    );
                }
            }
        });

        // 搜索面板 UI（浮动窗口，右上角）
        if self.search_state.is_open {
            egui::Window::new("Search")
                .title_bar(false)
                .resizable(false)
                .default_pos(egui::pos2(ctx.available_rect().right() - 350.0, 60.0))
                .default_size([340.0, 50.0])
                .fixed_size([340.0, 50.0])
                .frame(egui::Frame {
                    fill: egui::Color32::from_rgb(40, 40, 40),
                    ..Default::default()
                })
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        // 搜索输入框
                        ui.label("Search:");
                        let search_response = ui.text_edit_singleline(&mut self.search_state.query);

                        // 自动 focus 搜索框
                        if self.search_state.search_focused {
                            ui.memory_mut(|mem| mem.request_focus(search_response.id));
                            self.search_state.search_focused = false;
                        }

                        if search_response.changed() {
                            // 重新搜索
                            let session = self.session_manager.get_active_session_mut();
                            let terminal = session.terminal.lock();
                            let (matches, error) = search::SearchEngine::search(
                                &terminal.grid,
                                &self.search_state.query,
                                self.search_state.use_regex,
                                self.search_state.case_sensitive,
                            );
                            drop(terminal);
                            self.search_state.matches = matches;
                            self.search_state.error_message = error;
                            self.search_state.current_match_index = 0;
                        }

                        // 显示匹配计数
                        if !self.search_state.matches.is_empty() {
                            ui.label(format!(
                                "{}/{}",
                                self.search_state.current_match_index + 1,
                                self.search_state.matches.len()
                            ));
                        } else if !self.search_state.query.is_empty() {
                            ui.label("No matches");
                        }

                        // 上一个/下一个 按钮
                        if ui.button("↑").clicked() {
                            self.search_state.prev_match();
                        }
                        if ui.button("↓").clicked() {
                            self.search_state.next_match();
                        }

                        // 关闭按钮
                        if ui.button("✕").clicked() {
                            self.search_state.close();
                        }
                    });

                    // 显示错误信息（如正则表达式错误）
                    if let Some(error) = &self.search_state.error_message {
                        ui.label(egui::RichText::new(error).color(egui::Color32::RED));
                    }
                });
        }

        // 命令调色板 UI（中央弹窗）
        if self.command_palette.is_open {
            let screen_rect = ctx.screen_rect();
            let palette_width = 600.0;
            let palette_height = 400.0;
            let palette_pos = egui::pos2(
                (screen_rect.width() - palette_width) / 2.0,
                (screen_rect.height() - palette_height) / 3.0,
            );

            egui::Window::new("Command Palette")
                .title_bar(false)
                .resizable(false)
                .movable(true)
                .default_pos(palette_pos)
                .default_size([palette_width, palette_height])
                .fixed_size([palette_width, palette_height])
                .frame(egui::Frame {
                    fill: egui::Color32::from_rgb(40, 40, 40),
                    stroke: egui::Stroke::new(1.0, egui::Color32::from_rgb(100, 100, 100)),
                    ..Default::default()
                })
                .show(ctx, |ui| {
                    // 搜索输入框
                    ui.horizontal(|ui| {
                        ui.label("🔍");
                        let search_response =
                            ui.text_edit_singleline(&mut self.command_palette.search_query);
                        if search_response.changed() {
                            self.command_palette.update_search_results();
                        }
                        if self.command_palette.needs_focus {
                            search_response.request_focus();
                            self.command_palette.needs_focus = false;
                        }
                        if search_response.has_focus()
                            && self.command_palette.search_query.is_empty()
                        {
                            ui.label("Search commands...");
                        }
                    });

                    ui.separator();

                    // 命令列表
                    let results = self.command_palette.get_results();
                    let selected_index = self.command_palette.selected_index;

                    egui::ScrollArea::vertical()
                        .max_height(palette_height - 100.0)
                        .show(ui, |ui| {
                            for (idx, (cmd_info, _score)) in results.iter().enumerate() {
                                let is_selected = idx == selected_index;

                                let bg_color = if is_selected {
                                    egui::Color32::from_rgb(70, 70, 80)
                                } else {
                                    egui::Color32::TRANSPARENT
                                };

                                let item_response = ui.horizontal(|ui| {
                                    let item_rect = ui.available_rect_before_wrap();
                                    ui.painter().rect_filled(item_rect, 2.0, bg_color);

                                    // 分类标签
                                    let category_color = match cmd_info.category {
                                        command_palette::CommandCategory::Session => {
                                            egui::Color32::from_rgb(100, 150, 255)
                                        }
                                        command_palette::CommandCategory::Edit => {
                                            egui::Color32::from_rgb(100, 200, 100)
                                        }
                                        command_palette::CommandCategory::Search => {
                                            egui::Color32::from_rgb(255, 200, 100)
                                        }
                                        command_palette::CommandCategory::Terminal => {
                                            egui::Color32::from_rgb(150, 150, 255)
                                        }
                                        command_palette::CommandCategory::Window => {
                                            egui::Color32::from_rgb(200, 100, 200)
                                        }
                                        command_palette::CommandCategory::Config => {
                                            egui::Color32::from_rgb(200, 180, 100)
                                        }
                                    };

                                    ui.colored_label(
                                        category_color,
                                        format!("[{}]", cmd_info.category),
                                    );

                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new(&cmd_info.name).strong());
                                        ui.label(
                                            egui::RichText::new(&cmd_info.description)
                                                .size(10.0)
                                                .color(egui::Color32::from_rgb(150, 150, 150)),
                                        );
                                    });

                                    // 快捷键显示
                                    let keybinding_str = self
                                        .keybindings
                                        .bindings
                                        .iter()
                                        .find(|(_, cmd)| {
                                            if let Ok(parsed_cmd) =
                                                cmd.parse::<keybindings::Command>()
                                            {
                                                parsed_cmd == cmd_info.command
                                            } else {
                                                false
                                            }
                                        })
                                        .map(|(binding, _)| binding.clone())
                                        .unwrap_or_else(|| "No binding".to_string());

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                egui::RichText::new(keybinding_str)
                                                    .size(10.0)
                                                    .color(egui::Color32::from_rgb(100, 150, 200)),
                                            );
                                        },
                                    );
                                });

                                // Auto-scroll to keep selected item visible
                                if is_selected {
                                    item_response
                                        .response
                                        .scroll_to_me(Some(egui::Align::Center));
                                }

                                ui.separator();
                            }

                            // 如果没有结果
                            if results.is_empty() {
                                ui.label(
                                    egui::RichText::new("No commands found")
                                        .color(egui::Color32::from_rgb(150, 150, 150)),
                                );
                            }
                        });

                    // 底部提示
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("↑↓ Navigate  Enter Execute  Esc Cancel")
                                .size(10.0)
                                .color(egui::Color32::from_rgb(100, 100, 100)),
                        );
                    });
                });
        }

        // 帮助面板 UI（浮动窗口）
        let mut help_open = self.help_panel.is_open;
        self.help_panel.show(ctx, &mut help_open);
        self.help_panel.is_open = help_open;

        // 配置面板 UI（浮动窗口）
        let config_actions = self.config_panel.show(ctx, &self.current_theme);
        for action in config_actions {
            match action {
                config_panel::ConfigAction::CustomThemeApplied(theme) => {
                    self.current_theme = *theme.clone();
                    self.apply_runtime_config(ctx);
                }
                config_panel::ConfigAction::SaveRequested => {
                    // Apply all buffered edit values to config
                    self.config_panel.apply_to_config(&mut self.config);
                    // Update theme
                    if let Some(t) = theme::Theme::get_theme(&self.config.theme) {
                        self.current_theme = t.clone();
                    }
                    // Apply runtime changes (fonts, GPU, renderer)
                    self.apply_runtime_config(ctx);
                    // Save to file
                    if let Err(e) = self.config.save() {
                        eprintln!("[Config] Failed to save: {}", e);
                    }
                }
                config_panel::ConfigAction::ResetToDefaults => {
                    self.config = config::Config::default();
                    self.current_theme =
                        theme::Theme::get_theme(&self.config.theme).unwrap_or_default();
                    self.apply_runtime_config(ctx);
                    self.config_panel.sync_from_config(&self.config);
                    self.config_panel.edit_debug_overlay = self.debug_panel.is_open;
                    self.schedule_config_save();
                }
                config_panel::ConfigAction::DebugPanelToggled(open) => {
                    self.debug_panel.is_open = open;
                }
            }
        }

        // Debug overlay panel
        {
            let session = self.session_manager.get_active_session_mut();
            let terminal = session.terminal.lock();
            let grid_cols = terminal.grid.cols();
            let grid_rows = terminal.grid.rows();
            let scrollback_used = terminal.scrollback.len();
            let kitty_images_count = terminal.kitty_graphics.image_count();
            let kitty_memory_mb = terminal.kitty_graphics.image_memory_mb();
            let scrollback_max = terminal.max_scrollback();
            drop(terminal);
            let session_count = self.session_manager.len();
            let pending_output_bytes = self.pending_output.len();
            let texture_cache_size = self.renderer.texture_cache_len();
            let frame_budget_kb = self.adaptive_frame_budget / 1024;
            self.debug_panel.show(
                ctx,
                grid_cols,
                grid_rows,
                session_count,
                scrollback_used,
                scrollback_max,
                kitty_images_count,
                kitty_memory_mb,
                pending_output_bytes,
                texture_cache_size,
                frame_budget_kb,
            );
        }
    }

    // adjust_frame_budget moved to app::rendering module
    // Config and session save methods moved to app::window module
}

impl eframe::App for TerminalApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Fully transparent clear color to support window-level opacity
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, _ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // UI handled in update()
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let preserve_paste_event = {
            let terminal = self
                .session_manager
                .get_active_session_mut()
                .terminal
                .lock();
            terminal.is_paste_events_enabled()
        };

        // Fix: egui-winit swallows Ctrl+V press when clipboard has no text (e.g. image only).
        // It calls `return` after checking clipboard, so neither Paste nor Key::V pressed
        // appears in raw_input — only Key::V released survives.
        // Detect this case and inject Key::V pressed so the terminal receives 0x16.
        let has_ctrl_v_release = raw_input.events.iter().any(|evt| {
            matches!(evt,
                egui::Event::Key { key: egui::Key::V, pressed: false, modifiers, .. }
                if modifiers.ctrl && !modifiers.shift
            )
        });
        let has_ctrl_v_press = raw_input.events.iter().any(|evt| {
            matches!(evt,
                egui::Event::Key { key: egui::Key::V, pressed: true, modifiers, .. }
                if modifiers.ctrl && !modifiers.shift
            )
        });
        let has_paste_event = raw_input
            .events
            .iter()
            .any(|evt| matches!(evt, egui::Event::Paste(_)));

        if has_ctrl_v_release && !has_ctrl_v_press && !has_paste_event {
            // Insert Key::V pressed before the release event
            raw_input.events.insert(
                0,
                egui::Event::Key {
                    key: egui::Key::V,
                    physical_key: Some(egui::Key::V),
                    pressed: true,
                    repeat: false,
                    modifiers: raw_input.modifiers,
                },
            );
        }

        // egui-winit turns Ctrl/Cmd+C/X/V into semantic clipboard events and skips the
        // corresponding Key press. Restore those as Key events so the terminal can receive
        // control bytes, while still preventing egui's default text-edit shortcut behavior.
        let restore_shortcuts = should_restore_terminal_shortcut_event(ctx, raw_input.modifiers);

        normalize_terminal_shortcut_events(
            &mut raw_input.events,
            raw_input.modifiers,
            restore_shortcuts,
            preserve_paste_event,
        );
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 检查是否收到退出信号（SIGINT/SIGTERM）
        if SHUTDOWN_REQUESTED.load(Ordering::Relaxed) {
            crate::debug_log!("[SIGNAL] Shutdown requested, exiting gracefully");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        self.debug_panel.record_frame();

        // 自适应调整帧预算：根据帧时间动态调整每帧处理字节数
        self.adjust_frame_budget();

        // Collect events once per frame to avoid multiple clones
        self.frame_events.clear();
        ctx.input(|i| self.frame_events.extend(i.events.iter().cloned()));

        let active_session_idx = self.session_manager.active_index();
        let session = self.session_manager.get_active_session_mut();

        // Step 1: 处理 IME 事件
        for evt in &self.frame_events {
            if let egui::Event::Ime(ime_event) = evt {
                let mut terminal = session.terminal.lock();
                match ime_event {
                    egui::ImeEvent::Enabled => {
                        crate::debug_log!("[IME] Enabled");
                        terminal.ime_enabled = true;
                    }
                    egui::ImeEvent::Preedit(text) => {
                        crate::debug_log!("[IME] Preedit: {:?}", text);
                        terminal.set_preedit(text.clone(), text.len());
                    }
                    egui::ImeEvent::Commit(text) => {
                        crate::debug_log!("[IME] Commit: {:?}", text);
                        terminal.clear_preedit();
                        if !text.is_empty() {
                            let _ = session.shell.write(text.as_bytes());
                        }
                        // 不要在 commit 时置 ime_enabled = false
                        // commit 只是确认一个字/词，不代表用户要退出中文输入模式
                        // 只有 ImeEvent::Disabled 才是真正的 IME 关闭信号
                    }
                    egui::ImeEvent::Disabled => {
                        crate::debug_log!("[IME] Disabled");
                        terminal.ime_enabled = false;
                        terminal.clear_preedit();
                    }
                }
            }
        }
        // 使用 terminal 持久状态判断是否有预编辑，而不是帧局部变量
        // 这样即使跨帧也能正确抑制 Text 事件
        let has_preedit = {
            let terminal = session.terminal.lock();
            !terminal.preedit_text.is_empty()
        };

        let window_title = {
            let terminal = session.terminal.lock();
            terminal.window_title.clone()
        };
        if !window_title.is_empty() && window_title != self.last_window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(window_title));
            self.last_window_title = {
                let terminal = session.terminal.lock();
                terminal.window_title.clone()
            };
        }

        // Step 1.5: 处理累积的Ctrl+滚轮字体缩放
        // 检查是否有ctrl+scroll事件
        let has_ctrl_scroll_this_frame = {
            let ctrl_pressed = ctx.input(|i| i.modifiers.ctrl);
            ctrl_pressed && self.frame_events.iter().any(|evt| {
                matches!(evt, egui::Event::MouseWheel { modifiers, .. } if modifiers.ctrl)
            })
        };

        // 如果有累积值，并且（滚轮事件停止 或 累积超过1.0），则应用变化
        if self.font_size_accumulator.abs() > 0.0 {
            let should_apply = !has_ctrl_scroll_this_frame // 滚轮停止
                || self.font_size_accumulator.abs() >= 1.0; // 或累积超过1.0

            if should_apply {
                let steps = self.font_size_accumulator.floor() as i32;
                if steps != 0 {
                    let new_font_size = config::Config::clamp_font_size(
                        self.config.font_size + steps as f32,
                    );

                    if (new_font_size - self.config.font_size).abs() > 0.01 {
                        self.config.font_size = new_font_size;
                        self.apply_runtime_config(ctx);
                        self.schedule_config_save();
                    }

                    // 保留小数部分
                    self.font_size_accumulator -= steps as f32;
                }

                // 如果滚轮停止，清空累积器
                if !has_ctrl_scroll_this_frame {
                    self.font_size_accumulator = 0.0;
                }
            }
        }

        self.had_ctrl_scroll_last_frame = has_ctrl_scroll_this_frame;

        // Step 2: 处理快捷键 - 使用可配置的快捷键系统

        // 命令调色板快捷键 (Ctrl+Shift+P) - toggle
        if ctx.input(|i| i.key_pressed(egui::Key::P) && i.modifiers.ctrl && i.modifiers.shift) {
            if self.command_palette.is_open {
                self.command_palette.close();
            } else {
                self.command_palette.open();
            }
        }

        // 帮助面板快捷键 (Ctrl+?)
        if ctx.input(|i| i.key_pressed(egui::Key::Slash) && i.modifiers.ctrl) {
            self.help_panel.toggle();
        }

        // Debug overlay 快捷键 (F12)
        if ctx.input(|i| i.key_pressed(egui::Key::F12)) {
            self.debug_panel.toggle();
        }

        // 当命令调色板打开时，处理其事件
        if self.command_palette.is_open {
            let events_copy = self.frame_events.clone();
            for evt in &events_copy {
                match evt {
                    egui::Event::Key {
                        key,
                        modifiers: _,
                        pressed,
                        ..
                    } if *pressed => {
                        match key {
                            egui::Key::Escape => {
                                self.command_palette.close();
                            }
                            egui::Key::ArrowUp => {
                                self.command_palette.select_prev();
                            }
                            egui::Key::ArrowDown => {
                                self.command_palette.select_next();
                            }
                            egui::Key::Enter => {
                                if let Some(command) = self.command_palette.get_selected_command() {
                                    self.command_palette.execute_command(command.clone());
                                    self.command_palette.close();
                                    // 执行命令
                                    match command {
                                        keybindings::Command::SearchOpen => {
                                            self.search_state.toggle();
                                        }
                                        keybindings::Command::SearchClose => {
                                            self.search_state.close();
                                        }
                                        keybindings::Command::SessionNew => {
                                            let new_idx =
                                                self.create_session_with_current_config(None, None);
                                            self.session_manager.switch_session(new_idx);
                                            self.force_resize_session = true;
                                            self.schedule_session_save();
                                        }
                                        keybindings::Command::SessionClose => {
                                            if self.session_manager.len() > 1 {
                                                let active_idx =
                                                    self.session_manager.active_index();
                                                self.session_manager.close_session(active_idx);
                                                self.schedule_session_save();
                                            } else {
                                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                                return;
                                            }
                                        }
                                        keybindings::Command::TerminalSendEof => {
                                            let session =
                                                self.session_manager.get_active_session_mut();
                                            let _ = session.shell.write(&[0x04]);
                                            // EOF (Ctrl+D)
                                        }
                                        keybindings::Command::SessionNext => {
                                            self.session_manager.switch_to_next_session();
                                            self.force_resize_session = true;
                                        }
                                        keybindings::Command::SessionPrev => {
                                            self.session_manager.switch_to_prev_session();
                                            self.force_resize_session = true;
                                        }
                                        keybindings::Command::SessionJump(n) => {
                                            if n < 9 {
                                                self.session_manager.switch_session(n);
                                                self.force_resize_session = true;
                                            }
                                        }
                                        keybindings::Command::TerminalScrollUp => {
                                            let session =
                                                self.session_manager.get_active_session_mut();
                                            let mut terminal = session.terminal.lock();
                                            if !terminal.is_alt_buffer_active() {
                                                terminal.scroll(3);
                                            }
                                        }
                                        keybindings::Command::TerminalScrollDown => {
                                            let session =
                                                self.session_manager.get_active_session_mut();
                                            let mut terminal = session.terminal.lock();
                                            if !terminal.is_alt_buffer_active() {
                                                terminal.scroll(-3);
                                            }
                                        }
                                        // 分屏命令处理
                                        keybindings::Command::TerminalSplitVertical => {
                                            // 垂直分割（左右）
                                            let new_session_idx =
                                                self.create_session_with_current_config(None, None);
                                            let _ =
                                                self.layout_manager.split(new_session_idx, false);
                                            self.status_message = "Split vertically".to_string();
                                            self.schedule_session_save();
                                        }
                                        keybindings::Command::TerminalSplitHorizontal => {
                                            // 水平分割（上下）
                                            let new_session_idx =
                                                self.create_session_with_current_config(None, None);
                                            let _ =
                                                self.layout_manager.split(new_session_idx, true);
                                            self.status_message = "Split horizontally".to_string();
                                            self.schedule_session_save();
                                        }
                                        keybindings::Command::TerminalClosePane => {
                                            // 关闭当前窗格
                                            if let Err(e) = self.layout_manager.close_focused_pane()
                                            {
                                                self.status_message = e;
                                            }
                                        }
                                        keybindings::Command::PaneFocusNext => {
                                            // 切换到下一个窗格
                                            self.layout_manager
                                                .focus_pane(layout::PaneDirection::Next);
                                        }
                                        keybindings::Command::PaneFocusPrev => {
                                            // 切换到前一个窗格
                                            self.layout_manager
                                                .focus_pane(layout::PaneDirection::Prev);
                                        }
                                        keybindings::Command::ConfigOpen => {
                                            self.config_panel.open(&self.config);
                                            self.config_panel.edit_debug_overlay =
                                                self.debug_panel.is_open;
                                        }
                                        keybindings::Command::ConfigClose => {
                                            self.config_panel.close();
                                        }
                                        keybindings::Command::ConfigToggle => {
                                            self.config_panel.toggle(&self.config);
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }

            // 如果调色板打开，不处理其他快捷键
            if self.command_palette.is_open {
                // 获取命令调色板选中的命令，但不执行（仅在按 Enter 时执行）
                // render_ui 中会显示调色板
                self.render_ui(ctx);
                return;
            }
        }

        // 收集所有按下的快捷键
        let pressed_keys: Vec<(egui::Key, egui::Modifiers)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|evt| {
                    if let egui::Event::Key {
                        key,
                        modifiers,
                        pressed: true,
                        ..
                    } = evt
                    {
                        Some((*key, *modifiers))
                    } else {
                        None
                    }
                })
                .collect()
        });

        // 处理每个按下的快捷键
        for (key, modifiers) in pressed_keys {
            if let Some(keybinding_str) = build_keybinding_string(key, modifiers) {
                let command = self.keybindings.get_command(&keybinding_str);
                crate::debug_log!(
                    "[KEYBINDING] Looking up: '{}' => {:?}",
                    keybinding_str,
                    command
                );
                if let Some(command) = command {
                    match command {
                        keybindings::Command::SearchOpen => {
                            self.search_state.toggle();
                        }
                        keybindings::Command::SearchClose => {
                            self.search_state.close();
                        }
                        keybindings::Command::SessionNew => {
                            let new_idx = self.create_session_with_current_config(None, None);
                            self.session_manager.switch_session(new_idx);
                            self.force_resize_session = true;
                            self.schedule_session_save();
                        }
                        keybindings::Command::SessionClose => {
                            if self.session_manager.len() > 1 {
                                self.session_manager.close_session(active_session_idx);
                                self.schedule_session_save();
                            } else {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                return;
                            }
                        }
                        keybindings::Command::TerminalSendEof => {
                            let session = self.session_manager.get_active_session_mut();
                            let _ = session.shell.write(&[0x04]); // EOF (Ctrl+D)
                        }
                        keybindings::Command::SessionNext => {
                            self.session_manager.switch_to_next_session();
                            self.force_resize_session = true;
                        }
                        keybindings::Command::SessionPrev => {
                            self.session_manager.switch_to_prev_session();
                            self.force_resize_session = true;
                        }
                        keybindings::Command::SessionJump(n) => {
                            if n < 9 {
                                self.session_manager.switch_session(n);
                                self.force_resize_session = true;
                            }
                        }
                        keybindings::Command::TerminalScrollUp => {
                            let session = self.session_manager.get_active_session_mut();
                            let mut terminal = session.terminal.lock();
                            if !terminal.is_alt_buffer_active() {
                                terminal.scroll(3);
                            }
                        }
                        keybindings::Command::TerminalScrollDown => {
                            let session = self.session_manager.get_active_session_mut();
                            let mut terminal = session.terminal.lock();
                            if !terminal.is_alt_buffer_active() {
                                terminal.scroll(-3);
                            }
                        }
                        keybindings::Command::ConfigOpen => {
                            self.config_panel.open(&self.config);
                            self.config_panel.edit_debug_overlay = self.debug_panel.is_open;
                        }
                        keybindings::Command::ConfigClose => {
                            self.config_panel.close();
                        }
                        keybindings::Command::ConfigToggle => {
                            self.config_panel.toggle(&self.config);
                        }
                        // 其他命令在下面处理
                        _ => {}
                    }
                }
            }
        }

        // 获取当前活跃会话（在所有快捷键处理完后）
        let session_count_before = self.session_manager.len();
        let mut shell_exited = false;
        let session = self.session_manager.get_active_session_mut();

        // Step 2.5: 搜索面板事件处理
        if self.search_state.is_open {
            let events_copy = self.frame_events.clone();
            for evt in &events_copy {
                match evt {
                    egui::Event::Key {
                        key,
                        modifiers,
                        pressed,
                        ..
                    } if *pressed => match key {
                        egui::Key::Escape => {
                            self.search_state.close();
                        }
                        egui::Key::Enter => {
                            if !modifiers.shift {
                                self.search_state.next_match();
                            } else {
                                self.search_state.prev_match();
                            }
                        }
                        egui::Key::ArrowUp => {
                            self.search_state.history_prev();
                        }
                        egui::Key::ArrowDown => {
                            self.search_state.history_next();
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }

        // Step 3: 处理复制粘贴（从配置系统或硬编码的 Ctrl+Shift+C/V）
        let events_copy = self.frame_events.clone();
        let mut consumed_keys = std::collections::HashSet::new();

        let mut saw_ctrl_shift_c = false;
        let mut saw_ctrl_shift_v = false;
        let mut saw_semantic_paste = false;

        for evt in &events_copy {
            match evt {
                egui::Event::Key {
                    key,
                    modifiers,
                    pressed,
                    ..
                } => {
                    // 检查 Ctrl+Shift+C/V（按下事件）
                    if *pressed {
                        if *key == egui::Key::C && modifiers.ctrl && modifiers.shift {
                            crate::debug_log!("[EVENT] detected Ctrl+Shift+C (pressed=true)");
                            saw_ctrl_shift_c = true;
                        }
                        if *key == egui::Key::V && modifiers.ctrl && modifiers.shift {
                            crate::debug_log!("[EVENT] detected Ctrl+Shift+V (pressed=true)");
                            saw_ctrl_shift_v = true;
                        }
                    }

                    // 注意：不再检测 Ctrl+V 释放事件。
                    // 当 restore_shortcuts=true 时，egui 的 Paste 事件已被转换为
                    // Key::V pressed，由 ui.rs 发送 0x16 给 PTY，让应用自己处理剪贴板。
                    // 之前这里检测 Key::V release 会导致终端也读剪贴板并发送文本内容，
                    // 造成双重粘贴（应用收到 0x16 + bracketed paste 文本）。
                    // Ctrl+V 粘贴只应通过 Ctrl+Shift+V（显式）或 semantic Paste 事件处理。
                }
                egui::Event::Paste(content) => {
                    crate::debug_log!(
                        "[EVENT] detected Paste event: {:?}",
                        if content.is_empty() {
                            "empty"
                        } else {
                            "has content"
                        }
                    );
                    saw_semantic_paste = true;
                }
                _ => {}
            }
        }

        if saw_ctrl_shift_c {
            if let Some(clipboard) = &self.clipboard {
                let terminal = session.terminal.lock();
                if let Some(text) = terminal.copy_selection() {
                    let _ = clipboard.copy(&text);
                    consumed_keys.insert("Ctrl+Shift+C".to_string());
                }
            }
        }

        if saw_ctrl_shift_v {
            crate::debug_log!("[PASTE] ===== Ctrl+Shift+V triggered =====");
            if let Some(clipboard) = &self.clipboard {
                crate::debug_log!("[PASTE] clipboard available");
                if let Ok(content) = clipboard.paste_contents() {
                    match content {
                        ClipboardContent::Text(text) => {
                            crate::debug_log!("[PASTE] content type: TEXT ({} chars)", text.len());
                            // 文本内容：按原来的方式处理（支持括号粘贴）
                            let bytes = text.replace("\r\n", "\n").into_bytes();
                            if !bytes.is_empty() {
                                let bracketed_paste = {
                                    let terminal = session.terminal.lock();
                                    terminal.is_bracketed_paste_enabled()
                                };

                                crate::debug_log!(
                                    "[PASTE] sending {} bytes (bracketed={})",
                                    bytes.len(),
                                    bracketed_paste
                                );
                                let paste_bytes = if bracketed_paste {
                                    wrap_bracketed_paste(bytes)
                                } else {
                                    bytes
                                };
                                let _ = session.shell.write(&paste_bytes);
                                consumed_keys.insert("Ctrl+Shift+V".to_string());
                            } else {
                                crate::debug_log!("[PASTE] text content is empty");
                            }
                        }
                        ClipboardContent::Binary(bytes) => {
                            crate::debug_log!(
                                "[PASTE] content type: BINARY ({} bytes)",
                                bytes.len()
                            );
                            // 二进制内容（如图像）：使用 Kitty 图像协议
                            if !bytes.is_empty() {
                                crate::debug_log!(
                                    "[PASTE] detecting MIME type for {} bytes...",
                                    bytes.len()
                                );
                                if let Some(mime_type) = detect_image_mime_type(&bytes) {
                                    crate::debug_log!("[PASTE] MIME type detected: {}", mime_type);
                                    let paste_packet = kitty_graphics_payload(mime_type, &bytes);
                                    crate::debug_log!("[KITTY] Ctrl+Shift+V pasting {} bytes with mime_type={}, packet_size={}",
                                                    bytes.len(), mime_type, paste_packet.len());
                                    session.shell.write_async(paste_packet);
                                    consumed_keys.insert("Ctrl+Shift+V".to_string());
                                } else {
                                    crate::debug_log!(
                                        "[PASTE] MIME type NOT detected, ignoring binary data"
                                    );
                                }
                            } else {
                                crate::debug_log!("[PASTE] binary content is empty");
                            }
                        }
                    }
                } else {
                    crate::debug_log!("[PASTE] failed to get clipboard content");
                }
            } else {
                crate::debug_log!("[PASTE] clipboard not available");
            }
            crate::debug_log!("[PASTE] ===== Ctrl+Shift+V finished =====");
        }

        if saw_semantic_paste {
            crate::debug_log!("[PASTE] ===== Semantic Paste triggered =====");
            let paste_events_enabled = {
                let terminal = session.terminal.lock();
                let paste_events_enabled = terminal.is_paste_events_enabled();
                crate::debug_log!(
                    "[PASTE] terminal paste_events_enabled (mode 5522): {}",
                    paste_events_enabled
                );
                paste_events_enabled
            };

            if paste_events_enabled && self.clipboard.is_some() {
                // 应用支持粘贴事件协议，发送 MIME 类型列表，让应用请求
                crate::debug_log!("[PASTE] app supports paste events, building paste event");
                let terminal = Arc::clone(&session.terminal);
                let pty = session.shell.pty_writer();
                let _ = std::thread::Builder::new()
                    .name("paste-event-sender".to_string())
                    .spawn(move || {
                        let mime_types = ClipboardManager::new()
                            .and_then(|clipboard| clipboard.available_mime_types())
                            .unwrap_or_default();
                        crate::debug_log!("[PASTE] available MIME types: {:?}", mime_types);
                        let bytes = terminal.lock().build_paste_event(&mime_types);
                        crate::debug_log!(
                            "[OSC5522] sending unsolicited paste MIME list ({} bytes)",
                            bytes.len()
                        );
                        if let Err(e) = ShellSession::write_to_pty(&pty, &bytes) {
                            crate::debug_log!("[PASTE] Failed to write paste event: {}", e);
                        }
                    });
                consumed_keys.insert("PasteEvent".to_string());
            } else {
                if self.clipboard.is_none() {
                    crate::debug_log!("[PASTE] clipboard not available");
                } else {
                    crate::debug_log!("[PASTE] app does NOT support paste events");
                }
                // 应用不支持粘贴事件协议，需要特殊处理不同类型的内容
                crate::debug_log!(
                    "[PASTE] fallback: app doesn't support paste events, handling content directly"
                );
                if let Some(clipboard) = &self.clipboard {
                    if let Ok(content) = clipboard.paste_contents() {
                        match content {
                            ClipboardContent::Text(text) => {
                                crate::debug_log!(
                                    "[PASTE] fallback: TEXT content ({} chars)",
                                    text.len()
                                );
                                // 文本内容：按原来的方式处理（支持括号粘贴）
                                let bytes = text.replace("\r\n", "\n").into_bytes();
                                if !bytes.is_empty() {
                                    let bracketed_paste = {
                                        let terminal = session.terminal.lock();
                                        terminal.is_bracketed_paste_enabled()
                                    };

                                    crate::debug_log!(
                                        "[PASTE] fallback: sending text {} bytes (bracketed={})",
                                        bytes.len(),
                                        bracketed_paste
                                    );
                                    let paste_bytes = if bracketed_paste {
                                        wrap_bracketed_paste(bytes)
                                    } else {
                                        bytes
                                    };
                                    let _ = session.shell.write(&paste_bytes);
                                    consumed_keys.insert("PasteEvent".to_string());
                                } else {
                                    crate::debug_log!("[PASTE] fallback: text is empty");
                                }
                            }
                            ClipboardContent::Binary(bytes) => {
                                crate::debug_log!(
                                    "[PASTE] fallback: BINARY content ({} bytes)",
                                    bytes.len()
                                );
                                // 二进制内容（如图像）：使用 Kitty 图像协议
                                if !bytes.is_empty() {
                                    crate::debug_log!("[PASTE] fallback: detecting MIME type...");
                                    if let Some(mime_type) = detect_image_mime_type(&bytes) {
                                        let paste_packet =
                                            kitty_graphics_payload(mime_type, &bytes);
                                        crate::debug_log!("[KITTY] fallback: pasting {} bytes with mime_type={}, packet_size={}",
                                                        bytes.len(), mime_type, paste_packet.len());
                                        session.shell.write_async(paste_packet);
                                        consumed_keys.insert("PasteEvent".to_string());
                                    } else {
                                        // 未知的二进制格式，不发送（防止破坏终端）
                                        crate::debug_log!("[PASTE] fallback: MIME type NOT detected, ignoring binary data");
                                    }
                                } else {
                                    crate::debug_log!("[PASTE] fallback: binary is empty");
                                }
                            }
                        }
                    } else {
                        crate::debug_log!("[PASTE] fallback: failed to get clipboard content");
                    }
                } else {
                    crate::debug_log!("[PASTE] fallback: clipboard not available");
                }
            }
            crate::debug_log!("[PASTE] ===== Semantic Paste finished =====");
        }

        // Step 4: 处理普通键盘输入
        // 当搜索面板或配置面板打开时，不处理普通键盘输入（面板会处理输入）
        // 复用缓冲区减少内存分配
        self.keyboard_input_buffer.clear();
        if !self.search_state.is_open && !self.config_panel.is_open {
            let (
                keyboard_enhancement_flags,
                report_all_keys_mode,
                xterm_modify_other_keys,
                xterm_format_other_keys,
                application_cursor_keys,
            ) = {
                let terminal = session.terminal.lock();
                (
                    terminal.keyboard_enhancement_flags(),
                    terminal.is_report_all_keys_enabled(),
                    terminal.xterm_modify_other_keys(),
                    terminal.xterm_format_other_keys(),
                    terminal.is_application_cursor_keys(),
                )
            };
            // 转换 consumed_keys 为需要的格式（HashSet<&str>）
            let consumed_keys_refs: std::collections::HashSet<&str> =
                consumed_keys.iter().map(|s| s.as_str()).collect();
            self.renderer.handle_keyboard_input(
                ctx,
                &mut self.keyboard_input_buffer,
                &consumed_keys_refs,
                has_preedit,
                keyboard_enhancement_flags,
                report_all_keys_mode,
                xterm_modify_other_keys,
                xterm_format_other_keys,
                application_cursor_keys,
                &self.frame_events,
            );
        }

        let has_keyboard_input = !self.keyboard_input_buffer.is_empty();
        let has_cursor_move_input = !self.renderer.cursor_move_input.is_empty();

        // 有输入活动时更新最后活动时间
        if has_keyboard_input || has_cursor_move_input {
            self.last_activity_time = std::time::Instant::now();
        }

        {
            let mut input_guard = self.input_queue.lock();
            if has_keyboard_input {
                input_guard.extend(&self.keyboard_input_buffer);
            }
            if has_cursor_move_input {
                input_guard.extend(&self.renderer.cursor_move_input);
                self.renderer.cursor_move_input.clear();
            }
            if !input_guard.is_empty() {
                session.terminal.lock().scroll_to_bottom();
                let _ = session.shell.write(&input_guard);
                input_guard.clear();
            }
        }

        // Force repaint if we have any keyboard/cursor input - ensures input renders immediately
        if has_keyboard_input || has_cursor_move_input {
            ctx.request_repaint();
        }

        // Step 6: 处理 shell 事件
        // 关键：限制每帧处理的总字节数，防止大量 ANSI 数据阻塞 UI 线程导致假死。
        // 超出限制的数据保存到 pending_output，下一帧继续处理。
        // 使用自适应帧预算，根据帧时间动态调整
        let mut has_new_output = false;
        let max_bytes_per_frame = self.adaptive_frame_budget;
        let mut has_more_data = false;

        // 先取回上一帧未处理完的数据
        let mut accumulated_data = std::mem::take(&mut self.pending_output);
        if !accumulated_data.is_empty() {
            has_new_output = true;
        }

        // 从 channel 中收集数据，直到达到字节上限
        if accumulated_data.len() < max_bytes_per_frame {
            loop {
                match session.shell.events().try_recv() {
                    Ok(ShellEvent::Output(data)) => {
                        accumulated_data.extend(data);
                        has_new_output = true;
                        if accumulated_data.len() >= max_bytes_per_frame {
                            has_more_data = true;
                            break;
                        }
                    }
                    Ok(ShellEvent::Exit(code)) => {
                        crate::debug_log!("[SHELL EXIT] shell exited with code: {}", code);
                        self.status_message = format!("Shell exited with code: {}", code);
                        has_new_output = true;
                        shell_exited = true;
                        break;
                    }
                    Ok(ShellEvent::Error(e)) => {
                        self.status_message = format!("Error: {}", e);
                        has_new_output = true;
                        break;
                    }
                    Err(crossbeam::channel::TryRecvError::Empty) => break,
                    Err(crossbeam::channel::TryRecvError::Disconnected) => {
                        shell_exited = true;
                        break;
                    }
                }
            }
        } else {
            has_more_data = true;
        }

        // 如果累积数据超过帧限制，将多余部分保存到下一帧
        if accumulated_data.len() > max_bytes_per_frame {
            self.pending_output = accumulated_data.split_off(max_bytes_per_frame);
            has_more_data = true;
        }
        // 也检查 channel 中是否还有数据
        if !has_more_data && !session.shell.events().is_empty() {
            has_more_data = true;
        }

        // 处理本帧的数据
        if !accumulated_data.is_empty() {
            let mut terminal = session.terminal.lock();
            terminal.process_batch(&accumulated_data);
            self.status_message.clear();
            // 有输出时更新最后活动时间
            self.last_activity_time = std::time::Instant::now();
        }

        // Step 7: 发送终端输出回 shell（DSR 响应等）
        {
            let mut terminal = session.terminal.lock();
            let output = terminal.get_output();
            if !output.is_empty() {
                let _ = session.shell.write(&output);
            }
            let clipboard_requests = terminal.take_clipboard_read_requests();
            drop(terminal);

            if self.clipboard.is_some() && !clipboard_requests.is_empty() {
                let terminal = Arc::clone(&session.terminal);
                let pty = session.shell.pty_writer();
                let _ = std::thread::Builder::new()
                    .name("clipboard-request-handler".to_string())
                    .spawn(move || {
                        let Ok(clipboard) = ClipboardManager::new() else {
                            crate::debug_log!("[OSC5522] Failed to create clipboard manager");
                            return;
                        };

                        for request in clipboard_requests {
                            match request.kind {
                                terminal::ClipboardReadKind::MimeList => {
                                    let mime_types = clipboard
                                        .available_mime_types()
                                        .unwrap_or_default();
                                    let response = terminal.lock().build_paste_event(&mime_types);
                                    if let Err(e) = ShellSession::write_to_pty(&pty, &response) {
                                        crate::debug_log!("[OSC5522] Failed to write mime list response: {}", e);
                                    }
                                }
                                terminal::ClipboardReadKind::MimeData(mime_type) => {
                                    let data = clipboard.read_mime(&mime_type).unwrap_or_default();
                                    let response = if data.is_empty() {
                                        osc_5522_packet("type=read:status=ENOSYS", None)
                                    } else {
                                        clipboard_5522_response_for_mime(&mime_type, &data)
                                    };
                                    crate::debug_log!(
                                        "[OSC5522] responding to mime request mime={} bytes={}",
                                        mime_type,
                                        data.len()
                                    );
                                    if let Err(e) = ShellSession::write_to_pty(&pty, &response) {
                                        crate::debug_log!("[OSC5522] Failed to write mime data response: {}", e);
                                    }
                                }
                            }
                        }
                    });
            }
        }

        // Step 8: 光标闪烁
        // 优化逻辑：只有在完全空闲时才闪烁，有活动时保持常显
        let mut cursor_state_changed = false;
        {
            let terminal = session.terminal.lock();
            let app_wants_cursor_visible = terminal.is_cursor_visible();
            drop(terminal);

            if app_wants_cursor_visible {
                let now = std::time::Instant::now();
                let idle_duration = now.duration_since(self.last_activity_time);
                const IDLE_THRESHOLD: Duration = Duration::from_millis(1500); // 1.5秒空闲后才开始闪烁

                if idle_duration < IDLE_THRESHOLD {
                    // 有活动或刚有活动，光标保持常显
                    if !self.cursor_visible {
                        self.cursor_visible = true;
                        cursor_state_changed = true;
                    }
                    // 重置下次闪烁时间为空闲阈值后
                    self.next_cursor_blink_time = self.last_activity_time + IDLE_THRESHOLD;
                } else {
                    // 完全空闲，开始闪烁
                    if now >= self.next_cursor_blink_time {
                        self.cursor_visible = !self.cursor_visible;
                        cursor_state_changed = true;

                        debug_log!(
                            "[CURSOR] idle blink toggle: {}, next in 1000ms",
                            self.cursor_visible
                        );

                        // 计算下一次改变的时间
                        self.next_cursor_blink_time = now + Duration::from_millis(1000);
                    }
                }
            } else if self.cursor_visible {
                self.cursor_visible = false;
                cursor_state_changed = true;
            }
        }

        // Step 9: 滚动处理
        // 优化：批量处理键盘滚动，只获取一次锁
        let scroll_amount = if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown) && i.modifiers.ctrl) {
            Some(-3)
        } else if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp) && i.modifiers.ctrl) {
            Some(3)
        } else if ctx.input(|i| i.key_pressed(egui::Key::PageUp) && !i.modifiers.ctrl) {
            let terminal = session.terminal.lock();
            let (_, rows) = terminal.get_dimensions();
            drop(terminal);
            Some(rows as isize)
        } else if ctx.input(|i| i.key_pressed(egui::Key::PageDown) && !i.modifiers.ctrl) {
            let terminal = session.terminal.lock();
            let (_, rows) = terminal.get_dimensions();
            drop(terminal);
            Some(-(rows as isize))
        } else {
            None
        };

        if let Some(amount) = scroll_amount {
            let mut terminal = session.terminal.lock();
            terminal.scroll(amount);
        }

        let scroll_delta = ctx.input(|i| i.smooth_scroll_delta.y);

        // 检查是否启用鼠标报告
        let mouse_enabled = {
            let terminal = session.terminal.lock();
            terminal.is_mouse_enabled()
        };

        // 鼠标滚轮处理：
        // 1. 如果应用启用了鼠标报告（如 vim），滚轮会在下面的鼠标处理部分发送给应用
        // 2. 如果应用未启用鼠标，或在普通终端，滚轮用于查看历史
        if scroll_delta != 0.0 && !mouse_enabled {
            self.scroll_accumulator += scroll_delta;
            let line_h = self.renderer.line_height.max(1.0);
            let lines = (self.scroll_accumulator / line_h) as isize;
            if lines != 0 {
                self.scroll_accumulator -= lines as f32 * line_h;
                let scroll_lines = lines * self.config.scroll_speed as isize;
                let mut terminal = session.terminal.lock();
                terminal.scroll(scroll_lines);
            }
        }

        // Step 11: 鼠标处理（包括滚轮）
        let mouse_reports: Vec<String> = {
            let terminal = session.terminal.lock();
            if !terminal.is_mouse_enabled() {
                self.mouse_scroll_accumulator = 0.0;
                drop(terminal);
                Vec::new()
            } else {
                let mut reports = Vec::new();

                // 获取鼠标位置信息
                if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
                    let screen_rect = ctx.viewport_rect();
                    let char_width = self.renderer.char_width;
                    let line_height = self.renderer.line_height;

                    let clamped_x = (pos.x - screen_rect.left()).max(0.0);
                    let clamped_y = (pos.y - screen_rect.top()).max(0.0);

                    let col = if char_width > 0.0 {
                        ((clamped_x / char_width) as usize).min(self.cols - 1)
                    } else {
                        0
                    };
                    let row = if line_height > 0.0 {
                        ((clamped_y / line_height) as usize).min(self.rows - 1)
                    } else {
                        0
                    };

                    // 处理鼠标滚轮（当启用鼠标报告时）
                    let line_h = self.renderer.line_height.max(1.0);
                    let mut discrete_scroll_steps: isize = 0;
                    let mut point_scroll_delta: f32 = 0.0;

                    ctx.input(|i| {
                        for event in &i.events {
                            if let egui::Event::MouseWheel { unit, delta, .. } = event {
                                match unit {
                                    egui::MouseWheelUnit::Line => {
                                        discrete_scroll_steps += delta.y.round() as isize;
                                    }
                                    egui::MouseWheelUnit::Page => {
                                        discrete_scroll_steps +=
                                            delta.y.round() as isize * self.rows.max(1) as isize;
                                    }
                                    egui::MouseWheelUnit::Point => {
                                        point_scroll_delta += delta.y;
                                    }
                                }
                            }
                        }
                    });

                    if point_scroll_delta != 0.0 {
                        self.mouse_scroll_accumulator += point_scroll_delta;
                    }

                    let point_scroll_steps = (self.mouse_scroll_accumulator / line_h) as isize;
                    if point_scroll_steps != 0 {
                        self.mouse_scroll_accumulator -= point_scroll_steps as f32 * line_h;
                    }

                    let total_scroll_steps = discrete_scroll_steps + point_scroll_steps;
                    if total_scroll_steps != 0 {
                        let button = if total_scroll_steps > 0 { 64 } else { 65 };

                        for _ in 0..total_scroll_steps.unsigned_abs() {
                            if let Some(report) = terminal.get_mouse_report(button, col, row) {
                                reports.push(report);
                            }
                        }
                    }

                    // 处理鼠标按钮（使用 SmallVec 避免堆分配）
                    let button_pressed = ctx.input(|i| {
                        let mut btns: SmallVec<[u8; 3]> = SmallVec::new();
                        if i.pointer.button_pressed(egui::PointerButton::Primary) {
                            btns.push(0);
                        }
                        if i.pointer.button_pressed(egui::PointerButton::Secondary) {
                            btns.push(2);
                        }
                        if i.pointer.button_pressed(egui::PointerButton::Middle) {
                            btns.push(1);
                        }
                        btns
                    });

                    for button_num in button_pressed {
                        if let Some(report) = terminal.get_mouse_report(button_num, col, row) {
                            reports.push(report);
                        }
                    }
                }

                drop(terminal);
                reports
            }
        };

        let has_mouse_input = !mouse_reports.is_empty();
        if has_mouse_input {
            for report in mouse_reports {
                let _ = session.shell.write(report.as_bytes());
            }
        }

        // Step 12: 链接检测和交互
        {
            let mut terminal = session.terminal.lock();
            let grid_version = terminal.get_grid_version();
            let scroll_offset = terminal.scroll_offset;

            if grid_version != self.cached_links_grid_version
                || scroll_offset != self.cached_links_scroll_offset
            {
                let visible_cells = terminal.get_visible_cells();
                let row_wrapped = terminal.get_visible_row_wrapped();
                self.cached_links = self.link_detector.detect_links_in_visible_cells_with_wrapping(&visible_cells, &row_wrapped);
                self.cached_links_grid_version = grid_version;
                self.cached_links_scroll_offset = scroll_offset;
            }
            let links = self.cached_links.clone();
            drop(terminal);

            // 检测悬停的链接
            self.hovered_link = None;
            if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
                if let Some(content_rect) = self.renderer.last_content_rect {
                    let char_width = self.renderer.char_width;
                    let line_height = self.renderer.line_height;

                    let clamped_x =
                        (pos.x - content_rect.left()).clamp(0.0, content_rect.width().max(0.0));
                    let clamped_y =
                        (pos.y - content_rect.top()).clamp(0.0, content_rect.height().max(0.0));

                    let col = if char_width > 0.0 {
                        ((clamped_x / char_width) as usize).min(self.cols - 1)
                    } else {
                        0
                    };
                    let row = if line_height > 0.0 {
                        ((clamped_y / line_height) as usize).min(self.rows - 1)
                    } else {
                        0
                    };

                    // 查找当前位置是否有链接
                    if content_rect.contains(pos) {
                        for link in &links {
                            if link.line == row && col >= link.col_start && col < link.col_end {
                                self.hovered_link = Some(link.clone());
                                // 设置鼠标光标为手型
                                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                                break;
                            }
                        }
                    }
                }
            }

            // 处理 Ctrl+Click 打开链接
            if ctx.input(|i| {
                i.pointer.button_clicked(egui::PointerButton::Primary) && i.modifiers.ctrl
            }) {
                if let Some(link) = &self.hovered_link {
                    match link::open_link(link) {
                        Ok(_) => {
                            self.status_message = format!("Opened: {}", link.text);
                        }
                        Err(e) => {
                            self.status_message = format!("Failed to open link: {}", e);
                        }
                    }
                }
            }
        }

        // 检查光标闪烁是否需要进行（在render_ui之前完成）
        let app_wants_cursor_visible = {
            let terminal = session.terminal.lock();
            terminal.is_cursor_visible()
        };

        // 结束 session 的可变借用，render_ui 需要 &mut self
        #[allow(dropping_references)]
        drop(session);

        // 渲染 UI
        self.render_ui(ctx);

        // channel 中还有未处理的数据时，立即请求下一帧继续处理
        if has_more_data {
            ctx.request_repaint();
        } else {
            // 二次检查：render_ui 期间 PTY 线程可能又发送了新数据
            let has_pending_data = if !has_new_output {
                let session = self.session_manager.get_active_session_mut();
                !session.shell.events().is_empty()
            } else {
                false
            };
            let has_new_output = has_new_output || has_pending_data;

            let should_repaint = has_new_output
                || cursor_state_changed
                || has_keyboard_input
                || has_cursor_move_input
                || has_mouse_input
                || self.debug_panel.is_open;

            if should_repaint {
                ctx.request_repaint();
            } else if app_wants_cursor_visible {
                let now = std::time::Instant::now();
                let time_until_next = self.next_cursor_blink_time.saturating_duration_since(now);
                if time_until_next.as_millis() == 0 {
                    ctx.request_repaint();
                } else {
                    ctx.request_repaint_after(time_until_next);
                }
            } else {
                // 安全网：1000ms 超时防止极端竞态
                ctx.request_repaint_after(std::time::Duration::from_millis(1000));
            }
        }

        // Debounce 保存配置和会话
        self.flush_config_save();
        self.flush_session_save();

        // Handle shell exit: close current session
        if shell_exited {
            crate::debug_log!(
                "[SHELL EXIT] handling shell exit, session_count: {}",
                session_count_before
            );
            if session_count_before > 1 {
                // Close the current session if there are multiple sessions
                self.session_manager.close_session(active_session_idx);
                self.schedule_session_save();
                crate::debug_log!(
                    "[SHELL EXIT] closed session, remaining: {}",
                    self.session_manager.len()
                );
            } else {
                // Close the window if this is the only session
                crate::debug_log!("[SHELL EXIT] closing window");
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

impl Drop for TerminalApp {
    fn drop(&mut self) {
        // 保存配置
        if self.config_save_pending {
            if let Err(e) = self.config.save() {
                eprintln!("[Config] Failed to save on exit: {}", e);
            }
        }

        // 保存当前会话到持久化存储（包含每个 session 的 cwd 和 restorable commands）
        if let Ok(session_history_path) = config::Config::session_history_path() {
            let _ = session_persistence::ensure_session_history_dir(&session_history_path);

            let snapshots = self.session_manager.get_session_snapshots();
            let active_index = Some(self.session_manager.active_index());
            let snapshot =
                session_persistence::SessionsSnapshot::from_snapshots(snapshots, active_index);
            if let Err(e) = snapshot.save(&session_history_path) {
                eprintln!("[SessionPersistence] Failed to save sessions: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_terminal_shortcut_events, shortcut_event_to_key_event};
    use eframe::egui;

    #[test]
    fn copy_event_becomes_ctrl_c_key_event() {
        let modifiers = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };

        let event = shortcut_event_to_key_event(egui::Event::Copy, modifiers)
            .expect("copy event should map to a key event");

        assert_eq!(
            event,
            egui::Event::Key {
                key: egui::Key::C,
                physical_key: Some(egui::Key::C),
                pressed: true,
                repeat: false,
                modifiers,
            }
        );
    }

    #[test]
    fn paste_event_becomes_ctrl_shift_v_key_event_when_restored() {
        let modifiers = egui::Modifiers {
            ctrl: true,
            shift: true,
            command: true,
            ..Default::default()
        };
        let mut events = vec![egui::Event::Paste("ignored clipboard payload".to_owned())];

        normalize_terminal_shortcut_events(&mut events, modifiers, true, false);

        assert_eq!(
            events,
            vec![egui::Event::Key {
                key: egui::Key::V,
                physical_key: Some(egui::Key::V),
                pressed: true,
                repeat: false,
                modifiers,
            }]
        );
    }

    #[test]
    fn semantic_clipboard_events_are_dropped_when_not_restored() {
        let modifiers = egui::Modifiers::default();
        let mut events = vec![
            egui::Event::Copy,
            egui::Event::Paste("ignored".to_owned()),
            egui::Event::Text("a".to_owned()),
        ];

        normalize_terminal_shortcut_events(&mut events, modifiers, false, false);

        assert_eq!(events, vec![egui::Event::Text("a".to_owned())]);
    }

    #[test]
    fn semantic_paste_event_is_preserved_when_requested() {
        let modifiers = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let mut events = vec![egui::Event::Paste("ignored".to_owned())];

        normalize_terminal_shortcut_events(&mut events, modifiers, true, true);

        assert_eq!(events, vec![egui::Event::Paste("ignored".to_owned())]);
    }
}
