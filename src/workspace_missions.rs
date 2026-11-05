//! Native delivery-plan view. The shared host owns every operation and report.
//! Drafts are UI state keyed by stable Mission identity, never execution state.
use egui::{Button, Context, Ui};
use jterm_core::agent_workspace::{
    ConversationId, MissionWorkbench, WorkspaceMission, WorkspaceMissionReport,
    WorkspaceMissionTaskRef,
};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone)]
struct Draft {
    saved: WorkspaceMission,
    edited: WorkspaceMission,
}
impl Draft {
    fn new(saved: &WorkspaceMission) -> Self {
        Self {
            saved: saved.clone(),
            edited: saved.clone(),
        }
    }
    fn dirty(&self) -> bool {
        self.saved != self.edited
    }
    fn rebase_explicitly(&mut self, saved: &WorkspaceMission) {
        // This only follows the user's explicit conflict-resolution gesture.
        // Preserve every draft field, but use the current CAS/host metadata.
        self.edited.revision = saved.revision;
        self.edited.updated_at_ms = saved.updated_at_ms;
        self.edited.evidence_after_ms = saved.evidence_after_ms;
        self.saved = saved.clone();
    }
    fn sync(&mut self, saved: &WorkspaceMission) {
        // Acknowledged own writes may change host-owned metadata. A concurrent
        // edit must not silently rebase the user's dirty acceptance contract.
        let mut comparable = self.edited.clone();
        comparable.revision = saved.revision;
        comparable.updated_at_ms = saved.updated_at_ms;
        comparable.evidence_after_ms = saved.evidence_after_ms;
        if !self.dirty() || comparable == *saved {
            *self = Self::new(saved);
        }
    }
}

#[derive(Default)]
pub(crate) struct MissionPanel {
    pub(crate) open: bool,
    pub(crate) host: MissionWorkbench,
    drafts: HashMap<String, Draft>,
    title: String,
    goal: String,
    parent: String,
}
impl MissionPanel {
    pub(crate) fn open(&mut self) {
        self.open = true;
        self.host.refresh();
    }
    pub(crate) fn attention_rows(&self) -> Vec<(String, String)> {
        self.host
            .missions
            .iter()
            .filter(|mission| {
                self.host
                    .report_for(&mission.id)
                    .is_none_or(WorkspaceMissionReport::needs_you)
            })
            .map(|mission| (mission.id.clone(), mission.title.clone()))
            .collect()
    }
    fn sync_draft(&mut self) {
        if let Some(saved) = &self.host.selected {
            self.drafts
                .entry(saved.id.clone())
                .and_modify(|draft| draft.sync(saved))
                .or_insert_with(|| Draft::new(saved));
        }
    }
    fn poll(&mut self, ctx: &Context) {
        self.host.poll();
        self.sync_draft();
        if self.host.busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    pub(crate) fn show(
        &mut self,
        ctx: &Context,
        selected_task: Option<&ConversationId>,
        available: &[WorkspaceMissionTaskRef],
    ) -> Option<WorkspaceMissionTaskRef> {
        self.poll(ctx);
        if !self.open {
            return None;
        }
        let mut open = self.open;
        let mut navigate = None;
        let size = ctx.viewport_rect().size();
        egui::Window::new("Missions · delivery evidence")
            .id(egui::Id::new("workspace-missions"))
            .open(&mut open).default_width(700.0)
            .max_size(egui::vec2((size.x - 24.0).max(220.0), (size.y - 50.0).max(180.0)))
            .vscroll(true).show(ctx, |ui| {
                ui.small("Plans and dated evidence only. This view never starts tasks, tests, commits or pushes.");
                if ui.add_enabled(!self.host.busy, Button::new("Refresh saved Missions")).clicked() { self.host.refresh(); }
                if let Some(notice) = &self.host.notice { ui.colored_label(ui.visuals().warn_fg_color, notice); }
                if self.host.busy { ui.spinner(); }
                for mission in self.host.missions.clone() {
                    let state = self.host.report_for(&mission.id).map(|report| report.state.label()).unwrap_or("Not inspected · needs you");
                    let selected = self.host.selected.as_ref().is_some_and(|item| item.id == mission.id);
                    if ui.push_id(("mission-row", &mission.id), |ui| ui.selectable_label(selected, format!("{} · {state}", crate::review_text::visible_bounded(&mission.title, 120)))).inner.clicked() {
                        self.host.select(&mission.id);
                    }
                }
                egui::CollapsingHeader::new("Create a five-repository Mission").id_salt("mission-create").show(ui, |ui| {
                    ui.label("Title"); ui.text_edit_singleline(&mut self.title);
                    ui.label("Goal and acceptance intent"); ui.text_edit_multiline(&mut self.goal);
                    ui.label("Absolute parent directory containing jterm_core, anvil, forge, ember and frost");
                    ui.text_edit_singleline(&mut self.parent);
                    ui.small("Missing repositories remain visible as blocked. Creation writes only the shared Mission record.");
                    let valid = !self.title.trim().is_empty() && !self.goal.trim().is_empty() && PathBuf::from(self.parent.trim()).is_absolute();
                    if ui.add_enabled(valid && !self.host.busy, Button::new("Create Mission")).clicked() {
                        let roots = ["jterm_core", "anvil", "forge", "ember", "frost"].map(|name| (format!("beamiter/{name}"), PathBuf::from(self.parent.trim()).join(name)));
                        self.host.create(self.title.trim(), self.goal.trim(), &roots);
                    }
                });
                let Some(saved) = self.host.selected.clone() else { return; };
                self.sync_draft();
                ui.push_id(("mission-draft", &saved.id), |ui| {
                let draft = self.drafts.get_mut(&saved.id).expect("selected draft synchronized");
                ui.separator();
                ui.small(format!("{} · revision {}", saved.id, saved.revision));
                // Do not accept post-submit edits that cannot be reconciled
                // with the in-flight write's acknowledgement.
                ui.add_enabled_ui(!self.host.busy, |ui| {
                ui.label("Title"); ui.text_edit_singleline(&mut draft.edited.title);
                ui.label("Goal"); ui.text_edit_multiline(&mut draft.edited.goal);
                ui.label("Required validation environment"); ui.text_edit_singleline(&mut draft.edited.required_environment);
                for repository in &mut draft.edited.repositories {
                    ui.push_id(&repository.identity.full_name, |ui| {
                        ui.collapsing(&repository.identity.full_name, |ui| {
                            let mut path = repository.path.to_string_lossy().into_owned();
                            ui.label("Exact checkout / worktree path");
                            if ui.text_edit_singleline(&mut path).changed() { repository.path = PathBuf::from(path); }
                            ui.label("Target ref"); ui.text_edit_singleline(&mut repository.target_ref);
                            ui.monospace(format!("Baseline: {}", known(&repository.baseline)));
                            ui.monospace(format!("Candidate: {}", repository.candidate.as_deref().unwrap_or("not frozen")));
                            ui.monospace(format!("Required core: {}", repository.required_core_pin.as_deref().unwrap_or("not set")));
                            ui.label("Required checks (exact names; removing one changes acceptance)");
                            let mut remove = None;
                            for (index, check) in repository.required_checks.iter_mut().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.text_edit_singleline(check);
                                    if ui.small_button("Remove").clicked() { remove = Some(index); }
                                });
                            }
                            if let Some(index) = remove { repository.required_checks.remove(index); }
                            if ui.small_button("Add required check").clicked() { repository.required_checks.push(String::new()); }
                            for task in &repository.tasks {
                                ui.small(format!("Task {} · run {} · source {}", task.task_id, task.run_id, task.source_sequence));
                                let present = available.iter().any(|entry| same_task(entry, task));
                                if ui.add_enabled(present, Button::new("Open task / current Result")).clicked() { navigate = Some(task.clone()); }
                                if !present { ui.weak("Task is unavailable in this frontend. Saved run identity is retained."); }
                            }
                        });
                    });
                }
                });
                let dirty = draft.dirty();
                let conflict = draft.saved.revision != saved.revision;
                if dirty { ui.weak("Unsaved draft retained when you close this view or select another Mission."); }
                if conflict {
                    ui.colored_label(ui.visuals().warn_fg_color, "The saved Mission changed elsewhere. Your draft is retained; compare the current saved contract before resolving.");
                    ui.collapsing("Compare current saved contract", |ui| {
                        ui.monospace(serde_json::to_string_pretty(&saved).expect("Mission is serializable"));
                    });
                    ui.small("Keeping your draft adopts the current revision only. The next Save will replace saved fields with your draft; no write happens now.");
                    if ui.add_enabled(!self.host.busy, Button::new("Keep my draft against current revision")).clicked() {
                        draft.rebase_explicitly(&saved);
                    }
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(dirty && !conflict && !self.host.busy, Button::new("Save acceptance plan")).clicked() { self.host.save(draft.edited.clone()); }
                    if ui.add_enabled(dirty && !self.host.busy, Button::new("Discard draft changes")).clicked() { *draft = Draft::new(&saved); }
                    let ready = !dirty && !self.host.busy;
                    if ui.add_enabled(ready, Button::new("Inspect repositories (read-only)")).clicked() { self.host.inspect(); }
                    if ui.add_enabled(ready && self.host.can_adopt_candidates(), Button::new("Freeze observed candidates")).clicked() { self.host.adopt_candidates(); }
                    if ui.add_enabled(ready && selected_task.is_some(), Button::new("Link selected Task run")).clicked() {
                        if let Some(id) = selected_task { self.host.link_conversation(id); }
                    }
                });
                ui.small("Save edits before inspecting or linking. Freezing records clean observed commits and invalidates old evidence; it does not publish code.");
                if dirty {
                    ui.weak("Save this draft and inspect again. Prior saved evidence does not validate the edited acceptance contract.");
                } else if let Some(report) = self.host.report.as_ref().filter(|report| report.id == saved.id && report.revision == saved.revision) {
                    draw_report(ui, report);
                } else { ui.weak("No current inspection. An uninspected or changed plan is never presumed complete."); }
                });
            });
        self.open = open;
        navigate
    }
}
fn known(value: &str) -> &str {
    if value.is_empty() {
        "not captured"
    } else {
        value
    }
}
fn same_task(a: &WorkspaceMissionTaskRef, b: &WorkspaceMissionTaskRef) -> bool {
    a.task_id == b.task_id && a.conversation == b.conversation
}
fn draw_report(ui: &mut Ui, report: &WorkspaceMissionReport) {
    ui.separator();
    ui.strong(report.state.label());
    for error in &report.errors {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }
    for repository in &report.repositories {
        ui.group(|ui| {
            ui.strong(&repository.identity.full_name);
            ui.monospace(repository.path.display().to_string());
            ui.label(format!("Modified: {} · committed: {} · pushed: {} · integrated: {} · validated: {} · pins: {}", repository.modified, repository.committed, repository.pushed, repository.integrated, repository.validated, repository.pins_current));
            if let Some(observed) = &repository.observed {
                ui.monospace(format!("Observed HEAD: {} · dirty: {}", observed.head, observed.dirty));
                ui.small(format!("Fingerprint: {} · observed at {} ms UTC", observed.fingerprint, observed.observed_at_ms));
                ui.monospace(format!("Manifest core: {}\nLock core: {}", observed.manifest_core_pin.as_deref().unwrap_or("unavailable"), observed.lock_core_pin.as_deref().unwrap_or("unavailable")));
            }
            if let Some(remote) = &repository.pushed_observation { ui.small(format!("Push observation: {} {} @ {} · {} ms UTC", remote.repository.full_name, remote.ref_name, remote.head, remote.observed_at_ms)); }
            if let Some(integration) = &repository.integration_observation { ui.small(format!("Integration observation: {} @ {} contains {}: {} · {} ms UTC", integration.remote.ref_name, integration.remote.head, integration.candidate, integration.contains_candidate, integration.remote.observed_at_ms)); }
            for check in &repository.checks {
                ui.label(format!("{} · {} · {}", check.check_name, check.environment, check.status.label()));
                if let Some(receipt) = &check.receipt_id { ui.monospace(format!("Host receipt: {receipt}")); }
                if let Some(detail) = &check.detail { ui.small(detail); }
            }
            if !repository.missing_checks.is_empty() { ui.label(format!("Missing/current checks: {}", repository.missing_checks.join(", "))); }
            for error in &repository.errors { ui.colored_label(ui.visuals().error_fg_color, error); }
        });
    }
}

impl crate::app::state::TerminalApp {
    pub(crate) fn render_workspace_missions(&mut self, ctx: &Context) {
        if !self.config.agent_workspace_enabled {
            return;
        }
        if !self.agent_sessions.missions.open {
            self.agent_sessions.missions.poll(ctx);
            return;
        }
        let selected = self
            .workspace_selected_row()
            .filter(|row| row.kind == jterm_core::agent_workspace::ConversationKind::Task)
            .map(|row| row.id);
        let available: Vec<_> = if self.config.experimental_task_sidebar {
            self.workspace_task_rows()
                .into_iter()
                .map(|(task_id, row)| WorkspaceMissionTaskRef {
                    conversation: row.id,
                    task_id,
                    run_id: String::new(),
                    source_sequence: 0,
                })
                .collect()
        } else {
            Vec::new()
        };
        if let Some(task) = self
            .agent_sessions
            .missions
            .show(ctx, selected.as_ref(), &available)
        {
            self.workspace_open_mission_task(ctx, &task);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_saved_revision_preserves_dirty_draft_and_conflict_base() {
        let mut saved = WorkspaceMission::new("first", "goal");
        saved.revision = 1;
        let mut draft = Draft::new(&saved);
        draft.edited.title = "my edit".into();
        saved.revision = 2;
        saved.title = "external edit".into();
        draft.sync(&saved);
        assert_eq!(draft.edited.title, "my edit");
        assert_eq!(draft.edited.revision, 1);
        assert!(draft.dirty());
    }
    #[test]
    fn acknowledged_save_updates_metadata_without_false_dirty_state() {
        let mut saved = WorkspaceMission::new("first", "goal");
        saved.revision = 1;
        let mut draft = Draft::new(&saved);
        draft.edited.goal = "new goal".into();
        saved = draft.edited.clone();
        saved.revision = 2;
        saved.evidence_after_ms = 100;
        draft.sync(&saved);
        assert!(!draft.dirty());
        assert_eq!(draft.edited.revision, 2);
    }
    #[test]
    fn close_reopen_and_selection_keep_each_identity_draft() {
        let mut panel = MissionPanel::default();
        let first = WorkspaceMission::new("one", "goal");
        let second = WorkspaceMission::new("two", "goal");
        panel.host.selected = Some(first.clone());
        panel.sync_draft();
        panel.drafts.get_mut(&first.id).unwrap().edited.goal = "unsaved".into();
        panel.open = false;
        panel.host.selected = Some(second);
        panel.sync_draft();
        panel.open = true;
        panel.host.selected = Some(first.clone());
        panel.sync_draft();
        assert_eq!(panel.drafts[&first.id].edited.goal, "unsaved");
        assert_eq!(panel.drafts.len(), 2);
        assert!(!panel.host.busy);
    }
    #[test]
    fn uninspected_mission_is_an_attention_row_with_stable_identity() {
        let mut panel = MissionPanel::default();
        let mission = WorkspaceMission::new("one", "goal");
        panel.host.missions.push(mission.clone());
        assert_eq!(panel.attention_rows(), vec![(mission.id, mission.title)]);
        assert!(!panel.host.busy);
    }
    #[test]
    fn navigation_requires_source_qualified_task_identity() {
        let first = WorkspaceMissionTaskRef {
            conversation: ConversationId("host-a:task".into()),
            task_id: jterm_core::agent_task::TaskId::new(),
            run_id: "saved-run".into(),
            source_sequence: 1,
        };
        let mut other = first.clone();
        other.conversation = ConversationId("host-b:task".into());
        assert!(!same_task(&first, &other));
        other = first.clone();
        other.run_id = "newer-run".into();
        assert!(same_task(&first, &other), "navigation is explicitly to the current task, not a claim that a historical run is current");
    }
    #[test]
    fn native_panel_renders_narrow_and_wide_without_starting_operations() {
        for width in [320.0, 960.0] {
            let ctx = Context::default();
            let mut panel = MissionPanel {
                open: true,
                ..MissionPanel::default()
            };
            let mission = WorkspaceMission::four_terminals(
                "Five repositories",
                "Frozen evidence",
                "/missing-fixture",
            );
            panel.host.missions.push(mission.clone());
            panel.host.selected = Some(mission);
            for _ in 0..3 {
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 600.0),
                        )),
                        ..Default::default()
                    },
                    |_| {
                        assert!(panel.show(&ctx, None, &[]).is_none());
                    },
                );
                output.drop_without_applying_deltas();
            }
            assert!(!panel.host.busy);
            assert!(panel.host.notice.is_none());
            assert_eq!(panel.drafts.len(), 1);
        }
    }
    #[test]
    fn explicit_conflict_resolution_keeps_draft_and_uses_current_cas_revision() {
        let mut saved = WorkspaceMission::new("first", "goal");
        saved.revision = 1;
        let mut draft = Draft::new(&saved);
        draft.edited.title = "my edit".into();
        saved.revision = 2;
        saved.goal = "external goal".into();
        draft.sync(&saved);
        assert_eq!(draft.edited.revision, 1);
        draft.rebase_explicitly(&saved);
        assert_eq!(draft.edited.revision, 2);
        assert_eq!(draft.edited.title, "my edit");
        assert_eq!(draft.edited.goal, "goal");
        assert_eq!(draft.saved.goal, "external goal");
        assert!(draft.dirty());
    }
}
