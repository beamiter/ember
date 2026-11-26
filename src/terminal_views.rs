//! View-only lifetime changes. The original actor, PTY and parser remain owned.
use jterm_protocol::terminal::Attachment;

pub(crate) fn placeholder(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    pid: i32,
    ticket: &Attachment,
) -> bool {
    ui.painter()
        .rect_filled(rect, 0.0, egui::Color32::from_rgb(12, 18, 29));
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect.shrink(20.0)), |ui| {
        ui.strong("Terminal view detached");
        ui.label(
            "The original terminal continues running. Input and resize belong to an attached view.",
        );
        ui.small(format!("PTY {pid} · epoch {}", ticket.epoch));
        ui.button("Reattach terminal view").clicked()
    })
    .inner
}

impl crate::TerminalApp {
    pub(crate) fn retire_terminal_view_focus(&self, ctx: &egui::Context) {
        if ctx
            .data_mut(|data| data.remove_temp::<bool>(egui::Id::new("retire-terminal-view-focus")))
            .unwrap_or(false)
        {
            if let Some(id) = ctx.memory(|memory| memory.focused()) {
                ctx.memory_mut(|memory| memory.surrender_focus(id));
            }
        }
    }

    pub(crate) fn change_terminal_view(
        &mut self,
        ctx: &egui::Context,
        ticket: Attachment,
        attach: bool,
    ) {
        let Some(index) = self
            .session_manager
            .sessions()
            .iter()
            .position(|s| s.shell.view_ticket() == ticket)
        else {
            return;
        };
        let id = self.session_manager.sessions()[index]
            .metadata
            .session_id
            .clone();
        if self.vim9_editors.contains_key(&id) {
            self.set_status("Close the Vim9 workbench before detaching its terminal view");
            return;
        }
        if self
            .terminal_mouse_capture
            .as_ref()
            .is_some_and(|capture| capture.session_id == id)
        {
            self.set_status("Release the terminal mouse gesture before detaching");
            return;
        }
        if !self.session_manager.sessions_mut()[index].change_view(&ticket, attach) {
            self.set_status("Terminal input is still draining; try again shortly");
            return;
        }
        if !attach {
            if self
                .pending_paste_confirm
                .as_ref()
                .is_some_and(|pending| pending.session_id == id)
            {
                self.pending_paste_confirm = None;
            }
            if self
                .pending_link_activation
                .as_ref()
                .is_some_and(|pending| pending.session_id == id)
            {
                self.pending_link_activation = None;
            }
            if self
                .workflow_picker
                .as_ref()
                .is_some_and(|picker| picker.target_session_id() == Some(id.as_str()))
            {
                self.workflow_picker = None;
            }
            if self
                .workflow_args
                .as_ref()
                .is_some_and(|args| args.target_session_id() == Some(id.as_str()))
            {
                self.workflow_args = None;
            }
            if self
                .history_picker
                .as_ref()
                .is_some_and(|picker| picker.target_session_id() == Some(id.as_str()))
            {
                self.history_picker = None;
            }
            if self
                .block_review
                .as_ref()
                .is_some_and(|review| review.session_id == id)
            {
                self.block_review = None;
            }
            if self.block_search.session_id.as_deref() == Some(id.as_str()) {
                self.block_search.close();
            }
            self.search_state.close();
            self.keyboard_input_buffer.clear();
            ctx.data_mut(|data| {
                data.insert_temp(egui::Id::new("retire-terminal-view-focus"), true)
            });
            ctx.send_viewport_cmd(egui::ViewportCommand::IMEAllowed(false));
        }
        fn retire_renderer(renderer: &mut crate::ui::TerminalRenderer) {
            renderer.reset_ime_state();
            renderer.cancel_local_selection_capture();
            renderer.chat_input_action = None;
            renderer.cursor_move_input.clear();
            renderer.block_menu_action = None;
            renderer.block_click = None;
        }
        let targets: Vec<_> = self
            .layout()
            .panes()
            .iter()
            .enumerate()
            .filter_map(|(rank, pane)| (pane.session_idx == index).then_some(rank))
            .collect();
        if self.layout().panes().len() == 1 {
            retire_renderer(&mut self.renderer);
        }
        for rank in targets {
            if let Some(renderer) = self.pane_renderers.get_mut(rank) {
                retire_renderer(renderer);
            }
        }
        if attach {
            self.layout_mut().focus_session(index);
            self.sync_active_session_to_focused_pane();
            self.return_focus_to_terminal(ctx);
        }
        let current = self.session_manager.sessions()[index].shell.view_ticket();
        log::debug!(
            "terminal view attached={attach} session={} epoch={} generation={}",
            current.session,
            current.epoch,
            current.generation
        );
        ctx.request_repaint();
    }
}
