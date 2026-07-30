mod agent_panel;
mod app;
mod clipboard;
mod color;
mod command_palette;
mod config;
mod config_panel;
mod debug;
mod debug_panel;
mod execution_journal;
mod gpu;
mod help;
mod history_persistence;
mod jsh_ui;
mod keybindings;
mod kitty_graphics;
mod layout;
mod link;
mod pane_header;
mod pty;
mod search;
mod search_replace;
mod search_replace_panel;
mod session;
mod session_manager;
mod session_persistence;
mod shell;
mod sidebar;
mod tab_manager;
mod terminal;
mod theme;
mod ui;
mod windows_compat;

use crate::theme::ThemeExt as _;
use app::events::{
    normalize_terminal_shortcut_events, restore_missing_image_paste_key_event,
    semantic_paste_modifiers, should_restore_terminal_shortcut_event,
};
use base64::Engine;
use clipboard::{ClipboardContent, ClipboardManager};
use eframe::egui;
pub(crate) use jterm_core::atomic_file;
pub(crate) use jterm_core::char_width;
use parking_lot::Mutex as ParkingMutex;
use session::Session;
use session_manager::{ProtocolResponseQueueError, ProtocolResponseSender, SessionManager};
use shell::{ShellEvent, ShellSession};
use smallvec::SmallVec;
use std::collections::{HashMap, HashSet};
use std::process::Command;
#[cfg(test)]
use std::sync::atomic::AtomicU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use terminal::{clamp_terminal_dimensions, TerminalState};
use ui::{grid_position_from_content, TerminalRenderer};

// 全局标志，用于信号处理
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// A nonzero shell exit inside this window after spawn means the shell never
/// became interactive: no human could have typed `exit` that fast.
const SHELL_STARTUP_GRACE: Duration = Duration::from_millis(1500);
#[cfg(test)]
static NEXT_KITTY_PASTE_IMAGE_ID: AtomicU32 = AtomicU32::new(1);
#[cfg(test)]
const KITTY_BASE64_CHUNK_BYTES: usize = 4096;

/// 设置信号处理器，确保收到SIGINT/SIGTERM时能正常退出
/// 这允许Drop逻辑执行，从而清理所有jsh子进程
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
        libc::signal(
            libc::SIGINT,
            handle_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            handle_signal as *const () as libc::sighandler_t,
        );
    }
}

#[cfg(not(unix))]
fn setup_signal_handlers() {
    // Windows平台暂不支持
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
        config::FontBackendType::Fontdue => Box::new(gpu::fontdue_backend::FontdueAtlas::new(
            device,
            queue,
            font_bytes,
            bold_font_data,
            fallback_font_data,
            font_size_px,
            cfg.font_weight,
            cfg.subpixel_rendering,
        )),
        config::FontBackendType::AbGlyph => Box::new(gpu::ab_glyph_backend::AbGlyphAtlas::new(
            device,
            queue,
            font_bytes,
            bold_font_data,
            fallback_font_data,
            font_size_px,
            cfg.font_weight,
        )),
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

fn load_matching_fallback_fonts(
    fonts: &mut egui::FontDefinitions,
    loaded_paths: &mut HashMap<String, String>,
    family_candidates: &[&str],
    path_candidates: &[&str],
    font_name_prefix: &str,
    families: &[egui::FontFamily],
) -> Vec<String> {
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

    let mut loaded_names = Vec::new();
    for (idx, path) in resolved_paths.iter().enumerate() {
        let font_name = format!("{}_{}", font_name_prefix, idx);
        if load_font_from_path(fonts, loaded_paths, path, &font_name, families, false) {
            if let Some(name) = loaded_paths.get(path) {
                if !loaded_names.iter().any(|loaded| loaded == name) {
                    loaded_names.push(name.clone());
                }
            }
        }
    }

    loaded_names
}

/// 从 PNG 数据中提取宽度和高度
#[cfg(test)]
fn extract_png_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if data.len() < 24
        || !data.starts_with(PNG_SIGNATURE)
        || data.get(12..16) != Some(b"IHDR".as_slice())
    {
        return None;
    }

    // PNG 宽度在偏移 16-19，高度在 20-23
    let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);

    if width == 0 || height == 0 {
        return None;
    }

    crate::debug_log!("[KITTY] PNG dimensions: {}x{}", width, height);
    Some((width, height))
}

#[cfg(test)]
fn next_kitty_paste_image_id() -> u32 {
    loop {
        let id = NEXT_KITTY_PASTE_IMAGE_ID.fetch_add(1, Ordering::Relaxed);
        if id != 0 {
            return id;
        }
    }
}

/// Encode a PNG as standard Kitty direct-transfer APC chunks, followed by a
/// separate put command. Each base64 payload is at most 4096 bytes; the final
/// transfer chunk uses m=0 and retains normal RFC 4648 padding.
#[cfg(test)]
fn kitty_graphics_payload(mime_type: &str, data: &[u8]) -> Option<Vec<u8>> {
    crate::debug_log!(
        "[KITTY] generating payload: mime_type={}, data_size={}",
        mime_type,
        data.len()
    );

    if mime_type != "image/png" || extract_png_dimensions(data).is_none() {
        return None;
    }

    let encoded = base64::engine::general_purpose::STANDARD.encode(data);
    crate::debug_log!(
        "[KITTY] encoded data size (base64): {} bytes",
        encoded.len()
    );

    let image_id = next_kitty_paste_image_id();
    let chunks: Vec<&[u8]> = encoded
        .as_bytes()
        .chunks(KITTY_BASE64_CHUNK_BYTES)
        .collect();
    let mut output = Vec::with_capacity(encoded.len() + chunks.len() * 32 + 48);
    for (index, chunk) in chunks.iter().enumerate() {
        let more = u8::from(index + 1 < chunks.len());
        if index == 0 {
            output.extend_from_slice(format!("\x1b_Ga=t,f=100,i={image_id},m={more};").as_bytes());
        } else {
            output.extend_from_slice(format!("\x1b_Gm={more};").as_bytes());
        }
        output.extend_from_slice(chunk);
        output.extend_from_slice(b"\x1b\\");
    }
    output.extend_from_slice(format!("\x1b_Ga=p,i={image_id}\x1b\\").as_bytes());

    crate::debug_log!("[KITTY] final packet size: {} bytes", output.len());
    Some(output)
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

    let cjk_fallbacks = load_matching_fallback_fonts(
        &mut fonts,
        &mut loaded_font_paths,
        &[
            // Prefer plain TTF/OTF fallbacks for the terminal GPU atlas. Some
            // rasterizers used below do not accept TTC collections directly.
            "Droid Sans Fallback",
            "Noto Sans CJK SC",
            "Noto Sans CJK",
            "Source Han Sans SC",
            "WenQuanYi Zen Hei",
            "AR PL UMing CN",
        ],
        &[
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
            "/usr/share/fonts/wenquanyi/wqy-zenhei.ttc",
            "/usr/share/fonts/google-noto-sans-cjk-fonts/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-sans-cjk-vf-fonts/NotoSansCJK-VF.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJKsc-Regular.otf",
        ],
        "cjk",
        &[egui::FontFamily::Monospace, egui::FontFamily::Proportional],
    );

    if cjk_fallbacks.is_empty() {
        eprintln!("[Fonts] Warning: no CJK fallback font file could be loaded");
    }

    let symbol_fallbacks = load_matching_fallback_fonts(
        &mut fonts,
        &mut loaded_font_paths,
        &[
            "Symbols Nerd Font Mono",
            "Symbols Nerd Font",
            "Noto Sans Symbols 2",
            "Noto Sans Symbols",
            "DejaVu Sans",
            "Noto Emoji",
        ],
        &[
            "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoSansSymbols-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoEmoji-Regular.ttf",
            "/usr/share/fonts/NerdFonts/SymbolsNerdFontMono-Regular.ttf",
            "/usr/share/fonts/NerdFonts/SymbolsNerdFont-Regular.ttf",
            "/usr/share/fonts/TTF/SymbolsNerdFontMono-Regular.ttf",
            "/usr/share/fonts/TTF/SymbolsNerdFont-Regular.ttf",
            "/usr/share/fonts/truetype/nerd-fonts/SymbolsNerdFontMono-Regular.ttf",
            "/usr/share/fonts/truetype/nerd-fonts/SymbolsNerdFont-Regular.ttf",
        ],
        "symbols",
        &[egui::FontFamily::Monospace, egui::FontFamily::Proportional],
    );

    if symbol_fallbacks.is_empty() {
        eprintln!("[Fonts] Warning: no symbol fallback font file could be loaded");
    }

    let mono_font_data: Option<Vec<u8>> = fonts
        .font_data
        .get("monospace_unicode")
        .map(|fd| fd.font.to_vec());
    let fallback_font_data: Vec<Vec<u8>> = cjk_fallbacks
        .iter()
        .chain(symbol_fallbacks.iter())
        .filter_map(|font_name| fonts.font_data.get(font_name))
        .map(|fd| fd.font.to_vec())
        .collect();

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
        let pipeline =
            gpu::pipeline::GridPipeline::new(&render_state.device, render_state.target_format);

        let mut renderer = render_state.renderer.write();
        if let Some(gpu_res) = renderer
            .callback_resources
            .get_mut::<gpu::callback::GpuResources>()
        {
            gpu_res.replace_font_resources(atlas, pipeline);
        } else {
            let gpu_resources =
                gpu::callback::GpuResources::new(atlas, pipeline, &render_state.device);
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
    // Keep operational failures observable by default while allowing
    // `RUST_LOG=jterm2=debug` (or any normal env_logger filter) to opt into
    // deeper diagnostics.
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .format_timestamp_millis()
        .try_init();

    // 设置panic hook，记录panic信息
    // 注意：panic时Drop可能不会被调用，但我们依赖PR_SET_PDEATHSIG确保子进程退出
    std::panic::set_hook(Box::new(|panic_info| {
        eprintln!("[PANIC] jterm2 panicked: {}", panic_info);
        eprintln!("[PANIC] Child jsh processes should exit due to PR_SET_PDEATHSIG");
    }));

    // 设置信号处理，确保收到SIGINT/SIGTERM时能正常清理
    setup_signal_handlers();

    // Shared jterm_core modules brand themselves per app (env prefixes,
    // prompt strings) from this identity.
    jterm_core::identity::init(jterm_core::identity::AppIdentity {
        app_name: "jterm2",
        app_id: "io.github.beamiter.jterm2",
    });

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
            let scale = cfg_clone
                .ui_scale
                .unwrap_or_else(|| cc.egui_ctx.native_pixels_per_point().unwrap_or(1.0));
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

/// True when a clipboard paste should ask the user to confirm before being
/// sent to the PTY. Trips on:
/// - any newline (`\n` after CRLF normalization), since the most common
///   foot-gun is a multi-line block that runs commands without review;
/// - large payloads (> [`PASTE_CONFIRM_THRESHOLD_BYTES`]) that the user
///   probably wants to look at before unleashing.
///
/// Bracketed-paste mode is *not* enough on its own: the receiving program
/// (e.g. plain `bash`) may still execute on the first newline.
fn should_confirm_paste(text: &str) -> bool {
    text.contains('\n') || text.len() > crate::app::state::PASTE_CONFIRM_THRESHOLD_BYTES
}

/// Normalize every terminal line-ending form before applying the paste safety
/// policy. A lone carriage return is an executable Enter in canonical shells,
/// so leaving it untouched would bypass newline confirmation.
fn normalize_paste_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn paste_text_into_session(
    session: &mut Session,
    text: String,
    paste_confirm: bool,
    submit_after_paste: bool,
    pending_paste_confirm: &mut Option<crate::app::state::PendingPasteConfirm>,
) -> Result<bool, crate::shell::ShellWriteError> {
    let normalized = normalize_paste_text(&text);
    if normalized.is_empty() {
        return Ok(false);
    }

    let bracketed_paste = {
        let terminal = session.terminal.lock();
        terminal.is_bracketed_paste_enabled()
    };

    if paste_confirm && should_confirm_paste(&normalized) {
        *pending_paste_confirm = Some(crate::app::state::PendingPasteConfirm {
            text: normalized,
            session_id: session.metadata.session_id.clone(),
            bracketed: bracketed_paste,
            submit_after_paste,
        });
    } else {
        // Retain the normalized source until the all-or-nothing shell enqueue
        // succeeds. On transient backpressure the confirmation flow becomes a
        // durable retry surface even when confirmations were otherwise off.
        let paste_bytes = encode_terminal_paste(&normalized, bracketed_paste, submit_after_paste);
        if let Err(error) = session.shell.write(&paste_bytes) {
            if error.is_backpressure() {
                *pending_paste_confirm = Some(crate::app::state::PendingPasteConfirm {
                    text: normalized,
                    session_id: session.metadata.session_id.clone(),
                    bracketed: bracketed_paste,
                    submit_after_paste,
                });
            }
            return Err(error);
        }
    }

    Ok(true)
}

fn encode_terminal_paste(text: &str, bracketed: bool, submit_after_paste: bool) -> Vec<u8> {
    let payload = if submit_after_paste {
        text.strip_suffix('\n').unwrap_or(text)
    } else {
        text
    };
    let mut bytes = if bracketed {
        wrap_bracketed_paste(payload.as_bytes().to_vec())
    } else {
        payload.as_bytes().to_vec()
    };
    if submit_after_paste {
        // Enter must be outside ESC[200~/ESC[201~. Bash/Readline deliberately
        // does not execute newlines contained inside a bracketed paste.
        bytes.push(b'\r');
    }
    bytes
}

pub(crate) fn wrap_bracketed_paste(payload: Vec<u8>) -> Vec<u8> {
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

const OSC_5522_DATA_CHUNK_BYTES: usize = 4096;
const MAX_OSC_5522_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

struct ClipboardRequestGuard(Arc<std::sync::atomic::AtomicBool>);

impl Drop for ClipboardRequestGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn enqueue_terminal_protocol_response(
    response_tx: &ProtocolResponseSender,
    terminal: &Arc<ParkingMutex<TerminalState>>,
    response: Vec<u8>,
    context: &str,
) {
    if let Err((error, mut response)) = response_tx.try_enqueue_critical(response) {
        if error == ProtocolResponseQueueError::Full {
            // The parser pump stops while the per-session queue is non-empty,
            // so this bounded batch is retried before any newer PTY requests
            // are accepted. Prepend to preserve response order.
            let mut terminal = terminal.lock();
            response.append(&mut terminal.output_buffer);
            terminal.output_buffer = response;
        } else {
            log::debug!("{context} cancelled: {error}");
        }
    }
}

/// Worker responses wait for space in the bounded per-session protocol FIFO,
/// so transient shell-writer pressure cannot lose an accepted query. The
/// clipboard reader is globally single-flight and closing a session wakes the
/// waiter. Only a permanently oversized response is replaced with the small
/// protocol-specific failure response.
fn enqueue_worker_protocol_response(
    response_tx: &ProtocolResponseSender,
    response: Vec<u8>,
    fallback: Vec<u8>,
    context: &str,
) {
    match response_tx.enqueue_blocking(response) {
        Ok(()) => {}
        Err(ProtocolResponseQueueError::Closed) => {
            log::debug!("{context} target session closed before its response was queued");
        }
        Err(error) => {
            log::warn!("{context} response replaced by bounded fallback: {error}");
            if let Err(fallback_error) = response_tx.enqueue_blocking(fallback) {
                log::debug!("{context} fallback was cancelled: {fallback_error}");
            }
        }
    }
}

fn send_osc5522_worker_response(response_tx: &ProtocolResponseSender, response: Vec<u8>) {
    enqueue_worker_protocol_response(
        response_tx,
        response,
        osc_5522_packet("type=read:status=EBUSY", None),
        "OSC 5522",
    );
}

fn service_osc5522_clipboard_requests(
    clipboard_available: bool,
    in_flight: &Arc<AtomicBool>,
    terminal: Arc<ParkingMutex<TerminalState>>,
    response_tx: ProtocolResponseSender,
    mut requests: Vec<terminal::ClipboardReadRequest>,
) {
    if requests.is_empty() {
        return;
    }
    if !clipboard_available {
        for _ in &requests {
            enqueue_terminal_protocol_response(
                &response_tx,
                &terminal,
                osc_5522_packet("type=read:status=ENOSYS", None),
                "OSC 5522 ENOSYS response",
            );
        }
        return;
    }
    if in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        for _ in &requests {
            enqueue_terminal_protocol_response(
                &response_tx,
                &terminal,
                osc_5522_packet("type=read:status=EBUSY", None),
                "OSC 5522 busy response",
            );
        }
        return;
    }

    let in_flight_for_thread = Arc::clone(in_flight);
    let error_tx = response_tx.clone();
    let error_terminal = Arc::clone(&terminal);
    let request_count = requests.len();
    let spawn_result = std::thread::Builder::new()
        .name("clipboard-request-handler".to_string())
        .spawn(move || {
            let _guard = ClipboardRequestGuard(in_flight_for_thread);
            let Ok(clipboard) = ClipboardManager::new() else {
                crate::debug_log!("[OSC5522] Failed to create clipboard manager");
                for _ in 0..request_count {
                    send_osc5522_worker_response(
                        &response_tx,
                        osc_5522_packet("type=read:status=ENOSYS", None),
                    );
                }
                return;
            };

            for request in requests.drain(..) {
                let terminal::ClipboardReadKind::MimeData(mime_type) = request.kind;
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
                send_osc5522_worker_response(&response_tx, response);
            }
        });
    if let Err(error) = spawn_result {
        in_flight.store(false, Ordering::Release);
        log::warn!("failed to spawn OSC 5522 clipboard handler: {error}");
        for _ in 0..request_count {
            enqueue_terminal_protocol_response(
                &error_tx,
                &error_terminal,
                osc_5522_packet("type=read:status=ENOSYS", None),
                "OSC 5522 spawn-error response",
            );
        }
    }
}

fn clipboard_5522_response_for_mime(mime_type: &str, data: &[u8]) -> Vec<u8> {
    clipboard_5522_response_for_mime_with_limit(mime_type, data, MAX_OSC_5522_RESPONSE_BYTES)
}

fn clipboard_5522_response_for_mime_with_limit(
    mime_type: &str,
    data: &[u8],
    max_bytes: usize,
) -> Vec<u8> {
    if data.len() > max_bytes {
        // OSC 5522 has no dedicated "too large" read error. EPERM is the
        // closest truthful response: terminal policy denied this read.
        return osc_5522_packet("type=read:status=EPERM", None);
    }

    let encoded_mime = base64::engine::general_purpose::STANDARD.encode(mime_type.as_bytes());
    let mut output = Vec::new();
    output.extend_from_slice(&osc_5522_packet("type=read:status=OK", None));
    for chunk in data.chunks(OSC_5522_DATA_CHUNK_BYTES) {
        let encoded_data = base64::engine::general_purpose::STANDARD.encode(chunk);
        output.extend_from_slice(&osc_5522_packet(
            &format!("type=read:status=DATA:mime={}", encoded_mime),
            Some(&encoded_data),
        ));
    }
    output.extend_from_slice(&osc_5522_packet("type=read:status=DONE", None));
    output
}

fn link_at_pointer(
    links: &[link::Link],
    pointer: egui::Pos2,
    content_rect: egui::Rect,
    char_width: f32,
    line_height: f32,
    cols: usize,
    rows: usize,
) -> Option<link::Link> {
    if !content_rect.contains(pointer) || cols == 0 || rows == 0 {
        return None;
    }
    let (row, col) =
        grid_position_from_content(pointer, content_rect, char_width, line_height, cols, rows);
    links
        .iter()
        .find(|link| link.line == row && col >= link.col_start && col < link.col_end)
        .cloned()
}

#[derive(Debug)]
pub(crate) struct DesktopNotification {
    title: String,
    body: String,
}

const DESKTOP_NOTIFICATION_QUEUE_CAPACITY: usize = 8;

fn desktop_notification_channel() -> (
    crossbeam_channel::Sender<DesktopNotification>,
    crossbeam_channel::Receiver<DesktopNotification>,
) {
    crossbeam_channel::bounded(DESKTOP_NOTIFICATION_QUEUE_CAPACITY)
}

fn wait_for_child_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> std::io::Result<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            // `kill` can race a natural exit. Either way `wait` is mandatory:
            // dropping Child does not reap it on Unix.
            let _ = child.kill();
            return child.wait();
        }
        std::thread::sleep(
            deadline
                .saturating_duration_since(now)
                .min(Duration::from_millis(10)),
        );
    }
}

fn spawn_desktop_notification_worker() -> Option<crossbeam_channel::Sender<DesktopNotification>> {
    let (tx, rx) = desktop_notification_channel();
    std::thread::Builder::new()
        .name("desktop-notification-worker".to_string())
        .spawn(move || {
            while let Ok(notification) = rx.recv() {
                match std::process::Command::new("notify-send")
                    .arg("--")
                    .arg(notification.title)
                    .arg(notification.body)
                    .spawn()
                {
                    Ok(mut child) => {
                        if let Err(error) =
                            wait_for_child_with_timeout(&mut child, Duration::from_secs(2))
                        {
                            log::debug!("desktop notification wait failed: {error}");
                        }
                    }
                    Err(error) => log::debug!("desktop notification unavailable: {error}"),
                }
            }
        })
        .ok()?;
    Some(tx)
}

/// Shared rate window for every desktop-notification source (OSC 9/777 and
/// long-command toasts), so their combined output stays bounded.
const NOTIFICATION_RATE_WINDOW: Duration = Duration::from_secs(5);
const MAX_NOTIFICATIONS_PER_WINDOW: usize = 4;

/// Restart the shared notification rate window once it has elapsed. Callers
/// then check `notifications_in_window` against [`MAX_NOTIFICATIONS_PER_WINDOW`]
/// and count their own successful sends.
fn roll_notification_rate_window(
    window_started: &mut std::time::Instant,
    notifications_in_window: &mut usize,
) {
    let now = std::time::Instant::now();
    if now.duration_since(*window_started) >= NOTIFICATION_RATE_WINDOW {
        *window_started = now;
        *notifications_in_window = 0;
    }
}

fn show_desktop_notification(
    notification_tx: Option<&crossbeam_channel::Sender<DesktopNotification>>,
    window_started: &mut std::time::Instant,
    notifications_in_window: &mut usize,
    title: String,
    body: String,
) {
    roll_notification_rate_window(window_started, notifications_in_window);
    if *notifications_in_window >= MAX_NOTIFICATIONS_PER_WINDOW {
        return;
    }
    let Some(notification_tx) = notification_tx else {
        return;
    };
    if notification_tx
        .try_send(DesktopNotification { title, body })
        .is_ok()
    {
        *notifications_in_window += 1;
    }
}

/// jterm1-parity gate for the long-command desktop toast
/// (`block_view`'s `notify_long_blocks` check): the command must be a real
/// foreground command (jterm1 skips background blocks, whose command line is
/// empty), the config flag must be on, and the measured duration must reach
/// the threshold. jterm2 adds the egui window focus state: a completion the
/// user just watched on screen needs no toast.
fn should_notify_long_command(
    config: &config::Config,
    command: Option<&str>,
    duration_ms: Option<u64>,
    watched: bool,
) -> bool {
    if !config.notify_long_blocks || watched {
        return false;
    }
    if !command.map(str::trim).is_some_and(|cmd| !cmd.is_empty()) {
        return false;
    }
    duration_ms.is_some_and(|ms| ms >= config.notify_long_block_threshold_ms)
}

/// Post the long-command-finished notification when the gates above and the
/// shared rate window allow it. Free function over disjoint fields because the
/// completion sites hold a mutable borrow of the session manager.
fn maybe_notify_long_command(
    config: &config::Config,
    window_started: &mut std::time::Instant,
    notifications_in_window: &mut usize,
    completed: &crate::terminal::CompletedCommandOutput,
    watched: bool,
) {
    if !should_notify_long_command(
        config,
        completed.command.as_deref(),
        completed.duration_ms,
        watched,
    ) {
        return;
    }
    // An untrusted PTY can claim any `duration=` in OSC 133;D, so this path
    // shares the OSC 9/777 rate window instead of trusting the threshold to
    // keep notify-send spawns rare.
    roll_notification_rate_window(window_started, notifications_in_window);
    if *notifications_in_window >= MAX_NOTIFICATIONS_PER_WINDOW {
        return;
    }
    *notifications_in_window += 1;
    let command = completed.command.as_deref().unwrap_or_default().trim();
    jterm_core::notify::long_block_finished(
        command,
        completed.exit_code.unwrap_or(0),
        completed.duration_ms.unwrap_or(0),
    );
}

fn reported_capture_button(capture: Option<(bool, u8)>) -> Option<u8> {
    capture.and_then(|(reported_to_app, button)| reported_to_app.then_some(button))
}

const MAX_MOUSE_WHEEL_REPORTS_PER_FRAME: isize = 64;

fn bounded_wheel_step_accumulate(current: isize, delta: f32, multiplier: usize) -> isize {
    let multiplier = isize::try_from(multiplier).unwrap_or(isize::MAX);
    current
        .saturating_add((delta.round() as isize).saturating_mul(multiplier))
        .clamp(
            -MAX_MOUSE_WHEEL_REPORTS_PER_FRAME,
            MAX_MOUSE_WHEEL_REPORTS_PER_FRAME,
        )
}

fn captured_release_button(
    capture: Option<(bool, u8)>,
    released: &[u8],
    pointer_any_down: bool,
) -> Option<u8> {
    reported_capture_button(capture).filter(|button| released.contains(button) || !pointer_any_down)
}

fn queue_mouse_control(
    queue: &mut std::collections::VecDeque<crate::app::state::PendingMouseControl>,
    kind: crate::app::state::PendingMouseControlKind,
    bytes: Vec<u8>,
) {
    if !queue.iter().any(|pending| pending.kind == kind) {
        queue.push_back(crate::app::state::PendingMouseControl { kind, bytes });
    }
}

fn flush_pending_mouse_controls<E>(
    queue: &mut std::collections::VecDeque<crate::app::state::PendingMouseControl>,
    press_accepted: &mut bool,
    mut send: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), E> {
    while let Some(pending) = queue.front() {
        send(&pending.bytes)?;
        let accepted = queue.pop_front().expect("front existed above");
        if accepted.kind == crate::app::state::PendingMouseControlKind::Press {
            *press_accepted = true;
        }
    }
    Ok(())
}

fn flush_mouse_controls(
    capture: &mut crate::app::state::TerminalMouseCapture,
) -> Result<(), crate::shell::ShellWriteError> {
    let write_tx = capture.write_tx.clone();
    flush_pending_mouse_controls(
        &mut capture.pending_controls,
        &mut capture.press_accepted,
        |bytes| write_tx.try_send(bytes.to_vec()),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrimaryCopyRoute {
    None,
    CapturedLocal,
    Generic,
    SuppressCaptured,
}

fn primary_copy_route(
    capture: Option<(bool, bool, u8)>,
    capture_finished: bool,
    primary_released: bool,
) -> PrimaryCopyRoute {
    match capture {
        Some((reported_to_app, local_selection_cancelled, 0)) if capture_finished => {
            if !reported_to_app && !local_selection_cancelled {
                PrimaryCopyRoute::CapturedLocal
            } else {
                PrimaryCopyRoute::SuppressCaptured
            }
        }
        _ if primary_released => PrimaryCopyRoute::Generic,
        _ => PrimaryCopyRoute::None,
    }
}

fn take_tagged_cursor_move(
    target: &mut Option<usize>,
    bytes: &mut Vec<u8>,
) -> Option<(usize, Vec<u8>)> {
    let target = target.take();
    let bytes = std::mem::take(bytes);
    if bytes.is_empty() {
        None
    } else {
        target.map(|target| (target, bytes))
    }
}

fn mouse_capture_allows_lossy(capture: Option<&crate::app::state::TerminalMouseCapture>) -> bool {
    capture.is_none_or(|capture| {
        mouse_sequence_allows_lossy(
            capture.reported_to_app,
            capture.press_accepted,
            capture.release_observed,
            capture.pending_controls.is_empty(),
        )
    })
}

fn mouse_capture_is_complete(capture: &crate::app::state::TerminalMouseCapture) -> bool {
    mouse_sequence_is_complete(
        capture.reported_to_app,
        capture.press_accepted,
        capture.release_observed,
        capture.pending_controls.is_empty(),
    )
}

fn mouse_sequence_allows_lossy(
    reported_to_app: bool,
    press_accepted: bool,
    release_observed: bool,
    controls_empty: bool,
) -> bool {
    !reported_to_app || (press_accepted && !release_observed && controls_empty)
}

fn mouse_sequence_is_complete(
    reported_to_app: bool,
    press_accepted: bool,
    release_observed: bool,
    controls_empty: bool,
) -> bool {
    release_observed && (!reported_to_app || (press_accepted && controls_empty))
}

fn spawn_osc52_clipboard_writer(
    clipboard_available: bool,
) -> Option<crossbeam_channel::Sender<String>> {
    if !clipboard_available {
        return None;
    }
    let (tx, rx) = crossbeam_channel::bounded::<String>(1);
    std::thread::Builder::new()
        .name("osc52-clipboard-writer".to_string())
        .spawn(move || {
            let Ok(clipboard) = ClipboardManager::new() else {
                return;
            };
            while let Ok(text) = rx.recv() {
                if let Err(error) = clipboard.copy(&text) {
                    log::warn!("OSC 52 clipboard write failed: {error}");
                }
            }
        })
        .ok()?;
    Some(tx)
}

fn enqueue_osc52_clipboard_write(
    tx: Option<&crossbeam_channel::Sender<String>>,
    window_started: &mut std::time::Instant,
    writes_in_window: &mut usize,
    text: String,
) {
    const WINDOW: Duration = Duration::from_secs(1);
    const MAX_WRITES_PER_WINDOW: usize = 2;

    let now = std::time::Instant::now();
    if now.duration_since(*window_started) >= WINDOW {
        *window_started = now;
        *writes_in_window = 0;
    }
    let Some(tx) = tx else {
        return;
    };
    if *writes_in_window >= MAX_WRITES_PER_WINDOW {
        return;
    }
    if tx.try_send(text).is_ok() {
        *writes_in_window += 1;
    }
}

const MAX_OSC52_CLIPBOARD_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const OSC52_READ_RATE_WINDOW: Duration = Duration::from_secs(1);
const MAX_OSC52_READS_PER_WINDOW: usize = 2;

fn osc52_clipboard_response_with_limit(content: &str, max_response_bytes: usize) -> Vec<u8> {
    const PREFIX: &[u8] = b"\x1b]52;c;";
    const TERMINATOR: &[u8] = b"\x1b\\";
    let overhead = PREFIX.len() + TERMINATOR.len();
    if max_response_bytes < overhead {
        return Vec::new();
    }

    let encoded_len = content
        .len()
        .checked_add(2)
        .map(|length| length / 3)
        .and_then(|length| length.checked_mul(4));
    let content = if encoded_len.is_some_and(|length| length <= max_response_bytes - overhead) {
        content
    } else {
        // OSC 52 has no standardized error response. Replying with an empty
        // selection is bounded and lets a querying application stop waiting.
        ""
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
    let mut response = Vec::with_capacity(overhead + encoded.len());
    response.extend_from_slice(PREFIX);
    response.extend_from_slice(encoded.as_bytes());
    response.extend_from_slice(TERMINATOR);
    response
}

fn osc52_read_rate_limit_allows(
    now: std::time::Instant,
    window_started: &mut std::time::Instant,
    reads_in_window: &mut usize,
) -> bool {
    if now.duration_since(*window_started) >= OSC52_READ_RATE_WINDOW {
        *window_started = now;
        *reads_in_window = 0;
    }
    if *reads_in_window >= MAX_OSC52_READS_PER_WINDOW {
        return false;
    }
    *reads_in_window += 1;
    true
}

fn service_osc52_clipboard_query(
    clipboard_available: bool,
    in_flight: &Arc<AtomicBool>,
    terminal: Arc<ParkingMutex<TerminalState>>,
    response_tx: ProtocolResponseSender,
    window_started: &mut std::time::Instant,
    reads_in_window: &mut usize,
) {
    let empty_response =
        || osc52_clipboard_response_with_limit("", MAX_OSC52_CLIPBOARD_RESPONSE_BYTES);
    if !osc52_read_rate_limit_allows(std::time::Instant::now(), window_started, reads_in_window)
        || !clipboard_available
    {
        enqueue_terminal_protocol_response(
            &response_tx,
            &terminal,
            empty_response(),
            "OSC 52 empty response",
        );
        return;
    }
    if in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        enqueue_terminal_protocol_response(
            &response_tx,
            &terminal,
            empty_response(),
            "OSC 52 busy response",
        );
        return;
    }

    let in_flight_for_thread = Arc::clone(in_flight);
    let error_tx = response_tx.clone();
    let spawn_result = std::thread::Builder::new()
        .name("osc52-clipboard-reader".to_string())
        .spawn(move || {
            let _guard = ClipboardRequestGuard(in_flight_for_thread);
            let content = ClipboardManager::new()
                .and_then(|clipboard| clipboard.paste())
                .unwrap_or_default();
            let response =
                osc52_clipboard_response_with_limit(&content, MAX_OSC52_CLIPBOARD_RESPONSE_BYTES);
            let fallback =
                osc52_clipboard_response_with_limit("", MAX_OSC52_CLIPBOARD_RESPONSE_BYTES);
            enqueue_worker_protocol_response(&response_tx, response, fallback, "OSC 52");
        });
    if let Err(error) = spawn_result {
        in_flight.store(false, Ordering::Release);
        log::warn!("failed to spawn OSC 52 clipboard reader: {error}");
        enqueue_terminal_protocol_response(
            &error_tx,
            &terminal,
            empty_response(),
            "OSC 52 spawn-error response",
        );
    }
}

// key_to_string and build_keybinding_string moved to app::events module

impl TerminalApp {
    /// Route click-to-cursor arrows produced by the previous render before
    /// collecting any input from this frame. The terminal pointer is a stable
    /// identity tag while the session owns its `Arc`; stale/unroutable bytes
    /// are discarded instead of being delivered to a replacement PTY.
    fn stage_prior_cursor_moves(&mut self, terminal_input_blocked: bool) -> (bool, bool) {
        let mut tagged_moves = Vec::with_capacity(self.pane_renderers.len() + 1);
        if let Some(tagged) = take_tagged_cursor_move(
            &mut self.renderer.cursor_move_terminal_ptr,
            &mut self.renderer.cursor_move_input,
        ) {
            tagged_moves.push(tagged);
        }
        for renderer in &mut self.pane_renderers {
            if let Some(tagged) = take_tagged_cursor_move(
                &mut renderer.cursor_move_terminal_ptr,
                &mut renderer.cursor_move_input,
            ) {
                tagged_moves.push(tagged);
            }
        }

        if terminal_input_blocked {
            return (false, false);
        }

        let mut had_input = false;
        let mut overflow = false;
        for (target, bytes) in tagged_moves {
            let routed = self
                .session_manager
                .sessions_mut()
                .iter_mut()
                .find(|session| Arc::as_ptr(&session.terminal) as usize == target);
            if let Some(session) = routed {
                had_input = true;
                overflow |= !session.queue_input(&bytes);
            }
        }
        (had_input, overflow)
    }

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

        let mut session_restore_notice = None;
        let mut session_persistence_blocked = false;

        // 仅在首个实例且配置允许时恢复会话。损坏文件先移到旁路备份；
        // 若备份失败则禁止本进程保存，绝不让新建的单会话覆盖原始证据。
        let saved_snapshot = if cfg.restore_session && is_first_instance {
            match cfg.resolved_session_history_path() {
                Ok(path) => {
                    match session_persistence::SessionsSnapshot::load_with_warnings(&path) {
                        Ok((snapshot, warnings)) => {
                            if !warnings.is_empty() {
                                for warning in &warnings {
                                    eprintln!("[SessionPersistence] WARNING: {warning}");
                                }
                                session_restore_notice = Some(format!(
                                    "Session restore adjusted {} unsafe value(s)",
                                    warnings.len()
                                ));
                            }
                            (!snapshot.sessions.is_empty()).then_some(snapshot)
                        }
                        Err(error) => {
                            eprintln!(
                                "[SessionPersistence] Failed to load {}: {error}",
                                path.display()
                            );
                            match session_persistence::quarantine_corrupt_snapshot(&path) {
                                Ok(backup) => {
                                    session_restore_notice = Some(format!(
                                        "Session restore failed; original moved to {}",
                                        backup.display()
                                    ));
                                }
                                Err(backup_error) => {
                                    session_persistence_blocked = true;
                                    session_restore_notice = Some(format!(
                                    "Session restore failed and was not overwritten: {backup_error}"
                                ));
                                }
                            }
                            None
                        }
                    }
                }
                Err(error) => {
                    session_persistence_blocked = true;
                    session_restore_notice =
                        Some(format!("Session persistence is unavailable: {error}"));
                    None
                }
            }
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
            .and_then(|s| s.sessions.first()?.session_id.as_deref().map(String::from))
            .filter(|id| session::is_valid_jsh_session_id(id))
            .unwrap_or_else(session::generate_session_id);
        let saved_active_index = saved_snapshot.as_ref().and_then(|s| s.active_index);
        let saved_tab_layouts: Vec<session_persistence::LayoutSnapshot> = saved_snapshot
            .as_ref()
            .map(|snapshot| snapshot.tabs.clone())
            .unwrap_or_default();
        let saved_active_tab = saved_snapshot.as_ref().and_then(|s| s.active_tab);
        let terminal = TerminalState::new(cols, rows);

        let configured_shell = std::env::var("JTERM2_SHELL").ok().or(cfg.shell.clone());

        let shell = match ShellSession::new_with_cwd(
            cols,
            rows,
            first_cwd.as_deref(),
            Some(&first_session_id),
            configured_shell.as_deref(),
            None,
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
                match ShellSession::new_with_cwd(
                    cols,
                    rows,
                    None,
                    Some(&first_session_id),
                    configured_shell.as_deref(),
                    None,
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

        let session = Session::with_default_name_and_session_id(
            0,
            Arc::new(ParkingMutex::new(terminal)),
            shell,
            first_session_id,
        );
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
        let osc52_clipboard_write_tx = spawn_osc52_clipboard_writer(clipboard.is_some());

        let keybindings = match keybindings::KeyBindings::load() {
            Ok(bindings) => bindings,
            Err(error) => {
                eprintln!("[Keybindings] Failed to load keybindings.toml; using defaults: {error}");
                keybindings::KeyBindings::default()
            }
        };

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

        // 布局引用稳定 session ID，因此某个 shell 恢复失败时可以只折叠
        // 对应分支；旧版/损坏快照则从真正的活跃 session 回退到单 pane。
        let restored_session_ids: Vec<String> = session_manager
            .sessions()
            .iter()
            .map(|session| session.metadata.session_id.clone())
            .collect();
        let tabs = tab_manager::TabManager::restore(
            &saved_tab_layouts,
            &restored_session_ids,
            session_manager.active_index(),
            saved_active_tab,
        );
        if let Some(focused_idx) = tabs.active_focused_session() {
            session_manager.switch_session(focused_idx);
        }

        // Multi-pane renderers are allocated on demand and trimmed when panes
        // close, avoiding four full GPU/text caches in the common one-pane case.
        let pane_renderers = Vec::new();

        // 命令面板/搜索历史:启动时一次性读盘,失败回 Default(load 已吞日志)。
        let history = config::Config::ui_history_path()
            .map(|p| history_persistence::HistorySnapshot::load(&p))
            .unwrap_or_default();

        let mut command_palette = command_palette::CommandPalette::new();
        command_palette.restore_recent_commands(history.recent_commands);
        let mut search_state = search::SearchState::new();
        for entry in history.search_history.into_iter().take(50) {
            search_state.history.push_back(entry);
        }
        let initial_status_expires_at = session_restore_notice
            .as_ref()
            .map(|_| std::time::Instant::now() + Duration::from_secs(10));
        let initial_status_message = session_restore_notice.unwrap_or_default();

        Ok(TerminalApp {
            session_manager,
            renderer,
            clipboard,
            clipboard_request_in_flight: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            osc52_clipboard_write_tx,
            osc52_write_window_started: std::time::Instant::now(),
            osc52_writes_in_window: 0,
            osc52_read_window_started: std::time::Instant::now(),
            osc52_reads_in_window: 0,
            cols,
            rows,
            next_cursor_blink_time: std::time::Instant::now() + Duration::from_millis(1000),
            cursor_visible: true,
            last_activity_time: std::time::Instant::now(),
            status_message: initial_status_message,
            status_expires_at: initial_status_expires_at,
            last_window_title: String::new(),
            hovered_tab_index: None,
            dragging_tab: None,
            drag_start_pos: None,
            tab_drag_origin: None,
            current_mouse_x: 0.0,
            tab_scroll_offset: 0.0,
            renaming_tab: None,
            search_state,
            sidebar: {
                let mut sb = sidebar::Sidebar::new();
                sb.visible = false; // 默认隐藏，opt-in 切换

                // 三个视图在两种 tab 栏布局下都可用：Top 模式下侧边栏的
                // Sessions 列表与顶部 tab 栏并存(与 jterm3 一致)。
                sb.view = cfg.sidebar_view;
                sb
            },
            command_sidebar: Default::default(),
            search_replace_panel: search_replace_panel::SearchReplacePanel::new(),
            link_detector: link::LinkDetector::new(link::LinkDetectionConfig::default()),
            hovered_link: None,
            cached_links: Vec::new(),
            cached_links_grid_version: 0,
            cached_links_scroll_offset: 0,
            cached_links_terminal_ptr: usize::MAX,
            keybindings,
            command_palette,
            force_resize_session: false,
            current_theme,
            tabs,
            pane_renderers,
            dragging_divider: None,
            pane_status_cache: pane_header::PaneStatusCache::new(),
            git_strip_cache: pane_header::GitStripCache::new(),
            pane_drag: None,
            help_panel: help::HelpPanel::new(),
            config_panel: config_panel::ConfigPanel::new(),
            debug_panel: debug_panel::DebugPanel::new(),
            agent_panel: agent_panel::AgentPanel::new(),
            jsh_notice: jsh_ui::JshNotice::default(),
            config: cfg.clone(),
            config_save_pending: false,
            config_save_deadline: std::time::Instant::now(),
            session_save_pending: !session_persistence_blocked,
            session_save_deadline: std::time::Instant::now() + std::time::Duration::from_secs(1),
            session_persistence_blocked,
            _lock_file: lock_file,
            mouse_scroll_accumulator: 0.0,
            terminal_mouse_capture: None,
            last_terminal_mouse_motion: None,
            font_size_accumulator: 0.0,
            had_ctrl_scroll_last_frame: false,
            frame_events: Vec::new(),
            paste_key_state: Default::default(),
            keyboard_input_buffer: Vec::new(),
            adaptive_frame_budget: 65536, // 初始值 64KB
            config_last_mtime: config::Config::config_mtime(),
            config_last_check: std::time::Instant::now(),
            smooth_scroll_velocity: 0.0,
            smooth_scroll_pixel_offset: 0.0,
            pending_paste_confirm: None,
            paste_dont_ask_again: false,
            notification_tx: spawn_desktop_notification_worker(),
            notification_window_started: std::time::Instant::now(),
            notifications_in_window: 0,
        })
    }

    fn apply_runtime_config(&mut self, ctx: &egui::Context) {
        // Apply UI scale: use config value if provided, otherwise use native DPI
        let scale = self
            .config
            .ui_scale
            .unwrap_or_else(|| ctx.native_pixels_per_point().unwrap_or(1.0));
        ctx.set_pixels_per_point(scale);

        // eframe chooses Glow vs WGPU when the native window is created; that
        // backend cannot be swapped by hot reload. Keep the newly configured
        // value for the next launch, but apply all other settings against the
        // renderer that is actually alive in this process.
        let runtime_uses_wgpu = self.renderer.wgpu_render_state.is_some();
        let mut runtime_config = self.config.clone();
        runtime_config.app_renderer = if runtime_uses_wgpu {
            config::AppRendererType::Wgpu
        } else {
            config::AppRendererType::Glow
        };
        configure_fonts_and_gpu(
            ctx,
            self.renderer.wgpu_render_state.as_ref(),
            &runtime_config,
        );
        apply_theme_visuals(ctx, &self.current_theme);

        self.renderer.font_size = self.config.font_size;
        self.renderer.padding = self.config.padding;
        self.renderer.line_spacing = self.config.line_spacing;
        self.renderer.scrollbar_visibility = self.config.scrollbar_visibility.clone();
        self.renderer.theme = self.current_theme.clone();
        self.renderer.opacity = self.config.opacity;
        self.renderer.font_ligatures = self.config.font_ligatures;
        self.renderer.gpu_rendering = runtime_uses_wgpu && self.config.gpu_rendering;
        self.renderer.sync_font_metrics(ctx);
        self.renderer.invalidate_font_cache();

        for renderer in &mut self.pane_renderers {
            renderer.font_size = self.config.font_size;
            renderer.padding = self.config.padding;
            renderer.line_spacing = self.config.line_spacing;
            renderer.scrollbar_visibility = self.config.scrollbar_visibility.clone();
            renderer.theme = self.current_theme.clone();
            renderer.opacity = self.config.opacity;
            renderer.font_ligatures = self.config.font_ligatures;
            renderer.gpu_rendering = runtime_uses_wgpu && self.config.gpu_rendering;
            renderer.sync_font_metrics(ctx);
            renderer.invalidate_font_cache();
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
        let old_len = self.session_manager.len();
        let new_idx =
            self.session_manager
                .new_session(name, tags, cols, rows, self.config.scrollback_lines);
        if self.session_manager.len() > old_len {
            // 会话索引是全局的，所以插入要在每个 tab 的树里重编号，不只是
            // 当前 tab。新会话本身还没有归属，由调用方决定是分屏还是开新 tab。
            self.tabs.on_session_inserted(new_idx);
        }
        new_idx
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
        if !matches!(self.config.tab_bar_position, config::TabBarPosition::Top) {
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
    fn render_sidebar(&mut self, root_ui: &mut egui::Ui) {
        if !self.sidebar.visible {
            // 展开按钮统一由顶部栏内的 ☰ 负责(Top 模式在 tab 栏，Sidebar 模式在精简顶部栏)，
            // 不再使用浮动按钮，避免覆盖终端内容。
            return;
        }

        // Follow the shell's authoritative OSC 7 cwd (or the local process
        // cwd fallback) instead of guessing that a queued `cd` succeeded.
        // This also keeps the file tree correct after users type `cd` by hand.
        if self.sidebar.view == sidebar::SidebarView::Files {
            let reported_cwd = {
                let session = self.session_manager.get_active_session_mut();
                let osc7 = session.terminal.lock().current_working_dir.clone();
                osc7.or_else(|| jterm_core::process::process_cwd(session.get_shell_pid()))
            };
            let changed_directory = reported_cwd
                .map(std::path::PathBuf::from)
                .filter(|path| path.is_dir() && self.sidebar.current_dir != *path);
            if let Some(path) = changed_directory {
                self.sidebar.set_current_dir(path);
            }
        }

        // 树遍历期间只收集动作，闭包结束后再 mutate，规避借用冲突
        let mut toggle_path: Option<std::path::PathBuf> = None;
        let mut select_path: Option<std::path::PathBuf> = None;
        let mut cd_path: Option<std::path::PathBuf> = None;
        let mut do_refresh = false;
        let mut view_changed = false;

        let panel_bg = theme::Theme::rgb_to_color32(self.current_theme.ui.panel_bg);
        egui::Panel::left("file_tree")
            .resizable(true)
            .default_size(self.sidebar.width)
            .frame(egui::Frame::NONE.fill(panel_bg).inner_margin(6.0))
            .show(root_ui, |ui| {
                ui.horizontal(|ui| {
                    // Sessions 在两种 tab 栏布局下都可选：Top 模式下它是顶部
                    // tab 栏之外的一份纵向标签列表，而非替代品。
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
                    if ui
                        .selectable_label(
                            self.sidebar.view == sidebar::SidebarView::Commands,
                            egui::RichText::new("Commands").strong(),
                        )
                        .clicked()
                    {
                        self.sidebar.view = sidebar::SidebarView::Commands;
                        view_changed = true;
                    }
                    if self.sidebar.view == sidebar::SidebarView::Files
                        && ui.button("⟳").on_hover_text("Refresh").clicked()
                    {
                        do_refresh = true;
                    }
                });
                ui.separator();

                match self.sidebar.view {
                    sidebar::SidebarView::Sessions => self.render_sidebar_sessions(ui),
                    sidebar::SidebarView::Commands => self.render_sidebar_commands(ui),
                    sidebar::SidebarView::Files => {
                        if let Some(dir) = self
                            .sidebar
                            .current_dir
                            .file_name()
                            .and_then(|n| n.to_str())
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
                }
            });

        // 闭包结束，安全 mutate
        self.execute_pending_command_sidebar_action();
        if let Some(p) = toggle_path {
            self.sidebar.toggle_node(&p);
        }
        if let Some(p) = select_path {
            self.sidebar.selected_path = Some(p);
        }
        if let Some(p) = cd_path {
            let quoted = jterm_core::process::shell_quote_path(&p.to_string_lossy());
            let cmd = format!("cd {}\n", quoted);
            let paste_result = {
                let session = self.session_manager.get_active_session_mut();
                paste_text_into_session(
                    session,
                    cmd,
                    self.config.paste_confirm,
                    true,
                    &mut self.pending_paste_confirm,
                )
            };
            match paste_result {
                Ok(true) if self.pending_paste_confirm.is_some() => {
                    self.status_message =
                        "请确认目录切换命令；文件树将在 shell 切换后同步".to_string();
                    self.status_expires_at =
                        Some(std::time::Instant::now() + Duration::from_secs(4));
                }
                Ok(true) => {
                    self.status_message =
                        "目录切换命令已发送；文件树将跟随 shell 工作目录".to_string();
                    self.status_expires_at =
                        Some(std::time::Instant::now() + Duration::from_secs(4));
                }
                Ok(false) => {
                    self.status_message = "目录切换命令为空，未发送".to_string();
                    self.status_expires_at =
                        Some(std::time::Instant::now() + Duration::from_secs(4));
                }
                Err(error) => {
                    self.status_message = format!("目录切换命令发送失败：{error}");
                    self.status_expires_at =
                        Some(std::time::Instant::now() + Duration::from_secs(4));
                }
            }
        }
        if do_refresh {
            self.sidebar.refresh();
        }
        if view_changed {
            // 记住用户选择的视图，下次默认沿用。
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
            let label = format!("{} {}/", arrow, node.name);
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
            let resp = ui.selectable_label(is_selected, format!("  {}", node.name));
            if resp.clicked() {
                *select = Some(node.path.clone());
            }
        }
    }

    #[allow(deprecated)]
    fn render_ui(&mut self, root_ui: &mut egui::Ui) {
        // egui 0.35 起 Panel/CentralPanel 都改成在 Ui 上 .show(ui, ...) 调用;
        // 但仍有部分代码(浮窗 Window、各种 input/viewport 操作)需要 &Context,
        // 这里克隆一份作为局部 ctx 供下游使用(Arc 引用计数,几乎零成本)。
        let ctx_owned = root_ui.ctx().clone();
        let ctx = &ctx_owned;

        let frame = egui::Frame::NONE.inner_margin(0.0);

        // 顶部栏(全宽)：必须在 render_sidebar 之前声明，egui 会把先声明的面板
        // 分配到容器边缘的完整范围 —— 因此顶栏横跨整个窗口，侧边栏落在其下方，
        // 而不是侧边栏贯穿到顶部。
        let mut close_requested = false;
        egui::Panel::top("top_bar")
            .frame(egui::Frame::NONE)
            .resizable(false)
            .show(root_ui, |ui| {
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

        // jterm2 prefers jsh as its shell, so it is worth noticing when the
        // machine has none or an old one. The row draws nothing until the
        // background check has something actionable to offer.
        if self.render_jsh_notice(root_ui) {
            self.install_or_update_jsh();
        }

        // 侧边栏：在顶栏之后声明，占据顶栏下方区域的左侧。
        self.render_sidebar(root_ui);

        egui::CentralPanel::default()
            .frame(frame)
            .show(root_ui, |ui| {
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

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let preserve_paste_event = {
            let terminal = self
                .session_manager
                .get_active_session_mut()
                .terminal
                .lock();
            terminal.is_paste_events_enabled()
        };

        // Egui-winit swallows Ctrl+V press when clipboard has no text (e.g. image only),
        // leaving only V's release. Track real/semantic V presses across frames so only
        // a genuinely orphaned release restores the application-facing Ctrl+V event.
        if raw_input.focused {
            restore_missing_image_paste_key_event(&mut raw_input.events, &mut self.paste_key_state);
        } else {
            self.paste_key_state.reset();
        }

        // Event::Paste has no per-event modifiers. Recover Shift from V's
        // release when a whole Ctrl+Shift+V chord lands in one input batch;
        // the batch-level modifier snapshot may already be empty by now.
        let shortcut_modifiers = semantic_paste_modifiers(&raw_input.events, raw_input.modifiers);

        // egui-winit turns Ctrl/Cmd+C/X/V into semantic clipboard events and skips the
        // corresponding Key press. Restore those as Key events so the terminal can receive
        // control bytes, while still preventing egui's default text-edit shortcut behavior.
        let restore_shortcuts = should_restore_terminal_shortcut_event(ctx, shortcut_modifiers);

        normalize_terminal_shortcut_events(
            &mut raw_input.events,
            shortcut_modifiers,
            restore_shortcuts,
            preserve_paste_event,
        );
    }

    fn ui(&mut self, root_ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // eframe 0.35 起将原来的 App::update 拆成了 `logic` 和 `ui` 两段:
        // 这里把整个 update 的逻辑迁到 ui 中。许多下游代码(viewport 命令、输入查询、重绘请求)
        // 仍需要 &Context,从 root_ui 上 clone 一份(Arc 引用计数,几乎零成本)即可,
        // 与 root_ui 的可变借用互不冲突。
        let ctx_owned = root_ui.ctx().clone();
        let ctx = &ctx_owned;

        // 检查是否收到退出信号（SIGINT/SIGTERM）
        if SHUTDOWN_REQUESTED.load(Ordering::Relaxed) {
            crate::debug_log!("[SIGNAL] Shutdown requested, exiting gracefully");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        self.debug_panel.record_frame();

        // A stateful mouse edge admitted in an earlier frame is older than
        // every keyboard/IME event arriving now. Retry it before any session
        // gets a chance to flush user input. If capacity is still unavailable,
        // keep accepting bytes only into that session's bounded retry buffer
        // and hold a one-frame admission barrier for the captured writer.
        let mut retire_mouse_capture = false;
        let mut prior_mouse_write_error = None;
        let prior_mouse_control_result = self
            .terminal_mouse_capture
            .as_mut()
            .filter(|capture| capture.reported_to_app && !capture.pending_controls.is_empty())
            .map(flush_mouse_controls);
        if let Some(Err(error)) = prior_mouse_control_result {
            retire_mouse_capture = !error.is_backpressure();
            prior_mouse_write_error = Some(error);
        }
        if self
            .terminal_mouse_capture
            .as_ref()
            .is_some_and(mouse_capture_is_complete)
        {
            retire_mouse_capture = true;
        }
        if retire_mouse_capture {
            self.terminal_mouse_capture = None;
            self.last_terminal_mouse_motion = None;
        }
        let user_input_barrier_session_id = self
            .terminal_mouse_capture
            .as_ref()
            .filter(|capture| capture.reported_to_app && !capture.pending_controls.is_empty())
            .map(|capture| capture.session_id.clone());
        if let Some(error) = prior_mouse_write_error {
            self.set_status_for(format!("鼠标报告发送失败：{error}"), Duration::from_secs(3));
            if error.is_backpressure() {
                ctx.request_repaint_after(Duration::from_millis(10));
            }
        }

        // Render-time click navigation is tagged with its originating
        // terminal and staged now, before this frame's IME, Ctrl commands, or
        // keyboard bytes. A modal that already owned input when the frame
        // began discards the stale navigation instead.
        let initially_blocked = self.terminal_input_blocked(ctx);
        let (has_cursor_move_input, cursor_move_retry_overflow) =
            self.stage_prior_cursor_moves(initially_blocked);

        // Keep every PTY moving, not just the focused tab. All visible panes
        // receive priority, while hidden tabs rotate fairly. Background
        // parsing consumes at most half of the global adaptive byte budget;
        // whatever it does not use remains available to the active session.
        let visible_sessions: Vec<usize> = self.layout().session_indices();
        let background_budget = if self.session_manager.len() > 1 {
            self.adaptive_frame_budget / 2
        } else {
            0
        };
        let background_parse_started = std::time::Instant::now();
        let mut background_pump = self.session_manager.pump_inactive_sessions(
            background_budget,
            &visible_sessions,
            user_input_barrier_session_id.as_deref(),
        );
        let mut terminal_parse_time = background_parse_started.elapsed();
        let window_focused = ctx.input(|input| input.viewport().focused.unwrap_or(true));
        for (session_idx, completed) in background_pump.completed_command_outputs.drain(..) {
            self.agent_panel.handle_completed(session_idx, &completed);
            if let Some(session) = self.session_manager.sessions().get(session_idx) {
                self.git_strip_cache
                    .mark_command_finished(&session.metadata.session_id);
            }
            // A visible split pane is watched exactly when the window itself
            // has focus; a hidden tab is never watched.
            maybe_notify_long_command(
                &self.config,
                &mut self.notification_window_started,
                &mut self.notifications_in_window,
                &completed,
                window_focused && visible_sessions.contains(&session_idx),
            );
            if let Err(error) = execution_journal::submit(completed) {
                log::warn!("jsh execution output journal queue rejected an event: {error:?}");
            }
        }
        for (session_idx, error) in background_pump.errors.drain(..) {
            log::warn!("background session {}: {}", session_idx + 1, error);
        }
        for (session_idx, requests) in background_pump.clipboard_requests.drain(..) {
            let response_tx = self.session_manager.protocol_response_sender(session_idx);
            if let Some(session) = self.session_manager.get_session_mut(session_idx) {
                if let Some(response_tx) = response_tx {
                    service_osc5522_clipboard_requests(
                        self.clipboard.is_some(),
                        &self.clipboard_request_in_flight,
                        Arc::clone(&session.terminal),
                        response_tx,
                        requests,
                    );
                }
            }
        }
        if self.config.osc52_clipboard_write {
            for (_session_idx, text) in background_pump.osc52_writes.drain(..) {
                enqueue_osc52_clipboard_write(
                    self.osc52_clipboard_write_tx.as_ref(),
                    &mut self.osc52_write_window_started,
                    &mut self.osc52_writes_in_window,
                    text,
                );
            }
        }
        if self.config.osc52_clipboard_read {
            for session_idx in background_pump.osc52_queries.drain(..) {
                let response_route = self
                    .session_manager
                    .protocol_response_sender(session_idx)
                    .zip(
                        self.session_manager
                            .sessions()
                            .get(session_idx)
                            .map(|session| Arc::clone(&session.terminal)),
                    );
                if let Some((response_tx, terminal)) = response_route {
                    service_osc52_clipboard_query(
                        self.clipboard.is_some(),
                        &self.clipboard_request_in_flight,
                        terminal,
                        response_tx,
                        &mut self.osc52_read_window_started,
                        &mut self.osc52_reads_in_window,
                    );
                }
            }
        }
        for (_session_idx, title, body) in background_pump.notifications.drain(..) {
            show_desktop_notification(
                self.notification_tx.as_ref(),
                &mut self.notification_window_started,
                &mut self.notifications_in_window,
                title,
                body,
            );
        }
        background_pump.exited_indices.sort_unstable();
        background_pump.exited_indices.dedup();
        // 按稳定 ID 而不是索引关闭:关掉一个会话会让它之后的索引整体左移,
        // 而关掉一个只剩一个窗格的 tab 还会连带关掉该 tab 的其他会话,索引
        // 可能往任意方向漂移。ID 查不到就说明它已经被前一次关闭带走了。
        let exited_ids: Vec<String> = background_pump
            .exited_indices
            .iter()
            .filter_map(|&idx| {
                self.session_manager
                    .sessions()
                    .get(idx)
                    .map(|session| session.metadata.session_id.clone())
            })
            .collect();
        for session_id in exited_ids {
            let Some(session_idx) = self.session_manager.index_of(&session_id) else {
                continue;
            };
            if self.session_manager.len() > 1 && session_idx != self.session_manager.active_index()
            {
                self.close_session_or_owning_tab(session_idx);
                self.schedule_session_save();
            }
        }
        let background_processed_bytes = background_pump.bytes_processed;
        let active_output_budget = self
            .adaptive_frame_budget
            .saturating_sub(background_processed_bytes)
            .max(1);
        let background_had_output = background_pump.had_output;
        let background_has_more = background_pump.has_more;

        // Collect events once per frame to avoid multiple clones
        self.frame_events.clear();
        ctx.input(|i| self.frame_events.extend(i.events.iter().cloned()));

        let mut terminal_input_blocked = self.terminal_input_blocked(ctx);
        let has_preedit = if terminal_input_blocked {
            // UI text fields and modal dialogs own IME/keyboard input while open.
            // In particular, an IME commit must never be mirrored into the PTY.
            self.session_manager
                .get_active_session_mut()
                .terminal
                .lock()
                .clear_preedit();
            false
        } else {
            self.handle_ime_events(ctx)
        };

        if !terminal_input_blocked {
            self.handle_font_zoom(ctx);
        }

        // Step 2: 处理快捷键 - 使用可配置的快捷键系统。
        // 命令面板与帮助面板也通过 Command 派发，确保按键会被消费且
        // 帮助文案始终反映当前绑定。

        let (palette_requested_close, palette_owned_input) = self.handle_command_palette_input(ctx);
        if palette_requested_close {
            return;
        }

        if self.handle_keybindings(ctx, terminal_input_blocked || palette_owned_input) {
            return;
        }

        // A command handled above may have opened or closed a modal. Re-evaluate
        // newly opened surfaces, but never release a frame that a UI surface
        // owned at its start: later events in the same OS batch must not escape
        // into the PTY after the modal-closing shortcut.
        terminal_input_blocked = app::input::terminal_input_blocked_after_commands(
            terminal_input_blocked,
            palette_owned_input,
            self.terminal_input_blocked(ctx),
        );

        // Route pointer input to the pane under the pointer before taking the
        // active-session borrow below. The renderer used to switch focus only
        // at the end of the frame, which sent a click (and mouse protocol
        // coordinates) to the previously focused PTY.
        let pointer_targets_terminal = !terminal_input_blocked
            && ctx.input(|input| {
                input.pointer.button_pressed(egui::PointerButton::Primary)
                    || input.pointer.button_pressed(egui::PointerButton::Secondary)
                    || input.pointer.button_pressed(egui::PointerButton::Middle)
                    || input
                        .events
                        .iter()
                        .any(|event| matches!(event, egui::Event::MouseWheel { .. }))
            });
        if pointer_targets_terminal && self.layout().panes().len() > 1 {
            if let Some(pos) =
                ctx.input(|input| input.pointer.interact_pos().or(input.pointer.hover_pos()))
            {
                let on_divider = self.layout().divider_at(pos).is_some();
                if !on_divider && self.layout_mut().focus_pane_at(pos).is_some() {
                    self.sync_active_session_to_focused_pane();
                }
            }
        }

        let active_session_idx = self.session_manager.active_index();
        let active_pane_renderer_idx = (self.layout().panes().len() > 1).then(|| {
            self.layout()
                .panes()
                .iter()
                .position(|pane| pane.id == self.layout().focused_pane_id)
        });
        let active_pane_renderer_idx = active_pane_renderer_idx.flatten();
        let active_terminal_content_rect = if let Some(index) = active_pane_renderer_idx {
            self.pane_renderers
                .get(index)
                .and_then(|renderer| renderer.last_content_rect)
        } else {
            self.renderer.last_content_rect
        };
        let pointer_over_active_terminal = ctx
            .input(|input| input.pointer.interact_pos().or(input.pointer.hover_pos()))
            .zip(active_terminal_content_rect)
            .is_some_and(|(pointer, rect)| rect.contains(pointer));

        // 获取当前活跃会话（在所有快捷键处理完后）
        let session_count_before = self.session_manager.len();
        let mut shell_exited = false;
        // A shell that dies before it could ever have shown a prompt is a
        // startup failure, not the user leaving. Closing the window on it
        // makes jterm2 look like it "exits as soon as it runs", hiding the
        // real cause (bad `shell =` config, unusable cwd, wrong binary).
        let mut shell_startup_failed = false;

        // Step 2.5: 搜索面板事件处理
        if self.pending_paste_confirm.is_none() && !self.search_replace_panel.is_open {
            self.handle_search_panel_input();
        }

        if let Some(Err(error)) = self
            .session_manager
            .flush_protocol_responses(active_session_idx)
        {
            if !error.is_backpressure() {
                log::warn!("active protocol response queue stopped: {error}");
            }
        }
        let active_protocol_responses = self
            .session_manager
            .protocol_response_sender(active_session_idx)
            .expect("active session has an aligned protocol response queue");

        // Snapshot active session index before mutably borrowing session_manager;
        // we use it to tag pending-paste confirmations so they only deliver to
        // the same tab if the user hasn't switched away.
        let session = self.session_manager.get_active_session_mut();
        let active_session_id = session.metadata.session_id.clone();

        // Step 3: semantic application paste events. Host copy/paste keyboard
        // shortcuts are dispatched above through configurable commands.
        let events_copy =
            app::input::routed_terminal_events(&self.frame_events, terminal_input_blocked);
        let mut consumed_keys = std::collections::HashSet::new();

        let saw_semantic_paste = events_copy.iter().any(|event| {
            if let egui::Event::Paste(_content) = event {
                crate::debug_log!(
                    "[EVENT] detected Paste event: {:?}",
                    if _content.is_empty() {
                        "empty"
                    } else {
                        "has content"
                    }
                );
                true
            } else {
                false
            }
        });

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
                // MIME discovery is host clipboard I/O and build_paste_event
                // replaces the terminal's single-use grant. Serialize it with
                // OSC reads so concurrent Paste events cannot race tokens or
                // create an unbounded helper/thread population.
                if self
                    .clipboard_request_in_flight
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    crate::debug_log!("[PASTE] app supports paste events, building paste event");
                    let terminal = Arc::clone(&session.terminal);
                    let response_tx = active_protocol_responses.clone();
                    let in_flight = Arc::clone(&self.clipboard_request_in_flight);
                    let spawn_result = std::thread::Builder::new()
                        .name("paste-event-sender".to_string())
                        .spawn(move || {
                            let _guard = ClipboardRequestGuard(in_flight);
                            let mime_types = ClipboardManager::new()
                                .and_then(|clipboard| clipboard.available_mime_types())
                                .unwrap_or_default();
                            crate::debug_log!("[PASTE] available MIME types: {:?}", mime_types);
                            let bytes = terminal.lock().build_paste_event(&mime_types);
                            crate::debug_log!(
                                "[OSC5522] sending unsolicited paste MIME list ({} bytes)",
                                bytes.len()
                            );
                            if let Err(error) = response_tx.enqueue_blocking(bytes) {
                                log::debug!("OSC 5522 unsolicited paste event cancelled: {error}");
                            }
                        });
                    if let Err(error) = spawn_result {
                        self.clipboard_request_in_flight
                            .store(false, Ordering::Release);
                        log::warn!("failed to spawn OSC 5522 paste event worker: {error}");
                        self.status_message = "剪贴板正忙，请重试粘贴".to_string();
                        self.status_expires_at =
                            Some(std::time::Instant::now() + Duration::from_secs(3));
                    }
                } else {
                    self.status_message = "剪贴板正忙，请稍后重试粘贴".to_string();
                    self.status_expires_at =
                        Some(std::time::Instant::now() + Duration::from_secs(3));
                }
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
                                match paste_text_into_session(
                                    session,
                                    text,
                                    self.config.paste_confirm,
                                    false,
                                    &mut self.pending_paste_confirm,
                                ) {
                                    Ok(true) => {
                                        consumed_keys.insert("PasteEvent".to_string());
                                    }
                                    Ok(false) => {
                                        crate::debug_log!("[PASTE] fallback: text is empty");
                                    }
                                    Err(error) => {
                                        self.status_message = format!("粘贴失败：{error}");
                                        self.status_expires_at = Some(
                                            std::time::Instant::now() + Duration::from_secs(4),
                                        );
                                    }
                                }
                            }
                            ClipboardContent::Binary(_bytes) => {
                                crate::debug_log!(
                                    "[PASTE] fallback: BINARY content ({} bytes)",
                                    _bytes.len()
                                );
                                crate::debug_log!(
                                    "[PASTE] refusing to send {} binary bytes as PTY input; app did not negotiate OSC 5522",
                                    _bytes.len()
                                );
                                self.status_message = "图像粘贴需要应用支持 OSC 5522".to_string();
                                self.status_expires_at =
                                    Some(std::time::Instant::now() + Duration::from_secs(4));
                                consumed_keys.insert("PasteEvent".to_string());
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
        if !terminal_input_blocked {
            let (
                keyboard_enhancement_flags,
                report_all_keys_mode,
                xterm_modify_other_keys,
                xterm_format_other_keys,
                application_cursor_keys,
                alt_screen,
            ) = {
                let terminal = session.terminal.lock();
                (
                    terminal.keyboard_enhancement_flags(),
                    terminal.is_report_all_keys_enabled(),
                    terminal.xterm_modify_other_keys(),
                    terminal.xterm_format_other_keys(),
                    terminal.is_application_cursor_keys(),
                    terminal.is_alt_buffer_active(),
                )
            };
            // 转换 consumed_keys 为需要的格式（HashSet<&str>）
            let consumed_keys_refs: std::collections::HashSet<&str> =
                consumed_keys.iter().map(|s| s.as_str()).collect();
            let input_renderer = active_pane_renderer_idx
                .and_then(|index| self.pane_renderers.get(index))
                .unwrap_or(&self.renderer);
            input_renderer.handle_keyboard_input(
                ctx,
                &mut self.keyboard_input_buffer,
                &consumed_keys_refs,
                has_preedit,
                keyboard_enhancement_flags,
                report_all_keys_mode,
                xterm_modify_other_keys,
                xterm_format_other_keys,
                application_cursor_keys,
                alt_screen,
                &self.frame_events,
            );
        }

        let has_keyboard_input = !self.keyboard_input_buffer.is_empty();

        // 有输入活动时更新最后活动时间
        if has_keyboard_input || has_cursor_move_input {
            self.last_activity_time = std::time::Instant::now();
        }

        // The retry buffer is per-session and sent as one FIFO message. Do not
        // split arbitrary bytes into frame-sized chunks: terminal replies could
        // otherwise interleave inside a UTF-8/key escape/paste sequence.
        let mut terminal_write_error = None;
        let mut input_retry_overflow = cursor_move_retry_overflow;
        {
            if has_keyboard_input {
                input_retry_overflow |= !session.queue_input(&self.keyboard_input_buffer);
            }
            let user_input_flush_blocked =
                crate::session_manager::user_input_is_blocked_by_mouse_edge(
                    &session.metadata.session_id,
                    user_input_barrier_session_id.as_deref(),
                );
            if !user_input_flush_blocked && !session.pending_input.is_empty() {
                session.terminal.lock().scroll_to_bottom();
                match session.shell.write(&session.pending_input) {
                    Ok(()) => session.pending_input.clear(),
                    Err(error) => {
                        if !error.is_backpressure() {
                            session.pending_input.clear();
                        }
                        terminal_write_error = Some(error);
                    }
                }
            }
        }
        if let Some(error) = terminal_write_error {
            if error.is_backpressure() {
                self.status_message = "终端输入繁忙，正在重试…".to_string();
                ctx.request_repaint_after(Duration::from_millis(10));
            } else {
                self.status_message = format!("终端输入失败：{error}");
            }
            self.status_expires_at = Some(std::time::Instant::now() + Duration::from_secs(3));
        }
        if input_retry_overflow {
            self.status_message = "终端输入重试缓冲区已满，新输入未发送".to_string();
            self.status_expires_at = Some(std::time::Instant::now() + Duration::from_secs(4));
        }
        if !session.pending_input.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(10));
        }

        // Force repaint if we have any keyboard/cursor input - ensures input renders immediately
        if has_keyboard_input || has_cursor_move_input {
            ctx.request_repaint();
        }

        // Step 6: 处理 shell 事件
        // 关键：限制每帧处理的总字节数，防止大量 ANSI 数据阻塞 UI 线程导致假死。
        // 超出限制的数据保存到当前 session 自己的 pending_output，
        // 下一帧继续处理。绝不能放在 TerminalApp 全局缓冲中：帧间切 tab
        // 会把旧 session 的 ANSI 字节流喂给新 session，造成串屏和终端模式污染。
        // 使用自适应帧预算，根据帧时间动态调整
        let mut has_new_output = false;
        let max_bytes_per_frame = active_output_budget;
        let mut has_more_data = false;
        let mut active_processed_bytes = 0;

        // 先取回上一帧未处理完的数据
        let mut accumulated_data = std::mem::take(&mut session.pending_output);
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
                        let uptime = session.shell.uptime();
                        if code != 0 && uptime < SHELL_STARTUP_GRACE {
                            shell_startup_failed = true;
                            self.status_message = format!(
                                "Shell failed to start (exit code {code}). Check the `shell` setting in the config panel."
                            );
                            self.status_expires_at = None;
                        } else {
                            self.status_message = format!("Shell exited with code: {}", code);
                            self.status_expires_at =
                                Some(std::time::Instant::now() + Duration::from_secs(6));
                        }
                        has_new_output = true;
                        shell_exited = true;
                        break;
                    }
                    Ok(ShellEvent::Error(e)) => {
                        self.status_message = format!("Error: {}", e);
                        self.status_expires_at =
                            Some(std::time::Instant::now() + Duration::from_secs(6));
                        has_new_output = true;
                        break;
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
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
            session.pending_output = accumulated_data.split_off(max_bytes_per_frame);
            has_more_data = true;
        }
        // 也检查 channel 中是否还有数据
        if !has_more_data && !session.shell.events().is_empty() {
            has_more_data = true;
        }

        // 处理本帧的数据
        if !accumulated_data.is_empty() {
            if active_protocol_responses.has_pending() {
                // Preserve PTY byte order and stop accepting more protocol
                // requests until their older replies have entered the bounded
                // shell writer. The current chunk precedes the split-off tail.
                accumulated_data.append(&mut session.pending_output);
                session.pending_output = accumulated_data;
                has_more_data = true;
            } else {
                let mut terminal = session.terminal.lock();
                let active_parse_started = std::time::Instant::now();
                terminal.process_batch(&accumulated_data);
                terminal.check_sync_output_timeout();
                terminal_parse_time += active_parse_started.elapsed();
                active_processed_bytes = accumulated_data.len();
                let completed_outputs = terminal.take_completed_command_outputs();
                // 不再每帧清空 status_message:它由 set_status*/current_status_for_display
                // 按时长自动过期,否则任何快速输出都会把瞬时反馈瞬间吞掉。
                // 有输出时更新最后活动时间
                self.last_activity_time = std::time::Instant::now();
                drop(terminal);
                for completed in completed_outputs {
                    self.agent_panel
                        .handle_completed(active_session_idx, &completed);
                    self.git_strip_cache
                        .mark_command_finished(&session.metadata.session_id);
                    // The active pane is on screen, so its completion was
                    // watched whenever the window itself had focus.
                    maybe_notify_long_command(
                        &self.config,
                        &mut self.notification_window_started,
                        &mut self.notifications_in_window,
                        &completed,
                        window_focused,
                    );
                    if let Err(error) = execution_journal::submit(completed) {
                        log::warn!(
                            "jsh execution output journal queue rejected an event: {error:?}"
                        );
                    }
                }
            }
        }

        let processed_bytes = background_processed_bytes.saturating_add(active_processed_bytes);
        let output_backlogged = background_has_more
            || has_more_data
            || processed_bytes >= self.adaptive_frame_budget.saturating_mul(3) / 4;
        self.adaptive_frame_budget = app::rendering::adapt_frame_budget(
            self.adaptive_frame_budget,
            processed_bytes,
            terminal_parse_time,
            output_backlogged,
        );

        // Step 7: 发送终端输出回 shell（DSR 响应等）
        {
            let mut terminal = session.terminal.lock();
            terminal.check_sync_output_timeout();
            let output = terminal.get_output();
            if let Err((error, mut output)) = active_protocol_responses.try_enqueue(output) {
                if error == ProtocolResponseQueueError::Full {
                    output.append(&mut terminal.output_buffer);
                    terminal.output_buffer = output;
                    has_more_data = true;
                } else {
                    log::warn!("terminal protocol response queue rejected output: {error}");
                }
            }
            let clipboard_requests = terminal.take_clipboard_read_requests();
            drop(terminal);
            if let Err(error) = active_protocol_responses.flush(&session.shell) {
                if !error.is_backpressure() {
                    log::warn!("terminal protocol response queue stopped: {error}");
                }
            }
            service_osc5522_clipboard_requests(
                self.clipboard.is_some(),
                &self.clipboard_request_in_flight,
                Arc::clone(&session.terminal),
                active_protocol_responses.clone(),
                clipboard_requests,
            );
        }

        // OSC 52 clipboard handling
        {
            let mut terminal = session.terminal.lock();
            if let Some(text) = terminal.take_osc52_clipboard_set() {
                if self.config.osc52_clipboard_write {
                    enqueue_osc52_clipboard_write(
                        self.osc52_clipboard_write_tx.as_ref(),
                        &mut self.osc52_write_window_started,
                        &mut self.osc52_writes_in_window,
                        text,
                    );
                }
            }
            let osc52_query = terminal.take_osc52_clipboard_query();
            drop(terminal);
            // Reading the clipboard exposes user data to a terminal program,
            // so it remains opt-in. Even when enabled, the external helper
            // and base64 encoding run only on the bounded background path.
            if osc52_query && self.config.osc52_clipboard_read {
                service_osc52_clipboard_query(
                    self.clipboard.is_some(),
                    &self.clipboard_request_in_flight,
                    Arc::clone(&session.terminal),
                    active_protocol_responses.clone(),
                    &mut self.osc52_read_window_started,
                    &mut self.osc52_reads_in_window,
                );
            }
        }

        // OSC 9/777 desktop notifications
        {
            let mut terminal = session.terminal.lock();
            let notifications: Vec<_> = terminal.pending_notifications.drain(..).collect();
            drop(terminal);
            for (title, body) in notifications {
                show_desktop_notification(
                    self.notification_tx.as_ref(),
                    &mut self.notification_window_started,
                    &mut self.notifications_in_window,
                    title,
                    body,
                );
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
        let page_scroll_key = (!terminal_input_blocked)
            .then(|| {
                ctx.input(|i| {
                    if i.key_pressed(egui::Key::PageUp) {
                        Some((egui::Key::PageUp, i.modifiers))
                    } else if i.key_pressed(egui::Key::PageDown) {
                        Some((egui::Key::PageDown, i.modifiers))
                    } else {
                        None
                    }
                })
            })
            .flatten();
        let scroll_amount = page_scroll_key.and_then(|(key, modifiers)| {
            let terminal = session.terminal.lock();
            let (_, rows) = terminal.get_dimensions();
            app::input::viewport_scroll_delta(key, modifiers, rows)
        });

        if let Some(amount) = scroll_amount {
            let mut terminal = session.terminal.lock();
            terminal.scroll(amount);
        }

        let scroll_delta = ctx.input(|i| i.smooth_scroll_delta.y);
        let ctrl_scroll_this_frame = self.frame_events.iter().any(
            |event| matches!(event, egui::Event::MouseWheel { modifiers, .. } if modifiers.ctrl),
        );

        // 检查是否启用鼠标报告
        let mouse_enabled = {
            let terminal = session.terminal.lock();
            terminal.is_mouse_enabled()
        };
        let shift_mouse_bypass = ctx.input(|input| input.modifiers.shift);

        let middle_paste_requested = !terminal_input_blocked
            && (!mouse_enabled || shift_mouse_bypass)
            && pointer_over_active_terminal
            && ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Middle));

        if middle_paste_requested {
            if let Some(clipboard) = &self.clipboard {
                let primary_text = clipboard.paste_primary().unwrap_or_default();
                let text = if primary_text.is_empty() {
                    clipboard.paste().unwrap_or_default()
                } else {
                    primary_text
                };
                if let Err(error) = paste_text_into_session(
                    session,
                    text,
                    self.config.paste_confirm,
                    false,
                    &mut self.pending_paste_confirm,
                ) {
                    self.status_message = format!("粘贴失败：{error}");
                    self.status_expires_at =
                        Some(std::time::Instant::now() + Duration::from_secs(4));
                }
            }
        }

        // 鼠标滚轮处理：
        // 1. 如果应用启用了鼠标报告（如 vim），滚轮会在下面的鼠标处理部分发送给应用
        // 2. 如果应用未启用鼠标，或在普通终端，滚轮用于查看历史
        if !terminal_input_blocked
            && pointer_over_active_terminal
            && scroll_delta != 0.0
            && !ctrl_scroll_this_frame
            && (!mouse_enabled || shift_mouse_bypass)
        {
            // 0.35 阻尼系数：原始的 scroll_speed 直接乘 delta 会让单次滚轮累积约 7 倍位移，滑得太快
            const SCROLL_VELOCITY_DAMPING: f32 = 0.35;
            self.smooth_scroll_velocity +=
                scroll_delta * self.config.scroll_speed as f32 * SCROLL_VELOCITY_DAMPING;
        }

        // Smooth scroll physics
        if self.smooth_scroll_velocity.abs() > 0.1 {
            self.smooth_scroll_velocity *= 0.88;

            let line_h = active_pane_renderer_idx
                .and_then(|index| self.pane_renderers.get(index))
                .unwrap_or(&self.renderer)
                .line_height
                .max(1.0);

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
            if let Some(index) = active_pane_renderer_idx {
                if let Some(renderer) = self.pane_renderers.get_mut(index) {
                    renderer.scroll_pixel_offset = -self.smooth_scroll_pixel_offset;
                }
            } else {
                self.renderer.scroll_pixel_offset = -self.smooth_scroll_pixel_offset;
            }
            if !hit_boundary {
                ctx.request_repaint();
            }
        } else if self.smooth_scroll_velocity.abs() > 0.0 {
            self.smooth_scroll_velocity = 0.0;
            self.smooth_scroll_pixel_offset = 0.0;
            if let Some(index) = active_pane_renderer_idx {
                if let Some(renderer) = self.pane_renderers.get_mut(index) {
                    renderer.scroll_pixel_offset = 0.0;
                }
            } else {
                self.renderer.scroll_pixel_offset = 0.0;
            }
        }

        // Step 11: 鼠标处理（包括滚轮）
        let terminal_button_pressed = ctx.input(|input| {
            if input.pointer.button_pressed(egui::PointerButton::Primary) {
                Some(0)
            } else if input.pointer.button_pressed(egui::PointerButton::Secondary) {
                Some(2)
            } else if input.pointer.button_pressed(egui::PointerButton::Middle) {
                Some(1)
            } else {
                None
            }
        });
        let terminal_buttons_released = ctx.input(|input| {
            let mut buttons: SmallVec<[u8; 3]> = SmallVec::new();
            if input.pointer.button_released(egui::PointerButton::Primary) {
                buttons.push(0);
            }
            if input
                .pointer
                .button_released(egui::PointerButton::Secondary)
            {
                buttons.push(2);
            }
            if input.pointer.button_released(egui::PointerButton::Middle) {
                buttons.push(1);
            }
            buttons
        });
        let pointer_pos =
            ctx.input(|input| input.pointer.interact_pos().or(input.pointer.hover_pos()));
        if let Some(button) = terminal_button_pressed.filter(|button| {
            self.terminal_mouse_capture.is_none()
                && (mouse_enabled || *button == 0)
                && !terminal_input_blocked
                && pointer_over_active_terminal
        }) {
            let pointer_renderer = active_pane_renderer_idx
                .and_then(|index| self.pane_renderers.get(index))
                .unwrap_or(&self.renderer);
            let content_rect = pointer_renderer
                .last_content_rect
                .unwrap_or_else(|| ctx.viewport_rect());
            let (mouse_cols, mouse_rows) = session.terminal.lock().get_dimensions();
            let (last_row, last_col) = grid_position_from_content(
                pointer_pos.unwrap_or_else(|| content_rect.center()),
                content_rect,
                pointer_renderer.char_width,
                pointer_renderer.line_height,
                mouse_cols,
                mouse_rows,
            );
            self.terminal_mouse_capture = Some(crate::app::state::TerminalMouseCapture {
                session_id: active_session_id.clone(),
                reported_to_app: mouse_enabled && !shift_mouse_bypass,
                button,
                terminal: Arc::clone(&session.terminal),
                write_tx: session.shell.write_sender(),
                content_rect,
                char_width: pointer_renderer.char_width,
                line_height: pointer_renderer.line_height,
                last_col,
                last_row,
                pending_controls: std::collections::VecDeque::new(),
                press_accepted: false,
                release_observed: false,
                local_selection_cancelled: false,
            });
        }

        let pointer_any_down = ctx.input(|input| input.pointer.any_down());
        let capture_button_explicitly_released = self
            .terminal_mouse_capture
            .as_ref()
            .is_some_and(|capture| terminal_buttons_released.contains(&capture.button));
        let capture_finished = self.terminal_mouse_capture.is_some()
            && (capture_button_explicitly_released || !pointer_any_down);
        if capture_finished {
            if let Some(capture) = self.terminal_mouse_capture.as_mut() {
                capture.release_observed = true;
            }
        }
        let primary_copy_route = primary_copy_route(
            self.terminal_mouse_capture.as_ref().map(|capture| {
                (
                    capture.reported_to_app,
                    capture.local_selection_cancelled,
                    capture.button,
                )
            }),
            capture_finished,
            terminal_buttons_released.contains(&0),
        );
        let local_primary_selection_terminal =
            if primary_copy_route == PrimaryCopyRoute::CapturedLocal {
                self.terminal_mouse_capture
                    .as_ref()
                    .map(|capture| Arc::clone(&capture.terminal))
            } else {
                None
            };
        let capture_for_route = self.terminal_mouse_capture.as_ref();
        let capture_route_state =
            capture_for_route.map(|capture| (capture.reported_to_app, capture.button));
        let pointer_routes_to_terminal =
            pointer_over_active_terminal || capture_for_route.is_some();
        let sequence_reports_to_app = capture_route_state
            .map(|(reported_to_app, _)| reported_to_app)
            .unwrap_or(!shift_mouse_bypass);
        let reported_capture_release = captured_release_button(
            capture_route_state,
            &terminal_buttons_released,
            pointer_any_down,
        );
        let only_release = terminal_input_blocked;

        let (
            mouse_terminal,
            mouse_write_tx,
            mouse_session_id,
            content_rect,
            char_width,
            line_height,
            fallback_cell,
        ) = if let Some(capture) = capture_for_route {
            (
                Arc::clone(&capture.terminal),
                capture.write_tx.clone(),
                capture.session_id.clone(),
                capture.content_rect,
                capture.char_width,
                capture.line_height,
                Some((capture.last_row, capture.last_col)),
            )
        } else {
            let pointer_renderer = active_pane_renderer_idx
                .and_then(|index| self.pane_renderers.get(index))
                .unwrap_or(&self.renderer);
            (
                Arc::clone(&session.terminal),
                session.shell.write_sender(),
                active_session_id.clone(),
                pointer_renderer
                    .last_content_rect
                    .unwrap_or_else(|| ctx.viewport_rect()),
                pointer_renderer.char_width,
                pointer_renderer.line_height,
                None,
            )
        };
        let mut mouse_route_closed = false;
        let lossy_mouse_reports: Vec<Vec<u8>> = if (!sequence_reports_to_app
            || !pointer_routes_to_terminal
            || (terminal_input_blocked && reported_capture_release.is_none()))
            || (pointer_pos.is_none() && fallback_cell.is_none())
        {
            self.mouse_scroll_accumulator = 0.0;
            Vec::new()
        } else {
            let terminal = mouse_terminal.lock();
            if !terminal.is_mouse_enabled() {
                self.mouse_scroll_accumulator = 0.0;
                // The application disabled mouse reporting while a sequence was
                // active. An unaccepted press can be retired, but a release
                // already encoded behind backpressure must still follow its
                // accepted press on the original writer route.
                let queued_release = self.terminal_mouse_capture.as_ref().is_some_and(|capture| {
                    capture.pending_controls.iter().any(|pending| {
                        pending.kind == crate::app::state::PendingMouseControlKind::Release
                    })
                });
                mouse_route_closed = self.terminal_mouse_capture.is_some() && !queued_release;
                drop(terminal);
                Vec::new()
            } else {
                let mut reports = Vec::new();
                let (mouse_cols, mouse_rows) = terminal.get_dimensions();
                let (row, col) = pointer_pos
                    .map(|pos| {
                        grid_position_from_content(
                            pos,
                            content_rect,
                            char_width,
                            line_height,
                            mouse_cols,
                            mouse_rows,
                        )
                    })
                    .or(fallback_cell)
                    .unwrap_or((0, 0));
                if pointer_pos.is_some() {
                    if let Some(capture) = self.terminal_mouse_capture.as_mut() {
                        if capture.session_id == mouse_session_id {
                            capture.last_col = col;
                            capture.last_row = row;
                        }
                    }
                }

                if !only_release {
                    // 处理鼠标滚轮（当启用鼠标报告时）
                    let line_h = line_height.max(1.0);
                    let mut discrete_scroll_steps: isize = 0;
                    let mut point_scroll_delta: f32 = 0.0;

                    ctx.input(|i| {
                        for event in &i.events {
                            if let egui::Event::MouseWheel {
                                unit,
                                delta,
                                modifiers,
                                ..
                            } = event
                            {
                                if modifiers.ctrl {
                                    continue;
                                }
                                match unit {
                                    egui::MouseWheelUnit::Line => {
                                        discrete_scroll_steps = bounded_wheel_step_accumulate(
                                            discrete_scroll_steps,
                                            delta.y,
                                            1,
                                        );
                                    }
                                    egui::MouseWheelUnit::Page => {
                                        discrete_scroll_steps = bounded_wheel_step_accumulate(
                                            discrete_scroll_steps,
                                            delta.y,
                                            mouse_rows.max(1),
                                        );
                                    }
                                    egui::MouseWheelUnit::Point => {
                                        if delta.y.is_finite() {
                                            let limit =
                                                line_h * MAX_MOUSE_WHEEL_REPORTS_PER_FRAME as f32;
                                            point_scroll_delta =
                                                (point_scroll_delta + delta.y).clamp(-limit, limit);
                                        }
                                    }
                                }
                            }
                        }
                    });

                    if point_scroll_delta != 0.0 {
                        let limit = line_h * MAX_MOUSE_WHEEL_REPORTS_PER_FRAME as f32;
                        self.mouse_scroll_accumulator = (self.mouse_scroll_accumulator
                            + point_scroll_delta)
                            .clamp(-limit, limit);
                    }

                    let point_scroll_steps = ((self.mouse_scroll_accumulator / line_h) as isize)
                        .clamp(
                            -MAX_MOUSE_WHEEL_REPORTS_PER_FRAME,
                            MAX_MOUSE_WHEEL_REPORTS_PER_FRAME,
                        );
                    if point_scroll_steps != 0 {
                        self.mouse_scroll_accumulator -= point_scroll_steps as f32 * line_h;
                    }

                    let total_scroll_steps = discrete_scroll_steps
                        .saturating_add(point_scroll_steps)
                        .clamp(
                            -MAX_MOUSE_WHEEL_REPORTS_PER_FRAME,
                            MAX_MOUSE_WHEEL_REPORTS_PER_FRAME,
                        );
                    if total_scroll_steps != 0 {
                        let button = if total_scroll_steps > 0 { 64 } else { 65 };

                        for _ in 0..total_scroll_steps.unsigned_abs() {
                            if let Some(report) = terminal.get_mouse_report(button, col, row) {
                                reports.push(report);
                            }
                        }
                    }

                    if let Some(capture) = self.terminal_mouse_capture.as_ref() {
                        if capture.reported_to_app
                            && terminal_button_pressed == Some(capture.button)
                        {
                            if let Some(report) =
                                terminal.get_mouse_report(capture.button, col, row)
                            {
                                if let Some(capture) = self.terminal_mouse_capture.as_mut() {
                                    queue_mouse_control(
                                        &mut capture.pending_controls,
                                        crate::app::state::PendingMouseControlKind::Press,
                                        report,
                                    );
                                }
                            }
                        }
                    }

                    let pointer_moved =
                        ctx.input(|input| input.pointer.delta() != egui::Vec2::ZERO);
                    let motion_button = reported_capture_button(capture_route_state);
                    if pointer_moved
                        && terminal.should_report_mouse_motion(motion_button.is_some())
                        && self.last_terminal_mouse_motion.as_ref().is_none_or(
                            |(session_id, last_col, last_row)| {
                                session_id != &mouse_session_id
                                    || *last_col != col
                                    || *last_row != row
                            },
                        )
                    {
                        let base_button = motion_button.unwrap_or(3);
                        if let Some(report) =
                            terminal.get_mouse_report(base_button.saturating_add(32), col, row)
                        {
                            reports.push(report);
                            self.last_terminal_mouse_motion =
                                Some((mouse_session_id.clone(), col, row));
                        }
                    }
                }

                // A release is emitted exactly once and only for a press
                // captured by this terminal. Mode 1002 therefore cannot
                // see an orphan release after a drag began elsewhere.
                if let Some(button) = reported_capture_release {
                    if let Some(report) = terminal.get_mouse_release_report(button, col, row) {
                        if let Some(capture) = self.terminal_mouse_capture.as_mut() {
                            queue_mouse_control(
                                &mut capture.pending_controls,
                                crate::app::state::PendingMouseControlKind::Release,
                                report,
                            );
                        }
                    }
                }

                drop(terminal);
                reports
            }
        };

        let has_mouse_input = !lossy_mouse_reports.is_empty()
            || self
                .terminal_mouse_capture
                .as_ref()
                .is_some_and(|capture| !capture.pending_controls.is_empty());
        let mut mouse_write_error = None;
        if !mouse_route_closed {
            if let Some(capture) = self.terminal_mouse_capture.as_mut() {
                if capture.reported_to_app {
                    if let Err(error) = flush_mouse_controls(capture) {
                        if !error.is_backpressure() {
                            mouse_route_closed = true;
                        }
                        mouse_write_error = Some(error);
                    }
                }
            }
        }

        if !mouse_route_closed
            && mouse_write_error.is_none()
            && mouse_capture_allows_lossy(self.terminal_mouse_capture.as_ref())
        {
            for report in lossy_mouse_reports {
                if let Err(error) = mouse_write_tx.try_send(report) {
                    if !error.is_backpressure() && self.terminal_mouse_capture.is_some() {
                        mouse_route_closed = true;
                    }
                    mouse_write_error = Some(error);
                    break;
                }
            }
        }
        if let Some(error) = mouse_write_error {
            // Motion/wheel are deliberately lossy. Press/release remain queued
            // on the capture and are retried in order on the next frame.
            self.status_message = format!("鼠标报告发送失败：{error}");
            self.status_expires_at = Some(std::time::Instant::now() + Duration::from_secs(3));
            if error.is_backpressure() && self.terminal_mouse_capture.is_some() {
                ctx.request_repaint_after(Duration::from_millis(10));
            }
        }
        let capture_complete = self
            .terminal_mouse_capture
            .as_ref()
            .is_some_and(mouse_capture_is_complete);
        if mouse_route_closed || capture_complete {
            self.terminal_mouse_capture = None;
            self.last_terminal_mouse_motion = None;
        }

        // Step 12: 链接检测和交互
        if terminal_input_blocked {
            self.hovered_link = None;
        } else {
            let terminal_ptr = Arc::as_ptr(&session.terminal) as usize;
            let mut terminal = session.terminal.lock();
            let grid_version = terminal.get_grid_version();
            let scroll_offset = terminal.scroll_offset;
            let (link_cols, link_rows) = terminal.get_dimensions();
            let pointer = ctx.input(|input| input.pointer.hover_pos());

            self.hovered_link = if let Some(renderer_idx) = active_pane_renderer_idx {
                let needs_refresh = self
                    .pane_renderers
                    .get(renderer_idx)
                    .is_some_and(|renderer| {
                        grid_version != renderer.cached_links_grid_version
                            || scroll_offset != renderer.cached_links_scroll_offset
                            || terminal_ptr != renderer.cached_links_terminal_ptr
                    });
                if needs_refresh {
                    let visible_cells = terminal.get_visible_cells();
                    let row_wrapped = terminal.get_visible_row_wrapped();
                    let links = self
                        .link_detector
                        .detect_links_in_visible_cells_with_wrapping(&visible_cells, &row_wrapped);
                    if let Some(renderer) = self.pane_renderers.get_mut(renderer_idx) {
                        renderer.cached_links = Arc::new(links);
                        renderer.cached_links_grid_version = grid_version;
                        renderer.cached_links_scroll_offset = scroll_offset;
                        renderer.cached_links_terminal_ptr = terminal_ptr;
                    }
                }
                self.pane_renderers
                    .get(renderer_idx)
                    .and_then(|renderer| Some((renderer, pointer?, renderer.last_content_rect?)))
                    .and_then(|(renderer, pointer, rect)| {
                        link_at_pointer(
                            &renderer.cached_links,
                            pointer,
                            rect,
                            renderer.char_width,
                            renderer.line_height,
                            link_cols,
                            link_rows,
                        )
                    })
            } else {
                if grid_version != self.cached_links_grid_version
                    || scroll_offset != self.cached_links_scroll_offset
                    || terminal_ptr != self.cached_links_terminal_ptr
                {
                    let visible_cells = terminal.get_visible_cells();
                    let row_wrapped = terminal.get_visible_row_wrapped();
                    self.cached_links = self
                        .link_detector
                        .detect_links_in_visible_cells_with_wrapping(&visible_cells, &row_wrapped);
                    self.cached_links_grid_version = grid_version;
                    self.cached_links_scroll_offset = scroll_offset;
                    self.cached_links_terminal_ptr = terminal_ptr;
                }
                pointer
                    .zip(self.renderer.last_content_rect)
                    .and_then(|(pointer, rect)| {
                        link_at_pointer(
                            &self.cached_links,
                            pointer,
                            rect,
                            self.renderer.char_width,
                            self.renderer.line_height,
                            link_cols,
                            link_rows,
                        )
                    })
            };
            drop(terminal);
            if self.hovered_link.is_some() {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }

            // 链接悬停提示:在指针右下方画一个浮层显示完整 URL 和 Ctrl+Click 操作提示。
            // OSC8 等链接显示的"文本"可能与真实目标不同(例如 "click here"),
            // 鼠标悬停透出真实跳转目标,避免用户被诱导点击未知链接。
            if let Some(ref link) = self.hovered_link {
                if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
                    egui::Area::new(egui::Id::new("link_hover_tooltip"))
                        .order(egui::Order::Tooltip)
                        .fixed_pos(pos + egui::vec2(14.0, 18.0))
                        .interactable(false)
                        .show(ctx, |ui| {
                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                ui.set_max_width(520.0);
                                ui.label(egui::RichText::new(&link.text).monospace());
                                ui.label(egui::RichText::new("Ctrl+Click 打开").small().weak());
                            });
                        });
                }
            }

            // 处理 Ctrl+Click 打开链接
            if ctx.input(|i| {
                i.pointer.button_clicked(egui::PointerButton::Primary) && i.modifiers.ctrl
            }) {
                if let Some(link) = self.hovered_link.clone() {
                    let (msg, dur) = match link::open_link(&link) {
                        Ok(_) => (
                            format!("Opened: {}", link.text),
                            Duration::from_millis(2500),
                        ),
                        Err(e) => (
                            format!("Failed to open link: {}", e),
                            Duration::from_secs(4),
                        ),
                    };
                    self.status_message = msg;
                    self.status_expires_at = Some(std::time::Instant::now() + dur);
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
        self.render_ui(root_ui);

        if !self.terminal_input_blocked(ctx) {
            let selection_for_primary = match primary_copy_route {
                PrimaryCopyRoute::CapturedLocal => local_primary_selection_terminal
                    .and_then(|terminal| terminal.lock().copy_selection()),
                PrimaryCopyRoute::Generic => {
                    let session = self.session_manager.get_active_session_mut();
                    let terminal = session.terminal.lock();
                    if terminal.is_mouse_enabled() {
                        None
                    } else {
                        terminal.copy_selection()
                    }
                }
                PrimaryCopyRoute::None | PrimaryCopyRoute::SuppressCaptured => None,
            };
            if let (Some(clipboard), Some(text)) = (&self.clipboard, selection_for_primary) {
                if !text.is_empty() {
                    let _ = clipboard.copy_primary(&text);
                }
            }
        }

        // channel 中还有未处理的数据时，立即请求下一帧继续处理
        if has_more_data || background_has_more {
            ctx.request_repaint();
        } else {
            // 二次检查：render_ui 期间 PTY 线程可能又发送了新数据
            let has_pending_data = if !has_new_output {
                let session = self.session_manager.get_active_session_mut();
                !session.shell.events().is_empty()
            } else {
                false
            };
            let has_new_output = has_new_output || background_had_output || has_pending_data;

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
        self.check_config_hot_reload(ctx);

        // Handle shell exit: close current session
        if shell_exited {
            crate::debug_log!(
                "[SHELL EXIT] handling shell exit, session_count: {}",
                session_count_before
            );
            if session_count_before > 1 {
                // Close the current session if there are multiple sessions.
                // If it was its tab's only pane, the tab goes with it.
                self.close_session_or_owning_tab(active_session_idx);
                self.schedule_session_save();
                crate::debug_log!(
                    "[SHELL EXIT] closed session, remaining: {}",
                    self.session_manager.len()
                );
            } else if shell_startup_failed {
                // The last shell never reached a prompt. Closing here would
                // make the window vanish before the user can read why, so keep
                // it open with the failure message on screen instead.
                crate::debug_log!("[SHELL EXIT] startup failure, keeping window open");
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
        // 保存 agent 会话（取消/清空会话时会删除快照文件）
        self.agent_panel.persist();

        // 保存配置
        if self.config_save_pending {
            if let Err(e) = self.config.save() {
                eprintln!("[Config] Failed to save on exit: {}", e);
            }
        }

        // 保存当前会话到持久化存储（包含每个 session 的 cwd）。只有持有实例锁
        // 的主实例能更新共享快照，避免后开的临时窗口在退出时覆盖完整状态。
        if self._lock_file.is_some() && !self.session_persistence_blocked {
            if let Ok(session_history_path) = self.config.resolved_session_history_path() {
                let _ = session_persistence::ensure_session_history_dir(&session_history_path);

                let snapshot = self.current_sessions_snapshot();
                if let Err(e) = snapshot.save(&session_history_path) {
                    eprintln!("[SessionPersistence] Failed to save sessions: {}", e);
                }
            }
        }

        // A completed command's rendered output is written off the UI thread.
        // Give already accepted snapshots a bounded chance to reach the shared
        // jsh journal before process teardown terminates that worker.
        if !execution_journal::flush(Duration::from_secs(2)) {
            log::warn!("timed out flushing jsh execution output journal");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bounded_wheel_step_accumulate, captured_release_button, clipboard_5522_response_for_mime,
        clipboard_5522_response_for_mime_with_limit, desktop_notification_channel,
        encode_terminal_paste, flush_pending_mouse_controls, kitty_graphics_payload,
        mouse_sequence_allows_lossy, mouse_sequence_is_complete, normalize_paste_text,
        osc52_clipboard_response_with_limit, osc52_read_rate_limit_allows, primary_copy_route,
        queue_mouse_control, reported_capture_button, roll_notification_rate_window,
        should_confirm_paste, should_notify_long_command, show_desktop_notification,
        take_tagged_cursor_move, wait_for_child_with_timeout, wrap_bracketed_paste,
        ClipboardRequestGuard, DesktopNotification, PrimaryCopyRoute,
        DESKTOP_NOTIFICATION_QUEUE_CAPACITY, KITTY_BASE64_CHUNK_BYTES, MAX_OSC52_READS_PER_WINDOW,
        OSC52_READ_RATE_WINDOW, OSC_5522_DATA_CHUNK_BYTES,
    };
    use crate::app::events::{
        normalize_terminal_shortcut_events, restore_missing_image_paste_key_event,
        semantic_paste_modifiers, shortcut_event_to_key_event, PasteKeyState,
    };
    use base64::Engine as _;
    use eframe::egui;
    use image::ImageEncoder as _;

    #[cfg(unix)]
    #[test]
    fn background_helper_is_reaped_after_exit_and_timeout() {
        let mut quick = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        assert!(
            wait_for_child_with_timeout(&mut quick, std::time::Duration::from_secs(1))
                .unwrap()
                .success()
        );
        assert!(quick.try_wait().unwrap().is_some());

        let mut slow = std::process::Command::new("sh")
            .args(["-c", "exec sleep 5"])
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        let _ =
            wait_for_child_with_timeout(&mut slow, std::time::Duration::from_millis(20)).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(slow.try_wait().unwrap().is_some());
    }

    #[test]
    fn long_command_notification_gates_mirror_jterm1() {
        let config = crate::config::Config {
            notify_long_blocks: true,
            notify_long_block_threshold_ms: 10_000,
            ..crate::config::Config::default()
        };

        // Long enough, unwatched, real command: notify.
        assert!(should_notify_long_command(
            &config,
            Some("cargo build"),
            Some(10_000),
            false,
        ));
        // Below the threshold, or with no measured duration: stay silent.
        assert!(!should_notify_long_command(
            &config,
            Some("cargo build"),
            Some(9_999),
            false,
        ));
        assert!(!should_notify_long_command(
            &config,
            Some("cargo build"),
            None,
            false,
        ));
        // jterm1's background blocks carry an empty command line and never
        // notify; the same holds for a missing or whitespace-only command.
        assert!(!should_notify_long_command(
            &config,
            None,
            Some(60_000),
            false
        ));
        assert!(!should_notify_long_command(
            &config,
            Some("   "),
            Some(60_000),
            false,
        ));
        // A completion the user watched on a focused visible pane is silent.
        assert!(!should_notify_long_command(
            &config,
            Some("cargo build"),
            Some(60_000),
            true,
        ));

        let disabled = crate::config::Config {
            notify_long_blocks: false,
            ..config
        };
        assert!(!should_notify_long_command(
            &disabled,
            Some("cargo build"),
            Some(60_000),
            false,
        ));
    }

    #[test]
    fn notification_rate_window_resets_only_after_it_elapses() {
        let mut window_started = std::time::Instant::now();
        let mut in_window = super::MAX_NOTIFICATIONS_PER_WINDOW;

        // Inside the window the exhausted counter stays exhausted.
        roll_notification_rate_window(&mut window_started, &mut in_window);
        assert_eq!(in_window, super::MAX_NOTIFICATIONS_PER_WINDOW);

        // Once the window has elapsed the counter resets.
        window_started = std::time::Instant::now() - super::NOTIFICATION_RATE_WINDOW;
        roll_notification_rate_window(&mut window_started, &mut in_window);
        assert_eq!(in_window, 0);
        assert!(window_started.elapsed() < super::NOTIFICATION_RATE_WINDOW);
    }

    #[test]
    fn desktop_notification_queue_is_bounded_without_spending_failed_rate_slots() {
        let (production_tx, _production_rx) = desktop_notification_channel();
        for index in 0..DESKTOP_NOTIFICATION_QUEUE_CAPACITY {
            production_tx
                .try_send(DesktopNotification {
                    title: index.to_string(),
                    body: String::new(),
                })
                .unwrap();
        }
        assert!(matches!(
            production_tx.try_send(DesktopNotification {
                title: "overflow".to_string(),
                body: String::new(),
            }),
            Err(crossbeam_channel::TrySendError::Full(_))
        ));

        let (tx, rx) = crossbeam_channel::bounded(1);
        let mut window_started = std::time::Instant::now();
        let mut sent = 0;

        show_desktop_notification(
            Some(&tx),
            &mut window_started,
            &mut sent,
            "title".to_string(),
            "body".to_string(),
        );
        assert_eq!(sent, 1);
        let notification = rx.recv().unwrap();
        assert_eq!(notification.title, "title");
        assert_eq!(notification.body, "body");

        tx.try_send(DesktopNotification {
            title: "queue filler".to_string(),
            body: String::new(),
        })
        .unwrap();
        show_desktop_notification(
            Some(&tx),
            &mut window_started,
            &mut sent,
            "dropped".to_string(),
            "full queue".to_string(),
        );
        assert_eq!(sent, 1);

        drop(rx);
        show_desktop_notification(
            Some(&tx),
            &mut window_started,
            &mut sent,
            "dropped".to_string(),
            "closed queue".to_string(),
        );
        assert_eq!(sent, 1);
    }

    #[test]
    fn risky_paste_detection_covers_newlines_and_large_single_lines() {
        assert!(!should_confirm_paste("printf safe"));
        assert!(should_confirm_paste("first\nsecond"));
        assert!(should_confirm_paste(
            &"x".repeat(crate::app::state::PASTE_CONFIRM_THRESHOLD_BYTES + 1)
        ));
    }

    #[test]
    fn paste_normalization_cannot_hide_enter_as_a_carriage_return() {
        assert_eq!(
            normalize_paste_text("first\rsecond\r\nthird"),
            "first\nsecond\nthird"
        );
        assert!(should_confirm_paste(&normalize_paste_text(
            "printf risky\r"
        )));
    }

    #[test]
    fn bracketed_paste_cannot_embed_an_early_terminator() {
        let wrapped = wrap_bracketed_paste(b"safe\x1b[201~injected".to_vec());
        assert_eq!(wrapped, b"\x1b[200~safeinjected\x1b[201~");
        assert_eq!(
            wrapped
                .windows(b"\x1b[201~".len())
                .filter(|window| *window == b"\x1b[201~")
                .count(),
            1
        );
    }

    #[test]
    fn submitted_ui_command_places_enter_after_bracketed_paste() {
        assert_eq!(
            encode_terminal_paste("cd '/tmp'\n", true, true),
            b"\x1b[200~cd '/tmp'\x1b[201~\r"
        );
        assert_eq!(
            encode_terminal_paste("cd '/tmp'\n", false, true),
            b"cd '/tmp'\r"
        );
    }

    #[test]
    fn synthetic_wheel_deltas_are_saturating_and_bounded() {
        assert_eq!(
            bounded_wheel_step_accumulate(0, f32::INFINITY, usize::MAX),
            64
        );
        assert_eq!(
            bounded_wheel_step_accumulate(0, f32::NEG_INFINITY, usize::MAX),
            -64
        );
        assert_eq!(bounded_wheel_step_accumulate(63, 1000.0, 512), 64);
        assert_eq!(bounded_wheel_step_accumulate(-63, -1000.0, 512), -64);
        assert_eq!(bounded_wheel_step_accumulate(7, f32::NAN, 512), 7);
    }
    fn encoded_test_png(width: u32, height: u32) -> Vec<u8> {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        let mut value = 0x1234_5678_u32;
        for _ in 0..width as usize * height as usize {
            value = value.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            pixels.extend_from_slice(&value.to_le_bytes());
        }

        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&pixels, width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        png
    }

    fn kitty_packet_bodies(packet: &[u8]) -> Vec<&str> {
        std::str::from_utf8(packet)
            .unwrap()
            .split("\x1b\\")
            .filter(|body| !body.is_empty())
            .collect()
    }

    #[test]
    fn clipboard_request_guard_releases_the_single_flight_slot() {
        let in_flight = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        drop(ClipboardRequestGuard(std::sync::Arc::clone(&in_flight)));
        assert!(!in_flight.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn osc52_response_is_bounded_before_base64_allocation() {
        let normal = osc52_clipboard_response_with_limit("hello", 64);
        assert_eq!(normal, b"\x1b]52;c;aGVsbG8=\x1b\\");

        let capped = osc52_clipboard_response_with_limit(&"x".repeat(128), 16);
        assert_eq!(capped, b"\x1b]52;c;\x1b\\");
        assert!(capped.len() <= 16);
        assert!(osc52_clipboard_response_with_limit("x", 4).is_empty());
    }

    #[test]
    fn osc52_read_rate_limit_resets_after_its_window() {
        let base = std::time::Instant::now();
        let mut window = base;
        let mut count = 0;
        for _ in 0..MAX_OSC52_READS_PER_WINDOW {
            assert!(osc52_read_rate_limit_allows(base, &mut window, &mut count));
        }
        assert!(!osc52_read_rate_limit_allows(base, &mut window, &mut count));
        assert!(osc52_read_rate_limit_allows(
            base + OSC52_READ_RATE_WINDOW,
            &mut window,
            &mut count,
        ));
        assert_eq!(count, 1);
    }

    #[test]
    fn mouse_drag_reports_require_the_press_time_capture() {
        assert_eq!(reported_capture_button(None), None);
        assert_eq!(reported_capture_button(Some((false, 0))), None);
        assert_eq!(reported_capture_button(Some((true, 2))), Some(2));

        assert_eq!(captured_release_button(None, &[0], false), None);
        assert_eq!(captured_release_button(Some((false, 0)), &[0], false), None);
        assert_eq!(captured_release_button(Some((true, 0)), &[2], true), None);
        assert_eq!(
            captured_release_button(Some((true, 0)), &[0], false),
            Some(0)
        );
        // Some backends only report the button-up state after the pointer
        // leaves the window. The captured last cell still receives release.
        assert_eq!(
            captured_release_button(Some((true, 2)), &[], false),
            Some(2)
        );
    }

    #[test]
    fn mouse_control_edges_remain_ordered_and_gate_lossy_reports() {
        use crate::app::state::PendingMouseControlKind::{Press, Release};

        let mut controls = std::collections::VecDeque::new();
        queue_mouse_control(&mut controls, Press, b"press".to_vec());
        queue_mouse_control(&mut controls, Release, b"release".to_vec());
        queue_mouse_control(&mut controls, Press, b"duplicate".to_vec());
        assert_eq!(controls.len(), 2);
        assert_eq!(controls[0].kind, Press);
        assert_eq!(controls[1].kind, Release);

        assert!(!mouse_sequence_allows_lossy(true, false, false, false));
        assert!(mouse_sequence_allows_lossy(true, true, false, true));
        assert!(!mouse_sequence_allows_lossy(true, true, true, true));
        assert!(!mouse_sequence_is_complete(true, true, true, false));
        assert!(mouse_sequence_is_complete(true, true, true, true));
        assert!(mouse_sequence_is_complete(false, false, true, true));
    }

    #[test]
    fn backpressured_mouse_edges_stay_at_the_front_until_both_are_accepted() {
        use crate::app::state::PendingMouseControlKind::{Press, Release};

        let mut controls = std::collections::VecDeque::new();
        queue_mouse_control(&mut controls, Press, b"press".to_vec());
        queue_mouse_control(&mut controls, Release, b"release".to_vec());
        let mut press_accepted = false;

        let blocked: Result<(), ()> =
            flush_pending_mouse_controls(&mut controls, &mut press_accepted, |_| Err(()));
        assert_eq!(blocked, Err(()));
        assert!(!press_accepted);
        assert_eq!(controls.len(), 2);

        let mut admitted = Vec::new();
        flush_pending_mouse_controls(&mut controls, &mut press_accepted, |bytes| {
            admitted.extend_from_slice(bytes);
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(admitted, b"pressrelease");
        assert!(press_accepted);
        assert!(controls.is_empty());
    }

    #[test]
    fn prior_render_cursor_bytes_keep_their_route_and_precede_current_input() {
        let mut target = Some(17);
        let mut cursor = b"\x1b[C".to_vec();
        let (routed_to, mut ordered) =
            take_tagged_cursor_move(&mut target, &mut cursor).expect("tagged cursor input");
        assert_eq!(routed_to, 17);
        ordered.extend_from_slice("中".as_bytes());
        ordered.push(0x03);
        assert_eq!(
            ordered,
            [b"\x1b[C".as_slice(), "中".as_bytes(), &[0x03]].concat()
        );
        assert!(target.is_none());
        assert!(cursor.is_empty());

        let mut stale_target = None;
        let mut stale = b"\x1b[D".to_vec();
        assert!(take_tagged_cursor_move(&mut stale_target, &mut stale).is_none());
        assert!(stale.is_empty());
    }

    #[test]
    fn captured_primary_release_never_falls_back_to_the_replacement_terminal() {
        assert_eq!(
            primary_copy_route(Some((false, false, 0)), true, true),
            PrimaryCopyRoute::CapturedLocal
        );
        assert_eq!(
            primary_copy_route(Some((true, false, 0)), true, true),
            PrimaryCopyRoute::SuppressCaptured
        );
        assert_eq!(
            primary_copy_route(Some((false, true, 0)), true, true),
            PrimaryCopyRoute::SuppressCaptured
        );
        assert_eq!(
            primary_copy_route(None, false, true),
            PrimaryCopyRoute::Generic
        );
        assert_eq!(
            primary_copy_route(Some((true, false, 2)), true, false),
            PrimaryCopyRoute::None
        );
    }

    #[test]
    fn osc_5522_response_chunks_data_at_4096_bytes() {
        let data = vec![0x5a; OSC_5522_DATA_CHUNK_BYTES + 1];
        let response = clipboard_5522_response_for_mime("application/octet-stream", &data);
        let response = String::from_utf8(response).unwrap();
        let packets: Vec<&str> = response
            .split("\x1b\\")
            .filter(|packet| packet.contains("status=DATA"))
            .collect();
        assert_eq!(packets.len(), 2);

        let decoded: Vec<Vec<u8>> = packets
            .iter()
            .map(|packet| {
                let payload = packet.rsplit_once(';').unwrap().1;
                base64::engine::general_purpose::STANDARD
                    .decode(payload)
                    .unwrap()
            })
            .collect();
        assert_eq!(decoded[0].len(), OSC_5522_DATA_CHUNK_BYTES);
        assert_eq!(decoded[1].len(), 1);
        assert_eq!(decoded.concat(), data);
        assert_eq!(response.matches("status=OK").count(), 1);
        assert_eq!(response.matches("status=DONE").count(), 1);
    }

    #[test]
    fn osc_5522_response_rejects_data_over_policy_limit() {
        let response = clipboard_5522_response_for_mime_with_limit("text/plain", b"12345", 4);
        assert_eq!(
            String::from_utf8(response).unwrap(),
            "\x1b]5522;type=read:status=EPERM\x1b\\"
        );
    }

    #[test]
    fn kitty_png_payload_uses_standard_bounded_chunks_and_put() {
        let png = encoded_test_png(64, 64);
        let packet = kitty_graphics_payload("image/png", &png).unwrap();
        let bodies = kitty_packet_bodies(&packet);
        assert!(
            bodies.len() >= 3,
            "expected multiple transfer chunks plus put"
        );

        let transfer_bodies = &bodies[..bodies.len() - 1];
        let mut encoded = String::new();
        let mut image_id = None;
        for (index, body) in transfer_bodies.iter().enumerate() {
            let body = body.strip_prefix("\x1b_G").unwrap();
            let (control, payload) = body.split_once(';').unwrap();
            assert!(payload.len() <= KITTY_BASE64_CHUNK_BYTES);
            let expected_more = u8::from(index + 1 < transfer_bodies.len());
            let expected_more_control = format!("m={expected_more}");
            assert!(control.split(',').any(|part| part == expected_more_control));

            if index == 0 {
                assert!(control.split(',').any(|part| part == "a=t"));
                assert!(control.split(',').any(|part| part == "f=100"));
                image_id = control
                    .split(',')
                    .find_map(|part| part.strip_prefix("i="))
                    .map(str::to_owned);
            } else {
                assert_eq!(control, expected_more_control);
            }
            encoded.push_str(payload);
        }

        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&encoded)
                .unwrap(),
            png
        );
        let expected_put = format!("\x1b_Ga=p,i={}", image_id.unwrap());
        assert_eq!(bodies.last().copied(), Some(expected_put.as_str()));
    }

    #[test]
    fn kitty_png_payload_marks_a_single_transfer_final_and_keeps_padding() {
        let mut png = encoded_test_png(1, 1);
        while png.len().is_multiple_of(3) {
            png.push(0);
        }
        let packet = kitty_graphics_payload("image/png", &png).unwrap();
        let bodies = kitty_packet_bodies(&packet);
        assert_eq!(bodies.len(), 2);
        let (_, payload) = bodies[0].split_once(';').unwrap();
        assert!(bodies[0].contains("a=t,f=100"));
        assert!(bodies[0].contains("m=0"));
        assert!(payload.ends_with('='));
        assert!(bodies[1].contains("a=p"));
    }

    #[test]
    fn kitty_image_paste_rejects_non_png_and_invalid_png() {
        assert!(kitty_graphics_payload("image/jpeg", b"\xff\xd8\xff\xe0").is_none());
        assert!(kitty_graphics_payload("image/png", b"not a png").is_none());
    }

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

    #[test]
    fn ctrl_shift_v_remains_explicit_text_paste_with_osc_5522_enabled() {
        let modifiers = egui::Modifiers {
            ctrl: true,
            shift: true,
            command: true,
            ..Default::default()
        };
        let mut events = vec![egui::Event::Paste("clipboard text".to_owned())];

        normalize_terminal_shortcut_events(&mut events, modifiers, true, true);

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
    fn released_ctrl_shift_v_recovers_shift_for_text_paste_routing() {
        let event_modifiers = egui::Modifiers {
            ctrl: true,
            shift: true,
            command: true,
            ..Default::default()
        };
        let mut events = vec![
            egui::Event::Paste("clipboard text".to_owned()),
            egui::Event::Key {
                key: egui::Key::V,
                physical_key: Some(egui::Key::V),
                pressed: false,
                repeat: false,
                modifiers: event_modifiers,
            },
        ];

        let recovered = semantic_paste_modifiers(&events, egui::Modifiers::NONE);
        normalize_terminal_shortcut_events(&mut events, recovered, true, true);

        assert!(events.iter().any(|event| {
            matches!(event,
                egui::Event::Key {
                    key: egui::Key::V,
                    pressed: true,
                    modifiers,
                    ..
                } if modifiers.ctrl && modifiers.shift
            )
        }));
        assert!(!events
            .iter()
            .any(|event| matches!(event, egui::Event::Paste(_))));
    }

    #[test]
    fn released_plain_ctrl_v_stays_an_osc_5522_paste_event() {
        let event_modifiers = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let mut events = vec![
            egui::Event::Paste("clipboard text".to_owned()),
            egui::Event::Key {
                key: egui::Key::V,
                physical_key: Some(egui::Key::V),
                pressed: false,
                repeat: false,
                modifiers: event_modifiers,
            },
        ];

        let recovered = semantic_paste_modifiers(&events, egui::Modifiers::NONE);
        normalize_terminal_shortcut_events(&mut events, recovered, true, true);

        assert!(events
            .iter()
            .any(|event| matches!(event, egui::Event::Paste(_))));
        assert!(!recovered.shift);
    }

    #[test]
    fn image_only_clipboard_restores_ctrl_v_after_ctrl_is_released() {
        let mut state = PasteKeyState::default();
        let expected_modifiers = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let mut events = vec![egui::Event::Key {
            key: egui::Key::V,
            physical_key: Some(egui::Key::V),
            pressed: false,
            repeat: false,
            // This is what arrives when Ctrl is released before V.
            modifiers: egui::Modifiers::NONE,
        }];

        assert!(restore_missing_image_paste_key_event(
            &mut events,
            &mut state
        ));
        assert_eq!(
            events[0],
            egui::Event::Key {
                key: egui::Key::V,
                physical_key: Some(egui::Key::V),
                pressed: true,
                repeat: false,
                modifiers: expected_modifiers,
            }
        );
    }

    #[test]
    fn image_paste_restoration_does_not_duplicate_existing_paste_input() {
        let mut state = PasteKeyState::default();
        let modifiers = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let mut events = vec![
            egui::Event::Key {
                key: egui::Key::V,
                physical_key: Some(egui::Key::V),
                pressed: true,
                repeat: false,
                modifiers,
            },
            egui::Event::Key {
                key: egui::Key::V,
                physical_key: Some(egui::Key::V),
                pressed: false,
                repeat: false,
                modifiers,
            },
        ];

        assert!(!restore_missing_image_paste_key_event(
            &mut events,
            &mut state
        ));
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn semantic_text_paste_suppresses_v_release_in_a_later_frame() {
        let mut state = PasteKeyState::default();
        let mut paste_frame = vec![egui::Event::Paste("clipboard text".to_owned())];
        assert!(!restore_missing_image_paste_key_event(
            &mut paste_frame,
            &mut state
        ));

        let mut release_frame = vec![egui::Event::Key {
            key: egui::Key::V,
            physical_key: Some(egui::Key::V),
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }];
        assert!(!restore_missing_image_paste_key_event(
            &mut release_frame,
            &mut state
        ));
        assert_eq!(release_frame.len(), 1);
        assert!(matches!(
            release_frame[0],
            egui::Event::Key {
                key: egui::Key::V,
                pressed: false,
                ..
            }
        ));
    }

    #[test]
    fn ordinary_v_press_suppresses_release_restoration_across_frames() {
        let mut state = PasteKeyState::default();
        let mut press_frame = vec![egui::Event::Key {
            key: egui::Key::V,
            physical_key: Some(egui::Key::V),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }];
        assert!(!restore_missing_image_paste_key_event(
            &mut press_frame,
            &mut state
        ));

        let mut release_frame = vec![egui::Event::Key {
            key: egui::Key::V,
            physical_key: Some(egui::Key::V),
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }];
        assert!(!restore_missing_image_paste_key_event(
            &mut release_frame,
            &mut state
        ));
        assert_eq!(release_frame.len(), 1);
    }
}
