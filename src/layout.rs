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
    const MAX_PANES: usize = 2;

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
        if !self.can_split() {
            return Err(format!("Maximum {} panes reached", Self::MAX_PANES));
        }

        let new_id = PaneId(self.pane_counter);
        let new_pane = Pane::new(new_id, session_idx);
        self.pane_counter += 1;

        self.panes.push(new_pane);
        // 新分出的窗格获得焦点(符合大多数终端的行为)
        self.focused_pane_id = new_id;

        self.mode = if horizontal {
            SplitMode::HorizontalSplit { ratio: 0.5 }
        } else {
            SplitMode::VerticalSplit { ratio: 0.5 }
        };

        Ok(())
    }

    /// 当前布局是否还能继续分屏。调用方应在创建新 shell 前检查，避免分屏
    /// 已满时产生一个用户没有请求的孤立会话。
    pub fn can_split(&self) -> bool {
        self.panes.len() < Self::MAX_PANES
    }

    /// 让某个会话出现在当前布局中：若它已经在某个窗格中则只移动焦点，
    /// 否则用它替换当前焦点窗格。用于 tab 切换时保持“活跃会话 = 可见焦点窗格”。
    pub fn show_session(&mut self, session_idx: usize) {
        if let Some(pane) = self
            .panes
            .iter()
            .find(|pane| pane.session_idx == session_idx)
        {
            self.focused_pane_id = pane.id;
            return;
        }

        let focused_index = self
            .panes
            .iter()
            .position(|pane| pane.id == self.focused_pane_id)
            .unwrap_or(0);
        if let Some(pane) = self.panes.get_mut(focused_index) {
            pane.session_idx = session_idx;
            self.focused_pane_id = pane.id;
        }
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

    /// 某个会话被关闭后,修正所有窗格保存的 session_idx。
    /// 会话向量在 `removed_idx` 处删除一项后,其后所有会话索引整体 -1;
    /// 指向被删会话的窗格回退到 `fallback_idx`(关闭后的新活跃会话),
    /// 避免悬空/越界索引导致渲染错位。
    pub fn on_session_removed(&mut self, removed_idx: usize, fallback_idx: usize) {
        let removed_focused_pane = self
            .panes
            .iter()
            .any(|pane| pane.id == self.focused_pane_id && pane.session_idx == removed_idx);

        // 分屏时关闭一个正在显示的会话，应同时收起对应窗格。把它改指向
        // fallback 会造成两个窗格显示同一会话，并留下无法理解的空分屏。
        if self.panes.len() > 1
            && self
                .panes
                .iter()
                .any(|pane| pane.session_idx == removed_idx)
        {
            self.panes.retain(|pane| pane.session_idx != removed_idx);
        }

        for pane in &mut self.panes {
            if pane.session_idx == removed_idx {
                pane.session_idx = fallback_idx;
            } else if pane.session_idx > removed_idx {
                pane.session_idx -= 1;
            }
        }

        if self.panes.len() == 1 {
            self.mode = SplitMode::Single;
        }
        if removed_focused_pane
            || !self
                .panes
                .iter()
                .any(|pane| pane.id == self.focused_pane_id)
        {
            if let Some(pane) = self.panes.first() {
                self.focused_pane_id = pane.id;
            }
        }
    }

    /// 会话 tab 重排后同步窗格中保存的索引，使窗格继续显示同一个会话。
    pub fn on_session_reordered(&mut self, from_idx: usize, to_idx: usize) {
        if from_idx == to_idx {
            return;
        }
        for pane in &mut self.panes {
            pane.session_idx = if pane.session_idx == from_idx {
                to_idx
            } else if from_idx < to_idx && pane.session_idx > from_idx && pane.session_idx <= to_idx
            {
                pane.session_idx - 1
            } else if to_idx < from_idx && pane.session_idx >= to_idx && pane.session_idx < from_idx
            {
                pane.session_idx + 1
            } else {
                pane.session_idx
            };
        }
    }

    /// 新会话插入 tab 向量后，原会话在插入点及其后的索引整体右移。
    pub fn on_session_inserted(&mut self, inserted_idx: usize) {
        for pane in &mut self.panes {
            if pane.session_idx >= inserted_idx {
                pane.session_idx += 1;
            }
        }
    }

    /// 切换焦点窗格（通过方向）
    pub fn focus_pane(&mut self, direction: PaneDirection) -> bool {
        if self.panes.len() == 1 {
            return false;
        }

        let current_idx = self
            .panes
            .iter()
            .position(|p| p.id == self.focused_pane_id)
            .unwrap_or(0);
        match direction {
            PaneDirection::Next => {
                let next_idx = (current_idx + 1) % self.panes.len();
                self.focused_pane_id = self.panes[next_idx].id;
                true
            }
            PaneDirection::Prev => {
                let next_idx = if current_idx == 0 {
                    self.panes.len() - 1
                } else {
                    current_idx - 1
                };
                self.focused_pane_id = self.panes[next_idx].id;
                true
            }
            PaneDirection::Left => self.focus_physical_pane(current_idx, 1, 0, false),
            PaneDirection::Right => self.focus_physical_pane(current_idx, 0, 1, false),
            PaneDirection::Up => self.focus_physical_pane(current_idx, 1, 0, true),
            PaneDirection::Down => self.focus_physical_pane(current_idx, 0, 1, true),
        }
    }

    /// 按布局中的物理方向聚焦相邻窗格。只有两窗格时顺序是稳定的：
    /// `panes[0]` 位于左/上，`panes[1]` 位于右/下。边缘按键不回绕。
    fn focus_physical_pane(
        &mut self,
        current_idx: usize,
        from_idx: usize,
        to_idx: usize,
        horizontal_split: bool,
    ) -> bool {
        let matching_axis = matches!(
            (self.mode, horizontal_split),
            (SplitMode::VerticalSplit { .. }, false) | (SplitMode::HorizontalSplit { .. }, true)
        );
        if !matching_axis || current_idx != from_idx {
            return false;
        }
        let Some(target) = self.panes.get(to_idx) else {
            return false;
        };
        self.focused_pane_id = target.id;
        true
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

    /// 沿指定物理方向移动分隔线。左右仅作用于左右分屏，上下仅作用于
    /// 上下分屏。返回 `true` 表示分割比例实际发生了变化。
    pub fn resize_split(&mut self, direction: PaneDirection, step: f32) -> bool {
        let delta = match (self.mode, direction) {
            (SplitMode::VerticalSplit { .. }, PaneDirection::Left) => -step,
            (SplitMode::VerticalSplit { .. }, PaneDirection::Right) => step,
            (SplitMode::HorizontalSplit { .. }, PaneDirection::Up) => -step,
            (SplitMode::HorizontalSplit { .. }, PaneDirection::Down) => step,
            _ => return false,
        };
        let before = self.split_ratio();
        self.adjust_split_ratio(delta);
        self.split_ratio()
            .zip(before)
            .is_some_and(|(after, before)| (after - before).abs() > f32::EPSILON)
    }

    fn split_ratio(&self) -> Option<f32> {
        match self.mode {
            SplitMode::VerticalSplit { ratio } | SplitMode::HorizontalSplit { ratio } => {
                Some(ratio)
            }
            SplitMode::Single => None,
        }
    }

    /// 获取所有窗格
    pub fn panes(&self) -> &[Pane] {
        &self.panes
    }

    /// 返回当前焦点窗格对应的 session 索引
    pub fn focused_session_idx(&self) -> Option<usize> {
        self.panes
            .iter()
            .find(|p| p.id == self.focused_pane_id)
            .map(|p| p.session_idx)
    }

    /// 根据坐标设置焦点窗格,命中则返回该窗格的 session 索引。
    /// 用于点击某个窗格时把输入焦点切换过去。
    pub fn focus_pane_at(&mut self, pos: egui::Pos2) -> Option<usize> {
        let hit = self
            .panes
            .iter()
            .find(|p| p.rect.contains(pos))
            .map(|p| (p.id, p.session_idx));
        if let Some((id, idx)) = hit {
            self.focused_pane_id = id;
            Some(idx)
        } else {
            None
        }
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
}

/// 窗格方向
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneDirection {
    Next,
    Prev,
    Left,
    Right,
    Up,
    Down,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn showing_session_focuses_existing_pane_or_replaces_focused_pane() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();

        layout.show_session(0);
        assert_eq!(layout.focused_session_idx(), Some(0));
        assert_eq!(layout.panes[1].session_idx, 1);

        layout.show_session(2);
        assert_eq!(layout.focused_session_idx(), Some(2));
        assert_eq!(layout.panes[0].session_idx, 2);
        assert_eq!(layout.panes[1].session_idx, 1);
    }

    #[test]
    fn removing_visible_session_collapses_duplicate_split() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();

        layout.on_session_removed(1, 0);

        assert_eq!(layout.panes.len(), 1);
        assert_eq!(layout.mode, SplitMode::Single);
        assert_eq!(layout.focused_session_idx(), Some(0));
    }

    #[test]
    fn reordering_sessions_preserves_pane_identity() {
        let mut layout = LayoutManager::new(1);
        layout.split(3, false).unwrap();

        layout.on_session_reordered(1, 3);

        assert_eq!(layout.panes[0].session_idx, 3);
        assert_eq!(layout.panes[1].session_idx, 2);
    }

    #[test]
    fn inserting_session_keeps_existing_panes_on_their_sessions() {
        let mut layout = LayoutManager::new(0);
        layout.split(2, false).unwrap();

        layout.on_session_inserted(1);

        assert_eq!(layout.panes[0].session_idx, 0);
        assert_eq!(layout.panes[1].session_idx, 3);
    }

    #[test]
    fn split_limit_is_reported_before_creating_more_work() {
        let mut layout = LayoutManager::new(0);
        assert!(layout.can_split());
        layout.split(1, true).unwrap();
        assert!(!layout.can_split());
        assert!(layout.split(2, true).is_err());
    }

    #[test]
    fn physical_focus_follows_layout_axis_without_wrapping() {
        let mut vertical = LayoutManager::new(0);
        vertical.split(1, false).unwrap();
        assert_eq!(vertical.focused_session_idx(), Some(1));
        assert!(vertical.focus_pane(PaneDirection::Left));
        assert_eq!(vertical.focused_session_idx(), Some(0));
        assert!(!vertical.focus_pane(PaneDirection::Left));
        assert!(!vertical.focus_pane(PaneDirection::Up));
        assert!(vertical.focus_pane(PaneDirection::Right));
        assert_eq!(vertical.focused_session_idx(), Some(1));

        let mut horizontal = LayoutManager::new(0);
        horizontal.split(1, true).unwrap();
        assert!(horizontal.focus_pane(PaneDirection::Up));
        assert_eq!(horizontal.focused_session_idx(), Some(0));
        assert!(!horizontal.focus_pane(PaneDirection::Left));
        assert!(horizontal.focus_pane(PaneDirection::Down));
        assert_eq!(horizontal.focused_session_idx(), Some(1));
        assert!(!horizontal.focus_pane(PaneDirection::Down));
    }

    #[test]
    fn directional_resize_only_moves_the_matching_axis() {
        let mut vertical = LayoutManager::new(0);
        vertical.split(1, false).unwrap();
        assert!(vertical.resize_split(PaneDirection::Left, 0.05));
        assert_eq!(vertical.mode, SplitMode::VerticalSplit { ratio: 0.45 });
        assert!(!vertical.resize_split(PaneDirection::Up, 0.05));
        assert!(vertical.resize_split(PaneDirection::Right, 0.05));
        assert_eq!(vertical.mode, SplitMode::VerticalSplit { ratio: 0.5 });

        let mut horizontal = LayoutManager::new(0);
        horizontal.split(1, true).unwrap();
        assert!(horizontal.resize_split(PaneDirection::Up, 0.05));
        assert_eq!(horizontal.mode, SplitMode::HorizontalSplit { ratio: 0.45 });
        assert!(!horizontal.resize_split(PaneDirection::Left, 0.05));
        assert!(horizontal.resize_split(PaneDirection::Down, 0.05));
        assert_eq!(horizontal.mode, SplitMode::HorizontalSplit { ratio: 0.5 });
    }
}
