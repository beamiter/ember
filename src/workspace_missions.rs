//! Native delivery-plan view. The shared host owns every operation and report.
//! Drafts are UI state keyed by stable Mission identity, never execution state.
use egui::{Button, Context, Ui};
use jterm_core::agent_workspace::{
    ConversationId, MissionWorkbench, WorkspaceMission, WorkspaceMissionAttention,
    WorkspaceMissionReport, WorkspaceMissionTaskRef, WorkspaceResultReport,
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
    attention: Option<WorkspaceMissionAttention>,
    evidence: crate::workspace_review::WorkspaceReview,
    attention_changes: bool,
    title: String,
    goal: String,
    parent: String,
}
impl MissionPanel {
    pub(crate) fn open(&mut self) {
        self.open = true;
        self.host.refresh();
    }
    pub(crate) fn attention_rows(&self) -> Vec<WorkspaceMissionAttention> {
        self.host.attention_items()
    }
    pub(crate) fn accepts_attention(&self, item: &WorkspaceMissionAttention) -> bool {
        !self.host.busy
            && !self.drafts.get(&item.mission_id).is_some_and(Draft::dirty)
            && self
                .host
                .missions
                .iter()
                .find(|mission| mission.id == item.mission_id)
                .is_some_and(|mission| {
                    mission
                        .attention_items(self.host.report_for(&mission.id))
                        .iter()
                        .any(|current| current == item)
                })
    }
    pub(crate) fn open_attention(&mut self, item: &WorkspaceMissionAttention) {
        if !self.accepts_attention(item) {
            return;
        }
        self.open = true;
        self.attention = Some(item.clone());
        self.attention_changes = false;
        self.evidence.clear_task();
        if !self
            .host
            .selected
            .as_ref()
            .is_some_and(|saved| saved.id == item.mission_id)
        {
            // Reopening a saved report invalidates its current gates in core.
            // The next poll retires this selection if the entry has changed.
            self.host.select(&item.mission_id);
        }
        if let Some(task) = &item.task {
            self.evidence.select_task(task.conversation.clone());
        }
    }
    fn selected_attention(&self) -> Option<&WorkspaceMissionAttention> {
        self.attention.as_ref().filter(|item| {
            self.accepts_attention(item)
                && self
                    .host
                    .selected
                    .as_ref()
                    .is_some_and(|saved| saved.id == item.mission_id)
        })
    }
    fn exact_result(&self, item: &WorkspaceMissionAttention) -> Option<&WorkspaceResultReport> {
        self.selected_attention()
            .filter(|selected| *selected == item)
            .and_then(|_| exact_receipt(item, self.evidence.report.as_ref()))
    }
    fn open_attention_result(&mut self, item: &WorkspaceMissionAttention, changes: bool) {
        if self.exact_result(item).is_some() {
            self.attention_changes = changes;
        }
    }
    fn reconcile_attention(&mut self) {
        // A stable ID alone cannot retain a changed receipt, revision or reason.
        // Busy requests temporarily hide the result, not retire a valid choice.
        if !self.host.busy && self.attention.is_some() && self.selected_attention().is_none() {
            self.attention = None;
            self.evidence.clear_task();
        }
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
        self.evidence.poll();
        self.reconcile_attention();
        if self.host.busy || self.evidence.report_loading {
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
                self.draw_attention(ui);
            });
        self.open = open;
        navigate
    }
    fn draw_attention(&mut self, ui: &mut Ui) {
        let Some(mission_id) = self.host.selected.as_ref().map(|saved| saved.id.clone()) else {
            return;
        };
        if self.drafts.get(&mission_id).is_some_and(Draft::dirty) {
            return;
        }
        let items: Vec<_> = self
            .attention_rows()
            .into_iter()
            .filter(|item| item.mission_id == mission_id)
            .collect();
        if items.is_empty() {
            return;
        }
        ui.separator();
        ui.strong(format!("Needs you · {} next actions", items.len()));
        for item in items {
            ui.push_id(("mission-attention", &item.id), |ui| {
                let location = item.repository.as_ref().map_or("Mission", |repo| repo.full_name.as_str());
                let check = item.check_name.as_ref().map_or(String::new(), |name| format!(" · {name}"));
                let label = format!("{} · {}{}", visible(location), item.kind.label(), visible(&check));
                if ui.add_enabled(self.accepts_attention(&item), Button::new(label).wrap()).clicked() {
                    self.open_attention(&item);
                }
                if self.selected_attention() != Some(&item) {
                    return;
                }
                ui.small(visible(&item.detail));
                if let Some(candidate) = &item.candidate { ui.monospace(format!("Candidate: {}", visible(candidate))); }
                if let Some(environment) = &item.environment { ui.small(format!("Required environment: {}", visible(environment))); }
                if let Some(receipt) = &item.receipt_id { ui.monospace(format!("Receipt: {}", visible(receipt))); }
                let Some(task) = &item.task else {
                    if item.receipt_id.is_some() {
                        ui.weak("This receipt has no unambiguous linked task run. No task shortcut is available.");
                    }
                    return;
                };
                ui.small(format!("Task {} · run {} · source {}", task.task_id, visible(&task.run_id), task.source_sequence));
                ui.small(format!("Conversation: {}", visible(&task.conversation.0)));
                let captured = self.exact_result(&item).is_some();
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(captured, Button::new("Show validation log")).clicked() {
                        self.open_attention_result(&item, false);
                    }
                    if ui.add_enabled(captured, Button::new("Show captured diff")).clicked() {
                        self.open_attention_result(&item, true);
                    }
                    if ui.add_enabled(!self.evidence.report_loading && self.accepts_attention(&item), Button::new("Reload exact receipt")).clicked() {
                        self.evidence.refresh_result();
                    }
                });
                if self.evidence.report_loading { ui.spinner(); }
                if let Some(notice) = &self.evidence.notice { ui.weak(visible(notice)); }
                let Some(report) = self.exact_result(&item) else {
                    ui.weak("The exact run receipt is unavailable in the current host view. Inspect again; newer output is not this receipt.");
                    return;
                };
                draw_receipt(ui, report, self.attention_changes);
            });
        }
    }
}
fn known(value: &str) -> &str {
    if value.is_empty() {
        "not captured"
    } else {
        value
    }
}
fn visible(value: &str) -> String {
    crate::review_text::visible_bounded(value, 4096)
}
fn exact_receipt<'a>(
    item: &WorkspaceMissionAttention,
    report: Option<&'a WorkspaceResultReport>,
) -> Option<&'a WorkspaceResultReport> {
    report.filter(|report| item.matches_result(report))
}
fn draw_receipt(ui: &mut Ui, report: &WorkspaceResultReport, changes: bool) {
    ui.small(format!(
        "Receipt capture · run {} · event {}",
        visible(&report.run_id),
        report.source_sequence
    ));
    if let Some(error) = &report.error {
        ui.weak(visible(error));
    }
    ui.label(visible(&report.validation_status));
    if !report.validation_current {
        ui.weak("Validation is stale for the current worktree capture.");
    }
    if let Some(validation) = &report.validation {
        ui.monospace(format!(
            "Validation input HEAD: {}\nValidation input fingerprint: {}",
            visible(&validation.head),
            visible(&validation.fingerprint)
        ));
    }
    if changes {
        ui.weak("Current worktree capture, not necessarily the validation input.");
        if let Some(current) = &report.current {
            ui.monospace(format!(
                "Captured HEAD: {}\nCaptured fingerprint: {}\nCaptured at: {} ms UTC",
                visible(&current.head),
                visible(&current.fingerprint),
                current.captured_at_ms
            ));
            ui.monospace(crate::review_text::visible_bounded(
                &current.diff,
                64 * 1024,
            ));
            if current.diff_truncated || current.diff.len() > 64 * 1024 {
                ui.weak("The captured diff is a retained excerpt.");
            }
        } else {
            ui.weak("This receipt has no captured diff.");
        }
    } else {
        ui.monospace(crate::review_text::visible_bounded(
            &report.validation_output,
            64 * 1024,
        ));
        if report.validation_output_truncated || report.validation_output.len() > 64 * 1024 {
            ui.weak("The validation log is a retained excerpt.");
        }
    }
    ui.weak("Read-only receipt view. Current worktree data is a separate dated capture; inspect again after new work.");
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
    fn attention_panel() -> MissionPanel {
        let mut panel = MissionPanel::default();
        let mut mission =
            WorkspaceMission::four_terminals("Delivery", "Exact evidence", "/missing-fixture");
        mission.revision = 1;
        panel.host.missions.push(mission.clone());
        panel.host.selected = Some(mission);
        panel.sync_draft();
        panel
    }
    fn receipt_fixture() -> (WorkspaceMissionAttention, WorkspaceResultReport) {
        let mut item = attention_panel().attention_rows().remove(0);
        let task_id = jterm_core::agent_task::TaskId::new();
        let task = WorkspaceMissionTaskRef {
            conversation: ConversationId(format!(
                "jterm:task:v1:{}",
                serde_json::to_string(&("host", "namespace", task_id.to_string())).unwrap()
            )),
            task_id,
            run_id: "saved:run".into(),
            source_sequence: 7,
        };
        item.kind = jterm_core::agent_workspace::WorkspaceMissionAttentionKind::ValidationFailed;
        item.receipt_id = Some("workspace:saved:run:9".into());
        item.task = Some(task.clone());
        let result = WorkspaceResultReport {
            conversation: task.conversation,
            task_id,
            run_id: task.run_id,
            source_sequence: 9,
            baseline: None,
            current: None,
            validation: None,
            validation_current: false,
            validation_status: "Failed".into(),
            validation_output: "Exact saved failure".into(),
            validation_output_truncated: false,
            error: None,
        };
        (item, result)
    }
    #[test]
    fn attention_callbacks_compare_every_entry_field_not_only_stable_id() {
        let mut panel = attention_panel();
        let item = panel.attention_rows().remove(0);
        assert_eq!(item.kind.label(), "Inspect current delivery");
        assert!(panel.accepts_attention(&item));
        for field in 0..12 {
            let mut changed = item.clone();
            match field {
                0 => changed.id.push('x'),
                1 => changed.mission_id.push('x'),
                2 => changed.revision += 1,
                3 => changed.mission_title.push('x'),
                4 => {
                    changed.repository =
                        Some(panel.host.missions[0].repositories[0].identity.clone())
                }
                5 => changed.candidate = Some("b".repeat(40)),
                6 => {
                    changed.kind =
                        jterm_core::agent_workspace::WorkspaceMissionAttentionKind::ReviewEvidence
                }
                7 => changed.check_name = Some("validation".into()),
                8 => changed.environment = Some("different environment".into()),
                9 => changed.receipt_id = Some("another receipt".into()),
                10 => changed.task = receipt_fixture().0.task,
                _ => changed.detail.push('x'),
            }
            assert!(!panel.accepts_attention(&changed), "changed field {field}");
            panel.open_attention(&changed);
            assert!(panel.attention.is_none());
            assert!(!panel.open);
            assert!(!panel.evidence.report_loading);
        }
        panel.host.missions[0].revision += 1;
        let revised = panel.attention_rows().remove(0);
        assert_eq!(item.id, revised.id);
        assert!(!panel.accepts_attention(&item));
        assert!(panel.accepts_attention(&revised));
    }
    #[test]
    fn attention_callbacks_reject_busy_and_unsaved_contracts() {
        let mut panel = attention_panel();
        let item = panel.attention_rows().remove(0);
        panel.host.busy = true;
        panel.open_attention(&item);
        assert!(panel.attention.is_none());
        panel.host.busy = false;
        panel.drafts.get_mut(&item.mission_id).unwrap().edited.goal = "Unsaved acceptance".into();
        panel.open_attention(&item);
        assert!(panel.attention.is_none());
        assert!(!panel.evidence.report_loading);
        assert!(!panel.host.busy);
    }
    #[test]
    fn attention_without_task_opens_reason_without_requesting_a_result() {
        let mut panel = attention_panel();
        let item = panel.attention_rows().remove(0);
        panel.open_attention(&item);
        assert!(panel.open);
        assert_eq!(panel.selected_attention(), Some(&item));
        assert!(!panel.evidence.report_loading);
        assert!(!panel.host.busy);
        panel.open_attention_result(&item, true);
        assert!(!panel.attention_changes);
    }
    #[test]
    fn changed_reason_or_selection_retires_cached_receipt() {
        for change in 0..3 {
            let mut panel = attention_panel();
            let item = panel.attention_rows().remove(0);
            panel.open_attention(&item);
            panel.evidence.report = Some(receipt_fixture().1);
            match change {
                0 => panel.host.missions[0].title.push_str(" updated"),
                1 => panel.host.selected = Some(WorkspaceMission::new("Another", "goal")),
                _ => panel
                    .drafts
                    .get_mut(&item.mission_id)
                    .unwrap()
                    .edited
                    .goal
                    .push_str(" edited"),
            }
            panel.reconcile_attention();
            assert!(panel.attention.is_none());
            assert!(panel.evidence.report.is_none());
        }
    }
    #[test]
    fn receipt_navigation_rejects_newer_run_event_source_and_task() {
        let (item, report) = receipt_fixture();
        assert!(exact_receipt(&item, Some(&report)).is_some());
        // Stale validation can still be inspected; it is never a current pass.
        assert!(!report.validation_current);
        assert!(exact_receipt(&item, None).is_none());
        for change in 0..4 {
            let mut changed = report.clone();
            match change {
                0 => changed.run_id = "newer run".into(),
                1 => changed.source_sequence += 1,
                2 => changed.conversation.0 = "another source".into(),
                _ => changed.task_id = jterm_core::agent_task::TaskId::new(),
            }
            assert!(exact_receipt(&item, Some(&changed)).is_none());
        }
        let mut stale = item.clone();
        stale.task.as_mut().unwrap().source_sequence = 10;
        assert!(exact_receipt(&stale, Some(&report)).is_none());
        let mut panel = attention_panel();
        panel.attention = Some(item.clone());
        panel.evidence.report = Some(report);
        panel.open_attention_result(&item, true);
        assert!(
            !panel.attention_changes,
            "an exact report cannot authorize a stale queue callback"
        );
        assert!(panel.exact_result(&item).is_none());
    }
    #[test]
    fn missing_or_ambiguous_task_and_missing_receipt_have_no_result_shortcut() {
        let (mut item, report) = receipt_fixture();
        // Shared core omits the task for both missing and ambiguous links.
        let linked = item.task.take();
        assert!(exact_receipt(&item, Some(&report)).is_none());
        item.task = linked;
        item.receipt_id = None;
        assert!(exact_receipt(&item, Some(&report)).is_none());
    }
    #[test]
    fn projected_missing_and_ambiguous_links_keep_reason_without_result_shortcut() {
        use jterm_core::agent_workspace::{
            WorkspaceMissionCheckReport, WorkspaceMissionCheckStatus,
            WorkspaceMissionLocalEvidence, WorkspaceMissionRepository,
        };
        let (item, result) = receipt_fixture();
        let task = item.task.unwrap();
        let mut mission = WorkspaceMission::new("One repository", "Validate exact run");
        let mut repository =
            WorkspaceMissionRepository::new("beamiter/jterm_core", "/fixture/core").unwrap();
        repository.baseline = "a".repeat(40);
        repository.candidate = Some("b".repeat(40));
        repository.tasks.push(task.clone());
        mission.repositories.push(repository);
        let mut report = mission.project(&[]);
        let repository = &mut report.repositories[0];
        repository.observed = Some(WorkspaceMissionLocalEvidence {
            baseline: "a".repeat(40),
            head: "b".repeat(40),
            fingerprint: "fixture".into(),
            modified: false,
            dirty: false,
            manifest_core_pin: None,
            lock_core_pin: None,
            observed_at_ms: 10,
        });
        repository.committed = true;
        repository.pushed = true;
        repository.integrated = true;
        repository.pins_current = true;
        repository.errors.clear();
        repository.checks = vec![WorkspaceMissionCheckReport {
            check_name: "validation".into(),
            environment: mission.required_environment.clone(),
            status: WorkspaceMissionCheckStatus::Failed,
            receipt_id: Some("workspace:saved:run:9".into()),
            detail: Some("Exact failure".into()),
        }];
        let linked = mission.attention_items(Some(&report)).remove(0);
        assert_eq!(linked.task.as_ref(), Some(&task));
        assert!(exact_receipt(&linked, Some(&result)).is_some());
        for ambiguous in [false, true] {
            let mut changed = mission.clone();
            if ambiguous {
                let mut other_source = task.clone();
                other_source.conversation.0 =
                    other_source.conversation.0.replace("namespace", "other");
                changed.repositories[0].tasks.push(other_source);
            } else {
                changed.repositories[0].tasks.clear();
            }
            let unlinked = changed.attention_items(Some(&report)).remove(0);
            assert_eq!(unlinked.kind, linked.kind);
            assert_eq!(unlinked.receipt_id, linked.receipt_id);
            assert!(unlinked.task.is_none());
            assert!(exact_receipt(&unlinked, Some(&result)).is_none());
        }
    }
    #[test]
    fn receipt_log_and_capture_render_narrow_and_wide_without_host_operations() {
        let (_, mut report) = receipt_fixture();
        report.current = Some(jterm_core::agent_task::worktree::WorktreeReviewSnapshot {
            head: "b".repeat(40),
            branch: "fixture".into(),
            base_commit: "a".repeat(40),
            fingerprint: "separate-current-capture".into(),
            status: "M file".into(),
            diff: "+ current worktree, not validation input".into(),
            diff_truncated: true,
            status_truncated: false,
            untracked_files: 0,
            captured_at_ms: 100,
            attribution: "fixture".into(),
        });
        for width in [320.0, 960.0] {
            for changes in [false, true] {
                let ctx = Context::default();
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 600.0),
                        )),
                        ..Default::default()
                    },
                    |ui| draw_receipt(ui, &report, changes),
                );
                output.drop_without_applying_deltas();
            }
        }
    }
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
        let rows = panel.attention_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mission_id, mission.id);
        assert_eq!(rows[0].mission_title, mission.title);
        assert_eq!(rows[0].kind.label(), "Complete the acceptance plan");
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
            panel.sync_draft();
            let attention = panel.attention_rows().remove(0);
            panel.open_attention(&attention);
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
