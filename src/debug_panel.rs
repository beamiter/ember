use egui::{Color32, RichText};
use std::collections::VecDeque;
use std::time::Instant;

const FRAME_HISTORY_SIZE: usize = 120;

pub struct DebugPanel {
    pub is_open: bool,
    frame_times: VecDeque<f64>,
    last_frame_instant: Instant,
    memory_cache_kb: Option<u64>,
    frames_since_memory_read: u32,
}

impl DebugPanel {
    pub fn new() -> Self {
        Self {
            is_open: false,
            frame_times: VecDeque::with_capacity(FRAME_HISTORY_SIZE),
            last_frame_instant: Instant::now(),
            memory_cache_kb: None,
            frames_since_memory_read: 0,
        }
    }

    pub fn toggle(&mut self) {
        self.is_open = !self.is_open;
    }

    pub fn record_frame(&mut self) {
        let now = Instant::now();
        let delta_ms = now.duration_since(self.last_frame_instant).as_secs_f64() * 1000.0;
        self.last_frame_instant = now;

        if self.frame_times.len() >= FRAME_HISTORY_SIZE {
            self.frame_times.pop_front();
        }
        self.frame_times.push_back(delta_ms);

        self.frames_since_memory_read += 1;
        if self.frames_since_memory_read >= 30 {
            self.memory_cache_kb = Self::read_memory_rss_kb();
            self.frames_since_memory_read = 0;
        }
    }

    fn avg_frame_time_ms(&self) -> f64 {
        if self.frame_times.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.frame_times.iter().sum();
        sum / self.frame_times.len() as f64
    }

    fn fps(&self) -> f64 {
        let avg = self.avg_frame_time_ms();
        if avg > 0.0 {
            1000.0 / avg
        } else {
            0.0
        }
    }

    fn read_memory_rss_kb() -> Option<u64> {
        let content = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in content.lines() {
            if line.starts_with("VmRSS:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    return parts[1].parse::<u64>().ok();
                }
            }
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &self,
        ctx: &egui::Context,
        grid_cols: usize,
        grid_rows: usize,
        session_count: usize,
        scrollback_used: usize,
        scrollback_max: usize,
        kitty_images_count: usize,
        kitty_memory_mb: u64,
        pending_output_bytes: usize,
        texture_cache_size: usize,
        frame_budget_kb: usize,
    ) {
        if !self.is_open {
            return;
        }

        let label_color = Color32::from_rgb(100, 200, 255);
        let value_color = Color32::from_rgb(220, 220, 220);

        egui::Window::new("debug_overlay")
            .title_bar(false)
            .resizable(false)
            .movable(false)
            .anchor(egui::Align2::RIGHT_TOP, [-8.0, 38.0])
            .frame(egui::Frame {
                fill: Color32::from_rgba_unmultiplied(20, 20, 20, 200),
                stroke: egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(80, 80, 80, 150)),
                inner_margin: egui::Margin::same(8),
                corner_radius: egui::CornerRadius::same(4),
                ..Default::default()
            })
            .show(ctx, |ui| {
                let fps = self.fps();
                let frame_ms = self.avg_frame_time_ms();

                let mem_str = match self.memory_cache_kb {
                    Some(kb) => format!("{:.1} MB", kb as f64 / 1024.0),
                    None => "N/A".to_string(),
                };

                let scale = ctx.pixels_per_point();
                let zoom = ctx.zoom_factor();

                let lines: Vec<(&str, String)> = vec![
                    ("FPS", format!("{:.1}", fps)),
                    ("Frame", format!("{:.2} ms", frame_ms)),
                    ("Budget", format!("{} KB", frame_budget_kb)),
                    ("Memory", mem_str),
                    ("Scale", format!("{:.2}x (zoom {:.2})", scale, zoom)),
                    ("Grid", format!("{}x{}", grid_cols, grid_rows)),
                    ("Sessions", format!("{}", session_count)),
                    (
                        "Scrollback",
                        format!("{} / {}", scrollback_used, scrollback_max),
                    ),
                    ("Pending", format!("{} B", pending_output_bytes)),
                    ("Tex Cache", format!("{} imgs", texture_cache_size)),
                    ("Kitty Images", format!("{}", kitty_images_count)),
                    ("Kitty Memory", format!("{} MB", kitty_memory_mb)),
                ];

                for (label, value) in &lines {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(*label)
                                .size(10.0)
                                .monospace()
                                .color(label_color),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(value)
                                    .size(10.0)
                                    .monospace()
                                    .color(value_color),
                            );
                        });
                    });
                }
            });
    }
}
