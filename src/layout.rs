//! Split-pane layout for ember, backed by `jterm_core::pane_layout`.
//!
//! The tmux-style n-ary [`PaneTree`] (leaves are session indices) replaces the
//! old app-local binary tree: splitting along a node's existing axis joins the
//! new pane as a sibling instead of nesting, and divider drags snap near even.
//! This module keeps ember's public `LayoutManager` API and the persisted
//! binary `LayoutSnapshot` format: snapshots are converted to and from the
//! n-ary tree on save/restore, so existing session files keep working.

use egui::Rect;
use std::collections::{HashMap, HashSet};

use jterm_core::pane_layout::{
    collect_pane_rects, directional_focus_target, set_divider_share, split_node_rect,
    PaneRect as CorePaneRect, PaneTree, Rect as CoreRect,
};
pub use jterm_core::pane_layout::{Axis as SplitAxis, DividerId, PaneDirection as CoreDirection};

/// 窗格 ID。现在等同于该窗格显示的 session 索引（布局树保证每个会话
/// 至多出现在一个窗格里），保留类型只为兼容既有调用方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(pub usize);

/// Directional edge of a target pane selected by a tab-to-content drop.
/// Left/right create a side-by-side split; top/bottom create a stacked split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneDropDirection {
    Left,
    Right,
    Top,
    Bottom,
}

impl PaneDropDirection {
    fn axis(self) -> SplitAxis {
        match self {
            Self::Left | Self::Right => SplitAxis::Vertical,
            Self::Top | Self::Bottom => SplitAxis::Horizontal,
        }
    }

    fn inserts_before(self) -> bool {
        matches!(self, Self::Left | Self::Top)
    }

    pub fn horizontal(self) -> bool {
        self.axis() == SplitAxis::Horizontal
    }
}

/// Resolve the directional drop zone at `pos`. The outer quarter of each pane
/// is actionable; the center is deliberately inert so a cancelled or imprecise
/// drag cannot restructure the layout. Corners belong to left/right, producing
/// stable, non-overlapping hit regions.
pub fn pane_drop_zone(rect: Rect, pos: egui::Pos2) -> Option<PaneDropDirection> {
    if !rect.contains(pos)
        || !rect.width().is_finite()
        || !rect.height().is_finite()
        || rect.width() <= 0.0
        || rect.height() <= 0.0
    {
        return None;
    }
    let x_band = rect.width() * 0.25;
    let y_band = rect.height() * 0.25;
    if pos.x <= rect.left() + x_band {
        Some(PaneDropDirection::Left)
    } else if pos.x >= rect.right() - x_band {
        Some(PaneDropDirection::Right)
    } else if pos.y <= rect.top() + y_band {
        Some(PaneDropDirection::Top)
    } else if pos.y >= rect.bottom() - y_band {
        Some(PaneDropDirection::Bottom)
    } else {
        None
    }
}

/// Rectangle painted for one directional drop zone. This is kept beside the
/// hit-test so visuals and behavior cannot drift apart.
pub fn pane_drop_zone_rect(rect: Rect, direction: PaneDropDirection) -> Option<Rect> {
    if !rect.width().is_finite()
        || !rect.height().is_finite()
        || rect.width() <= 0.0
        || rect.height() <= 0.0
    {
        return None;
    }
    let x_band = rect.width() * 0.25;
    let y_band = rect.height() * 0.25;
    Some(match direction {
        PaneDropDirection::Left => Rect::from_min_max(
            rect.left_top(),
            egui::pos2(rect.left() + x_band, rect.bottom()),
        ),
        PaneDropDirection::Right => Rect::from_min_max(
            egui::pos2(rect.right() - x_band, rect.top()),
            rect.right_bottom(),
        ),
        PaneDropDirection::Top => Rect::from_min_max(
            egui::pos2(rect.left() + x_band, rect.top()),
            egui::pos2(rect.right() - x_band, rect.top() + y_band),
        ),
        PaneDropDirection::Bottom => Rect::from_min_max(
            egui::pos2(rect.left() + x_band, rect.bottom() - y_band),
            egui::pos2(rect.right() - x_band, rect.bottom()),
        ),
    })
}

/// 可交互分隔线及它所属的分割节点区域。`container_rect` 是分割节点自己
/// 的矩形，拖动时把指针位置换算成该节点内的比例。
#[derive(Debug, Clone, PartialEq)]
pub struct SplitDivider {
    pub id: DividerId,
    pub axis: SplitAxis,
    pub rect: Rect,
    pub container_rect: Rect,
}

/// 单个窗格的状态（按需从布局树重建的缓存视图）。
#[derive(Debug, Clone)]
pub struct Pane {
    pub id: PaneId,
    pub session_idx: usize,
    pub rect: Rect,
    pub focused: bool,
}

/// 递归窗格布局。每次分屏只拆分当前焦点窗格，因此可以自由组合左右和
/// 上下布局，而不是把整个窗口限制成固定的两个区域。
pub struct LayoutManager {
    /// DFS 顺序的窗格缓存，随树结构/焦点/几何变化重建。
    pub panes: Vec<Pane>,
    pub focused_pane_id: PaneId,
    tree: PaneTree,
    last_container_rect: Rect,
    /// Transient focus mode. The recursive split tree stays intact while the
    /// renderer sees only the focused pane; this state is not persisted.
    zoomed: bool,
}

fn core_rect(rect: Rect) -> CoreRect {
    CoreRect {
        x: rect.left(),
        y: rect.top(),
        width: rect.width(),
        height: rect.height(),
    }
}

fn egui_rect(rect: CoreRect) -> Rect {
    Rect::from_min_size(
        egui::pos2(rect.x, rect.y),
        egui::vec2(rect.width, rect.height),
    )
}

impl LayoutManager {
    const MIN_SPLIT_RATIO: f32 = 0.1;
    const MAX_SPLIT_RATIO: f32 = 0.9;
    const MAX_RESTORED_LAYOUT_DEPTH: usize = 64;
    /// 分隔线视觉上保持轻量，但命中区域需要足够宽，避免高 DPI 下难以抓取。
    const DIVIDER_HIT_HALF_WIDTH: f32 = 5.0;

    /// 创建单窗格布局
    pub fn new(session_idx: usize) -> Self {
        let mut layout = LayoutManager {
            panes: Vec::new(),
            focused_pane_id: PaneId(session_idx),
            tree: PaneTree::Leaf(session_idx),
            last_container_rect: Rect::ZERO,
            zoomed: false,
        };
        layout.rebuild_panes();
        layout
    }

    /// 将运行期布局转换成稳定 session ID 快照（沿用二叉持久化格式，
    /// n 叉节点在保存时折叠成嵌套二叉分割，比例保持一致）。
    pub fn to_snapshot(
        &self,
        session_ids: &[String],
    ) -> Option<crate::session_persistence::LayoutSnapshot> {
        let root = Self::snapshot_node(&self.tree, session_ids)?;
        let focused_session_id = self
            .focused_session_idx()
            .and_then(|idx| session_ids.get(idx))
            .cloned();
        // Tab 级别的标记由 TabManager 拥有，布局本身不知道它们；调用方在
        // 写快照时补上。
        Some(crate::session_persistence::LayoutSnapshot {
            root,
            focused_session_id,
            pinned: false,
            marked: false,
            private_title: false,
        })
    }

    fn snapshot_node(
        node: &PaneTree,
        session_ids: &[String],
    ) -> Option<crate::session_persistence::LayoutNodeSnapshot> {
        match node {
            PaneTree::Leaf(session_idx) => {
                Some(crate::session_persistence::LayoutNodeSnapshot::Pane {
                    session_id: session_ids.get(*session_idx)?.clone(),
                })
            }
            PaneTree::Split {
                axis,
                children,
                ratios,
            } => Self::snapshot_split(*axis, children, ratios, session_ids),
        }
    }

    /// 把一个 n 叉分割右折叠成嵌套二叉快照：`[a, b, c]` 变成
    /// `Split(a, A, Split(b/(b+c), B, C))`，恢复时可无损展平回来。
    fn snapshot_split(
        axis: SplitAxis,
        children: &[PaneTree],
        ratios: &[f32],
        session_ids: &[String],
    ) -> Option<crate::session_persistence::LayoutNodeSnapshot> {
        match children {
            [] => None,
            [only] => Self::snapshot_node(only, session_ids),
            [first, rest @ ..] => {
                let total: f32 = ratios.iter().sum();
                let share = ratios.first().copied().unwrap_or(0.5);
                let ratio = if total > f32::EPSILON {
                    share / total
                } else {
                    0.5
                };
                Some(crate::session_persistence::LayoutNodeSnapshot::Split {
                    horizontal: axis == SplitAxis::Horizontal,
                    ratio,
                    first: Box::new(Self::snapshot_node(first, session_ids)?),
                    second: Box::new(Self::snapshot_split(
                        axis,
                        rest,
                        ratios.get(1..).unwrap_or(&[]),
                        session_ids,
                    )?),
                })
            }
        }
    }

    /// 从稳定 session ID 快照恢复一个 tab 的布局。缺失/重复 session 的叶子
    /// 会被移除，父 split 自动折叠；整棵树都没有可用会话时返回 `None`。
    ///
    /// 失败时刻意不回退到「显示某个兜底会话的单窗格」：那个会话很可能已经
    /// 属于另一个 tab，两个 tab 抢同一个 PTY 比丢掉一个空 tab 糟糕得多。空
    /// tab 里的会话会在 [`crate::tab_manager::TabManager::restore`] 里作为
    /// 孤儿被各自收养。
    pub fn try_from_snapshot(
        snapshot: &crate::session_persistence::LayoutSnapshot,
        session_ids: &[String],
        fallback_session_idx: Option<usize>,
    ) -> Option<Self> {
        let session_indices: HashMap<&str, usize> = session_ids
            .iter()
            .enumerate()
            .map(|(idx, id)| (id.as_str(), idx))
            .collect();
        let mut used_sessions = HashSet::new();
        let tree = Self::restore_node(&snapshot.root, &session_indices, &mut used_sessions, 0)?;

        let focused = snapshot
            .focused_session_id
            .as_deref()
            .and_then(|focused_id| session_indices.get(focused_id).copied())
            .filter(|idx| tree.contains_session(*idx))
            .or_else(|| fallback_session_idx.filter(|idx| tree.contains_session(*idx)))
            .or_else(|| tree.leaves().first().copied())
            .expect("restored layout root has no pane");

        let mut layout = LayoutManager {
            panes: Vec::new(),
            focused_pane_id: PaneId(focused),
            tree,
            last_container_rect: Rect::ZERO,
            zoomed: false,
        };
        layout.rebuild_panes();
        Some(layout)
    }

    fn restore_node(
        snapshot: &crate::session_persistence::LayoutNodeSnapshot,
        session_indices: &HashMap<&str, usize>,
        used_sessions: &mut HashSet<usize>,
        depth: usize,
    ) -> Option<PaneTree> {
        if depth > Self::MAX_RESTORED_LAYOUT_DEPTH {
            return None;
        }
        match snapshot {
            crate::session_persistence::LayoutNodeSnapshot::Pane { session_id } => {
                let session_idx = *session_indices.get(session_id.as_str())?;
                used_sessions
                    .insert(session_idx)
                    .then_some(PaneTree::Leaf(session_idx))
            }
            crate::session_persistence::LayoutNodeSnapshot::Split {
                horizontal,
                ratio,
                first,
                second,
            } => {
                let first = Self::restore_node(first, session_indices, used_sessions, depth + 1);
                let second = Self::restore_node(second, session_indices, used_sessions, depth + 1);
                match (first, second) {
                    (Some(first), Some(second)) => {
                        let axis = if *horizontal {
                            SplitAxis::Horizontal
                        } else {
                            SplitAxis::Vertical
                        };
                        let ratio = if ratio.is_finite() {
                            ratio.clamp(Self::MIN_SPLIT_RATIO, Self::MAX_SPLIT_RATIO)
                        } else {
                            0.5
                        };
                        Some(Self::join(axis, first, ratio, second, 1.0 - ratio))
                    }
                    (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
                    (None, None) => None,
                }
            }
        }
    }

    /// 组合两棵子树为一个 `axis` 分割；同轴子分割直接展平成兄弟节点
    /// （子比例按父级份额缩放），把保存时的嵌套二叉还原为 n 叉。
    fn join(
        axis: SplitAxis,
        first: PaneTree,
        first_share: f32,
        second: PaneTree,
        second_share: f32,
    ) -> PaneTree {
        let mut children = Vec::new();
        let mut ratios = Vec::new();
        for (node, share) in [(first, first_share), (second, second_share)] {
            match node {
                PaneTree::Split {
                    axis: child_axis,
                    children: grand,
                    ratios: grand_ratios,
                } if child_axis == axis => {
                    for (g, r) in grand.into_iter().zip(grand_ratios) {
                        children.push(g);
                        ratios.push(r * share);
                    }
                }
                other => {
                    children.push(other);
                    ratios.push(share);
                }
            }
        }
        PaneTree::Split {
            axis,
            children,
            ratios,
        }
    }

    /// 拆分当前焦点窗格并在新窗格中显示 `session_idx`。新窗格获得焦点。
    /// tmux 语义：焦点窗格的父分割已经是同一轴向时，新窗格作为兄弟平级
    /// 加入，而不是再嵌套一层。
    pub fn split(&mut self, session_idx: usize, horizontal: bool) -> Result<(), String> {
        let direction = if horizontal {
            PaneDropDirection::Bottom
        } else {
            PaneDropDirection::Right
        };
        self.split_session_at(self.focused_pane_id.0, session_idx, direction)
    }

    /// Insert an existing session beside a specific target leaf. This is the
    /// topology-only half of tab-to-pane drag/drop: session/PTY ownership stays
    /// in `SessionManager`, while this tree adopts exactly one leaf and focuses
    /// it. Invalid/self/duplicate requests leave the layout unchanged.
    pub fn split_session_at(
        &mut self,
        target_session_idx: usize,
        new_session_idx: usize,
        direction: PaneDropDirection,
    ) -> Result<(), String> {
        if target_session_idx == new_session_idx {
            return Err("Cannot split a session onto itself".to_string());
        }
        if !self.tree.contains_session(target_session_idx) {
            return Err("Target pane is missing from the layout".to_string());
        }
        if self.tree.contains_session(new_session_idx) {
            return Err("Session is already visible in a pane".to_string());
        }
        if !self
            .tree
            .split_leaf(target_session_idx, direction.axis(), new_session_idx)
        {
            return Err("Target pane is missing from the layout".to_string());
        }
        if direction.inserts_before() {
            self.tree.remap_sessions(&|session| {
                if session == target_session_idx {
                    new_session_idx
                } else if session == new_session_idx {
                    target_session_idx
                } else {
                    session
                }
            });
        }
        self.focused_pane_id = PaneId(new_session_idx);
        self.zoomed = false;
        self.rebuild_panes();
        Ok(())
    }

    /// 不再设置固定 pane 数量上限；只要当前焦点仍属于布局即可继续分屏。
    pub fn can_split(&self) -> bool {
        self.tree.contains_session(self.focused_pane_id.0)
    }

    /// Check whether splitting the focused pane would leave both children at
    /// least `minimum_pane_size` along the requested axis. Before the first
    /// frame has supplied viewport geometry, retain the legacy permissive
    /// behaviour; the renderer itself still handles tiny restored panes.
    pub fn can_split_focused_pane(&self, horizontal: bool, minimum_pane_size: egui::Vec2) -> bool {
        self.can_split_session_pane(self.focused_pane_id.0, horizontal, minimum_pane_size)
    }

    pub fn can_split_session_pane(
        &self,
        session_idx: usize,
        horizontal: bool,
        minimum_pane_size: egui::Vec2,
    ) -> bool {
        if !self.tree.contains_session(session_idx) {
            return false;
        }
        if self.last_container_rect.width() <= 0.0 || self.last_container_rect.height() <= 0.0 {
            return true;
        }

        // Zoom only changes what is rendered; capacity is judged against the
        // pane's real (unzoomed) share of the layout.
        let unzoomed_rect = self
            .tree_pane_rects()
            .into_iter()
            .find(|pane| pane.session == session_idx)
            .map(|pane| egui_rect(pane.rect))
            .unwrap_or(self.last_container_rect);

        let (available, minimum) = if horizontal {
            (unzoomed_rect.height(), minimum_pane_size.y)
        } else {
            (unzoomed_rect.width(), minimum_pane_size.x)
        };
        available.is_finite() && minimum.is_finite() && minimum > 0.0 && available >= minimum * 2.0
    }

    /// 关闭当前焦点窗格。其父分割会自动折叠，被关窗格的份额并入兄弟。
    pub fn close_focused_pane(&mut self) -> Result<(), String> {
        let leaves = self.tree.leaves();
        if leaves.len() == 1 {
            return Err("Cannot close the last pane".to_string());
        }
        let target = self.focused_pane_id.0;
        let focused_index = leaves
            .iter()
            .position(|&session| session == target)
            .ok_or_else(|| "Focused pane is missing from the layout".to_string())?;
        let next_focus = leaves
            .get(focused_index + 1)
            .or_else(|| focused_index.checked_sub(1).and_then(|i| leaves.get(i)))
            .copied()
            .ok_or_else(|| "Cannot close the last pane".to_string())?;

        self.tree.remove_leaf(target);
        self.focused_pane_id = PaneId(next_focus);
        self.zoomed = false;
        self.rebuild_panes();
        Ok(())
    }

    /// 把焦点移到显示 `session_idx` 的窗格。与 `show_session` 不同，这里
    /// 绝不替换窗格内容：tab 内的窗格归属是固定的，点错了只该无事发生。
    pub fn focus_session(&mut self, session_idx: usize) -> bool {
        if !self.tree.contains_session(session_idx) {
            return false;
        }
        self.focused_pane_id = PaneId(session_idx);
        self.rebuild_panes();
        true
    }

    /// 本布局树里出现的所有 session 索引（DFS 顺序）。tab 用它来判断自己
    /// 拥有哪些会话——关闭 tab 时这批会话要一起关掉。
    pub fn session_indices(&self) -> Vec<usize> {
        self.tree.leaves()
    }

    /// 该会话是否显示在本布局的某个窗格里。
    pub fn contains_session(&self, session_idx: usize) -> bool {
        self.tree.contains_session(session_idx)
    }

    pub fn pane_count(&self) -> usize {
        self.tree.leaf_count()
    }

    /// 摘掉显示 `session_idx` 的窗格，其份额并入兄弟。返回 false 表示它不
    /// 在本树中，或它是最后一个窗格——后者由调用方连整个 tab 一起关掉，
    /// 布局树本身没有「空」这个状态。
    pub fn remove_session_leaf(&mut self, session_idx: usize) -> bool {
        if !self.tree.contains_session(session_idx) || self.tree.leaf_count() <= 1 {
            return false;
        }
        let leaves = self.tree.leaves();
        let next_focus = leaves
            .iter()
            .position(|&session| session == session_idx)
            .and_then(|at| {
                leaves
                    .get(at + 1)
                    .or_else(|| at.checked_sub(1).and_then(|prev| leaves.get(prev)))
            })
            .copied();
        if !self.tree.remove_leaf(session_idx) {
            return false;
        }
        self.zoomed = false;
        if self.focused_pane_id.0 == session_idx {
            if let Some(next) = next_focus.or_else(|| self.tree.leaves().first().copied()) {
                self.focused_pane_id = PaneId(next);
            }
        }
        self.rebuild_panes();
        true
    }

    /// 会话从全局列表中删除后，把本树里比它大的索引整体左移一位。窗格
    /// 内容本身不变——这纯粹是索引重编号，供不拥有该会话的 tab 使用。
    pub fn shift_sessions_after_removal(&mut self, removed_idx: usize) {
        let remap = move |session: usize| {
            if session > removed_idx {
                session - 1
            } else {
                session
            }
        };
        self.tree.remap_sessions(&remap);
        self.focused_pane_id = PaneId(remap(self.focused_pane_id.0));
        self.rebuild_panes();
    }

    /// 新会话插入 tab 向量后，原会话在插入点及其后的索引整体右移。
    pub fn on_session_inserted(&mut self, inserted_idx: usize) {
        let remap = move |session: usize| {
            if session >= inserted_idx {
                session + 1
            } else {
                session
            }
        };
        self.tree.remap_sessions(&remap);
        self.focused_pane_id = PaneId(remap(self.focused_pane_id.0));
        self.rebuild_panes();
    }

    /// 切换焦点窗格（通过顺序或物理方向）。
    pub fn focus_pane(&mut self, direction: PaneDirection) -> bool {
        let leaves = self.tree.leaves();
        if leaves.len() == 1 {
            return false;
        }

        let current_idx = leaves
            .iter()
            .position(|&session| session == self.focused_pane_id.0)
            .unwrap_or(0);
        let next = match direction {
            PaneDirection::Next => Some(leaves[(current_idx + 1) % leaves.len()]),
            PaneDirection::Prev => {
                let next_idx = if current_idx == 0 {
                    leaves.len() - 1
                } else {
                    current_idx - 1
                };
                Some(leaves[next_idx])
            }
            PaneDirection::Left
            | PaneDirection::Right
            | PaneDirection::Up
            | PaneDirection::Down => {
                // Normalized geometry keeps directional navigation working
                // before the first frame supplies real pixel rectangles. The
                // reference container is much larger than the core's 1px edge
                // tolerance so proportions decide adjacency, not the epsilon.
                let unit = CoreRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1000.0,
                    height: 1000.0,
                };
                let mut rects = Vec::new();
                collect_pane_rects(&self.tree, unit, 0.0, &mut rects);
                directional_focus_target(
                    &rects,
                    self.focused_pane_id.0,
                    direction.core().expect("Next/Prev handled above"),
                )
            }
        };

        let Some(next) = next else {
            return false;
        };
        if next == self.focused_pane_id.0 {
            return false;
        }
        // A successful navigation reveals the full layout. A failed
        // directional command must not unexpectedly cancel zoom.
        self.zoomed = false;
        self.focused_pane_id = PaneId(next);
        self.rebuild_panes();
        true
    }

    /// 沿指定物理方向移动焦点窗格对应一侧的分隔线（tmux 语义：从最近的
    /// 轴向匹配祖先分割向外找有该侧分隔线的节点）。
    pub fn resize_split(&mut self, direction: PaneDirection, step: f32) -> bool {
        if self.zoomed {
            return false;
        }
        let Some(wanted) = direction.core() else {
            return false;
        };
        let forward = wanted.forward();
        let axis = wanted.axis();
        let Some(path) = self.tree.path_to_session(self.focused_pane_id.0) else {
            return false;
        };
        for k in (0..path.len()).rev() {
            let node_path = &path[..k];
            let child = path[k];
            let Some(PaneTree::Split {
                axis: node_axis,
                children,
                ratios,
            }) = self.tree.node_at_path_mut(node_path)
            else {
                continue;
            };
            if *node_axis != axis {
                continue;
            }
            let gap = if forward {
                (child + 1 < children.len()).then_some(child)
            } else {
                child.checked_sub(1)
            };
            let Some(gap) = gap else {
                continue;
            };
            let delta = if forward { step } else { -step };
            let first = ratios[gap] + delta;
            let changed = set_divider_share(ratios, gap, first, false);
            if changed {
                self.rebuild_panes();
            }
            return changed;
        }
        false
    }

    /// 设置某条分隔线两侧窗格对的比例（`ratio` 是前一个窗格在两者合计
    /// 中的占比；二叉分割下与旧语义一致）。
    pub fn set_split_ratio(&mut self, divider: &DividerId, ratio: f32) -> bool {
        let Some(PaneTree::Split { ratios, .. }) = self.tree.node_at_path_mut(&divider.path) else {
            return false;
        };
        if divider.gap + 1 >= ratios.len() {
            return false;
        }
        let pair = ratios[divider.gap] + ratios[divider.gap + 1];
        let first = ratio.clamp(Self::MIN_SPLIT_RATIO, Self::MAX_SPLIT_RATIO) * pair;
        let changed = set_divider_share(ratios, divider.gap, first, false);
        if changed {
            self.rebuild_panes();
        }
        changed
    }

    /// 把正在拖动的分隔线移到指针位置（带接近等分时的吸附）。
    pub fn drag_divider_to(&mut self, divider: &DividerId, pos: egui::Pos2) -> bool {
        let Some((axis, node_rect)) = split_node_rect(
            &self.tree,
            &divider.path,
            core_rect(self.last_container_rect),
            0.0,
        ) else {
            return false;
        };
        let local = match axis {
            SplitAxis::Vertical => (pos.x - node_rect.x) / node_rect.width.max(1.0),
            SplitAxis::Horizontal => (pos.y - node_rect.y) / node_rect.height.max(1.0),
        };
        let Some(PaneTree::Split { ratios, .. }) = self.tree.node_at_path_mut(&divider.path) else {
            return false;
        };
        if divider.gap + 1 >= ratios.len() {
            return false;
        }
        // Pointer fraction minus the children before this gap gives the
        // dragged child's new share of its pair.
        let before: f32 = ratios[..divider.gap].iter().sum();
        let first = local - before;
        let changed = set_divider_share(ratios, divider.gap, first, true);
        if changed {
            self.rebuild_panes();
        }
        changed
    }

    /// Toggle a temporary focused-pane view without modifying the split tree,
    /// shell sessions, or persisted layout. Returns false for a single pane.
    pub fn toggle_focused_pane_zoom(&mut self) -> bool {
        if self.tree.leaf_count() <= 1 {
            self.zoomed = false;
            return false;
        }
        self.zoomed = !self.zoomed;
        self.rebuild_panes();
        true
    }

    pub fn is_zoomed(&self) -> bool {
        self.zoomed
    }

    /// Reset every nested divider so each split's children share evenly.
    pub fn equalize_splits(&mut self) -> bool {
        fn equalize_node(node: &mut PaneTree) -> bool {
            match node {
                PaneTree::Leaf(_) => false,
                PaneTree::Split {
                    children, ratios, ..
                } => {
                    let even = 1.0 / children.len().max(1) as f32;
                    let mut changed = ratios.iter().any(|r| (*r - even).abs() > f32::EPSILON);
                    for r in ratios.iter_mut() {
                        *r = even;
                    }
                    for child in children.iter_mut() {
                        changed |= equalize_node(child);
                    }
                    changed
                }
            }
        }
        let changed = equalize_node(&mut self.tree);
        if changed {
            self.rebuild_panes();
        }
        changed
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
        self.tree
            .contains_session(self.focused_pane_id.0)
            .then_some(self.focused_pane_id.0)
    }

    /// 交换两个窗格显示的会话，用于拖拽标题栏重排布局。窗格的几何形状
    /// 保持不变，只是内容互换；焦点跟随被拖动的会话，这样拖完之后键盘
    /// 输入仍然落在用户刚刚移动的那个终端里。
    pub fn swap_sessions(&mut self, dragged: usize, target: usize) -> bool {
        if dragged == target
            || !self.tree.contains_session(dragged)
            || !self.tree.contains_session(target)
        {
            return false;
        }
        self.tree.remap_sessions(&|session| {
            if session == dragged {
                target
            } else if session == target {
                dragged
            } else {
                session
            }
        });
        self.focused_pane_id = PaneId(dragged);
        self.rebuild_panes();
        true
    }

    /// frost 的 `pane:swap`：焦点窗格与渲染顺序（DFS）中的下一个叶子互换
    /// 内容，末尾绕回第一个。几何形状与比例保持不变，焦点跟随被移动的会话
    /// （tmux 语义，与拖拽标题栏重排共用 [`Self::swap_sessions`]）。不足两个
    /// 窗格时返回 false。
    pub fn swap_focused_with_next(&mut self) -> bool {
        let leaves = self.tree.leaves();
        if leaves.len() < 2 {
            return false;
        }
        let position = leaves
            .iter()
            .position(|&session| session == self.focused_pane_id.0)
            .unwrap_or(0);
        let other = leaves[(position + 1) % leaves.len()];
        self.swap_sessions(self.focused_pane_id.0, other)
    }

    /// 根据坐标命中窗格但不改变焦点。拖拽过程中需要知道指针悬停在哪个
    /// 窗格上，而此时不应该把焦点提前移过去。
    pub fn session_at(&self, pos: egui::Pos2) -> Option<usize> {
        self.panes()
            .iter()
            .find(|pane| pane.rect.contains(pos))
            .map(|pane| pane.session_idx)
    }

    /// 根据坐标设置焦点窗格，命中则返回该窗格的 session 索引。
    pub fn focus_pane_at(&mut self, pos: egui::Pos2) -> Option<usize> {
        let hit = self
            .panes()
            .iter()
            .find(|pane| pane.rect.contains(pos))
            .map(|pane| pane.session_idx);
        if let Some(idx) = hit {
            self.focused_pane_id = PaneId(idx);
            self.rebuild_panes();
            Some(idx)
        } else {
            None
        }
    }

    /// 计算所有叶子 pane 的矩形。
    pub fn compute_pane_rects(&mut self, container: Rect) {
        self.last_container_rect = container;
        self.rebuild_panes();
    }

    fn tree_pane_rects(&self) -> Vec<CorePaneRect> {
        let mut rects = Vec::new();
        collect_pane_rects(
            &self.tree,
            core_rect(self.last_container_rect),
            0.0,
            &mut rects,
        );
        rects
    }

    fn rebuild_panes(&mut self) {
        self.panes = self
            .tree_pane_rects()
            .into_iter()
            .map(|pane| Pane {
                id: PaneId(pane.session),
                session_idx: pane.session,
                rect: egui_rect(pane.rect),
                focused: pane.session == self.focused_pane_id.0,
            })
            .collect();
        if self.zoomed {
            if let Some(focused) = self
                .panes
                .iter_mut()
                .find(|pane| pane.id == self.focused_pane_id)
            {
                focused.rect = self.last_container_rect;
            }
        }
    }

    /// 获取所有可交互分隔线。顺序为父节点到子节点。
    pub fn get_divider_rects(&self) -> Vec<SplitDivider> {
        if self.zoomed {
            return Vec::new();
        }
        let mut dividers = Vec::new();
        Self::collect_dividers(
            &self.tree,
            core_rect(self.last_container_rect),
            &mut Vec::new(),
            &mut dividers,
        );
        dividers
    }

    fn collect_dividers(
        node: &PaneTree,
        container: CoreRect,
        path: &mut Vec<usize>,
        out: &mut Vec<SplitDivider>,
    ) {
        let PaneTree::Split {
            axis,
            children,
            ratios,
        } = node
        else {
            return;
        };
        let n = children.len().max(1);
        let even = 1.0 / n as f32;
        let container_rect = egui_rect(container);
        let mut offset = 0.0;
        for (index, child) in children.iter().enumerate() {
            let share = ratios.get(index).copied().unwrap_or(even);
            let (child_rect, boundary) = match axis {
                SplitAxis::Vertical => {
                    let width = container.width * share;
                    let rect = CoreRect {
                        x: container.x + offset,
                        y: container.y,
                        width,
                        height: container.height,
                    };
                    offset += width;
                    (rect, container.x + offset)
                }
                SplitAxis::Horizontal => {
                    let height = container.height * share;
                    let rect = CoreRect {
                        x: container.x,
                        y: container.y + offset,
                        width: container.width,
                        height,
                    };
                    offset += height;
                    (rect, container.y + offset)
                }
            };

            if index + 1 < n {
                let rect = match axis {
                    SplitAxis::Vertical => Rect::from_min_max(
                        egui::pos2(
                            boundary - Self::DIVIDER_HIT_HALF_WIDTH,
                            container_rect.top(),
                        ),
                        egui::pos2(
                            boundary + Self::DIVIDER_HIT_HALF_WIDTH,
                            container_rect.bottom(),
                        ),
                    ),
                    SplitAxis::Horizontal => Rect::from_min_max(
                        egui::pos2(
                            container_rect.left(),
                            boundary - Self::DIVIDER_HIT_HALF_WIDTH,
                        ),
                        egui::pos2(
                            container_rect.right(),
                            boundary + Self::DIVIDER_HIT_HALF_WIDTH,
                        ),
                    ),
                };
                out.push(SplitDivider {
                    id: DividerId {
                        path: path.clone(),
                        gap: index,
                    },
                    axis: *axis,
                    rect,
                    container_rect,
                });
            }

            path.push(index);
            Self::collect_dividers(child, child_rect, path, out);
            path.pop();
        }
    }

    /// 返回命中位置的最深层分隔线；交叉点优先操作更局部的分割。
    pub fn divider_at(&self, pos: egui::Pos2) -> Option<SplitDivider> {
        self.get_divider_rects()
            .into_iter()
            .rev()
            .find(|divider| divider.rect.contains(pos))
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

impl PaneDirection {
    /// 物理方向对应的 core 方向；Next/Prev 没有物理方位。
    fn core(self) -> Option<CoreDirection> {
        match self {
            PaneDirection::Left => Some(CoreDirection::Left),
            PaneDirection::Right => Some(CoreDirection::Right),
            PaneDirection::Up => Some(CoreDirection::Up),
            PaneDirection::Down => Some(CoreDirection::Down),
            PaneDirection::Next | PaneDirection::Prev => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_rect() -> Rect {
        Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0))
    }

    #[test]
    fn focusing_a_session_never_moves_it_between_panes() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();

        assert!(layout.focus_session(0));
        assert_eq!(layout.focused_session_idx(), Some(0));
        assert_eq!(layout.panes[1].session_idx, 1);

        // A session this tab does not own is not pulled into a pane; that is
        // the tab boundary the old `show_session` used to punch through.
        assert!(!layout.focus_session(2));
        assert_eq!(layout.focused_session_idx(), Some(0));
        assert_eq!(layout.session_indices(), vec![0, 1]);
    }

    #[test]
    fn swapping_sessions_exchanges_contents_and_keeps_geometry() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();
        layout.split(2, true).unwrap();
        layout.compute_pane_rects(test_rect());
        let dividers = layout.get_divider_rects();
        assert!(layout.set_split_ratio(&dividers[0].id, 0.65));
        layout.compute_pane_rects(test_rect());
        let rects_before: Vec<Rect> = layout.panes().iter().map(|pane| pane.rect).collect();
        let sessions_before: Vec<usize> =
            layout.panes().iter().map(|pane| pane.session_idx).collect();

        assert!(layout.swap_sessions(0, 2));
        layout.compute_pane_rects(test_rect());
        let rects_after: Vec<Rect> = layout.panes().iter().map(|pane| pane.rect).collect();
        let sessions_after: Vec<usize> =
            layout.panes().iter().map(|pane| pane.session_idx).collect();

        assert_eq!(
            rects_before, rects_after,
            "a swap must not disturb the split geometry"
        );
        let expected: Vec<usize> = sessions_before
            .iter()
            .map(|&session| match session {
                0 => 2,
                2 => 0,
                other => other,
            })
            .collect();
        assert_eq!(sessions_after, expected);
        assert_eq!(
            layout.focused_session_idx(),
            Some(0),
            "focus follows the dragged session into its new pane"
        );

        // Degenerate and out-of-layout requests are refused rather than
        // silently remapping a session that is not on screen.
        assert!(!layout.swap_sessions(0, 0));
        assert!(!layout.swap_sessions(0, 9));
    }

    #[test]
    fn swap_focused_with_next_rotates_through_leaves_and_keeps_geometry() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();
        layout.split(2, true).unwrap();
        layout.compute_pane_rects(test_rect());
        let rects_before: Vec<Rect> = layout.panes().iter().map(|pane| pane.rect).collect();

        // Split 后焦点在新窗格 2;渲染顺序 [0,1,2] 中的下一个绕回是 0。
        assert_eq!(layout.focused_session_idx(), Some(2));
        assert!(layout.swap_focused_with_next());
        let sessions: Vec<usize> = layout.panes().iter().map(|pane| pane.session_idx).collect();
        assert_eq!(sessions, vec![2, 1, 0]);
        assert_eq!(
            layout.focused_session_idx(),
            Some(2),
            "focus follows the moved session into its new pane"
        );
        layout.compute_pane_rects(test_rect());
        let rects_after: Vec<Rect> = layout.panes().iter().map(|pane| pane.rect).collect();
        assert_eq!(
            rects_before, rects_after,
            "a swap must not disturb the split geometry"
        );

        // 焦点会话现在是第一个叶子,下一个是渲染顺序中的 1。
        assert!(layout.swap_focused_with_next());
        let sessions: Vec<usize> = layout.panes().iter().map(|pane| pane.session_idx).collect();
        assert_eq!(sessions, vec![1, 2, 0]);

        // 单窗格布局没有可交换的对象。
        let single = &mut LayoutManager::new(0);
        assert!(!single.swap_focused_with_next());
    }

    #[test]
    fn pane_drop_zones_are_directional_and_leave_the_center_inert() {
        let rect = Rect::from_min_max(egui::pos2(100.0, 50.0), egui::pos2(500.0, 350.0));
        assert_eq!(
            pane_drop_zone(rect, egui::pos2(110.0, 200.0)),
            Some(PaneDropDirection::Left)
        );
        assert_eq!(
            pane_drop_zone(rect, egui::pos2(490.0, 200.0)),
            Some(PaneDropDirection::Right)
        );
        assert_eq!(
            pane_drop_zone(rect, egui::pos2(300.0, 60.0)),
            Some(PaneDropDirection::Top)
        );
        assert_eq!(
            pane_drop_zone(rect, egui::pos2(300.0, 340.0)),
            Some(PaneDropDirection::Bottom)
        );
        assert_eq!(pane_drop_zone(rect, rect.center()), None);
        assert_eq!(pane_drop_zone(rect, egui::pos2(0.0, 0.0)), None);

        for direction in [
            PaneDropDirection::Left,
            PaneDropDirection::Right,
            PaneDropDirection::Top,
            PaneDropDirection::Bottom,
        ] {
            let zone = pane_drop_zone_rect(rect, direction).expect("valid pane zone");
            assert_eq!(pane_drop_zone(rect, zone.center()), Some(direction));
        }
    }

    #[test]
    fn directional_split_places_and_focuses_the_moved_session() {
        let mut left = LayoutManager::new(10);
        left.split_session_at(10, 20, PaneDropDirection::Left)
            .unwrap();
        assert_eq!(left.session_indices(), vec![20, 10]);
        assert_eq!(left.focused_session_idx(), Some(20));

        let mut top = LayoutManager::new(10);
        top.split_session_at(10, 20, PaneDropDirection::Top)
            .unwrap();
        assert_eq!(top.session_indices(), vec![20, 10]);
        assert_eq!(top.focused_session_idx(), Some(20));

        let before = top.session_indices();
        assert!(top
            .split_session_at(10, 10, PaneDropDirection::Right)
            .is_err());
        assert_eq!(top.session_indices(), before);
    }

    #[test]
    fn nested_layout_round_trip_preserves_geometry_and_focus_by_session_id() {
        let session_ids = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();
        layout.split(2, true).unwrap();
        layout.compute_pane_rects(test_rect());
        let dividers = layout.get_divider_rects();
        assert!(layout.set_split_ratio(&dividers[0].id, 0.65));
        assert!(layout.set_split_ratio(&dividers[1].id, 0.3));
        layout.compute_pane_rects(test_rect());
        let before: Vec<(usize, Rect)> = layout
            .panes()
            .iter()
            .map(|pane| (pane.session_idx, pane.rect))
            .collect();

        let snapshot = layout.to_snapshot(&session_ids).unwrap();
        let encoded = serde_json::to_string(&snapshot).unwrap();
        let decoded = serde_json::from_str(&encoded).unwrap();
        let mut restored =
            LayoutManager::try_from_snapshot(&decoded, &session_ids, Some(0)).unwrap();
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
    fn same_axis_splits_join_as_siblings_and_round_trip_through_binary_snapshots() {
        let session_ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();
        layout.split(2, false).unwrap();
        layout.compute_pane_rects(test_rect());

        // tmux join: three siblings in one vertical split → two dividers in
        // the same root container, shares 0.5 / 0.25 / 0.25.
        let dividers = layout.get_divider_rects();
        assert_eq!(dividers.len(), 2);
        assert_eq!(dividers[0].container_rect, test_rect());
        assert_eq!(dividers[1].container_rect, test_rect());
        let widths: Vec<f32> = layout.panes().iter().map(|p| p.rect.width()).collect();
        assert_eq!(widths, vec![500.0, 250.0, 250.0]);

        // The binary persisted format restores the exact same flat geometry.
        let snapshot = layout.to_snapshot(&session_ids).unwrap();
        let mut restored =
            LayoutManager::try_from_snapshot(&snapshot, &session_ids, Some(0)).unwrap();
        restored.compute_pane_rects(test_rect());
        let widths: Vec<f32> = restored.panes().iter().map(|p| p.rect.width()).collect();
        assert_eq!(widths, vec![500.0, 250.0, 250.0]);
        assert_eq!(restored.get_divider_rects().len(), 2);
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
            pinned: false,
            marked: false,
            private_title: false,
        };
        let session_ids = vec!["alpha".to_string(), "beta".to_string()];
        let restored = LayoutManager::try_from_snapshot(&snapshot, &session_ids, Some(1)).unwrap();

        assert_eq!(restored.panes().len(), 1);
        assert_eq!(restored.focused_session_idx(), Some(0));
        assert!(restored.get_divider_rects().is_empty());
    }

    #[test]
    fn entirely_unrestorable_layout_yields_no_tab() {
        use crate::session_persistence::{LayoutNodeSnapshot, LayoutSnapshot};

        let snapshot = LayoutSnapshot {
            root: LayoutNodeSnapshot::Pane {
                session_id: "gone".to_string(),
            },
            focused_session_id: Some("gone".to_string()),
            pinned: false,
            marked: false,
            private_title: false,
        };
        let session_ids = vec!["alpha".to_string(), "beta".to_string()];

        // No pane survives, so there is no tab to build. Substituting the
        // active session here would hand a live PTY to two tabs at once.
        assert!(LayoutManager::try_from_snapshot(&snapshot, &session_ids, Some(1)).is_none());
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
        layout.shift_sessions_after_removal(1);
        assert_eq!(layout.panes.len(), 1);
        assert_eq!(layout.focused_session_idx(), Some(2));
    }

    #[test]
    fn removing_a_sessions_leaf_collapses_its_split() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();

        assert!(layout.remove_session_leaf(1));

        assert_eq!(layout.panes.len(), 1);
        assert_eq!(layout.focused_session_idx(), Some(0));
    }

    #[test]
    fn the_last_pane_refuses_to_leave_the_layout() {
        let mut layout = LayoutManager::new(0);
        // An empty tree is not renderable; emptying a tab is the tab's job.
        assert!(!layout.remove_session_leaf(0));
        assert_eq!(layout.session_indices(), vec![0]);
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
        layout.compute_pane_rects(test_rect());

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
        let root_before = layout.get_divider_rects()[0].clone();
        let nested_before = layout.get_divider_rects()[1].clone();

        assert!(layout.resize_split(PaneDirection::Up, 0.05));
        layout.compute_pane_rects(test_rect());
        let root_after = layout.get_divider_rects()[0].clone();
        let nested_after = layout.get_divider_rects()[1].clone();
        assert_eq!(root_before.rect, root_after.rect);
        assert!(nested_after.rect.center().y < nested_before.rect.center().y);
    }

    #[test]
    fn divider_ratio_can_be_set_by_stable_id() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();
        layout.compute_pane_rects(test_rect());
        let divider = layout.get_divider_rects()[0].clone();

        assert!(layout.set_split_ratio(&divider.id, 0.7));
        layout.compute_pane_rects(test_rect());
        assert_eq!(layout.get_divider_rects()[0].rect.center().x, 700.0);
    }

    #[test]
    fn dragging_a_divider_snaps_near_the_even_point() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();
        layout.compute_pane_rects(test_rect());
        let divider = layout.get_divider_rects()[0].clone();

        // From an uneven split, 510/1000 = 0.51 lands within the snap epsilon
        // and settles exactly at the even point.
        assert!(layout.set_split_ratio(&divider.id, 0.6));
        assert!(layout.drag_divider_to(&divider.id, egui::pos2(510.0, 400.0)));
        layout.compute_pane_rects(test_rect());
        assert_eq!(layout.get_divider_rects()[0].rect.center().x, 500.0);

        assert!(layout.drag_divider_to(&divider.id, egui::pos2(700.0, 400.0)));
        layout.compute_pane_rects(test_rect());
        assert!((layout.get_divider_rects()[0].rect.center().x - 700.0).abs() < 0.01);
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
        assert!(layout.set_split_ratio(&dividers[0].id, 0.7));
        assert!(layout.set_split_ratio(&dividers[1].id, 0.3));

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
        let divider = layout.get_divider_rects()[0].clone();
        assert!(layout.set_split_ratio(&divider.id, 0.9));
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

    #[test]
    fn splitting_an_already_visible_session_is_rejected() {
        let mut layout = LayoutManager::new(0);
        layout.split(1, false).unwrap();
        assert!(layout.split(0, true).is_err());
        assert_eq!(layout.panes().len(), 2);
    }
}
