//! Selected-conversation inspection. Reading evidence never resumes execution.
use crate::app::state::{ReferenceInspectorTab, TerminalApp};
use crate::app::tasks::TaskSidebarAction;
use crate::workspace_context::ContextField;
use jterm_core::agent_workspace::{
    ConversationKind, MaterialKind, WorkspaceMaterial, WorkspaceOverview, WorkspaceRow,
};

fn field(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.weak(label);
    ui.label(crate::review_text::visible_bounded(value, 4096));
    ui.add_space(6.0);
}

impl TerminalApp {
    pub(crate) fn workspace_inspector_active(&self) -> bool {
        self.workspace_has_conversation() || self.agent_sessions.tools.selected_hit.is_some()
    }

    pub(crate) fn render_workspace_inspector(
        &mut self,
        ui: &mut egui::Ui,
        tab: ReferenceInspectorTab,
    ) {
        self.agent_sessions.review.start();
        let row = self.workspace_selected_row();
        if let Some(row) = &row {
            if row.kind == ConversationKind::Task
                && row.facts.source_sequence.is_none()
                && !self.agent_sessions.review.task_indexed(&row.id)
            {
                self.agent_sessions.context.deselect();
                self.agent_sessions.review.clear_task();
                ui.heading(crate::review_text::visible_bounded(&row.title, 180));
                ui.spinner();
                ui.label("Registering this task in the shared workspace…");
                if let Some(notice) = &self.agent_sessions.review.notice {
                    ui.label(notice);
                }
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            self.agent_sessions.context.select(row.id.clone());
            if row.kind == ConversationKind::Task {
                self.agent_sessions.review.select_task(row.id.clone());
            } else {
                self.agent_sessions.review.clear_task();
            }
        } else {
            self.agent_sessions.context.deselect();
            self.agent_sessions.review.clear_task();
        }
        if tab == ReferenceInspectorTab::Context && self.agent_sessions.tools.selected_hit.is_some()
        {
            self.render_workspace_evidence(ui);
            return;
        }
        let Some(row) = row else {
            self.render_workspace_overview(ui);
            ui.weak("Select a conversation to inspect its changes, context and run.");
            return;
        };
        let presentation = row.presentation();
        ui.heading(crate::review_text::visible_bounded(&row.title, 180));
        ui.weak(format!("{} · {}", row.provider, presentation.status_label));
        ui.separator();
        let task = self
            .agent_sessions
            .workspace_task
            .and_then(|id| self.task_manager.get(id))
            .cloned();
        match tab {
            ReferenceInspectorTab::Changes => {
                if let Some(task) = task {
                    field(ui, "Task baseline", &task.base_commit);
                    field(ui, "Worktree", &task.worktree_path.to_string_lossy());
                    if ui.button("Review task diff").clicked() {
                        self.task_sidebar.pending_action =
                            Some(TaskSidebarAction::ReviewDiff(task.id));
                    }
                    self.render_workspace_result(ui, &row, true);
                    if let Some(view) = self.agent_runtime.shared_snapshot(task.id) {
                        if view.file_changes.is_empty() {
                            ui.weak("No structured file events in the retained turn.");
                        }
                        for item in &view.file_changes {
                            for change in &item.changes {
                                field(
                                    ui,
                                    &format!("{} · {}", change.kind, item.status),
                                    &change.path,
                                );
                                if !change.path_exact {
                                    ui.weak("Provider path was shortened.");
                                }
                            }
                            if item.changes_truncated {
                                ui.weak("Additional events were omitted by the retention limit.");
                            }
                        }
                    }
                } else {
                    ui.label("This conversation has no structured task diff.");
                    ui.small("CLI output and API text do not establish file ownership. Open a project task to review its recorded baseline.");
                }
            }
            ReferenceInspectorTab::Context => {
                field(ui, "Project / working directory", &row.cwd);
                if let Some(branch) = &row.facts.branch {
                    field(ui, "Branch", branch);
                }
                if let Some(task) = task {
                    let inspector = jterm_core::agent_task::TaskInspector::from_task(&task);
                    if let Some(goal) = inspector.goal {
                        field(ui, "Task goal", &goal);
                    }
                    for item in inspector.fields.iter().filter(|item| {
                        matches!(
                            item.label,
                            "Origin"
                                | "Isolated worktree"
                                | "Review baseline"
                                | "Source session"
                                | "Source execution"
                                | "Validation command"
                        )
                    }) {
                        field(ui, item.label, &item.value);
                    }
                }
                self.render_workspace_context(ui, &row);
            }
            _ => {
                field(ui, "Execution", presentation.runtime_label);
                if let Some(state) = presentation.task_label {
                    field(ui, "Task", state);
                }
                if let Some(state) = presentation.validation_label {
                    field(ui, "Validation", state);
                }
                if let Some(detail) = presentation.detail {
                    field(ui, "Latest evidence", &detail);
                }
                if let Some(recorded) = row.facts.recorded_status {
                    field(ui, "Last recorded provider event", recorded.label());
                    ui.small("Recorded history is not a live permission request.");
                }
                if let Some(sequence) = row.facts.source_sequence {
                    field(ui, "Host event sequence", &sequence.to_string());
                }
                if let Some(task) = task {
                    if let Some(view) = self.agent_runtime.shared_snapshot(task.id) {
                        field(ui, "Provider session", view.phase.label());
                        if let Some(error) = &view.last_error {
                            field(ui, "Provider error", error);
                        }
                        field(
                            ui,
                            "Reported usage",
                            &view
                                .usage
                                .as_ref()
                                .map(|usage| usage.summary())
                                .unwrap_or_else(|| "Unavailable from this provider adapter".into()),
                        );
                        if !view.pending_approvals.is_empty() {
                            ui.label(format!(
                                "{} pending request(s)",
                                view.pending_approvals.len()
                            ));
                            if ui.button("View current request").clicked() {
                                self.task_sidebar.open_workspace_task(task.id);
                                self.task_sidebar.attention_navigation = view
                                    .pending_approvals
                                    .first()
                                    .map(|approval| (task.id, approval.id));
                                self.reference_inspector.narrow_drawer = false;
                            }
                        }
                    } else {
                        ui.weak("Provider usage is unavailable.");
                    }
                    self.render_workspace_result(ui, &row, false);
                    self.render_workspace_queue(ui, &row);
                }
                if !presentation.capabilities.known {
                    ui.weak("Adapter capabilities have not been reported by the host.");
                } else if presentation.capabilities.terminal {
                    ui.small("The provider TUI owns input. There is no second composer.");
                }
                ui.small("The host owns background work while it is alive. A host crash leaves unconfirmed work for explicit recovery; opening a view never replays it.");
                ui.separator();
                self.render_workspace_overview(ui);
            }
        }
    }

    fn render_workspace_context(&mut self, ui: &mut egui::Ui, row: &WorkspaceRow) {
        let context = &mut self.agent_sessions.context;
        ui.separator();
        ui.heading("Handoff context");
        ui.small("Only the reviewed fields and materials below go into a new handoff draft. Reading history does not select it automatically.");
        if context.loading() {
            ui.spinner();
        }
        let Some(draft) = context
            .draft()
            .cloned()
            .filter(|draft| draft.conversation == row.id)
        else {
            if let Some(error) = &context.notice {
                ui.label(error);
            }
            if ui.button("Load context").clicked() {
                context.reload();
            }
            return;
        };
        ui.weak(format!(
            "Saved revision {}{}",
            draft.revision,
            if context.dirty() {
                " · local changes"
            } else {
                ""
            }
        ));
        ui.push_id(("context-editor", &row.id.0), |ui| {
            for (key, label, mut value) in [
                (ContextField::Goal, "Goal", draft.goal),
                (ContextField::Completed, "Completed", draft.completed),
                (ContextField::Remaining, "Remaining", draft.remaining),
                (ContextField::Constraints, "Constraints", draft.constraints),
            ] {
                ui.weak(label);
                if ui.add(egui::TextEdit::multiline(&mut value).id_salt(key).desired_rows(3).desired_width(f32::INFINITY)).changed() {
                    context.set_field(key, value);
                }
                ui.add_space(ui.text_style_height(&egui::TextStyle::Body));
            }
            for material in &draft.materials {
                ui.push_id(&material.id, |ui| {
                    ui.strong(crate::review_text::visible_bounded(&material.label, 240));
                    if let Some(source) = &material.source { ui.weak(format!("{} · revision {}", source.event_id, source.revision)); }
                    if material.truncated { ui.weak("Selected or edited excerpt; inspect the referenced source for its original text."); }
                    let mut value = material.text.clone();
                    if ui.add(egui::TextEdit::multiline(&mut value).desired_rows(3).desired_width(f32::INFINITY)).changed() {
                        context.set_material(&material.id, value);
                    }
                    if ui.small_button("Remove material").clicked() { context.remove_material(&material.id); }
                });
            }
        });
        if context.conflicted() {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Another window saved a newer context. Your local draft is retained.",
            );
            ui.horizontal_wrapped(|ui| {
                if ui.button("Keep local for review").clicked() {
                    context.keep_local();
                }
                if ui.button("Use saved context").clicked() {
                    context.use_shared();
                }
            });
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("Add note").clicked() {
                context.add_note();
            }
            if ui
                .add_enabled(
                    !context.saving() && !context.loading(),
                    egui::Button::new("Refresh saved"),
                )
                .clicked()
            {
                context.reload();
            }
            let valid = context
                .draft()
                .is_some_and(|draft| draft.validate().is_ok());
            if ui
                .add_enabled(
                    context.dirty()
                        && !context.saving()
                        && !context.loading()
                        && !context.conflicted()
                        && valid,
                    egui::Button::new("Save context"),
                )
                .clicked()
            {
                context.save();
            }
        });
        if let Some(error) = &context.notice {
            ui.label(error);
        }
        let ready = !context.dirty()
            && !context.conflicted()
            && !context.loading()
            && !context.saving()
            && !context.building();
        ui.weak("Preview handoff to");
        ui.horizontal_wrapped(|ui| {
            for (provider, label) in [
                ("codex", "Codex"),
                ("claude", "Claude"),
                ("kimi", "Kimi"),
                ("api", "API chat"),
            ] {
                if ui.add_enabled(ready, egui::Button::new(label)).clicked() {
                    context.build_handoff(provider);
                }
            }
        });
        let packet = context.packet.clone().filter(|_| context.packet_current());
        let mut create = None;
        if let Some(packet) = packet {
            ui.separator();
            ui.strong(format!(
                "New {} draft · {} selected materials",
                packet.target_provider, packet.materials_count
            ));
            ui.label(crate::review_text::visible_bounded(&packet.body, 16 * 1024));
            ui.small("This creates a new conversation. The source provider session is not resumed and nothing is sent until you submit the draft.");
            if ui
                .button(if packet.target_provider == "api" {
                    "Create API draft"
                } else {
                    "Create task draft"
                })
                .clicked()
            {
                create = Some(packet);
            }
        }
        if let Some(packet) = create {
            let outcome = if packet.target_provider == "api" {
                self.ai_chat_panel.open_workspace_draft(
                    &self.config,
                    packet.title.clone(),
                    packet.body.clone(),
                );
                self.agent_sessions.workspace_task = None;
                self.reference_navigation_entered_canvas(ui.ctx());
                Ok(())
            } else {
                self.open_task_handoff(&packet)
            };
            self.agent_sessions.context.notice = Some(match outcome {
                Ok(()) => {
                    "New handoff draft prepared. Review it before creating or sending.".into()
                }
                Err(error) => error,
            });
        }
    }

    fn render_workspace_result(&mut self, ui: &mut egui::Ui, row: &WorkspaceRow, changes: bool) {
        ui.separator();
        ui.strong("Captured result");
        if ui
            .add_enabled(
                !self.agent_sessions.review.report_loading,
                egui::Button::new("Refresh result evidence"),
            )
            .clicked()
        {
            self.agent_sessions.review.refresh_result();
        }
        if self.agent_sessions.review.report_loading {
            ui.spinner();
        }
        if let Some(notice) = &self.agent_sessions.review.notice {
            ui.label(notice);
        }
        let Some(report) = self
            .agent_sessions
            .review
            .report
            .clone()
            .filter(|report| report.conversation == row.id)
        else {
            ui.weak("No host result capture is available yet.");
            return;
        };
        ui.weak(format!(
            "Run {} · captured event {}",
            report.run_id, report.source_sequence
        ));
        if row
            .facts
            .source_sequence
            .is_some_and(|current| current != report.source_sequence)
        {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Live and captured event versions differ. Refresh before relying on this capture.",
            );
        }
        if let Some(error) = &report.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        field(ui, "Validation evidence", &report.validation_status);
        if !report.validation_current {
            ui.weak("Validation is unbound or stale for these current files; a previous pass is not a current pass.");
        }
        if let Some(validation) = &report.validation {
            field(ui, "Validation command", &validation.command);
            field(ui, "Validation directory", &validation.cwd);
            field(ui, "Validated fingerprint", &validation.fingerprint);
        }
        if changes {
            if let Some(baseline) = &report.baseline {
                field(ui, "Captured baseline fingerprint", &baseline.fingerprint);
            }
            if let Some(current) = &report.current {
                field(ui, "Captured HEAD", &current.head);
                field(ui, "Current fingerprint", &current.fingerprint);
                ui.small(&current.attribution);
                ui.weak(format!(
                    "{} untracked file(s) · captured {} ms",
                    current.untracked_files, current.captured_at_ms
                ));
                ui.label(crate::review_text::visible_bounded(
                    &current.diff,
                    64 * 1024,
                ));
                if current.diff.len() > 64 * 1024 {
                    ui.weak(
                        "Preview shortened to 64 KiB. Use Review task diff for the retained diff.",
                    );
                }
                if ui
                    .add_enabled(
                        self.agent_sessions.context.selected() == Some(&row.id)
                            && self.agent_sessions.context.draft().is_some(),
                        egui::Button::new("Add captured diff to context"),
                    )
                    .clicked()
                {
                    self.agent_sessions.context.add_material(WorkspaceMaterial {
                        id: uuid::Uuid::new_v4().to_string(),
                        label: format!(
                            "Diff · run {} · event {}",
                            report.run_id, report.source_sequence
                        ),
                        kind: MaterialKind::Diff,
                        text: format!(
                            "Captured HEAD: {}\nFingerprint: {}\n{}\n\n{}",
                            current.head, current.fingerprint, current.attribution, current.diff
                        ),
                        source: None,
                        truncated: false,
                    });
                }
            }
        } else if !report.validation_output.is_empty() {
            ui.label(crate::review_text::visible_bounded(
                &report.validation_output,
                64 * 1024,
            ));
            if report.validation_output_truncated || report.validation_output.len() > 64 * 1024 {
                ui.weak("Validation log is a retained excerpt.");
            }
            if ui
                .add_enabled(
                    self.agent_sessions.context.selected() == Some(&row.id)
                        && self.agent_sessions.context.draft().is_some(),
                    egui::Button::new("Add validation log to context"),
                )
                .clicked()
            {
                self.agent_sessions.context.add_material(WorkspaceMaterial {
                    id: uuid::Uuid::new_v4().to_string(),
                    label: format!(
                        "Validation · run {} · event {}",
                        report.run_id, report.source_sequence
                    ),
                    kind: MaterialKind::Log,
                    text: report.validation_output.clone(),
                    source: None,
                    truncated: report.validation_output_truncated,
                });
            }
        }
    }

    fn render_workspace_queue(&mut self, ui: &mut egui::Ui, row: &WorkspaceRow) {
        let local: Vec<_> = self
            .workspace_task_rows()
            .into_iter()
            .map(|(_, row)| row)
            .collect();
        let rows = self.agent_sessions.review.combined_rows(&local);
        let review = &mut self.agent_sessions.review;
        ui.separator();
        ui.strong("Task queue");
        ui.small("Dependencies wait for accepted tasks. Ready tasks require an explicit Start from their existing task controls. Execution location: Local.");
        if let Some(projected) = review
            .queue
            .project(&rows)
            .into_iter()
            .find(|item| item.id == row.id)
        {
            field(ui, "Queue state", projected.state.label());
            if let Some(reason) = projected.reason {
                ui.label(reason);
            }
        }
        let intent = review
            .queue
            .entries
            .iter()
            .find(|entry| entry.intent.id == row.id)
            .map(|entry| entry.intent.clone());
        let enabled = !review.queue_loading && !review.queue_saving;
        if let Some(intent) = intent {
            if ui
                .add_enabled(enabled, egui::Button::new("Remove from queue"))
                .clicked()
            {
                review.remove_queue(row.id.clone());
            }
            ui.weak("Wait for accepted dependencies");
            for candidate in rows.iter().filter(|candidate| {
                candidate.kind == ConversationKind::Task
                    && candidate.id != row.id
                    && !candidate.archived
            }) {
                let mut selected = intent.dependencies.contains(&candidate.id);
                if ui
                    .add_enabled(
                        enabled,
                        egui::Checkbox::new(
                            &mut selected,
                            crate::review_text::visible_bounded(&candidate.title, 100),
                        ),
                    )
                    .changed()
                {
                    review.set_dependency(row.id.clone(), candidate.id.clone(), selected);
                }
            }
        } else if ui
            .add_enabled(enabled, egui::Button::new("Add task to queue"))
            .clicked()
        {
            review.set_queue(row.id.clone(), Vec::new());
        }
        if let Some(notice) = &review.notice {
            ui.label(notice);
        }
    }

    fn render_workspace_overview(&mut self, ui: &mut egui::Ui) {
        let local: Vec<_> = self
            .workspace_task_rows()
            .into_iter()
            .map(|(_, row)| row)
            .collect();
        let rows = self.agent_sessions.review.combined_rows(&local);
        let overview =
            WorkspaceOverview::from_rows(&rows, self.agent_sessions.review.catalog_complete);
        ui.strong("Workspace overview");
        ui.label(overview.resource_label());
        ui.weak(format!(
            "{} task(s) · {} accepted · {} unconfirmed · {} need you",
            overview.tasks,
            overview.accepted,
            overview.unconfirmed,
            overview.attention_total()
        ));
        for item in self.agent_sessions.review.queue.project(&rows) {
            ui.label(format!(
                "{} · {}",
                crate::review_text::visible_bounded(&item.title, 100),
                item.state.label()
            ));
            if let Some(reason) = item.reason {
                ui.small(reason);
            }
        }
        if ui.button("Refresh workspace").clicked() {
            self.agent_sessions.review.refresh();
        }
    }

    fn render_workspace_evidence(&mut self, ui: &mut egui::Ui) {
        let tools = &self.agent_sessions.tools;
        let Some(hit) = &tools.selected_hit else {
            return;
        };
        ui.heading("Saved evidence");
        field(ui, "Conversation", &hit.row.title);
        field(ui, "Source", hit.source.label());
        field(ui, "Event", &hit.anchor.event_id);
        field(ui, "Revision", &hit.anchor.revision);
        if tools.document_loading {
            ui.spinner();
        }
        if let Some(error) = &tools.notice {
            ui.label(error);
        }
        if let Some(document) = &tools.document {
            ui.label(crate::review_text::visible_multiline_bounded(
                &document.text,
                256 * 1024,
            ));
            if document.truncated {
                ui.weak("Only the retained, indexed excerpt is available.");
            }
        }
        let document = tools.document.clone();
        ui.separator();
        ui.small("Read-only evidence. Nothing has resumed and this text is not sent to a model.");
        if let Some(target) = self.workspace_selected_row().filter(|row| {
            self.agent_sessions.context.selected() == Some(&row.id)
                && self.agent_sessions.context.draft().is_some()
        }) {
            if let Some(document) = document {
                if ui
                    .button(format!(
                        "Add excerpt to {} context",
                        crate::review_text::visible_bounded(&target.title, 60)
                    ))
                    .clicked()
                {
                    self.agent_sessions.context.add_evidence(&document);
                }
            }
        }
        if ui
            .add_enabled(
                self.workspace_evidence_can_open(),
                egui::Button::new("Open conversation"),
            )
            .clicked()
        {
            self.workspace_open_evidence(ui.ctx());
        }
        if ui.button("Close evidence").clicked() {
            self.agent_sessions.tools.close_document();
        }
    }
}
