// Rendering coordination module

use super::state::TerminalApp;
use std::time::Duration;

impl TerminalApp {
    /// 自适应帧预算：根据帧时间动态调整处理量
    pub fn adjust_frame_budget(&mut self) {
        const TARGET_FRAME_MS: f64 = 16.0; // 目标 60 FPS
        const MIN_BUDGET: usize = 8192;    // 最小 8KB
        const MAX_BUDGET: usize = 131072;  // 最大 128KB
        const ADJUST_RATE: f64 = 0.1;      // 调整速率 10%

        let avg_frame_ms = self.debug_panel.get_avg_frame_time_ms();

        // 只有在有足够帧时间历史时才调整
        if avg_frame_ms > 0.0 {
            let current = self.adaptive_frame_budget as f64;
            let new_budget = if avg_frame_ms < TARGET_FRAME_MS * 0.8 {
                // 帧时间充裕，可以增加预算
                current * (1.0 + ADJUST_RATE)
            } else if avg_frame_ms > TARGET_FRAME_MS * 1.2 {
                // 帧时间紧张，减少预算
                current * (1.0 - ADJUST_RATE)
            } else {
                // 帧时间在目标范围内，保持不变
                current
            };

            self.adaptive_frame_budget = (new_budget as usize).clamp(MIN_BUDGET, MAX_BUDGET);
        }
    }
}
