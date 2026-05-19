use egui::Rect;

/// 窗格 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(pub usize);

/// 分屏模式
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SplitMode {
    /// 单窗格
    Single,
    /// 垂直分割（左右）
    VerticalSplit { ratio: f32 },
    /// 水平分割（上下）
    HorizontalSplit { ratio: f32 },
}

/// 单个窗格的状态
#[derive(Debug, Clone)]
pub struct Pane {
    pub id: PaneId,
    pub session_idx: usize,
    pub rect: Rect,
    pub focused: bool,
}

impl Pane {
    pub fn new(id: PaneId, session_idx: usize) -> Self {
        Pane {
            id,
            session_idx,
            rect: Rect::ZERO,
            focused: false,
        }
    }
}

/// 布局管理器 (MVP: 支持左右分栏)
pub struct LayoutManager {
    pub mode: SplitMode,
    pub panes: Vec<Pane>,
    pub focused_pane_id: PaneId,
    pane_counter: usize,
}

impl LayoutManager {
    /// 创建单窗格布局
    pub fn new(session_idx: usize) -> Self {
        let pane = Pane::new(PaneId(0), session_idx);
        LayoutManager {
            mode: SplitMode::Single,
            panes: vec![pane],
            focused_pane_id: PaneId(0),
            pane_counter: 1,
        }
    }

    /// 分割窗格（垂直/水平）
    pub fn split(&mut self, session_idx: usize, horizontal: bool) -> Result<(), String> {
        if self.panes.len() >= 4 {
            return Err("Maximum 4 panes reached".to_string());
        }

        // 只支持最多 2 个窗格（MVP）
        if self.panes.len() >= 2 {
            return Err("MVP supports max 2 panes".to_string());
        }

        let new_pane = Pane::new(PaneId(self.pane_counter), session_idx);
        self.pane_counter += 1;

        self.panes.push(new_pane);

        self.mode = if horizontal {
            SplitMode::HorizontalSplit { ratio: 0.5 }
        } else {
            SplitMode::VerticalSplit { ratio: 0.5 }
        };

        Ok(())
    }

    /// 关闭当前焦点的窗格
    pub fn close_focused_pane(&mut self) -> Result<(), String> {
        if self.panes.len() == 1 {
            return Err("Cannot close the last pane".to_string());
        }

        self.panes.retain(|p| p.id != self.focused_pane_id);

        if self.panes.len() == 1 {
            self.mode = SplitMode::Single;
            self.focused_pane_id = self.panes[0].id;
        } else {
            self.focused_pane_id = self.panes[0].id;
        }

        Ok(())
    }

    /// 切换焦点窗格（通过方向）
    pub fn focus_pane(&mut self, direction: PaneDirection) -> bool {
        if self.panes.len() == 1 {
            return false;
        }

        match direction {
            PaneDirection::Next => {
                let current_idx = self
                    .panes
                    .iter()
                    .position(|p| p.id == self.focused_pane_id)
                    .unwrap_or(0);
                let next_idx = (current_idx + 1) % self.panes.len();
                self.focused_pane_id = self.panes[next_idx].id;
                true
            }
            PaneDirection::Prev => {
                let current_idx = self
                    .panes
                    .iter()
                    .position(|p| p.id == self.focused_pane_id)
                    .unwrap_or(0);
                let next_idx = if current_idx == 0 {
                    self.panes.len() - 1
                } else {
                    current_idx - 1
                };
                self.focused_pane_id = self.panes[next_idx].id;
                true
            }
            _ => false, // 上下左右暂不支持（MVP）
        }
    }

    /// 调整分割比例
    pub fn adjust_split_ratio(&mut self, delta: f32) {
        match &mut self.mode {
            SplitMode::VerticalSplit { ratio } => {
                *ratio = (*ratio + delta).clamp(0.1, 0.9);
            }
            SplitMode::HorizontalSplit { ratio } => {
                *ratio = (*ratio + delta).clamp(0.1, 0.9);
            }
            _ => {}
        }
    }

    /// 获取所有窗格
    pub fn panes(&self) -> &[Pane] {
        &self.panes
    }

    /// 获取可变窗格列表
    pub fn panes_mut(&mut self) -> &mut [Pane] {
        &mut self.panes
    }

    /// 获取焦点窗格
    pub fn focused_pane(&self) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == self.focused_pane_id)
    }

    /// 获取焦点窗格会话索引
    pub fn focused_session_idx(&self) -> usize {
        self.focused_pane().map(|p| p.session_idx).unwrap_or(0)
    }

    /// 计算窗格矩形（基于容器矩形和分割比例）
    pub fn compute_pane_rects(&mut self, container: Rect) {
        match self.mode {
            SplitMode::Single => {
                if let Some(pane) = self.panes.get_mut(0) {
                    pane.rect = container;
                }
            }
            SplitMode::VerticalSplit { ratio } => {
                let width = container.width();
                let left_width = width * ratio;
                let right_width = width * (1.0 - ratio);

                if let Some(pane) = self.panes.get_mut(0) {
                    pane.rect = Rect::from_min_size(
                        container.min,
                        egui::vec2(left_width, container.height()),
                    );
                }

                if let Some(pane) = self.panes.get_mut(1) {
                    pane.rect = Rect::from_min_size(
                        egui::pos2(container.min.x + left_width, container.min.y),
                        egui::vec2(right_width, container.height()),
                    );
                }
            }
            SplitMode::HorizontalSplit { ratio } => {
                let height = container.height();
                let top_height = height * ratio;
                let bottom_height = height * (1.0 - ratio);

                if let Some(pane) = self.panes.get_mut(0) {
                    pane.rect = Rect::from_min_size(
                        container.min,
                        egui::vec2(container.width(), top_height),
                    );
                }

                if let Some(pane) = self.panes.get_mut(1) {
                    pane.rect = Rect::from_min_size(
                        egui::pos2(container.min.x, container.min.y + top_height),
                        egui::vec2(container.width(), bottom_height),
                    );
                }
            }
        }

        // 更新焦点状态
        for pane in &mut self.panes {
            pane.focused = pane.id == self.focused_pane_id;
        }
    }

    /// 获取分割线矩形（如果有的话）
    pub fn get_divider_rect(&self) -> Option<Rect> {
        match self.mode {
            SplitMode::VerticalSplit { ratio: _ } => {
                if let Some(pane0) = self.panes.first() {
                    let divider_x = pane0.rect.right();
                    Some(Rect::from_min_max(
                        egui::pos2(divider_x - 2.0, pane0.rect.top()),
                        egui::pos2(divider_x + 2.0, pane0.rect.bottom()),
                    ))
                } else {
                    None
                }
            }
            SplitMode::HorizontalSplit { ratio: _ } => {
                if let Some(pane0) = self.panes.first() {
                    let divider_y = pane0.rect.bottom();
                    Some(Rect::from_min_max(
                        egui::pos2(pane0.rect.left(), divider_y - 2.0),
                        egui::pos2(pane0.rect.right(), divider_y + 2.0),
                    ))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// 判断坐标是否在分割线上
    pub fn is_on_divider(&self, pos: egui::Pos2) -> bool {
        if let Some(divider_rect) = self.get_divider_rect() {
            divider_rect.contains(pos)
        } else {
            false
        }
    }

    /// 从坐标获取窗格 ID
    pub fn pane_at_pos(&self, pos: egui::Pos2) -> Option<PaneId> {
        self.panes
            .iter()
            .find(|p| p.rect.contains(pos))
            .map(|p| p.id)
    }
}

/// 窗格方向
#[derive(Debug, Clone, Copy)]
pub enum PaneDirection {
    Next,
    Prev,
    Up,
    Down,
    Left,
    Right,
}
