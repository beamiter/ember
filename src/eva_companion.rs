//! One window-level shared Eva overlay. No terminal bytes or scrollback writes.
use egui::{FontId, RichText, Ui};
use jterm_core::companion_host::{self as host, atlas, Request, RetirementCapability, SharedHost};
use std::time::Instant;

pub(crate) struct EvaHost {
    runtime: Option<SharedHost>,
    enabled: Option<bool>,
    closing: Option<host::Token>,
    close_requested: bool,
    force_close: bool,
    born: Instant,
    reduced_motion: bool,
    high_contrast: bool,
    quiet: bool,
    night: bool,
    use_global_memory: bool,
    pose_key: String,
    last_frame_at: Instant,
    card: bool,
    skin: atlas::Skin,
    motion: atlas::Motion,
    geometry: Option<host::Rect>,
    focus: Option<bool>,
    input: Option<(bool, bool)>,
    painted_pass: Option<u64>,
    revision: u64,
    connection_generation: u64,
    task: Option<(String, u64, host::TaskStatus)>,
    anchor: Option<(String, String)>,
    context: Option<host::ContextRef>,
    capture_requested: bool,
    selection_review: Option<host::SelectionEvidence>,
    approved_selection: Option<host::SelectionEvidence>,
    dialogue_draft: String,
    dialogue_turn: Option<(String, host::ContextRef)>,
    dialogue_text: String,
    dialogue_status: String,
    dialogue_displayed: bool,
    dialogue_display_pending: Option<(String, u64)>,
    memory_entries: Vec<host::MemoryEntry>,
    memory_managed_scope: Option<host::MemoryScope>,
    memory_scope_review: Option<(host::ContextRef, host::MemoryScope, String, String)>,
    task_scopes: Vec<host::TaskScopeInfo>,
    stored_scopes: Vec<host::MemoryScope>,
    next_stored_scope: Option<String>,
    next_task_scope: Option<String>,
    archive_scope_input: String,
    archive_scope_project: bool,
    memory_review: Option<(host::ContextRef, host::MemoryReview)>,
    memory_key: String,
    memory_value: String,
    memory_query: String,
    memory_global: bool,
    memory_paused: bool,
    memory_next: Option<i64>,
    memory_edit_revision: Option<u64>,
    memory_status: String,
    cache: Option<(String, std::sync::Arc<egui::Galley>)>,
    preview_cache: Option<(String, std::sync::Arc<egui::Galley>)>,
}
impl Default for EvaHost {
    fn default() -> Self {
        Self {
            runtime: None,
            enabled: None,
            closing: None,
            close_requested: false,
            force_close: false,
            born: Instant::now(),
            reduced_motion: false,
            high_contrast: false,
            quiet: false,
            night: false,
            use_global_memory: false,
            pose_key: String::new(),
            last_frame_at: Instant::now(),
            card: false,
            skin: atlas::Skin::Cat,
            motion: atlas::Motion::Calm,
            geometry: None,
            focus: None,
            input: None,
            painted_pass: None,
            revision: 0,
            connection_generation: 0,
            task: None,
            anchor: None,
            context: None,
            capture_requested: false,
            selection_review: None,
            approved_selection: None,
            dialogue_draft: String::new(),
            dialogue_turn: None,
            dialogue_text: String::new(),
            dialogue_status: String::new(),
            dialogue_displayed: false,
            dialogue_display_pending: None,
            memory_entries: vec![],
            memory_managed_scope: None,
            memory_scope_review: None,
            task_scopes: vec![],
            stored_scopes: vec![],
            next_stored_scope: None,
            next_task_scope: None,
            archive_scope_input: String::new(),
            archive_scope_project: false,
            memory_review: None,
            memory_key: String::new(),
            memory_value: String::new(),
            memory_query: String::new(),
            memory_global: false,
            memory_paused: false,
            memory_next: None,
            memory_edit_revision: None,
            memory_status: String::new(),
            cache: None,
            preview_cache: None,
        }
    }
}
impl EvaHost {
    pub fn reduced_motion(&mut self, reduced: bool) {
        self.reduced_motion = reduced;
    }
    pub fn configure(&mut self, enabled: bool) {
        self.enabled
            .get_or_insert(enabled || host::shared_mode_requested());
    }
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or_else(host::shared_mode_requested)
    }
    fn runtime(&mut self, ctx: &egui::Context) -> &mut SharedHost {
        self.runtime.get_or_insert_with(|| {
            let ctx = ctx.clone();
            SharedHost::from_env("ember", RetirementCapability::NativePresented, move || {
                ctx.request_repaint()
            })
        })
    }
    pub fn host_id(&mut self, ctx: &egui::Context) -> String {
        self.runtime(ctx).gate.host_id().into()
    }
    /// Start healthy withdrawal; never treat a timeout or IPC receipt as retirement.
    pub fn can_close_now(&mut self) -> bool {
        if !self.enabled() || self.force_close {
            return true;
        }
        let Some(runtime) = self.runtime.as_mut() else {
            return true;
        };
        if !self.close_requested {
            self.close_requested = true;
            self.closing = runtime.gate.possible_body_token().cloned();
            runtime.send(Request::Withdraw);
            return false;
        }
        if self.closing.is_none() {
            self.closing = runtime.gate.possible_body_token().cloned();
        }
        runtime.gate.withdrawal_accepted()
            && self
                .closing
                .as_ref()
                .is_none_or(|token| runtime.gate.retirement_accepted(token))
    }
    pub fn guard_close(&mut self, ctx: &egui::Context) {
        if !self.enabled() {
            return;
        }
        if self.close_requested {
            if let Some(runtime) = self.runtime.as_mut() {
                runtime.poll();
            }
            if self.can_close_now() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if self
                    .runtime
                    .as_ref()
                    .is_some_and(|r| r.gate.pending_retirement())
                {
                    ctx.request_repaint();
                }
            }
        } else if ctx.input(|i| i.viewport().close_requested()) && !self.can_close_now() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.request_repaint();
        }
    }
    pub fn presented(&mut self, pass: u64) {
        if let Some(r) = &mut self.runtime {
            r.frame_presented(pass);
            if let Some((turn_id, at)) = self.dialogue_display_pending.take() {
                if pass >= at
                    && self
                        .dialogue_turn
                        .as_ref()
                        .is_some_and(|(id, _)| id == &turn_id)
                {
                    r.send(Request::DialogueDisplayed { turn_id });
                    self.dialogue_displayed = true;
                }
            }
        }
    }
    pub fn root(&mut self, ui: &mut Ui, force_close_allowed: bool) {
        ui.ctx()
            .data_mut(|d| d.insert_temp(egui::Id::new("eva-shared-mode-active"), true));
        let pass = ui.ctx().cumulative_pass_nr();
        self.dialogue_display_pending = None;
        let runtime = self.runtime(ui.ctx());
        runtime.begin_frame(pass);
        let changed = runtime.poll();
        if runtime.gate.pending_retirement() {
            ui.ctx().request_repaint();
        }
        if changed {
            let generation = self.runtime.as_ref().unwrap().gate.connection_generation();
            if generation != self.connection_generation {
                self.connection_generation = generation;
                self.geometry = None;
                self.focus = None;
                self.input = None;
                self.task = None;
                self.memory_managed_scope = None;
                self.memory_scope_review = None;
                if let Some(context) = self.context.clone() {
                    self.runtime
                        .as_mut()
                        .unwrap()
                        .send(Request::ContextObserved { context });
                }
            }
            let gate = &self.runtime.as_ref().unwrap().gate;
            self.quiet = gate.quiet();
            self.night = gate.night();
            self.skin = atlas::Skin::from_key(gate.skin()).unwrap_or(atlas::Skin::Cat);
            self.motion = match gate.motion() {
                "full" => atlas::Motion::Full,
                "static" => atlas::Motion::Static,
                _ => atlas::Motion::Calm,
            };
        }
        while let Some(host::Reply::Dialogue {
            turn_id,
            context,
            status,
            reply,
        }) = self.runtime.as_mut().unwrap().take_dialogue()
        {
            if self.dialogue_turn.as_ref() == Some(&(turn_id, context)) {
                self.dialogue_status = status;
                if let Some(reply) = reply {
                    self.dialogue_text = reply;
                    self.dialogue_displayed = false;
                }
            }
        }
        while let Some(host::Reply::ContextAccessReview { evidence }) =
            self.runtime.as_mut().unwrap().take_context_access()
        {
            if self.context.as_ref() == Some(&evidence.context) {
                self.selection_review = Some(evidence);
            }
        }
        while let Some(reply) = self.runtime.as_mut().unwrap().take_memory() {
            match reply {
                host::Reply::MemoryScopes { scopes, next_after } => {
                    self.stored_scopes = scopes;
                    self.next_stored_scope = next_after;
                }
                host::Reply::TaskScopes { scopes, next_after } => {
                    self.task_scopes = scopes;
                    self.next_task_scope = next_after;
                }
                host::Reply::MemoryScopeReview {
                    context,
                    scope,
                    review_id,
                    digest,
                } if self.context.as_ref() == Some(&context) => {
                    self.memory_scope_review = Some((context, scope, review_id, digest));
                }
                host::Reply::MemoryList {
                    context,
                    scope,
                    entries,
                    next_before_id,
                    paused,
                    ..
                } if self.context.as_ref() == Some(&context)
                    && self.current_memory_scope().as_ref() == Some(&scope) =>
                {
                    self.memory_entries = entries;
                    self.memory_next = next_before_id;
                    self.memory_paused = paused;
                }
                host::Reply::MemoryReview { context, review }
                    if self.context.as_ref() == Some(&context) =>
                {
                    self.memory_review = Some((context, review));
                }
                host::Reply::MemoryResult { review_id, status } => {
                    self.memory_status = status;
                    if self
                        .memory_review
                        .as_ref()
                        .is_some_and(|(_, r)| Some(r.id) == review_id)
                    {
                        self.memory_review = None;
                    }
                }
                _ => {}
            }
        }
        let focused = ui.input(|i| i.viewport().focused.unwrap_or(false));
        if self.focus != Some(focused) {
            self.focus = Some(focused);
            self.runtime
                .as_mut()
                .unwrap()
                .send(Request::Focus { focused });
        }
        if ui.input_mut(|i| {
            i.consume_key(egui::Modifiers::ALT | egui::Modifiers::SHIFT, egui::Key::E)
        }) {
            self.card = !self.card;
        }
        if self.close_requested {
            egui::Window::new("Retiring Eva before closing").collapsible(false).show(ui.ctx(),|ui|{
                ui.label("Waiting for coordinator withdrawal and a verified cleared native frame. No timeout is treated as proof.");
                ui.label("Force close may leave global transfer recovery blocked; it will not report retirement success.");
                if ui.add_enabled(force_close_allowed,egui::Button::new("Force close — retirement remains unknown")).clicked(){self.force_close=true;ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);}
            });
        }
        // A text dock only: never another animated avatar or a top Tab strip.
        egui::Area::new("eva-companion-dock".into())
            .anchor(egui::Align2::RIGHT_TOP, [-16.0, 48.0])
            .show(ui.ctx(), |ui| {
                if ui
                    .button("Eva")
                    .on_hover_text("Eva companion card · Alt+Shift+E")
                    .clicked()
                {
                    self.card = !self.card;
                }
            });
        if self.card {
            let old_appearance = (self.skin, self.motion);
            let mut open = true;
            egui::Window::new("Eva companion")
                .id("eva-companion-card".into())
                .open(&mut open)
                .resizable(false)
                .default_width(440.0)
                .vscroll(true)
                .collapsible(false)
                .show(ui.ctx(), |ui| {
                    ui.label(self.runtime.as_ref().unwrap().gate.status());
                    if let Some(error)=self.runtime.as_ref().unwrap().gate.operation_error(){ui.label(format!("Last operation error: {error}"));}
                    ui.label(format!(
                        "Pose: {} · microphone not controlled here",
                        self.runtime.as_ref().unwrap().gate.pose()
                    ));
                    ui.label("Metadata only · terminal text is not shared");
                    if let Some((_, _, status)) = &self.task {
                        ui.label(format!("Accompanying task: {status:?}"));
                    }
                    ui.checkbox(&mut self.high_contrast, "High contrast glyphs");
                    let comfort_before=(self.quiet,self.night);
                    ui.horizontal(|ui|{ui.checkbox(&mut self.quiet,"Quiet hours");ui.checkbox(&mut self.night,"Night skin");});
                    if comfort_before!=(self.quiet,self.night){self.runtime.as_mut().unwrap().send(Request::Comfort{quiet:self.quiet,night:self.night});}
                    if ui.checkbox(&mut self.use_global_memory,"Use global memory for this conversation (explicit opt-in)").changed(){if let Some(context)=self.context.clone(){self.runtime.as_mut().unwrap().send(Request::MemoryUse{context,scopes:if self.use_global_memory{vec![host::MemoryScope::Global]}else{vec![]}});}}
                    ui.horizontal(|ui| {
                        for (label, mode) in [
                            ("Follow me", host::Presence::FollowMe),
                            ("Stay here", host::Presence::StayHere),
                            ("Hide", host::Presence::Hidden),
                        ] {
                            if ui.button(label).clicked() {
                                self.runtime
                                    .as_mut()
                                    .unwrap()
                                    .send(Request::Presence { mode });
                            }
                        }
                        if ui.button("Recall").clicked() {
                            self.runtime.as_mut().unwrap().send(Request::Recall);
                        }
                    });
                    egui::ComboBox::from_label("Skin")
                        .selected_text(self.skin.name())
                        .show_ui(ui, |ui| {
                            for skin in atlas::Skin::ALL {
                                ui.selectable_value(&mut self.skin, skin, skin.name());
                            }
                        });
                    ui.horizontal(|ui| {
                        ui.label("Motion");
                        for (label, motion) in [
                            ("Full", atlas::Motion::Full),
                            ("Calm", atlas::Motion::Calm),
                            ("Static", atlas::Motion::Static),
                        ] {
                            ui.selectable_value(&mut self.motion, motion, label);
                        }
                    });
                    ui.label("Skin preview is static; changing skin never creates a new identity.");
                    // Actual native atlas preview, explicitly Static. Live body is hidden while this card is open.
                    let key=format!("{:?}:{}:{}:{}",self.skin,self.high_contrast,ui.visuals().dark_mode,ui.ctx().pixels_per_point().to_bits());
                    if self.preview_cache.as_ref().is_none_or(|(old,_)|old!=&key){
                        let mut request=atlas::Request::new(self.skin,atlas::Pose::Idle);request.motion=atlas::Motion::Static;
                        if let Some(frame)=atlas::sample(request){self.preview_cache=Some((key,crate::eva_glyphs::shape(ui,&frame,self.skin,self.high_contrast)));}
                    }
                    if let Some((_,galley))=&self.preview_cache{let (_,rect)=ui.allocate_space(galley.size());ui.painter().galley(rect.min,galley.clone(),egui::Color32::WHITE);}
                    ui.collapsing("Exact original character sketch",|ui|{ui.label(RichText::new(self.skin.original_sketch()).monospace());});
                    ui.separator();
                    ui.label("Talk to Eva · independent GUI input, never a shell command");
                    if ui.button("Review selected terminal text (up to 4096 bytes)").clicked(){self.capture_requested=true;}
                    if let Some(evidence)=self.selection_review.clone(){
                        ui.label("Exact selected text to share once with Eva:");ui.label(&evidence.text);
                        ui.horizontal(|ui|{
                            if ui.button("Approve this selection once").clicked(){self.runtime.as_mut().unwrap().send(Request::ContextAccessGranted{context:evidence.context.clone(),consent_id:evidence.consent_id.clone()});self.approved_selection=Some(evidence);self.selection_review=None;}
                            if ui.button("Do not share").clicked(){self.selection_review=None;self.approved_selection=None;}
                        });
                    }
                    if self.approved_selection.is_some(){ui.label("One approved selection will accompany the next message; expires after 60 seconds.");}
                    ui.add(egui::TextEdit::multiline(&mut self.dialogue_draft).desired_rows(2).char_limit(4000));
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!self.dialogue_draft.trim().is_empty(),egui::Button::new("Send to Eva")).clicked() {
                            if let Some(context)=self.context.clone() {
                                let turn_id=uuid::Uuid::new_v4().to_string();
                                self.runtime.as_mut().unwrap().send(Request::ContextObserved{context:context.clone()});
                                self.runtime.as_mut().unwrap().send(Request::DialogueStart{turn_id:turn_id.clone(),context:context.clone(),text:std::mem::take(&mut self.dialogue_draft),approved_selection:self.approved_selection.take().filter(|e|e.context==context)});
                                self.dialogue_turn=Some((turn_id,context));self.dialogue_text.clear();self.dialogue_status="Awaiting coordinator".into();
                            }
                        }
                        if ui.button("Cancel reply (not task)").clicked(){if let Some((turn_id,_))=&self.dialogue_turn{self.runtime.as_mut().unwrap().send(Request::DialogueCancel{turn_id:turn_id.clone()});}}
                    });
                    ui.label(&self.dialogue_status);
                    if !self.dialogue_text.is_empty(){
                        if self.dialogue_turn.as_ref().is_some_and(|(_,c)|self.context.as_ref()!=Some(c)){ui.label("Reply belongs to an earlier pane/context; it is not attached to the current task.");}
                        let shown=ui.label(&self.dialogue_text);
                        if !self.dialogue_displayed && shown.rect.intersects(ui.clip_rect()) {if let Some((turn_id,_))=&self.dialogue_turn {self.dialogue_display_pending=Some((turn_id.clone(),pass));}}
                    }
                    ui.collapsing("Memory · inspect and explicitly review changes",|ui|self.memory_ui(ui));
                });
            if old_appearance != (self.skin, self.motion) {
                self.runtime.as_mut().unwrap().send(Request::Appearance {
                    skin: self.skin.key().into(),
                    motion: match self.motion {
                        atlas::Motion::Full => "full",
                        atlas::Motion::Calm => "calm",
                        atlas::Motion::Static => "static",
                    }
                    .into(),
                });
            }
            if !open {
                self.card = false;
            }
        }
    }
    pub fn take_capture_request(&mut self) -> bool {
        std::mem::take(&mut self.capture_requested)
    }
    pub fn captured_selection(&mut self, text: Option<String>) {
        let Some(mut text) = text else {
            self.dialogue_status = "No eligible local selection; nothing shared".into();
            return;
        };
        if text.len() > 4096 {
            let mut end = 4096;
            while !text.is_char_boundary(end) {
                end -= 1
            }
            text.truncate(end);
        }
        if text.is_empty() {
            return;
        }
        if let Some(context) = self.context.clone() {
            self.runtime
                .as_mut()
                .unwrap()
                .send(Request::ContextAccessRequested {
                    context,
                    text,
                    source_event_id: format!("selection-{}", uuid::Uuid::new_v4()),
                    captured_at_ms: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                });
        }
    }
    pub fn observe_context(
        &mut self,
        ctx: &egui::Context,
        pane_id: String,
        task_id: Option<String>,
        revision: u64,
    ) {
        let context = host::ContextRef {
            host_id: self.runtime(ctx).gate.host_id().into(),
            pane_id,
            task_id,
            project_id: None,
            revision: revision.max(1),
        };
        if self.context.as_ref() != Some(&context) {
            self.context = Some(context.clone());
            self.use_global_memory = false;
            self.memory_review = None;
            self.memory_managed_scope = None;
            self.memory_scope_review = None;
            self.selection_review = None;
            self.approved_selection = None;
            self.memory_entries.clear();
            self.runtime(ctx).send(Request::ContextObserved { context });
        }
    }
    fn current_memory_scope(&self) -> Option<host::MemoryScope> {
        self.memory_managed_scope.clone().or_else(|| {
            if self.memory_global {
                Some(host::MemoryScope::Global)
            } else {
                self.context
                    .as_ref()
                    .and_then(|c| c.task_id.clone())
                    .map(host::MemoryScope::Task)
            }
        })
    }
    fn memory_ui(&mut self, ui: &mut Ui) {
        let Some(context) = self.context.clone() else {
            ui.label("No current context");
            return;
        };
        ui.collapsing("Manage archived task/project memory (never model retrieval)",|ui|{
            ui.horizontal(|ui|{
                if ui.button("Discover task scopes").clicked(){self.runtime.as_mut().unwrap().send(Request::TaskScopes{after:None});}
                if ui.add_enabled(self.next_task_scope.is_some(),egui::Button::new("More scopes")).clicked(){self.runtime.as_mut().unwrap().send(Request::TaskScopes{after:self.next_task_scope.clone()});}
            });
            ui.horizontal(|ui|{
                if ui.button("Discover stored memory scopes").clicked(){self.runtime.as_mut().unwrap().send(Request::MemoryScopes{after:None});}
                if ui.add_enabled(self.next_stored_scope.is_some(),egui::Button::new("More stored scopes")).clicked(){self.runtime.as_mut().unwrap().send(Request::MemoryScopes{after:self.next_stored_scope.clone()});}
            });
            for scope in self.stored_scopes.clone(){
                ui.horizontal(|ui|{
                    ui.label(format!("Stored {scope:?}"));
                    if ui.button("Review stored scope access").clicked(){self.runtime.as_mut().unwrap().send(Request::MemoryScopeReviewRequested{context:context.clone(),scope});}
                });
            }
            for item in self.task_scopes.clone(){
                ui.label(format!("Task {} · owner {}",item.task_id,item.owner_host_id));
                ui.horizontal(|ui|{
                    if ui.button("Review task access").clicked(){self.runtime.as_mut().unwrap().send(Request::MemoryScopeReviewRequested{context:context.clone(),scope:host::MemoryScope::Task(item.task_id.clone())});}
                    if let Some(project)=&item.project_id {if ui.button("Review project access").clicked(){self.runtime.as_mut().unwrap().send(Request::MemoryScopeReviewRequested{context:context.clone(),scope:host::MemoryScope::Project(project.clone())});}}
                });
            }
            ui.horizontal(|ui|{ui.label("Exact archived scope ID");ui.add(egui::TextEdit::singleline(&mut self.archive_scope_input).char_limit(128));ui.checkbox(&mut self.archive_scope_project,"Project (otherwise task)");});
            if ui.button("Review this archived scope").clicked(){let id=self.archive_scope_input.trim().to_owned();let scope=if self.archive_scope_project{host::MemoryScope::Project(id)}else{host::MemoryScope::Task(id)};self.runtime.as_mut().unwrap().send(Request::MemoryScopeReviewRequested{context:context.clone(),scope});}
            if let Some((origin,scope,review_id,digest))=self.memory_scope_review.clone(){
                ui.label(format!("Management-only access: {scope:?}"));ui.small(format!("Digest {digest}"));
                ui.label("Allows inspection/edit/deletion only. Does not adopt the task or allow model retrieval.");
                if ui.add_enabled(origin==context,egui::Button::new("Approve exact management scope")).clicked(){
                    self.runtime.as_mut().unwrap().send(Request::MemoryScopeApprove{context:origin.clone(),review_id,digest});
                    self.memory_managed_scope=Some(scope.clone());self.memory_scope_review=None;self.memory_entries.clear();self.memory_review=None;self.memory_edit_revision=None;
                    self.runtime.as_mut().unwrap().send(Request::MemoryList{context:origin,scope,query:String::new(),before_id:None});
                }
                if ui.button("Dismiss scope review").clicked(){self.memory_scope_review=None;}
            }
        });
        if let Some(scope) = self.memory_managed_scope.clone() {
            ui.label(format!("Managing archived scope: {scope:?}"));
            if ui.button("Return to current task/global scope").clicked() {
                self.memory_managed_scope = None;
                self.memory_entries.clear();
                self.memory_review = None;
                self.memory_edit_revision = None;
            }
        }
        if ui
            .checkbox(
                &mut self.memory_global,
                "Global scope (shared across projects)",
            )
            .changed()
        {
            self.memory_entries.clear();
            self.memory_review = None;
            self.memory_edit_revision = None;
        }
        let scope = self.current_memory_scope();
        ui.label(if self.memory_managed_scope.is_some() {
            "Archived management only: changes below use the exact reviewed scope above, never model retrieval."
        } else if self.memory_global {
            "Global changes require explicit review below."
        } else {
            "Current task only. Choose Global explicitly if no task is active."
        });
        if ui
            .checkbox(&mut self.memory_paused, "Pause memory")
            .changed()
        {
            self.runtime.as_mut().unwrap().send(Request::MemoryPause {
                paused: self.memory_paused,
            });
        }
        ui.horizontal(|ui| {
            ui.label("Search");
            ui.text_edit_singleline(&mut self.memory_query);
            if ui.button("Inspect").clicked() {
                if let Some(scope) = scope.clone() {
                    self.runtime.as_mut().unwrap().send(Request::MemoryList {
                        context: context.clone(),
                        scope,
                        query: self.memory_query.clone(),
                        before_id: None,
                    });
                }
            }
            if ui
                .add_enabled(self.memory_next.is_some(), egui::Button::new("Next"))
                .clicked()
            {
                if let Some(scope) = scope.clone() {
                    self.runtime.as_mut().unwrap().send(Request::MemoryList {
                        context: context.clone(),
                        scope,
                        query: self.memory_query.clone(),
                        before_id: self.memory_next,
                    });
                }
            }
        });
        for entry in self.memory_entries.clone() {
            ui.label(format!("{} = {}", entry.key, entry.value));
            ui.small(format!(
                "Scope {:?} · revision {} · source {} · consent {}",
                entry.scope, entry.revision, entry.source_event_id, entry.consent_status
            ));
            ui.horizontal(|ui| {
                if ui.button("Edit draft").clicked() {
                    self.memory_key = entry.key.clone();
                    self.memory_value = entry.value.clone();
                    self.memory_edit_revision = Some(entry.revision);
                }
                if ui.button("Review deletion").clicked() {
                    self.runtime.as_mut().unwrap().send(Request::MemoryPropose {
                        context: context.clone(),
                        change: host::MemoryChange::Delete {
                            id: entry.id,
                            expected_revision: entry.revision,
                        },
                    });
                }
            });
        }
        ui.horizontal(|ui| {
            ui.label("Key");
            ui.text_edit_singleline(&mut self.memory_key);
        });
        ui.add(
            egui::TextEdit::multiline(&mut self.memory_value)
                .desired_rows(2)
                .char_limit(4000),
        );
        ui.horizontal(|ui| {
            if ui.button("Review save").clicked() {
                if let Some(scope) = scope.clone() {
                    self.runtime.as_mut().unwrap().send(Request::MemoryPropose {
                        context: context.clone(),
                        change: host::MemoryChange::Put {
                            scope,
                            key: self.memory_key.clone(),
                            value: self.memory_value.clone(),
                            source_event_id: format!("manual-{}", uuid::Uuid::new_v4()),
                            expected_revision: self.memory_edit_revision,
                            expires_at: None,
                        },
                    });
                }
            }
            if ui.button("Review clear scope").clicked() {
                if let Some(scope) = scope {
                    self.runtime.as_mut().unwrap().send(Request::MemoryPropose {
                        context: context.clone(),
                        change: host::MemoryChange::Clear { scope },
                    });
                }
            }
        });
        if let Some((origin, review)) = self.memory_review.clone() {
            ui.separator();
            ui.label("Review exact change (not saved yet)");
            ui.label(format!("{:?}", review.change));
            ui.small(format!("Digest {}", review.digest));
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        origin == context,
                        egui::Button::new("Approve this exact change"),
                    )
                    .clicked()
                {
                    self.runtime.as_mut().unwrap().send(Request::MemoryApprove {
                        context: origin,
                        review_id: review.id,
                        digest: review.digest,
                    });
                    self.memory_review = None;
                }
                if ui.button("Cancel change").clicked() {
                    self.runtime.as_mut().unwrap().send(Request::MemoryCancel {
                        review_id: review.id,
                    });
                    self.memory_review = None;
                }
            });
        }
        ui.label(&self.memory_status);
    }
    pub fn observe_task(
        &mut self,
        ctx: &egui::Context,
        task_id: String,
        revision: u64,
        status: host::TaskStatus,
    ) {
        let fact = (task_id, revision, status);
        if self.task.as_ref() != Some(&fact) {
            self.task = Some(fact.clone());
            self.runtime(ctx).send(Request::TaskObservation {
                task_id: fact.0,
                context_revision: fact.1,
                status: fact.2,
            });
        }
    }
    pub fn draw(
        &mut self,
        ui: &mut Ui,
        surface: Option<(String, String, egui::Rect)>,
        interactive: bool,
    ) {
        let anchor = surface
            .as_ref()
            .map(|(session, block, _)| (session.clone(), block.clone()));
        if self.anchor != anchor {
            self.anchor = anchor;
            self.geometry = None;
            self.cache = None;
        }
        let safe = surface.map(|(_, _, rect)| rect);
        let pass = ui.ctx().cumulative_pass_nr();
        if self.painted_pass == Some(pass) {
            return;
        }
        let selecting = ui.input(|i| i.pointer.any_down());
        let active = self.card
            || !interactive
            || ui.input(|i| {
                !i.keys_down.is_empty()
                    || i.events.iter().any(|e| {
                        matches!(
                            e,
                            egui::Event::Text(_) | egui::Event::Paste(_) | egui::Event::Ime(_)
                        )
                    })
            });
        if self.input != Some((active, selecting)) {
            self.input = Some((active, selecting));
            self.runtime(ui.ctx())
                .send(Request::Input { active, selecting });
        }
        // Never obscure composer, selection, IME, approvals, or uncertain space.
        let safe = crate::eva_glyphs::protected_region(safe, active, selecting);
        let geometry = safe.map(|r| host::Rect {
            x: r.min.x as f64,
            y: r.min.y as f64,
            width: r.width() as f64,
            height: r.height() as f64,
        });
        if geometry != self.geometry {
            self.geometry = geometry;
            self.revision += 1;
            let viewport = ui.clip_rect();
            let revision = self.revision;
            self.runtime(ui.ctx()).send(Request::Geometry {
                revision,
                viewport: host::Rect {
                    x: viewport.min.x as f64,
                    y: viewport.min.y as f64,
                    width: viewport.width() as f64,
                    height: viewport.height() as f64,
                },
                candidates: geometry.into_iter().collect(),
                exclusions: vec![],
            });
        }
        let Some(safe) = safe else { return };
        let runtime = self.runtime(ui.ctx());
        let Some(token) = runtime.gate.token().cloned() else {
            return;
        };
        let body_id = egui::Id::new((
            "eva-companion-body",
            token.boot_id,
            token.incarnation,
            token.epoch,
        ));
        let pose = atlas::Pose::from_key(runtime.gate.pose()).unwrap_or(atlas::Pose::Idle);
        let now = Instant::now();
        if self.pose_key != pose.key()
            || now.duration_since(self.last_frame_at) > std::time::Duration::from_secs(30)
        {
            self.born = now;
            self.pose_key = pose.key().into();
        }
        self.last_frame_at = now;
        let mut request = atlas::Request::new(self.skin, pose);
        request.motion = self.motion;
        request.reduced_motion = self.reduced_motion;
        request.elapsed_ms = self.born.elapsed().as_millis() as u64;
        let Some(frame) = atlas::sample(request) else {
            return;
        };
        let width = frame.width as f32 * 6.6;
        let height = frame.height as f32 * 11.0;
        if safe.width() < width || safe.height() < height {
            // Compact is a non-animated text alternative, not a second body.
            if safe.width() >= 100.0 && safe.height() >= 18.0 {
                ui.painter().text(
                    safe.right_bottom(),
                    egui::Align2::RIGHT_BOTTOM,
                    format!("Eva · {}", pose.key()),
                    FontId::monospace(11.0),
                    ui.visuals().text_color(),
                );
                self.runtime(ui.ctx()).body_submitted(pass);
                self.painted_pass = Some(pass);
            }
            return;
        }
        let key = format!(
            "{:?}:{:?}:{:?}:{}:{}:{}:{}:{:?}",
            self.skin,
            pose,
            self.motion,
            frame.frame_index,
            self.high_contrast,
            ui.visuals().dark_mode,
            ui.ctx().pixels_per_point().to_bits(),
            ui.visuals().text_color().to_array()
        );
        if self.cache.as_ref().is_none_or(|(k, _)| k != &key) {
            let galley = crate::eva_glyphs::shape(ui, &frame, self.skin, self.high_contrast);
            self.cache = Some((key, galley));
        }
        let galley = self.cache.as_ref().unwrap().1.clone();
        let Some(rect) = crate::eva_glyphs::paint(ui, safe, galley) else {
            return;
        };
        let response = ui
            .interact(rect, body_id, egui::Sense::click())
            .on_hover_text(&frame.accessible_label);
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &frame.accessible_label)
        });
        if response.clicked() {
            self.card = true;
        }
        self.runtime(ui.ctx()).body_submitted(pass);
        self.painted_pass = Some(pass);
        if let Some(ms) = frame.next_frame_in_ms {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(ms));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_mode_is_latched_until_window_restart() {
        let mut host = EvaHost::default();
        host.configure(true);
        host.configure(false);
        assert!(host.enabled());
        assert!(host.runtime.is_none());
    }
    #[test]
    fn settings_roundtrip_is_opt_in() {
        let default = crate::config::Config::default();
        assert!(!default.eva_companion_enabled);
        let mut enabled = default;
        enabled.eva_companion_enabled = true;
        let serialized = toml::to_string(&enabled).unwrap();
        let decoded: crate::config::Config = toml::from_str(&serialized).unwrap();
        assert!(decoded.eva_companion_enabled);
    }
    #[test]
    fn native_hook_is_after_successful_buffer_submission() {
        let glow = include_str!("../vendor/eframe/src/native/glow_integration.rs");
        assert!(
            glow.find("gl_surface.swap_buffers(context)?;").unwrap()
                < glow.find("app.on_native_frame_presented").unwrap()
        );
        let wgpu = include_str!("../vendor/egui-wgpu/src/winit.rs");
        assert!(
            wgpu.find("self.last_presented_viewport = None;").unwrap()
                < wgpu
                    .find("render_state.queue.present(output_frame);")
                    .unwrap()
        );
        assert!(
            wgpu.find("render_state.queue.present(output_frame);")
                .unwrap()
                < wgpu
                    .find("self.last_presented_viewport = Some(viewport_id);")
                    .unwrap()
        );
        let integration = include_str!("../vendor/eframe/src/native/wgpu_integration.rs");
        assert!(integration.contains("if painter.last_presented_viewport() == Some(viewport_id)"));
    }
}
