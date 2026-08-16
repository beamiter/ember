//! Tab ownership of split panes.
//!
//! Every tab owns a private [`LayoutManager`], so a split is a purely
//! tab-local event: it never adds a row to the tab bar, and the panes it
//! creates cannot outlive the tab. Previously a single global layout tree
//! held every session and the tab bar rendered one tab per session, which
//! made splitting look like it spawned tabs and made closing a tab yank one
//! pane out from under a split.
//!
//! Session indices stay global (they index [`crate::session_manager::SessionManager`]'s
//! flat vector); this type keeps every tab's tree agreeing with that vector
//! as sessions are inserted and removed.

use crate::layout::LayoutManager;

/// Per-tab flags the tab list and its context menu act on. They are pure UI
/// state — no session or pane depends on them — but both survive a restart,
/// so they live next to the layout that is already persisted per tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TabFlags {
    /// Pinned tabs sort to the front of the list and stay there.
    pub pinned: bool,
    /// Marking is the family's multi-select model: "关闭已标记标签页" acts on
    /// exactly this set.
    pub marked: bool,
    /// Replace the real tab title with a neutral label in all window chrome.
    pub private_title: bool,
}

/// One tab: its private pane layout plus the flags the user set on it.
struct Tab {
    layout: LayoutManager,
    flags: TabFlags,
}

impl Tab {
    fn new(layout: LayoutManager) -> Self {
        Tab {
            layout,
            flags: TabFlags::default(),
        }
    }
}

pub struct TabManager {
    tabs: Vec<Tab>,
    active: usize,
}

impl TabManager {
    /// 单 tab、单窗格的初始状态。
    pub fn new(session_idx: usize) -> Self {
        TabManager {
            tabs: vec![Tab::new(LayoutManager::new(session_idx))],
            active: 0,
        }
    }

    /// 从恢复出来的布局重建。空列表会退化成 `new(fallback_session_idx)`，
    /// 因为「零个 tab」不是一个可渲染的状态。
    fn from_layouts(
        layouts: impl IntoIterator<Item = (LayoutManager, TabFlags)>,
        active: usize,
        fallback_session_idx: usize,
    ) -> Self {
        let tabs: Vec<Tab> = layouts
            .into_iter()
            .map(|(layout, flags)| Tab { layout, flags })
            .collect();
        if tabs.is_empty() {
            return TabManager::new(fallback_session_idx);
        }
        let active = active.min(tabs.len() - 1);
        let mut manager = TabManager { tabs, active };
        // A snapshot written before pinning existed — or hand-edited — can
        // interleave pinned and unpinned tabs. Restoring re-establishes the
        // invariant instead of showing an order the app itself never produces.
        manager.reorder_pinned_first();
        manager
    }

    /// 从持久化快照重建 tab 列表。
    ///
    /// 关键的收尾是「收养孤儿」：sanitize 只保证同一个会话不会同时出现在两
    /// 个 tab 里，不保证每个会话都被某个 tab 收下。恢复失败的分支、旧格式里
    /// 游离在布局之外的会话都会落单，而落单的会话意味着一个活着却永远切不
    /// 到的 PTY。它们各自获得一个单窗格 tab。
    pub fn restore(
        saved_tabs: &[crate::session_persistence::LayoutSnapshot],
        session_ids: &[String],
        active_session_idx: usize,
        saved_active_tab: Option<usize>,
    ) -> Self {
        let mut layouts: Vec<(LayoutManager, TabFlags)> = saved_tabs
            .iter()
            .filter_map(|snapshot| {
                LayoutManager::try_from_snapshot(snapshot, session_ids, None).map(|layout| {
                    (
                        layout,
                        TabFlags {
                            pinned: snapshot.pinned,
                            marked: snapshot.marked,
                            private_title: snapshot.private_title,
                        },
                    )
                })
            })
            .collect();

        let mut adopted: std::collections::HashSet<usize> = layouts
            .iter()
            .flat_map(|(tab, _)| tab.session_indices())
            .collect();
        for session_idx in 0..session_ids.len() {
            if adopted.insert(session_idx) {
                layouts.push((LayoutManager::new(session_idx), TabFlags::default()));
            }
        }

        let active = saved_active_tab
            .filter(|idx| *idx < layouts.len())
            // 没记录活跃 tab（或它已失效）时跟着活跃会话走。
            .or_else(|| {
                layouts
                    .iter()
                    .position(|(tab, _)| tab.contains_session(active_session_idx))
            })
            .unwrap_or(0);
        Self::from_layouts(layouts, active, active_session_idx)
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    /// 恒为 false——最后一个 tab 关不掉。留着是为了和 `len` 配对，避免
    /// clippy::len_without_is_empty。
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    /// 每个 tab 的布局与它的标记状态，按 tab 顺序。持久化按这个顺序写出。
    pub fn layouts(&self) -> impl Iterator<Item = (&LayoutManager, TabFlags)> {
        self.tabs.iter().map(|tab| (&tab.layout, tab.flags))
    }

    /// 当前 tab 的布局。`active` 始终被维持在合法范围内，因此这里可以
    /// 返回引用而不是 Option——渲染路径每帧都要用它。
    pub fn active_layout(&self) -> &LayoutManager {
        &self.tabs[self.active.min(self.tabs.len() - 1)].layout
    }

    pub fn active_layout_mut(&mut self) -> &mut LayoutManager {
        let idx = self.active.min(self.tabs.len() - 1);
        &mut self.tabs[idx].layout
    }

    pub fn flags(&self, tab_idx: usize) -> TabFlags {
        self.tabs
            .get(tab_idx)
            .map(|tab| tab.flags)
            .unwrap_or_default()
    }

    /// 已标记的 tab 序号，升序。「关闭已标记标签页」以此为准。
    pub fn marked_tabs(&self) -> Vec<usize> {
        self.tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| tab.flags.marked)
            .map(|(idx, _)| idx)
            .collect()
    }

    /// 翻转标记位。返回翻转后的值；越界时返回 false 且不改变任何状态。
    pub fn toggle_marked(&mut self, tab_idx: usize) -> bool {
        match self.tabs.get_mut(tab_idx) {
            Some(tab) => {
                tab.flags.marked = !tab.flags.marked;
                tab.flags.marked
            }
            None => false,
        }
    }

    /// 翻转固定位并立即重排。返回翻转后的值。
    pub fn toggle_pinned(&mut self, tab_idx: usize) -> bool {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        tab.flags.pinned = !tab.flags.pinned;
        let pinned = tab.flags.pinned;
        self.reorder_pinned_first();
        pinned
    }

    /// Toggle title redaction without disturbing the title source itself.
    pub fn toggle_private_title(&mut self, tab_idx: usize) -> bool {
        match self.tabs.get_mut(tab_idx) {
            Some(tab) => {
                tab.flags.private_title = !tab.flags.private_title;
                tab.flags.private_title
            }
            None => false,
        }
    }

    /// 稳定重排，让固定的 tab 排到最前，同时保持 `active` 指向原来那个 tab。
    /// 与 anvil 的 `reorder_pinned_first` 语义一致。
    pub fn reorder_pinned_first(&mut self) {
        let mut order: Vec<usize> = (0..self.tabs.len()).collect();
        order.sort_by_key(|&idx| !self.tabs[idx].flags.pinned);
        if order.iter().enumerate().all(|(new, &old)| new == old) {
            return;
        }
        self.active = order
            .iter()
            .position(|&old| old == self.active)
            .unwrap_or(self.active);
        let mut taken: Vec<Option<Tab>> = self.tabs.drain(..).map(Some).collect();
        self.tabs = order
            .into_iter()
            .filter_map(|old| taken[old].take())
            .collect();
    }

    pub fn set_active(&mut self, tab_idx: usize) -> bool {
        if tab_idx >= self.tabs.len() || tab_idx == self.active {
            return false;
        }
        self.active = tab_idx;
        true
    }

    /// tab 的「当前选中窗格」所显示的会话。tab 标题、活跃会话路由都以它为准。
    pub fn focused_session_of(&self, tab_idx: usize) -> Option<usize> {
        self.tabs.get(tab_idx).and_then(|tab| {
            tab.layout
                .focused_session_idx()
                .or_else(|| tab.layout.session_indices().first().copied())
        })
    }

    pub fn active_focused_session(&self) -> Option<usize> {
        self.focused_session_of(self.active)
    }

    /// 某个 tab 拥有的全部会话。关闭 tab 时这些会话一起关闭。
    pub fn sessions_in(&self, tab_idx: usize) -> Vec<usize> {
        self.tabs
            .get(tab_idx)
            .map(|tab| tab.layout.session_indices())
            .unwrap_or_default()
    }

    /// 会话所属的 tab。布局保证一个会话至多出现在一个窗格里，因此至多一个 tab。
    pub fn tab_of_session(&self, session_idx: usize) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.layout.contains_session(session_idx))
    }

    /// 在当前 tab 之后插入一个新 tab 并激活它。会话索引必须已经存在于
    /// SessionManager 中，并且调用方已经调用过 [`Self::on_session_inserted`]。
    pub fn insert_tab_after_active(&mut self, session_idx: usize) -> usize {
        let at = (self.active + 1).min(self.tabs.len());
        self.tabs
            .insert(at, Tab::new(LayoutManager::new(session_idx)));
        self.active = at;
        at
    }

    /// 移除一个 tab（它的会话已经由调用方关闭）。返回 false 表示这是最后
    /// 一个 tab——窗口至少要留一个 tab。
    pub fn remove_tab(&mut self, tab_idx: usize) -> bool {
        if tab_idx >= self.tabs.len() || self.tabs.len() <= 1 {
            return false;
        }
        self.tabs.remove(tab_idx);
        if tab_idx < self.active {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        true
    }

    /// Resolve a requested drag destination without mutating the tab list.
    ///
    /// `requested_idx` is the tab under the pointer in the current list; the
    /// returned index is where the source will live after removal and insertion.
    /// Pinned tabs form a leading partition. Both drag previews and commits use
    /// this planner so the UI never advertises a destination that persistence
    /// would normalize away on the next launch.
    pub fn planned_reorder_destination(
        &self,
        from_idx: usize,
        requested_idx: usize,
    ) -> Option<usize> {
        if from_idx >= self.tabs.len() || requested_idx >= self.tabs.len() {
            return None;
        }
        // Pinned tabs form a leading partition. A drag may reorder within its
        // own partition, but cannot persist an interleaved order that restore
        // would immediately normalize on the next launch.
        let pinned_count = self.tabs.iter().filter(|tab| tab.flags.pinned).count();
        Some(if self.tabs[from_idx].flags.pinned {
            requested_idx.min(pinned_count.saturating_sub(1))
        } else {
            requested_idx.max(pinned_count)
        })
    }

    pub fn reorder(&mut self, from_idx: usize, requested_idx: usize) {
        let Some(to_idx) = self.planned_reorder_destination(from_idx, requested_idx) else {
            return;
        };
        if from_idx == to_idx {
            return;
        }
        let tab = self.tabs.remove(from_idx);
        self.tabs.insert(to_idx, tab);
        self.active = match self.active {
            a if a == from_idx => to_idx,
            a if from_idx < to_idx && a > from_idx && a <= to_idx => a - 1,
            a if to_idx < from_idx && a >= to_idx && a < from_idx => a + 1,
            a => a,
        };
    }

    /// Move a single-pane tab into a target pane as a directional split.
    ///
    /// Only layout leaves move: the `SessionManager` entry (and therefore the
    /// live PTY) is neither removed nor cloned. Validation precedes mutation, so
    /// self drops, split-tab sources, missing sessions, and duplicate ownership
    /// are exact no-ops. The moved session receives focus in the target tab.
    pub fn move_single_pane_tab_to_split(
        &mut self,
        source_session_idx: usize,
        target_session_idx: usize,
        direction: crate::layout::PaneDropDirection,
    ) -> bool {
        if source_session_idx == target_session_idx {
            return false;
        }
        let source_claims = self
            .tabs
            .iter()
            .flat_map(|tab| tab.layout.session_indices())
            .filter(|session_idx| *session_idx == source_session_idx)
            .count();
        let target_claims = self
            .tabs
            .iter()
            .flat_map(|tab| tab.layout.session_indices())
            .filter(|session_idx| *session_idx == target_session_idx)
            .count();
        if source_claims != 1 || target_claims != 1 {
            return false;
        }
        let Some(source_tab_idx) = self.tab_of_session(source_session_idx) else {
            return false;
        };
        let Some(target_tab_idx) = self.tab_of_session(target_session_idx) else {
            return false;
        };
        if source_tab_idx == target_tab_idx
            || self.tabs[source_tab_idx].layout.pane_count() != 1
            || self.tabs[source_tab_idx].layout.focused_session_idx() != Some(source_session_idx)
        {
            return false;
        }
        if self.tabs[target_tab_idx]
            .layout
            .split_session_at(target_session_idx, source_session_idx, direction)
            .is_err()
        {
            return false;
        }

        // The source had exactly one leaf and the target has already adopted
        // it, so removing the now-redundant tab cannot drop a session or PTY.
        self.tabs.remove(source_tab_idx);
        let target_tab_idx = if source_tab_idx < target_tab_idx {
            target_tab_idx - 1
        } else {
            target_tab_idx
        };
        self.active = target_tab_idx;
        true
    }

    /// Promote one pane from a split layout into its own ordinary tab. The
    /// source layout must retain at least one sibling; a single-pane/self drop
    /// is therefore a no-op. The promoted session becomes the active tab.
    pub fn promote_split_pane_to_tab(&mut self, session_idx: usize) -> bool {
        if self
            .tabs
            .iter()
            .flat_map(|tab| tab.layout.session_indices())
            .filter(|candidate| *candidate == session_idx)
            .count()
            != 1
        {
            return false;
        }
        let Some(source_tab_idx) = self.tab_of_session(session_idx) else {
            return false;
        };
        if self.tabs[source_tab_idx].layout.pane_count() <= 1
            || !self.tabs[source_tab_idx]
                .layout
                .remove_session_leaf(session_idx)
        {
            return false;
        }

        let inserted_at = (source_tab_idx + 1).min(self.tabs.len());
        self.tabs
            .insert(inserted_at, Tab::new(LayoutManager::new(session_idx)));
        self.active = inserted_at;
        // A promoted tab starts unpinned. Keep the existing invariant that all
        // pinned tabs lead while preserving `active` by identity.
        self.reorder_pinned_first();
        true
    }

    /// 新会话插入全局列表后，所有 tab 里 >= 插入点的索引整体右移。
    pub fn on_session_inserted(&mut self, inserted_idx: usize) {
        for tab in &mut self.tabs {
            tab.layout.on_session_inserted(inserted_idx);
        }
    }

    /// 会话从全局列表中删除后同步所有 tab。拥有它的 tab 先摘掉对应窗格
    /// （若那是它的最后一个窗格，则该 tab 已被调用方移除），其余 tab 只做
    /// 索引左移。
    pub fn on_session_removed(&mut self, removed_idx: usize) {
        for tab in &mut self.tabs {
            if tab.layout.contains_session(removed_idx) {
                tab.layout.remove_session_leaf(removed_idx);
            }
        }
        for tab in &mut self.tabs {
            tab.layout.shift_sessions_after_removal(removed_idx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split_active(tabs: &mut TabManager, session_idx: usize) {
        tabs.active_layout_mut()
            .split(session_idx, true)
            .expect("split");
    }

    #[test]
    fn splitting_stays_inside_the_active_tab() {
        let mut tabs = TabManager::new(0);
        tabs.insert_tab_after_active(1);
        assert_eq!(tabs.len(), 2);

        split_active(&mut tabs, 2);

        // The split added a pane, not a tab.
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.sessions_in(1), vec![1, 2]);
        assert_eq!(tabs.sessions_in(0), vec![0]);
    }

    #[test]
    fn a_tab_owns_every_session_in_its_panes() {
        let mut tabs = TabManager::new(0);
        tabs.insert_tab_after_active(1);
        split_active(&mut tabs, 2);
        split_active(&mut tabs, 3);

        let owned = tabs.sessions_in(1);
        assert_eq!(owned, vec![1, 2, 3]);
        for session in owned {
            assert_eq!(tabs.tab_of_session(session), Some(1));
        }
        assert_eq!(tabs.tab_of_session(0), Some(0));
    }

    #[test]
    fn the_tab_reports_the_selected_panes_session() {
        let mut tabs = TabManager::new(0);
        tabs.insert_tab_after_active(1);
        split_active(&mut tabs, 2);
        // A fresh split focuses the new pane.
        assert_eq!(tabs.active_focused_session(), Some(2));

        tabs.active_layout_mut()
            .focus_pane(crate::layout::PaneDirection::Prev);
        assert_eq!(tabs.active_focused_session(), Some(1));
        assert_eq!(tabs.focused_session_of(0), Some(0));
    }

    #[test]
    fn removing_a_session_reindexes_other_tabs() {
        let mut tabs = TabManager::new(0);
        tabs.insert_tab_after_active(1);
        split_active(&mut tabs, 2);
        tabs.set_active(0);

        // Session 1 closes: tab 1 loses that pane, and every index above it
        // shifts down in every tab.
        tabs.on_session_removed(1);

        assert_eq!(tabs.sessions_in(0), vec![0]);
        assert_eq!(tabs.sessions_in(1), vec![1]); // was session 2
    }

    #[test]
    fn closing_a_tabs_last_pane_leaves_the_tab_removable() {
        let mut tabs = TabManager::new(0);
        tabs.insert_tab_after_active(1);

        // Tab 1 holds a single pane, so the layout refuses to empty itself.
        assert!(!tabs.active_layout_mut().remove_session_leaf(1));
        assert!(tabs.remove_tab(1));
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs.active_index(), 0);
    }

    /// `TerminalApp::close_tab_synced` 的模型级复现:摘掉 tab,再按索引从大到
    /// 小逐个关闭它的会话。真正的风险是索引漂移——邻居 tab 必须仍然指向原来
    /// 那些会话。
    #[test]
    fn closing_a_tab_takes_all_its_sessions_without_disturbing_neighbours() {
        let mut tabs = TabManager::new(0); // tab0: [0]
        tabs.insert_tab_after_active(1); // tab1: [1]
        split_active(&mut tabs, 2); // tab1: [1, 2]
        split_active(&mut tabs, 3); // tab1: [1, 2, 3]
        tabs.insert_tab_after_active(4); // tab2: [4]
        assert_eq!(tabs.len(), 3);

        let mut owned = tabs.sessions_in(1);
        assert_eq!(owned, vec![1, 2, 3]);
        assert!(tabs.remove_tab(1));
        owned.sort_unstable_by(|a, b| b.cmp(a));
        for session_idx in owned {
            tabs.on_session_removed(session_idx);
        }

        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.sessions_in(0), vec![0]);
        // Session 4 was the only survivor above the closed tab, and it slid
        // down to index 1 as the three sessions below it went away.
        assert_eq!(tabs.sessions_in(1), vec![1]);
    }

    #[test]
    fn a_single_pane_tab_moves_into_a_directional_split_without_losing_a_session() {
        let mut tabs = TabManager::new(0); // source tab
        tabs.insert_tab_after_active(1); // visible target tab

        assert!(tabs.move_single_pane_tab_to_split(0, 1, crate::layout::PaneDropDirection::Left,));

        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs.sessions_in(0), vec![0, 1]);
        assert_eq!(tabs.active_index(), 0);
        assert_eq!(tabs.active_focused_session(), Some(0));
        assert_eq!(tabs.tab_of_session(0), Some(0));
        assert_eq!(tabs.tab_of_session(1), Some(0));
    }

    #[test]
    fn split_pane_promotion_preserves_every_leaf_and_focuses_the_new_tab() {
        let mut tabs = TabManager::new(0);
        tabs.insert_tab_after_active(1);
        split_active(&mut tabs, 2); // tab 1: [1, 2]

        assert!(tabs.promote_split_pane_to_tab(1));

        assert_eq!(tabs.len(), 3);
        assert_eq!(tabs.sessions_in(0), vec![0]);
        assert_eq!(tabs.sessions_in(1), vec![2]);
        assert_eq!(tabs.sessions_in(2), vec![1]);
        assert_eq!(tabs.active_index(), 2);
        assert_eq!(tabs.active_focused_session(), Some(1));
        let mut all: Vec<_> = (0..tabs.len())
            .flat_map(|tab_idx| tabs.sessions_in(tab_idx))
            .collect();
        all.sort_unstable();
        assert_eq!(all, vec![0, 1, 2]);
    }

    #[test]
    fn invalid_or_self_workspace_moves_are_no_ops() {
        let mut tabs = TabManager::new(0);
        tabs.insert_tab_after_active(1);
        split_active(&mut tabs, 2);

        assert!(!tabs.move_single_pane_tab_to_split(1, 0, crate::layout::PaneDropDirection::Right,));
        assert!(!tabs.move_single_pane_tab_to_split(
            0,
            0,
            crate::layout::PaneDropDirection::Bottom,
        ));
        assert!(!tabs.promote_split_pane_to_tab(0));
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.sessions_in(0), vec![0]);
        assert_eq!(tabs.sessions_in(1), vec![1, 2]);
    }

    #[test]
    fn duplicate_session_ownership_fails_closed_before_workspace_mutation() {
        let mut tabs = TabManager::new(0);
        tabs.insert_tab_after_active(1);
        // Build a deliberately hostile in-memory state that sanitized snapshots
        // cannot produce: session 0 is claimed by both tabs.
        split_active(&mut tabs, 0);
        let before: Vec<_> = (0..tabs.len()).map(|idx| tabs.sessions_in(idx)).collect();

        assert!(!tabs.move_single_pane_tab_to_split(0, 1, crate::layout::PaneDropDirection::Left,));
        assert!(!tabs.promote_split_pane_to_tab(0));
        assert_eq!(
            (0..tabs.len())
                .map(|idx| tabs.sessions_in(idx))
                .collect::<Vec<_>>(),
            before
        );
    }

    mod restore {
        use super::*;
        use crate::session_persistence::{LayoutNodeSnapshot, LayoutSnapshot};

        fn ids(n: usize) -> Vec<String> {
            (0..n).map(|i| format!("session-{i}")).collect()
        }

        fn pane(id: &str) -> LayoutNodeSnapshot {
            LayoutNodeSnapshot::Pane {
                session_id: id.to_string(),
            }
        }

        fn tab(root: LayoutNodeSnapshot, focused: Option<&str>) -> LayoutSnapshot {
            LayoutSnapshot {
                root,
                focused_session_id: focused.map(str::to_string),
                pinned: false,
                marked: false,
                private_title: false,
            }
        }

        /// 固定/标记是纯 UI 状态，但它们和布局一样必须跨重启存活，否则
        /// 「固定」在下次启动就成了空操作。
        #[test]
        fn pin_and_mark_survive_a_restore_and_pinned_tabs_lead() {
            let mut plain = tab(pane("session-0"), Some("session-0"));
            let mut marked = tab(pane("session-1"), Some("session-1"));
            marked.marked = true;
            marked.private_title = true;
            let mut pinned = tab(pane("session-2"), Some("session-2"));
            pinned.pinned = true;
            plain.pinned = false;

            let tabs = TabManager::restore(&[plain, marked, pinned], &ids(3), 1, Some(1));

            // 快照里固定的 tab 排在最后，恢复时被提到最前。
            assert_eq!(tabs.sessions_in(0), vec![2]);
            assert!(tabs.flags(0).pinned);
            assert_eq!(tabs.sessions_in(1), vec![0]);
            assert_eq!(tabs.sessions_in(2), vec![1]);
            assert!(tabs.flags(2).marked);
            assert!(tabs.flags(2).private_title);
            assert_eq!(tabs.marked_tabs(), vec![2]);
            // 重排跟着「用户上次看的那个 tab」走，而不是停在原来的序号上。
            assert_eq!(tabs.active_focused_session(), Some(1));
        }

        /// 旧快照没有这些字段，必须解析成「未固定未标记」而不是解析失败。
        #[test]
        fn legacy_snapshots_without_the_flags_restore_as_plain_tabs() {
            let legacy: LayoutSnapshot = serde_json::from_str(
                r#"{"root":{"kind":"pane","session_id":"session-0"},
                    "focused_session_id":"session-0"}"#,
            )
            .expect("legacy snapshot parses");
            assert!(!legacy.pinned);
            assert!(!legacy.marked);
            assert!(!legacy.private_title);

            let tabs = TabManager::restore(&[legacy], &ids(1), 0, Some(0));
            assert_eq!(tabs.flags(0), TabFlags::default());
        }

        #[test]
        fn private_title_toggles_without_changing_tab_identity() {
            let mut tabs = TabManager::new(0);
            assert!(!tabs.flags(0).private_title);
            assert!(tabs.toggle_private_title(0));
            assert_eq!(tabs.focused_session_of(0), Some(0));
            assert!(!tabs.toggle_private_title(0));
        }

        #[test]
        fn every_session_lands_in_exactly_one_tab() {
            // Two sessions share a tab; the third is not mentioned anywhere.
            let saved = vec![tab(
                LayoutNodeSnapshot::Split {
                    horizontal: true,
                    ratio: 0.5,
                    first: Box::new(pane("session-0")),
                    second: Box::new(pane("session-1")),
                },
                Some("session-1"),
            )];

            let tabs = TabManager::restore(&saved, &ids(3), 0, Some(0));

            assert_eq!(tabs.len(), 2);
            assert_eq!(tabs.sessions_in(0), vec![0, 1]);
            // The orphan was adopted instead of being left unreachable.
            assert_eq!(tabs.sessions_in(1), vec![2]);
            assert_eq!(tabs.focused_session_of(0), Some(1));
        }

        #[test]
        fn a_tab_that_cannot_be_restored_does_not_duplicate_a_live_session() {
            // The second tab references a session that no longer exists. It
            // must vanish, not fall back to a pane showing session 0 — that
            // would put session 0 in two tabs at once.
            let saved = vec![
                tab(pane("session-0"), Some("session-0")),
                tab(pane("session-gone"), None),
            ];

            let tabs = TabManager::restore(&saved, &ids(1), 0, Some(0));

            assert_eq!(tabs.len(), 1);
            assert_eq!(tabs.sessions_in(0), vec![0]);
        }

        #[test]
        fn no_saved_layout_gives_every_session_its_own_tab() {
            let tabs = TabManager::restore(&[], &ids(3), 2, None);

            assert_eq!(tabs.len(), 3);
            assert_eq!(tabs.sessions_in(0), vec![0]);
            assert_eq!(tabs.sessions_in(2), vec![2]);
            // With no recorded active tab, the one owning the active session wins.
            assert_eq!(tabs.active_index(), 2);
        }

        #[test]
        fn an_out_of_range_active_tab_falls_back_to_the_active_session() {
            let saved = vec![tab(pane("session-0"), None), tab(pane("session-1"), None)];

            let tabs = TabManager::restore(&saved, &ids(2), 1, Some(7));

            assert_eq!(tabs.active_index(), 1);
            assert_eq!(tabs.active_focused_session(), Some(1));
        }
    }

    /// 固定重排必须稳定（组内相对顺序不变），并且 `active` 要跟着原来那个
    /// tab 走——否则用户一固定标签页，屏幕上显示的就换成了别的会话。
    #[test]
    fn pinning_moves_a_tab_to_the_front_without_switching_the_active_one() {
        let mut tabs = TabManager::new(0);
        tabs.insert_tab_after_active(1);
        tabs.insert_tab_after_active(2);
        tabs.insert_tab_after_active(3);
        tabs.set_active(1); // 会话 1
        assert_eq!(tabs.active_focused_session(), Some(1));

        assert!(tabs.toggle_pinned(2)); // 固定持有会话 2 的那个 tab

        assert_eq!(tabs.sessions_in(0), vec![2]);
        assert!(tabs.flags(0).pinned);
        // 未固定的三个 tab 保持原有相对顺序。
        assert_eq!(tabs.sessions_in(1), vec![0]);
        assert_eq!(tabs.sessions_in(2), vec![1]);
        assert_eq!(tabs.sessions_in(3), vec![3]);
        // 活跃的仍然是会话 1，只是序号从 1 变成了 2。
        assert_eq!(tabs.active_index(), 2);
        assert_eq!(tabs.active_focused_session(), Some(1));

        // 取消固定把它放回未固定组的最前面，活跃标签页依旧不变。
        assert!(!tabs.toggle_pinned(0));
        assert_eq!(tabs.active_focused_session(), Some(1));
    }

    #[test]
    fn marking_tracks_exactly_the_tabs_the_user_marked() {
        let mut tabs = TabManager::new(0);
        tabs.insert_tab_after_active(1);
        tabs.insert_tab_after_active(2);

        assert!(tabs.marked_tabs().is_empty());
        assert!(tabs.toggle_marked(0));
        assert!(tabs.toggle_marked(2));
        assert_eq!(tabs.marked_tabs(), vec![0, 2]);

        assert!(!tabs.toggle_marked(0));
        assert_eq!(tabs.marked_tabs(), vec![2]);
        // 越界的目标既不改状态也不 panic。
        assert!(!tabs.toggle_marked(9));
        assert_eq!(tabs.marked_tabs(), vec![2]);
    }

    /// 标记跟着 tab 走，而不是跟着序号走：删掉前面的 tab 之后，标记必须
    /// 还在原来那个 tab 上。
    #[test]
    fn flags_follow_their_tab_across_removal() {
        let mut tabs = TabManager::new(0);
        tabs.insert_tab_after_active(1);
        tabs.insert_tab_after_active(2);
        assert!(tabs.toggle_marked(2));
        assert!(tabs.toggle_pinned(2));
        // 固定后它排到了最前。
        assert_eq!(tabs.sessions_in(0), vec![2]);

        assert!(tabs.remove_tab(1)); // 移除持有会话 0 的 tab
        tabs.on_session_removed(0);

        assert_eq!(tabs.len(), 2);
        assert_eq!(
            tabs.flags(0),
            TabFlags {
                pinned: true,
                marked: true,
                private_title: false,
            }
        );
        assert_eq!(tabs.flags(1), TabFlags::default());
    }

    #[test]
    fn the_last_tab_never_goes_away() {
        let mut tabs = TabManager::new(0);
        assert!(!tabs.remove_tab(0));
        assert_eq!(tabs.len(), 1);
    }

    #[test]
    fn reordering_follows_the_active_tab() {
        let mut tabs = TabManager::new(0);
        tabs.insert_tab_after_active(1);
        tabs.insert_tab_after_active(2);
        assert_eq!(tabs.active_index(), 2);
        assert_eq!(tabs.planned_reorder_destination(2, 0), Some(0));
        assert_eq!(tabs.planned_reorder_destination(3, 0), None);
        assert_eq!(tabs.planned_reorder_destination(0, 3), None);

        tabs.reorder(2, 0);
        assert_eq!(tabs.active_index(), 0);
        assert_eq!(tabs.sessions_in(0), vec![2]);
        assert_eq!(tabs.sessions_in(1), vec![0]);
        assert_eq!(tabs.sessions_in(2), vec![1]);
    }

    #[test]
    fn pinned_reorder_preview_matches_the_clamped_commit() {
        let mut tabs = TabManager::new(0);
        tabs.insert_tab_after_active(1);
        tabs.insert_tab_after_active(2);
        tabs.insert_tab_after_active(3);
        assert!(tabs.toggle_pinned(1));
        assert!(tabs.toggle_pinned(2));
        assert_eq!(tabs.sessions_in(0), vec![1]);
        assert_eq!(tabs.sessions_in(1), vec![2]);

        let source_session = tabs.sessions_in(0)[0];
        let preview_destination = tabs.planned_reorder_destination(0, 3);
        assert_eq!(preview_destination, Some(1));
        tabs.reorder(0, 3);

        assert_eq!(tabs.tab_of_session(source_session), preview_destination);
        assert_eq!(tabs.sessions_in(0), vec![2]);
        assert_eq!(tabs.sessions_in(1), vec![1]);
        assert!(tabs.flags(0).pinned);
        assert!(tabs.flags(1).pinned);
        assert!(!tabs.flags(2).pinned);
        assert!(!tabs.flags(3).pinned);
        assert_eq!(tabs.active_focused_session(), Some(3));
        assert_eq!(tabs.planned_reorder_destination(1, 3), Some(1));
    }

    #[test]
    fn unpinned_reorder_preview_matches_the_clamped_commit() {
        let mut tabs = TabManager::new(0);
        tabs.insert_tab_after_active(1);
        tabs.insert_tab_after_active(2);
        tabs.insert_tab_after_active(3);
        assert!(tabs.toggle_pinned(0));
        assert!(tabs.toggle_pinned(1));
        assert_eq!(tabs.sessions_in(0), vec![0]);
        assert_eq!(tabs.sessions_in(1), vec![1]);

        let source_session = tabs.sessions_in(3)[0];
        let preview_destination = tabs.planned_reorder_destination(3, 0);
        assert_eq!(preview_destination, Some(2));
        tabs.reorder(3, 0);

        assert_eq!(tabs.tab_of_session(source_session), preview_destination);
        assert_eq!(tabs.sessions_in(0), vec![0]);
        assert_eq!(tabs.sessions_in(1), vec![1]);
        assert_eq!(tabs.sessions_in(2), vec![3]);
        assert_eq!(tabs.sessions_in(3), vec![2]);
        assert!(tabs.flags(0).pinned);
        assert!(tabs.flags(1).pinned);
        assert!(!tabs.flags(2).pinned);
        assert!(!tabs.flags(3).pinned);
        assert_eq!(tabs.active_focused_session(), Some(3));
        assert_eq!(tabs.planned_reorder_destination(2, 0), Some(2));
    }

    #[test]
    fn hover_preview_origin_can_be_restored_after_indices_move() {
        let mut tabs = TabManager::new(0);
        tabs.insert_tab_after_active(1);
        tabs.insert_tab_after_active(2);
        tabs.set_active(0);
        let origin_session_idx = tabs.active_focused_session().unwrap();

        // Model a delayed hover preview, followed by an unrelated reorder.
        tabs.set_active(2);
        tabs.reorder(0, 2);
        assert_eq!(tabs.active_focused_session(), Some(2));

        let restored_tab = tabs.tab_of_session(origin_session_idx).unwrap();
        tabs.set_active(restored_tab);
        assert_eq!(tabs.active_focused_session(), Some(0));
    }

    #[test]
    fn inserting_a_session_shifts_indices_in_every_tab() {
        let mut tabs = TabManager::new(0);
        tabs.insert_tab_after_active(1);
        tabs.insert_tab_after_active(2);

        // A new session lands at index 1; everything at or above shifts up.
        tabs.on_session_inserted(1);

        assert_eq!(tabs.sessions_in(0), vec![0]);
        assert_eq!(tabs.sessions_in(1), vec![2]);
        assert_eq!(tabs.sessions_in(2), vec![3]);
    }
}
