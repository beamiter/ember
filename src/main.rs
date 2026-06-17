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
mod keybindings;
mod kitty_graphics;
mod layout;
mod link;
mod pty;
mod search;
mod search_replace;
mod search_replace_panel;
mod session;
mod session_manager;
mod session_persistence;
mod shell;
mod sidebar;
mod terminal;
mod theme;
mod ui;
mod windows_compat;

use app::events::{normalize_terminal_shortcut_events, should_restore_terminal_shortcut_event};
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
    let _hex_preview = if data.len() >= 8 {
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
        _hex_preview
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
                cfg.subpixel_rendering,
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
        let color_atlas_placeholder = render_state.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("color_atlas_init"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let color_atlas_view = color_atlas_placeholder.create_view(&wgpu::TextureViewDescriptor::default());
        let color_atlas_sampler = render_state.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("color_atlas_sampler_init"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let pipeline = gpu::pipeline::GridPipeline::new(
            &render_state.device,
            render_state.target_format,
            atlas.gpu_resources().0,
            atlas.gpu_resources().1,
            &color_atlas_view,
            &color_atlas_sampler,
        );

        let mut renderer = render_state.renderer.write();
        if let Some(gpu_res) = renderer
            .callback_resources
            .get_mut::<gpu::callback::GpuResources>()
        {
            gpu_res.atlas = atlas;
            gpu_res.pipeline = pipeline;
        } else {
            let gpu_resources = gpu::callback::GpuResources::new(atlas, pipeline, &render_state.device);
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

            TerminalApp::new(
                &cfg_clone,
                cc.egui_ctx.clone(),
                cc.wgpu_render_state.clone(),
            )
            .map(|app| Box::new(app) as Box<dyn eframe::App>)
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })
        }),
    )
}

// TerminalApp struct definition moved to app::state module
use app::state::TerminalApp;

// normalize_terminal_shortcut_events moved to app::events module

/// 用单引号安全包裹路径，供发送到 shell 的 cd 命令使用
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn wrap_bracketed_paste(payload: Vec<u8>) -> Vec<u8> {
    // 安全:剔除 payload 内嵌的粘贴结束序列 ESC[201~,否则恶意剪贴板可
    // 提前结束粘贴模式并注入随后被 shell 执行的命令(bracketed-paste 注入)。
    let end = b"\x1b[201~";
    let mut sanitized = Vec::with_capacity(payload.len());
    let mut i = 0;
    while i < payload.len() {
        if payload[i..].starts_with(end) {
            i += end.len();
        } else {
            sanitized.push(payload[i]);
            i += 1;
        }
    }
    let mut wrapped = Vec::with_capacity(sanitized.len() + 12);
    wrapped.extend_from_slice(b"\x1b[200~");
    wrapped.append(&mut sanitized);
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
    ) -> std::result::Result<Self, String> {
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
                match ShellSession::new(
                    cols,
                    rows,
                    configured_shell.as_deref(),
                    repaint_ctx.clone(),
                ) {
                    Ok(session) => session,
                    Err(e2) => {
                        return Err(format!(
                            "Cannot create shell session: {} (after fallback from: {})",
                            e2, e
                        ));
                    }
                }
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
        renderer.font_ligatures = cfg.font_ligatures;
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
            pr.font_ligatures = cfg.font_ligatures;
            pr.gpu_rendering = cfg.gpu_rendering;
            pr.wgpu_render_state = wgpu_render_state.clone();
            pane_renderers.push(pr);
        }

        Ok(TerminalApp {
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
            sidebar: {
                let mut sb = sidebar::Sidebar::new();
                sb.visible = false; // 默认隐藏，opt-in 切换
                sb.view = cfg.sidebar_view; // 恢复上次记住的视图(默认会话)
                sb
            },
            search_replace_panel: search_replace_panel::SearchReplacePanel::new(),
            link_detector: link::LinkDetector::new(link::LinkDetectionConfig::default()),
            hovered_link: None,
            cached_links: Vec::new(),
            cached_links_grid_version: 0,
            cached_links_scroll_offset: 0,
            cached_links_session_idx: usize::MAX,
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
            mouse_scroll_accumulator: 0.0,
            font_size_accumulator: 0.0,
            had_ctrl_scroll_last_frame: false,
            frame_events: Vec::new(),
            keyboard_input_buffer: Vec::new(),
            adaptive_frame_budget: 32768, // 初始值 32KB
            config_last_mtime: config::Config::config_mtime(),
            config_last_check: std::time::Instant::now(),
            smooth_scroll_velocity: 0.0,
            smooth_scroll_pixel_offset: 0.0,
        })
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
        self.renderer.font_ligatures = self.config.font_ligatures;
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
            renderer.font_ligatures = self.config.font_ligatures;
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

    /// 顶部水平 tab 栏是否应显示：Top 模式下始终显示。
    /// 即便只有一个会话也保留，因为栏内含有侧边栏 toggle 控件。
    fn show_top_tab_bar(&self) -> bool {
        matches!(self.config.tab_bar_position, config::TabBarPosition::Top)
    }

    /// 切换标签栏位置(顶部 ⇄ 侧边栏)，并同步侧边栏视图与配置。
    /// 由顶栏内的位置切换按钮调用(两种模式下均可触发)。
    fn toggle_tab_bar_position(&mut self) {
        self.config.tab_bar_position = match self.config.tab_bar_position {
            config::TabBarPosition::Top => config::TabBarPosition::Sidebar,
            config::TabBarPosition::Sidebar => config::TabBarPosition::Top,
        };
        if matches!(self.config.tab_bar_position, config::TabBarPosition::Top) {
            // 切回顶部模式时把侧边栏视图复位到文件视图，避免停留在 Sessions
            self.sidebar.view = sidebar::SidebarView::Files;
        } else {
            // 标签移入侧边栏：恢复上次记住的视图并确保侧边栏可见，否则标签不可达
            self.sidebar.view = self.config.sidebar_view;
            self.sidebar.visible = true;
            self.sidebar.refresh();
        }
        self.config_panel.sync_from_config(&self.config);
        self.schedule_config_save();
    }

    /// 渲染左侧文件树侧边栏。必须在 CentralPanel 之前调用，
    /// 否则中央区域不会正确收缩。
    #[allow(deprecated)]
    fn render_sidebar(&mut self, ctx: &egui::Context) {
        if !self.sidebar.visible {
            // 展开按钮统一由顶部栏内的 ☰ 负责(Top 模式在 tab 栏，Sidebar 模式在精简顶部栏)，
            // 不再使用浮动按钮，避免覆盖终端内容。
            return;
        }

        // 侧边栏 tab 模式：允许在「会话」与「文件」视图间切换；其余模式锁定为文件视图
        let sidebar_tab_mode =
            matches!(self.config.tab_bar_position, config::TabBarPosition::Sidebar);
        if !sidebar_tab_mode {
            self.sidebar.view = sidebar::SidebarView::Files;
        }

        // 树遍历期间只收集动作，闭包结束后再 mutate，规避借用冲突
        let mut toggle_path: Option<std::path::PathBuf> = None;
        let mut select_path: Option<std::path::PathBuf> = None;
        let mut cd_path: Option<std::path::PathBuf> = None;
        let mut do_refresh = false;
        let mut view_changed = false;

        let panel_bg = theme::Theme::rgb_to_color32(self.current_theme.ui.panel_bg);
        egui::SidePanel::left("file_tree")
            .resizable(true)
            .default_width(self.sidebar.width)
            .frame(egui::Frame::NONE.fill(panel_bg).inner_margin(6.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if sidebar_tab_mode {
                        // 分区切换：会话 / 文件
                        if ui
                            .selectable_label(
                                self.sidebar.view == sidebar::SidebarView::Sessions,
                                egui::RichText::new("Sessions").strong(),
                            )
                            .clicked()
                        {
                            self.sidebar.view = sidebar::SidebarView::Sessions;
                            view_changed = true;
                        }
                        if ui
                            .selectable_label(
                                self.sidebar.view == sidebar::SidebarView::Files,
                                egui::RichText::new("Files").strong(),
                            )
                            .clicked()
                        {
                            self.sidebar.view = sidebar::SidebarView::Files;
                            view_changed = true;
                        }
                    } else {
                        ui.label(egui::RichText::new("Files").strong());
                        if ui.button("⟳").on_hover_text("Refresh").clicked() {
                            do_refresh = true;
                        }
                    }
                });
                ui.separator();

                if self.sidebar.view == sidebar::SidebarView::Sessions {
                    self.render_sidebar_sessions(ui);
                } else {
                    if let Some(dir) =
                        self.sidebar.current_dir.file_name().and_then(|n| n.to_str())
                    {
                        ui.label(egui::RichText::new(dir).weak().small());
                    }
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if let Some(root) = &self.sidebar.root {
                            for child in &root.children {
                                Self::draw_tree_node(
                                    ui,
                                    child,
                                    &self.sidebar.selected_path,
                                    &mut toggle_path,
                                    &mut select_path,
                                    &mut cd_path,
                                );
                            }
                        }
                    });
                }
            });

        // 闭包结束，安全 mutate
        if let Some(p) = toggle_path {
            self.sidebar.toggle_node(&p);
        }
        if let Some(p) = select_path {
            self.sidebar.selected_path = Some(p);
        }
        if let Some(p) = cd_path {
            let quoted = shell_single_quote(&p.to_string_lossy());
            let cmd = format!("cd {}\n", quoted);
            let session = self.session_manager.get_active_session_mut();
            let _ = session.shell.write(cmd.as_bytes());
            self.sidebar.set_current_dir(p);
        }
        if do_refresh {
            self.sidebar.refresh();
        }
        if view_changed {
            // 记住用户在侧边栏 tab 模式下选择的视图，下次默认沿用
            self.config.sidebar_view = self.sidebar.view;
            self.schedule_config_save();
        }
    }

    /// 递归绘制文件树节点（关联函数，不持 &self 以避免借用冲突）
    fn draw_tree_node(
        ui: &mut egui::Ui,
        node: &sidebar::FileTreeNode,
        selected: &Option<std::path::PathBuf>,
        toggle: &mut Option<std::path::PathBuf>,
        select: &mut Option<std::path::PathBuf>,
        cd: &mut Option<std::path::PathBuf>,
    ) {
        let is_selected = selected.as_deref() == Some(node.path.as_path());
        if node.is_dir {
            let arrow = if node.expanded { "▼" } else { "▶" };
            let label = format!("{} 📁 {}", arrow, node.name);
            let resp = ui.selectable_label(is_selected, label);
            if resp.clicked() {
                *toggle = Some(node.path.clone());
                *select = Some(node.path.clone());
            }
            if resp.double_clicked() {
                *cd = Some(node.path.clone());
            }
            resp.on_hover_text("单击展开/折叠，双击进入目录 (cd)");
            if node.expanded {
                ui.indent(node.path.to_string_lossy(), |ui| {
                    for child in &node.children {
                        Self::draw_tree_node(ui, child, selected, toggle, select, cd);
                    }
                });
            }
        } else {
            let resp = ui.selectable_label(is_selected, format!("📄 {}", node.name));
            if resp.clicked() {
                *select = Some(node.path.clone());
            }
        }
    }

    #[allow(deprecated)]
    fn render_ui(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::NONE.inner_margin(0.0);

        // 顶部栏(全宽)：必须在 render_sidebar 之前声明，egui 会把先声明的面板
        // 分配到容器边缘的完整范围 —— 因此顶栏横跨整个窗口，侧边栏落在其下方，
        // 而不是侧边栏贯穿到顶部。
        let mut close_requested = false;
        egui::TopBottomPanel::top("top_bar")
            .frame(egui::Frame::NONE)
            .resizable(false)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                // Top 模式：完整水平标签栏(含 ☰ 与位置切换控件)。
                // Sidebar 模式：精简顶栏(仅 ☰ 与位置切换控件)，标签在侧边栏内。
                if self.show_top_tab_bar() {
                    if self.render_tab_bar(ui, ctx) {
                        close_requested = true;
                    }
                } else {
                    self.render_sidebar_mode_top_bar(ui, ctx);
                }
            });
        if close_requested {
            return;
        }

        // 侧边栏：在顶栏之后声明，占据顶栏下方区域的左侧。
        self.render_sidebar(ctx);

        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            self.render_terminal_content(ui, ctx);
        });

        self.render_floating_panels(ctx);
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
        let has_preedit = self.handle_ime_events(ctx);

        self.handle_font_zoom(ctx);

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

        if self.handle_command_palette_input(ctx) {
            return;
        }

        if self.handle_keybindings(ctx, active_session_idx) {
            return;
        }

        // 获取当前活跃会话（在所有快捷键处理完后）
        let session_count_before = self.session_manager.len();
        let mut shell_exited = false;

        // Step 2.5: 搜索面板事件处理
        self.handle_search_panel_input();

        let session = self.session_manager.get_active_session_mut();

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
                egui::Event::Paste(_content) => {
                    crate::debug_log!(
                        "[EVENT] detected Paste event: {:?}",
                        if _content.is_empty() {
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
                    if let Err(e) = clipboard.copy(&text) { log::warn!("{}", e); }
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
                let write_tx = session.shell.write_sender();
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
                        let _ = write_tx.send(bytes);
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
            terminal.check_sync_output_timeout();
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
                let write_tx = session.shell.write_sender();
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
                                    let _ = write_tx.send(response);
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
                                    let _ = write_tx.send(response);
                                }
                            }
                        }
                    });
            }
        }

        // OSC 52 clipboard handling
        {
            let mut terminal = session.terminal.lock();
            if let Some(text) = terminal.take_osc52_clipboard_set() {
                if self.config.osc52_clipboard_write {
                    if let Some(clipboard) = &self.clipboard {
                        if let Err(e) = clipboard.copy(&text) { log::warn!("{}", e); }
                    }
                }
            }
            if terminal.take_osc52_clipboard_query() {
                // 读取剪贴板会把内容回传给终端内程序,默认禁止,需显式开启。
                if self.config.osc52_clipboard_read {
                    let content = self
                        .clipboard
                        .as_ref()
                        .and_then(|c| c.paste().ok())
                        .unwrap_or_default();
                    terminal.respond_osc52_clipboard(&content);
                }
            }
        }

        // OSC 9/777 desktop notifications
        {
            let mut terminal = session.terminal.lock();
            let notifications: Vec<_> = terminal.pending_notifications.drain(..).collect();
            drop(terminal);
            for (title, body) in notifications {
                // `--` 终止选项解析,防止以 `-`/`--` 开头的标题或正文被当作 notify-send 选项注入。
                let _ = std::process::Command::new("notify-send")
                    .arg("--")
                    .arg(&title)
                    .arg(&body)
                    .spawn();
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
            // 0.35 阻尼系数：原始的 scroll_speed 直接乘 delta 会让单次滚轮累积约 7 倍位移，滑得太快
            const SCROLL_VELOCITY_DAMPING: f32 = 0.35;
            self.smooth_scroll_velocity +=
                scroll_delta * self.config.scroll_speed as f32 * SCROLL_VELOCITY_DAMPING;
        }

        // Smooth scroll physics
        if self.smooth_scroll_velocity.abs() > 0.1 {
            self.smooth_scroll_velocity *= 0.88;

            let line_h = self.renderer.line_height.max(1.0);

            // 抵达边界检测：在累积偏移前先看当前是否已到顶/到底(或处于备用屏幕)。
            // 若惯性继续往边界外推，会出现"跨行 → scroll 被钳制 → 偏移回弹"的逐帧抖动。
            let mut hit_boundary = {
                let terminal = session.terminal.lock();
                let at_top = terminal.scroll_offset >= terminal.scrollback_len();
                let at_bottom = terminal.scroll_offset == 0;
                let alt = terminal.is_alt_buffer();
                alt || (self.smooth_scroll_velocity > 0.0 && at_top)
                    || (self.smooth_scroll_velocity < 0.0 && at_bottom)
            };

            if !hit_boundary {
                self.smooth_scroll_pixel_offset += self.smooth_scroll_velocity;
                let lines = (self.smooth_scroll_pixel_offset / line_h) as isize;
                if lines != 0 {
                    self.smooth_scroll_pixel_offset -= lines as f32 * line_h;
                    let mut terminal = session.terminal.lock();
                    let before = terminal.scroll_offset as isize;
                    terminal.scroll(lines);
                    // 实际移动行数不等于请求行数 => 在本帧触及边界，立即停下惯性。
                    if terminal.scroll_offset as isize - before != lines {
                        hit_boundary = true;
                    }
                }
            }

            if hit_boundary {
                self.smooth_scroll_velocity = 0.0;
                self.smooth_scroll_pixel_offset = 0.0;
            }

            // 渲染偏移取负：shader 中 +offset 使内容上移，而 terminal.scroll(+lines)
            // 是向历史滚动(内容下移)。两者方向必须一致，否则每跨一行就会出现约
            // 2*行高的回跳——滚轮停下后的低速惯性阶段表现为上下抖动。
            self.renderer.scroll_pixel_offset = -self.smooth_scroll_pixel_offset;
            for pr in &mut self.pane_renderers {
                pr.scroll_pixel_offset = -self.smooth_scroll_pixel_offset;
            }
            if !hit_boundary {
                ctx.request_repaint();
            }
        } else if self.smooth_scroll_velocity.abs() > 0.0 {
            self.smooth_scroll_velocity = 0.0;
            self.smooth_scroll_pixel_offset = 0.0;
            self.renderer.scroll_pixel_offset = 0.0;
            for pr in &mut self.pane_renderers {
                pr.scroll_pixel_offset = 0.0;
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

                    let button_released = ctx.input(|i| {
                        let mut btns: SmallVec<[u8; 3]> = SmallVec::new();
                        if i.pointer.button_released(egui::PointerButton::Primary) {
                            btns.push(0);
                        }
                        if i.pointer.button_released(egui::PointerButton::Secondary) {
                            btns.push(2);
                        }
                        if i.pointer.button_released(egui::PointerButton::Middle) {
                            btns.push(1);
                        }
                        btns
                    });

                    for button_num in button_released {
                        if let Some(report) = terminal.get_mouse_release_report(button_num, col, row) {
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
                || active_session_idx != self.cached_links_session_idx
            {
                let visible_cells = terminal.get_visible_cells();
                let row_wrapped = terminal.get_visible_row_wrapped();
                self.cached_links = self.link_detector.detect_links_in_visible_cells_with_wrapping(&visible_cells, &row_wrapped);
                self.cached_links_grid_version = grid_version;
                self.cached_links_scroll_offset = scroll_offset;
                self.cached_links_session_idx = active_session_idx;
            }
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

                    if content_rect.contains(pos) {
                        for link in &self.cached_links {
                            if link.line == row && col >= link.col_start && col < link.col_end {
                                self.hovered_link = Some(link.clone());
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
        self.check_config_hot_reload();

        // Handle shell exit: close current session
        if shell_exited {
            crate::debug_log!(
                "[SHELL EXIT] handling shell exit, session_count: {}",
                session_count_before
            );
            if session_count_before > 1 {
                // Close the current session if there are multiple sessions
                self.close_session_synced(active_session_idx);
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
    use crate::app::events::{normalize_terminal_shortcut_events, shortcut_event_to_key_event};
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
