use egui::Rect;
use std::collections::{HashMap, HashSet};

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
    /// Transient focus mode. The recursive split tree stays intact while the
    /// renderer sees only the focused pane; this state is not persisted.
    zoomed: bool,
}

impl LayoutManager {
    const MIN_SPLIT_RATIO: f32 = 0.1;
    const MAX_SPLIT_RATIO: f32 = 0.9;
    const MAX_RESTORED_LAYOUT_DEPTH: usize = 64;
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
            zoomed: false,
        }
    }

    /// 将运行期 PaneId/session_idx 布局转换成稳定 session ID 快照。
    pub fn to_snapshot(
        &self,
        session_ids: &[String],
    ) -> Option<crate::session_persistence::LayoutSnapshot> {
        let root = Self::snapshot_node(self.root.as_ref()?, &self.panes, session_ids)?;
        let focused_session_id = self
            .focused_session_idx()
            .and_then(|idx| session_ids.get(idx))
            .cloned();
        Some(crate::session_persistence::LayoutSnapshot {
            root,
            focused_session_id,
        })
    }

    fn snapshot_node(
        node: &LayoutNode,
        panes: &[Pane],
        session_ids: &[String],
    ) -> Option<crate::session_persistence::LayoutNodeSnapshot> {
        match node {
            LayoutNode::Pane(pane_id) => {
                let session_idx = panes.iter().find(|pane| pane.id == *pane_id)?.session_idx;
                Some(crate::session_persistence::LayoutNodeSnapshot::Pane {
                    session_id: session_ids.get(session_idx)?.clone(),
                })
            }
            LayoutNode::Split {
                axis,
                ratio,
                first,
                second,
                ..
            } => Some(crate::session_persistence::LayoutNodeSnapshot::Split {
                horizontal: *axis == SplitAxis::Horizontal,
                ratio: *ratio,
                first: Box::new(Self::snapshot_node(first, panes, session_ids)?),
                second: Box::new(Self::snapshot_node(second, panes, session_ids)?),
            }),
        }
    }

    /// 从稳定 session ID 快照恢复布局。缺失/重复 session 的叶子会被移除，
    /// 父 split 自动折叠；整个布局无效时安全退回 `fallback_session_idx`。
    pub fn from_snapshot(
        snapshot: &crate::session_persistence::LayoutSnapshot,
        session_ids: &[String],
        fallback_session_idx: usize,
    ) -> Self {
        let session_indices: HashMap<&str, usize> = session_ids
            .iter()
            .enumerate()
            .map(|(idx, id)| (id.as_str(), idx))
            .collect();
        let mut used_sessions = HashSet::new();
        let mut panes = Vec::new();
        let mut next_pane_id = 0;
        let mut next_split_id = 0;
        let root = Self::restore_node(
            &snapshot.root,
            &session_indices,
            &mut used_sessions,
            &mut panes,
            &mut next_pane_id,
            &mut next_split_id,
            0,
        );

        let Some(root) = root else {
            return Self::new(fallback_session_idx);
        };
        let focused_pane_id = snapshot
            .focused_session_id
            .as_deref()
            .and_then(|focused_id| session_indices.get(focused_id))
            .and_then(|focused_idx| {
                panes
                    .iter()
                    .find(|pane| pane.session_idx == *focused_idx)
                    .map(|pane| pane.id)
            })
            .or_else(|| {
                panes
                    .iter()
                    .find(|pane| pane.session_idx == fallback_session_idx)
                    .map(|pane| pane.id)
            })
            .or_else(|| panes.first().map(|pane| pane.id))
            .expect("restored layout root has no pane");

        let mut layout = LayoutManager {
            panes,
            focused_pane_id,
            pane_counter: next_pane_id,
            split_counter: next_split_id,
            root: Some(root),
            last_container_rect: Rect::ZERO,
            zoomed: false,
        };
        layout.update_focus_flags();
        layout
    }

    #[allow(clippy::too_many_arguments)]
    fn restore_node(
        snapshot: &crate::session_persistence::LayoutNodeSnapshot,
        session_indices: &HashMap<&str, usize>,
        used_sessions: &mut HashSet<usize>,
        panes: &mut Vec<Pane>,
        next_pane_id: &mut usize,
        next_split_id: &mut usize,
        depth: usize,
    ) -> Option<LayoutNode> {
        if depth > Self::MAX_RESTORED_LAYOUT_DEPTH {
            return None;
        }
        match snapshot {
            crate::session_persistence::LayoutNodeSnapshot::Pane { session_id } => {
                let session_idx = *session_indices.get(session_id.as_str())?;
                if !used_sessions.insert(session_idx) {
                    return None;
                }
                let pane_id = PaneId(*next_pane_id);
                *next_pane_id += 1;
                panes.push(Pane::new(pane_id, session_idx));
                Some(LayoutNode::Pane(pane_id))
            }
            crate::session_persistence::LayoutNodeSnapshot::Split {
                horizontal,
                ratio,
                first,
                second,
            } => {
                let first = Self::restore_node(
                    first,
                    session_indices,
                    used_sessions,
                    panes,
                    next_pane_id,
                    next_split_id,
                    depth + 1,
                );
                let second = Self::restore_node(
                    second,
                    session_indices,
                    used_sessions,
                    panes,
                    next_pane_id,
                    next_split_id,
                    depth + 1,
                );
                match (first, second) {
                    (Some(first), Some(second)) => {
                        let split_id = SplitId(*next_split_id);
                        *next_split_id += 1;
                        let ratio = if ratio.is_finite() {
                            ratio.clamp(Self::MIN_SPLIT_RATIO, Self::MAX_SPLIT_RATIO)
                        } else {
                            0.5
                        };
                        Some(LayoutNode::Split {
                            id: split_id,
                            axis: if *horizontal {
                                SplitAxis::Horizontal
                            } else {
                                SplitAxis::Vertical
                            },
                            ratio,
                            first: Box::new(first),
                            second: Box::new(second),
                        })
                    }
                    (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
                    (None, None) => None,
                }
            }
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
        self.zoomed = false;
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

    /// Check whether splitting the focused pane would leave both children at
    /// least `minimum_pane_size` along the requested axis. Before the first
    /// frame has supplied viewport geometry, retain the legacy permissive
    /// behaviour; the renderer itself still handles tiny restored panes.
    pub fn can_split_focused_pane(&self, horizontal: bool, minimum_pane_size: egui::Vec2) -> bool {
        let Some(pane) = self
            .panes
            .iter()
            .find(|pane| pane.id == self.focused_pane_id)
        else {
            return false;
        };

        if self.last_container_rect.width() <= 0.0 || self.last_container_rect.height() <= 0.0 {
            return true;
        }

        let unzoomed_rect = if self.zoomed {
            let mut rects = Vec::with_capacity(self.panes.len());
            if let Some(root) = &self.root {
                Self::collect_pane_rects(root, self.last_container_rect, &mut rects);
            }
            rects
                .into_iter()
                .find(|(pane_id, _)| *pane_id == pane.id)
                .map(|(_, rect)| rect)
                .unwrap_or(pane.rect)
        } else {
            pane.rect
        };

        let (available, minimum) = if horizontal {
            (unzoomed_rect.height(), minimum_pane_size.y)
        } else {
            (unzoomed_rect.width(), minimum_pane_size.x)
        };
        available.is_finite() && minimum.is_finite() && minimum > 0.0 && available >= minimum * 2.0
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
        self.zoomed = false;
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
        self.zoomed = false;
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
        // A successful navigation reveals the full layout. A failed
        // directional command must not unexpectedly cancel zoom.
        self.zoomed = false;
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
        if self.zoomed {
            return false;
        }
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

    /// Toggle a temporary focused-pane view without modifying the split tree,
    /// shell sessions, or persisted layout. Returns false for a single pane.
    pub fn toggle_focused_pane_zoom(&mut self) -> bool {
        if self.panes.len() <= 1 {
            self.zoomed = false;
            return false;
        }
        self.zoomed = !self.zoomed;
        true
    }

    pub fn is_zoomed(&self) -> bool {
        self.zoomed
    }

    /// Reset every nested divider to an even 50/50 split.
    pub fn equalize_splits(&mut self) -> bool {
        self.root.as_mut().is_some_and(Self::equalize_node)
    }

    fn equalize_node(node: &mut LayoutNode) -> bool {
        match node {
            LayoutNode::Pane(_) => false,
            LayoutNode::Split {
                ratio,
                first,
                second,
                ..
            } => {
                let changed = (*ratio - 0.5).abs() > f32::EPSILON;
                *ratio = 0.5;
                changed | Self::equalize_node(first) | Self::equalize_node(second)
            }
        }
    }

    /// 获取当前可见窗格。聚焦缩放时只暴露焦点 pane，底层树保持不变。
    pub fn panes(&self) -> &[Pane] {
        if self.zoomed {
            self.panes
                .iter()
                .position(|pane| pane.id == self.focused_pane_id)
                .map(|index| &self.panes[index..index + 1])
                .unwrap_or(&[])
        } else {
            &self.panes
        }
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
            .panes()
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
        if self.zoomed {
            if let Some(focused) = self
                .panes
                .iter_mut()
                .find(|pane| pane.id == self.focused_pane_id)
            {
                focused.rect = container;
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
        if self.zoomed {
            return Vec::new();
        }
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
    fn nested_layout_round_trip_preserves_geometry_and_focus_by_session_id() {
        let session_ids = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();
        layout.split(2, true).unwrap();
        layout.set_split_ratio(SplitId(0), 0.65);
        layout.set_split_ratio(SplitId(1), 0.3);
        layout.compute_pane_rects(test_rect());
        let before: Vec<(usize, Rect)> = layout
            .panes()
            .iter()
            .map(|pane| (pane.session_idx, pane.rect))
            .collect();

        let snapshot = layout.to_snapshot(&session_ids).unwrap();
        let encoded = serde_json::to_string(&snapshot).unwrap();
        let decoded = serde_json::from_str(&encoded).unwrap();
        let mut restored = LayoutManager::from_snapshot(&decoded, &session_ids, 0);
        restored.compute_pane_rects(test_rect());
        let after: Vec<(usize, Rect)> = restored
            .panes()
            .iter()
            .map(|pane| (pane.session_idx, pane.rect))
            .collect();

        assert_eq!(after, before);
        assert_eq!(restored.focused_session_idx(), Some(2));
    }

    #[test]
    fn restore_prunes_missing_and_duplicate_sessions_and_collapses_splits() {
        use crate::session_persistence::{LayoutNodeSnapshot, LayoutSnapshot};

        let snapshot = LayoutSnapshot {
            root: LayoutNodeSnapshot::Split {
                horizontal: false,
                ratio: f32::NAN,
                first: Box::new(LayoutNodeSnapshot::Pane {
                    session_id: "alpha".to_string(),
                }),
                second: Box::new(LayoutNodeSnapshot::Split {
                    horizontal: true,
                    ratio: 0.75,
                    first: Box::new(LayoutNodeSnapshot::Pane {
                        session_id: "missing".to_string(),
                    }),
                    second: Box::new(LayoutNodeSnapshot::Pane {
                        session_id: "alpha".to_string(),
                    }),
                }),
            },
            focused_session_id: Some("missing".to_string()),
        };
        let session_ids = vec!["alpha".to_string(), "beta".to_string()];
        let restored = LayoutManager::from_snapshot(&snapshot, &session_ids, 1);

        assert_eq!(restored.panes().len(), 1);
        assert_eq!(restored.focused_session_idx(), Some(0));
        assert!(restored.get_divider_rects().is_empty());
    }

    #[test]
    fn entirely_unrestorable_layout_falls_back_to_active_session() {
        use crate::session_persistence::{LayoutNodeSnapshot, LayoutSnapshot};

        let snapshot = LayoutSnapshot {
            root: LayoutNodeSnapshot::Pane {
                session_id: "gone".to_string(),
            },
            focused_session_id: Some("gone".to_string()),
        };
        let session_ids = vec!["alpha".to_string(), "beta".to_string()];
        let restored = LayoutManager::from_snapshot(&snapshot, &session_ids, 1);

        assert_eq!(restored.panes().len(), 1);
        assert_eq!(restored.focused_session_idx(), Some(1));
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
    fn split_capacity_depends_on_focused_pane_size_and_axis() {
        let mut layout = LayoutManager::new(0);
        let minimum = egui::vec2(100.0, 70.0);

        // Geometry is not known until the first render, so startup commands
        // remain possible and rendering provides the hard safety net.
        assert!(layout.can_split_focused_pane(false, minimum));

        layout.compute_pane_rects(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(300.0, 120.0),
        ));
        assert!(layout.can_split_focused_pane(false, minimum));
        assert!(!layout.can_split_focused_pane(true, minimum));

        layout.split(1, false).unwrap();
        layout.compute_pane_rects(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(300.0, 120.0),
        ));
        assert!(!layout.can_split_focused_pane(false, minimum));
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

    #[test]
    fn zoom_exposes_only_the_focused_pane_without_losing_the_layout() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();
        layout.split(2, true).unwrap();
        layout.compute_pane_rects(test_rect());

        assert!(layout.toggle_focused_pane_zoom());
        layout.compute_pane_rects(test_rect());
        assert!(layout.is_zoomed());
        assert_eq!(layout.panes().len(), 1);
        assert_eq!(layout.panes()[0].session_idx, 2);
        assert_eq!(layout.panes()[0].rect, test_rect());
        assert!(layout.get_divider_rects().is_empty());

        assert!(layout.toggle_focused_pane_zoom());
        layout.compute_pane_rects(test_rect());
        assert!(!layout.is_zoomed());
        assert_eq!(layout.panes().len(), 3);
        assert_eq!(layout.get_divider_rects().len(), 2);
    }

    #[test]
    fn equalize_resets_every_nested_divider() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();
        layout.split(2, true).unwrap();
        layout.compute_pane_rects(test_rect());
        let dividers = layout.get_divider_rects();
        assert!(layout.set_split_ratio(dividers[0].id, 0.7));
        assert!(layout.set_split_ratio(dividers[1].id, 0.3));

        assert!(layout.equalize_splits());
        layout.compute_pane_rects(test_rect());
        let dividers = layout.get_divider_rects();
        assert_eq!(dividers[0].rect.center().x, test_rect().center().x);
        assert_eq!(
            dividers[1].rect.center().y,
            dividers[1].container_rect.center().y
        );
        assert!(!layout.equalize_splits());
    }

    #[test]
    fn zoom_cannot_bypass_the_underlying_pane_minimum_size() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();
        layout.compute_pane_rects(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(100.0, 100.0),
        ));
        let divider = layout.get_divider_rects()[0];
        assert!(layout.set_split_ratio(divider.id, 0.9));
        layout.compute_pane_rects(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(100.0, 100.0),
        ));
        assert!(layout.toggle_focused_pane_zoom());
        layout.compute_pane_rects(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(100.0, 100.0),
        ));

        assert!(!layout.can_split_focused_pane(false, egui::vec2(30.0, 30.0)));
    }

    #[test]
    fn failed_directional_focus_keeps_zoom_active() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();
        assert!(layout.toggle_focused_pane_zoom());

        assert!(!layout.focus_pane(PaneDirection::Right));
        assert!(layout.is_zoomed());
    }
}
