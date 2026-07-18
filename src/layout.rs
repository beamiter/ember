use egui::Rect;

/// 窗格 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(pub usize);

/// 分割节点 ID。拖动分隔线时用稳定 ID 锁定具体分割，避免指针移出
/// 分隔线后误操作另一个节点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SplitId(pub usize);

/// 分割轴：垂直分割产生左右窗格，水平分割产生上下窗格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitAxis {
    Vertical,
    Horizontal,
}

/// 可交互分隔线及它所属的布局区域。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SplitDivider {
    pub id: SplitId,
    pub axis: SplitAxis,
    pub rect: Rect,
    pub container_rect: Rect,
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

#[derive(Debug, Clone)]
enum LayoutNode {
    Pane(PaneId),
    Split {
        id: SplitId,
        axis: SplitAxis,
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

/// 递归窗格布局。每次分屏只拆分当前焦点窗格，因此可以自由组合左右和
/// 上下布局，而不是把整个窗口限制成固定的两个区域。
pub struct LayoutManager {
    pub panes: Vec<Pane>,
    pub focused_pane_id: PaneId,
    pane_counter: usize,
    split_counter: usize,
    root: Option<LayoutNode>,
    last_container_rect: Rect,
}

impl LayoutManager {
    const MIN_SPLIT_RATIO: f32 = 0.1;
    const MAX_SPLIT_RATIO: f32 = 0.9;
    /// 分隔线视觉上保持轻量，但命中区域需要足够宽，避免高 DPI 下难以抓取。
    const DIVIDER_HIT_HALF_WIDTH: f32 = 5.0;

    /// 创建单窗格布局
    pub fn new(session_idx: usize) -> Self {
        let pane = Pane::new(PaneId(0), session_idx);
        LayoutManager {
            panes: vec![pane],
            focused_pane_id: PaneId(0),
            pane_counter: 1,
            split_counter: 0,
            root: Some(LayoutNode::Pane(PaneId(0))),
            last_container_rect: Rect::ZERO,
        }
    }

    /// 拆分当前焦点窗格并在新窗格中显示 `session_idx`。新窗格获得焦点。
    pub fn split(&mut self, session_idx: usize, horizontal: bool) -> Result<(), String> {
        if !self.can_split() {
            return Err("No focused pane to split".to_string());
        }

        let new_id = PaneId(self.pane_counter);
        let split_id = SplitId(self.split_counter);
        let axis = if horizontal {
            SplitAxis::Horizontal
        } else {
            SplitAxis::Vertical
        };
        let Some(root) = &mut self.root else {
            return Err("No focused pane to split".to_string());
        };
        if !Self::split_node(root, self.focused_pane_id, new_id, split_id, axis) {
            return Err("Focused pane is missing from the layout".to_string());
        }

        let focused_index = self
            .panes
            .iter()
            .position(|pane| pane.id == self.focused_pane_id)
            .map(|index| index + 1)
            .unwrap_or(self.panes.len());
        self.panes
            .insert(focused_index, Pane::new(new_id, session_idx));
        self.pane_counter += 1;
        self.split_counter += 1;
        self.focused_pane_id = new_id;
        self.update_focus_flags();
        Ok(())
    }

    fn split_node(
        node: &mut LayoutNode,
        target: PaneId,
        new_pane: PaneId,
        split_id: SplitId,
        axis: SplitAxis,
    ) -> bool {
        match node {
            LayoutNode::Pane(id) if *id == target => {
                *node = LayoutNode::Split {
                    id: split_id,
                    axis,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Pane(target)),
                    second: Box::new(LayoutNode::Pane(new_pane)),
                };
                true
            }
            LayoutNode::Pane(_) => false,
            LayoutNode::Split { first, second, .. } => {
                Self::split_node(first, target, new_pane, split_id, axis)
                    || Self::split_node(second, target, new_pane, split_id, axis)
            }
        }
    }

    /// 不再设置固定 pane 数量上限；只要当前焦点仍属于布局即可继续分屏。
    pub fn can_split(&self) -> bool {
        self.panes
            .iter()
            .any(|pane| pane.id == self.focused_pane_id)
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
            self.update_focus_flags();
            return;
        }

        if let Some(pane) = self
            .panes
            .iter_mut()
            .find(|pane| pane.id == self.focused_pane_id)
        {
            pane.session_idx = session_idx;
        }
        self.update_focus_flags();
    }

    /// 关闭当前焦点窗格。其父分割会自动折叠为仍存在的兄弟节点。
    pub fn close_focused_pane(&mut self) -> Result<(), String> {
        if self.panes.len() == 1 {
            return Err("Cannot close the last pane".to_string());
        }

        let target = self.focused_pane_id;
        let focused_index = self
            .panes
            .iter()
            .position(|pane| pane.id == target)
            .ok_or_else(|| "Focused pane is missing from the layout".to_string())?;
        let next_focus = self
            .panes
            .get(focused_index + 1)
            .or_else(|| focused_index.checked_sub(1).and_then(|i| self.panes.get(i)))
            .map(|pane| pane.id)
            .ok_or_else(|| "Cannot close the last pane".to_string())?;

        self.remove_pane(target);
        self.focused_pane_id = next_focus;
        self.update_focus_flags();
        Ok(())
    }

    fn remove_pane(&mut self, target: PaneId) {
        self.panes.retain(|pane| pane.id != target);
        self.root = self
            .root
            .take()
            .and_then(|node| Self::remove_node(node, target));
    }

    fn remove_node(node: LayoutNode, target: PaneId) -> Option<LayoutNode> {
        match node {
            LayoutNode::Pane(id) => (id != target).then_some(LayoutNode::Pane(id)),
            LayoutNode::Split {
                id,
                axis,
                ratio,
                first,
                second,
            } => {
                let first = Self::remove_node(*first, target);
                let second = Self::remove_node(*second, target);
                match (first, second) {
                    (Some(first), Some(second)) => Some(LayoutNode::Split {
                        id,
                        axis,
                        ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    }),
                    (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
                    (None, None) => None,
                }
            }
        }
    }

    /// 某个会话被关闭后,修正所有窗格保存的 session_idx。
    pub fn on_session_removed(&mut self, removed_idx: usize, fallback_idx: usize) {
        let removed_panes: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|pane| pane.session_idx == removed_idx)
            .map(|pane| pane.id)
            .collect();

        // 分屏时关闭一个正在显示的 session，同时移除它的 pane。保留最后
        // 一个 pane，由下方 fallback 接管，确保布局永远非空。
        for pane_id in removed_panes {
            if self.panes.len() > 1 {
                self.remove_pane(pane_id);
            }
        }

        for pane in &mut self.panes {
            if pane.session_idx == removed_idx {
                pane.session_idx = fallback_idx;
            } else if pane.session_idx > removed_idx {
                pane.session_idx -= 1;
            }
        }

        if !self
            .panes
            .iter()
            .any(|pane| pane.id == self.focused_pane_id)
        {
            if let Some(pane) = self.panes.first() {
                self.focused_pane_id = pane.id;
            }
        }
        self.update_focus_flags();
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

    /// 切换焦点窗格（通过顺序或物理方向）。
    pub fn focus_pane(&mut self, direction: PaneDirection) -> bool {
        if self.panes.len() == 1 {
            return false;
        }

        let current_idx = self
            .panes
            .iter()
            .position(|pane| pane.id == self.focused_pane_id)
            .unwrap_or(0);
        let next_id = match direction {
            PaneDirection::Next => Some(self.panes[(current_idx + 1) % self.panes.len()].id),
            PaneDirection::Prev => {
                let next_idx = if current_idx == 0 {
                    self.panes.len() - 1
                } else {
                    current_idx - 1
                };
                Some(self.panes[next_idx].id)
            }
            PaneDirection::Left
            | PaneDirection::Right
            | PaneDirection::Up
            | PaneDirection::Down => self.physical_neighbor(direction),
        };

        let Some(next_id) = next_id else {
            return false;
        };
        if next_id == self.focused_pane_id {
            return false;
        }
        self.focused_pane_id = next_id;
        self.update_focus_flags();
        true
    }

    fn physical_neighbor(&self, direction: PaneDirection) -> Option<PaneId> {
        let root = self.root.as_ref()?;
        let unit = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let mut rects = Vec::with_capacity(self.panes.len());
        Self::collect_pane_rects(root, unit, &mut rects);
        let current = rects.iter().find(|(id, _)| *id == self.focused_pane_id)?.1;

        rects
            .into_iter()
            .filter(|(id, _)| *id != self.focused_pane_id)
            .filter_map(|(id, candidate)| {
                Self::directional_score(current, candidate, direction).map(|score| (id, score))
            })
            .min_by(|(_, left), (_, right)| {
                left.0
                    .total_cmp(&right.0)
                    .then_with(|| left.1.total_cmp(&right.1))
                    .then_with(|| left.2.total_cmp(&right.2))
            })
            .map(|(id, _)| id)
    }

    fn directional_score(
        current: Rect,
        candidate: Rect,
        direction: PaneDirection,
    ) -> Option<(f32, f32, f32)> {
        const EPSILON: f32 = 0.0001;
        let (axis_gap, orthogonal_gap, center_gap) = match direction {
            PaneDirection::Left if candidate.right() <= current.left() + EPSILON => (
                (current.left() - candidate.right()).max(0.0),
                Self::interval_gap(
                    current.top(),
                    current.bottom(),
                    candidate.top(),
                    candidate.bottom(),
                ),
                (current.center().y - candidate.center().y).abs(),
            ),
            PaneDirection::Right if candidate.left() >= current.right() - EPSILON => (
                (candidate.left() - current.right()).max(0.0),
                Self::interval_gap(
                    current.top(),
                    current.bottom(),
                    candidate.top(),
                    candidate.bottom(),
                ),
                (current.center().y - candidate.center().y).abs(),
            ),
            PaneDirection::Up if candidate.bottom() <= current.top() + EPSILON => (
                (current.top() - candidate.bottom()).max(0.0),
                Self::interval_gap(
                    current.left(),
                    current.right(),
                    candidate.left(),
                    candidate.right(),
                ),
                (current.center().x - candidate.center().x).abs(),
            ),
            PaneDirection::Down if candidate.top() >= current.bottom() - EPSILON => (
                (candidate.top() - current.bottom()).max(0.0),
                Self::interval_gap(
                    current.left(),
                    current.right(),
                    candidate.left(),
                    candidate.right(),
                ),
                (current.center().x - candidate.center().x).abs(),
            ),
            _ => return None,
        };
        Some((orthogonal_gap, axis_gap, center_gap))
    }

    fn interval_gap(a_min: f32, a_max: f32, b_min: f32, b_max: f32) -> f32 {
        if a_max < b_min {
            b_min - a_max
        } else if b_max < a_min {
            a_min - b_max
        } else {
            0.0
        }
    }

    /// 沿指定物理方向移动离当前 pane 最近、轴向匹配的分隔线。
    pub fn resize_split(&mut self, direction: PaneDirection, step: f32) -> bool {
        let axis = match direction {
            PaneDirection::Left | PaneDirection::Right => SplitAxis::Vertical,
            PaneDirection::Up | PaneDirection::Down => SplitAxis::Horizontal,
            PaneDirection::Next | PaneDirection::Prev => return false,
        };
        let delta = match direction {
            PaneDirection::Left | PaneDirection::Up => -step,
            PaneDirection::Right | PaneDirection::Down => step,
            PaneDirection::Next | PaneDirection::Prev => return false,
        };
        let Some(root) = &self.root else {
            return false;
        };
        let (_, split_id) = Self::nearest_matching_split(root, self.focused_pane_id, axis);
        let Some(split_id) = split_id else {
            return false;
        };
        Self::adjust_node_ratio(
            self.root.as_mut().expect("layout root disappeared"),
            split_id,
            delta,
        )
    }

    fn nearest_matching_split(
        node: &LayoutNode,
        target: PaneId,
        wanted_axis: SplitAxis,
    ) -> (bool, Option<SplitId>) {
        match node {
            LayoutNode::Pane(id) => (*id == target, None),
            LayoutNode::Split {
                id,
                axis,
                first,
                second,
                ..
            } => {
                let (contains, nearest) = Self::nearest_matching_split(first, target, wanted_axis);
                let (contains, nearest) = if contains {
                    (contains, nearest)
                } else {
                    Self::nearest_matching_split(second, target, wanted_axis)
                };
                if !contains {
                    return (false, None);
                }
                (true, nearest.or((*axis == wanted_axis).then_some(*id)))
            }
        }
    }

    fn adjust_node_ratio(node: &mut LayoutNode, split_id: SplitId, delta: f32) -> bool {
        match node {
            LayoutNode::Pane(_) => false,
            LayoutNode::Split {
                id,
                ratio,
                first,
                second,
                ..
            } => {
                if *id == split_id {
                    let before = *ratio;
                    *ratio = (*ratio + delta).clamp(Self::MIN_SPLIT_RATIO, Self::MAX_SPLIT_RATIO);
                    (*ratio - before).abs() > f32::EPSILON
                } else {
                    Self::adjust_node_ratio(first, split_id, delta)
                        || Self::adjust_node_ratio(second, split_id, delta)
                }
            }
        }
    }

    /// 直接设置某条分隔线的比例（拖动操作使用）。
    pub fn set_split_ratio(&mut self, split_id: SplitId, ratio: f32) -> bool {
        let Some(root) = &mut self.root else {
            return false;
        };
        Self::set_node_ratio(
            root,
            split_id,
            ratio.clamp(Self::MIN_SPLIT_RATIO, Self::MAX_SPLIT_RATIO),
        )
    }

    fn set_node_ratio(node: &mut LayoutNode, split_id: SplitId, new_ratio: f32) -> bool {
        match node {
            LayoutNode::Pane(_) => false,
            LayoutNode::Split {
                id,
                ratio,
                first,
                second,
                ..
            } => {
                if *id == split_id {
                    let changed = (*ratio - new_ratio).abs() > f32::EPSILON;
                    *ratio = new_ratio;
                    changed
                } else {
                    Self::set_node_ratio(first, split_id, new_ratio)
                        || Self::set_node_ratio(second, split_id, new_ratio)
                }
            }
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
            .find(|pane| pane.id == self.focused_pane_id)
            .map(|pane| pane.session_idx)
    }

    /// 根据坐标设置焦点窗格,命中则返回该窗格的 session 索引。
    pub fn focus_pane_at(&mut self, pos: egui::Pos2) -> Option<usize> {
        let hit = self
            .panes
            .iter()
            .find(|pane| pane.rect.contains(pos))
            .map(|pane| (pane.id, pane.session_idx));
        if let Some((id, idx)) = hit {
            self.focused_pane_id = id;
            self.update_focus_flags();
            Some(idx)
        } else {
            None
        }
    }

    /// 计算所有叶子 pane 的矩形。
    pub fn compute_pane_rects(&mut self, container: Rect) {
        self.last_container_rect = container;
        let Some(root) = &self.root else {
            return;
        };
        let mut rects = Vec::with_capacity(self.panes.len());
        Self::collect_pane_rects(root, container, &mut rects);
        for (pane_id, rect) in rects {
            if let Some(pane) = self.panes.iter_mut().find(|pane| pane.id == pane_id) {
                pane.rect = rect;
            }
        }
        self.update_focus_flags();
    }

    fn collect_pane_rects(node: &LayoutNode, container: Rect, out: &mut Vec<(PaneId, Rect)>) {
        match node {
            LayoutNode::Pane(id) => out.push((*id, container)),
            LayoutNode::Split {
                axis,
                ratio,
                first,
                second,
                ..
            } => {
                let (first_rect, second_rect) = Self::split_rect(container, *axis, *ratio);
                Self::collect_pane_rects(first, first_rect, out);
                Self::collect_pane_rects(second, second_rect, out);
            }
        }
    }

    fn split_rect(container: Rect, axis: SplitAxis, ratio: f32) -> (Rect, Rect) {
        match axis {
            SplitAxis::Vertical => {
                let split_x = container.left() + container.width() * ratio;
                (
                    Rect::from_min_max(container.min, egui::pos2(split_x, container.bottom())),
                    Rect::from_min_max(egui::pos2(split_x, container.top()), container.max),
                )
            }
            SplitAxis::Horizontal => {
                let split_y = container.top() + container.height() * ratio;
                (
                    Rect::from_min_max(container.min, egui::pos2(container.right(), split_y)),
                    Rect::from_min_max(egui::pos2(container.left(), split_y), container.max),
                )
            }
        }
    }

    /// 获取所有可交互分隔线。顺序为父节点到子节点。
    pub fn get_divider_rects(&self) -> Vec<SplitDivider> {
        let mut dividers = Vec::with_capacity(self.panes.len().saturating_sub(1));
        if let Some(root) = &self.root {
            Self::collect_dividers(root, self.last_container_rect, &mut dividers);
        }
        dividers
    }

    fn collect_dividers(node: &LayoutNode, container: Rect, out: &mut Vec<SplitDivider>) {
        let LayoutNode::Split {
            id,
            axis,
            ratio,
            first,
            second,
        } = node
        else {
            return;
        };
        let (first_rect, second_rect) = Self::split_rect(container, *axis, *ratio);
        let rect = match axis {
            SplitAxis::Vertical => Rect::from_min_max(
                egui::pos2(
                    first_rect.right() - Self::DIVIDER_HIT_HALF_WIDTH,
                    container.top(),
                ),
                egui::pos2(
                    first_rect.right() + Self::DIVIDER_HIT_HALF_WIDTH,
                    container.bottom(),
                ),
            ),
            SplitAxis::Horizontal => Rect::from_min_max(
                egui::pos2(
                    container.left(),
                    first_rect.bottom() - Self::DIVIDER_HIT_HALF_WIDTH,
                ),
                egui::pos2(
                    container.right(),
                    first_rect.bottom() + Self::DIVIDER_HIT_HALF_WIDTH,
                ),
            ),
        };
        out.push(SplitDivider {
            id: *id,
            axis: *axis,
            rect,
            container_rect: container,
        });
        Self::collect_dividers(first, first_rect, out);
        Self::collect_dividers(second, second_rect, out);
    }

    /// 返回命中位置的最深层分隔线；交叉点优先操作更局部的分割。
    pub fn divider_at(&self, pos: egui::Pos2) -> Option<SplitDivider> {
        self.get_divider_rects()
            .into_iter()
            .rev()
            .find(|divider| divider.rect.contains(pos))
    }

    fn update_focus_flags(&mut self) {
        for pane in &mut self.panes {
            pane.focused = pane.id == self.focused_pane_id;
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

    fn test_rect() -> Rect {
        Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0))
    }

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
    fn repeated_splits_target_the_focused_pane_without_a_fixed_limit() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();
        layout.split(2, true).unwrap();
        layout.split(3, false).unwrap();
        layout.compute_pane_rects(test_rect());

        assert_eq!(layout.panes.len(), 4);
        assert!(layout.can_split());
        assert_eq!(layout.focused_session_idx(), Some(3));
        assert_eq!(layout.get_divider_rects().len(), 3);

        let pane0 = layout
            .panes
            .iter()
            .find(|pane| pane.session_idx == 0)
            .unwrap();
        let pane1 = layout
            .panes
            .iter()
            .find(|pane| pane.session_idx == 1)
            .unwrap();
        let pane2 = layout
            .panes
            .iter()
            .find(|pane| pane.session_idx == 2)
            .unwrap();
        let pane3 = layout
            .panes
            .iter()
            .find(|pane| pane.session_idx == 3)
            .unwrap();
        assert_eq!(pane0.rect.width(), 500.0);
        assert_eq!(pane1.rect.height(), 400.0);
        assert_eq!(pane2.rect.width(), 250.0);
        assert_eq!(pane3.rect.width(), 250.0);
    }

    #[test]
    fn closing_nested_pane_collapses_only_its_parent_split() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();
        layout.split(2, true).unwrap();
        layout.close_focused_pane().unwrap();
        layout.compute_pane_rects(test_rect());

        assert_eq!(layout.panes.len(), 2);
        assert_eq!(layout.get_divider_rects().len(), 1);
        assert_eq!(layout.focused_session_idx(), Some(1));
        assert!(layout
            .panes
            .iter()
            .all(|pane| pane.rect.height() == test_rect().height()));
    }

    #[test]
    fn closing_pane_then_session_removal_keeps_remaining_focus_and_indices() {
        let mut layout = LayoutManager::new(1);
        layout.split(3, false).unwrap();
        assert!(layout.focus_pane(PaneDirection::Left));

        layout.close_focused_pane().unwrap();
        assert_eq!(layout.focused_session_idx(), Some(3));

        // Session 1 is removed; the remaining session shifts from 3 to 2.
        layout.on_session_removed(1, 2);
        assert_eq!(layout.panes.len(), 1);
        assert_eq!(layout.focused_session_idx(), Some(2));
    }

    #[test]
    fn removing_visible_session_collapses_its_split() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();

        layout.on_session_removed(1, 0);

        assert_eq!(layout.panes.len(), 1);
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
    fn physical_focus_follows_nested_layout_without_wrapping() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();
        layout.split(2, true).unwrap();

        assert!(layout.focus_pane(PaneDirection::Up));
        assert_eq!(layout.focused_session_idx(), Some(1));
        assert!(layout.focus_pane(PaneDirection::Left));
        assert_eq!(layout.focused_session_idx(), Some(0));
        assert!(!layout.focus_pane(PaneDirection::Left));
        assert!(layout.focus_pane(PaneDirection::Right));
        assert_eq!(layout.focused_session_idx(), Some(1));
        assert!(layout.focus_pane(PaneDirection::Down));
        assert_eq!(layout.focused_session_idx(), Some(2));
        assert!(!layout.focus_pane(PaneDirection::Down));
    }

    #[test]
    fn directional_resize_uses_the_nearest_matching_split() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();
        layout.split(2, true).unwrap();
        layout.compute_pane_rects(test_rect());
        let root_before = layout.get_divider_rects()[0];
        let nested_before = layout.get_divider_rects()[1];

        assert!(layout.resize_split(PaneDirection::Up, 0.05));
        layout.compute_pane_rects(test_rect());
        let root_after = layout.get_divider_rects()[0];
        let nested_after = layout.get_divider_rects()[1];
        assert_eq!(root_before.rect, root_after.rect);
        assert!(nested_after.rect.center().y < nested_before.rect.center().y);
    }

    #[test]
    fn divider_ratio_can_be_set_by_stable_id() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();
        layout.compute_pane_rects(test_rect());
        let divider = layout.get_divider_rects()[0];

        assert!(layout.set_split_ratio(divider.id, 0.7));
        layout.compute_pane_rects(test_rect());
        assert_eq!(layout.get_divider_rects()[0].rect.center().x, 700.0);
    }
}
