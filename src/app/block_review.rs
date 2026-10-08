//! Read-only, bounded command inspection. Identities are terminal-owned sequences:
//! shell-supplied ids may be reused and must never retarget an open review.
use super::state::TerminalApp;
use crate::terminal::{CommandRecord, TerminalState};
use eframe::egui;

#[derive(Clone, Debug)]
pub(crate) struct BlockReview {
    pub session_id: String,
    pub sequences: Vec<u64>,
    pub active: usize,
    armed: bool,
    error: Option<String>,
}

impl BlockReview {
    fn capture(session_id: String, selected: &[String], terminal: &TerminalState) -> Option<Self> {
        let sequences: Vec<_> = terminal
            .command_records()
            .iter()
            .filter(|record| record.complete && selected.contains(&record.id))
            .map(|record| record.sequence)
            .collect();
        if sequences.is_empty() || sequences.len() != selected.len() {
            return None;
        }
        Some(Self {
            session_id,
            sequences,
            active: 0,
            armed: false,
            error: None,
        })
    }

    fn resolve<'a>(&self, terminal: &'a TerminalState) -> Option<Vec<&'a CommandRecord>> {
        self.sequences
            .iter()
            .map(|sequence| {
                terminal
                    .command_records()
                    .iter()
                    .find(|record| record.sequence == *sequence && record.complete)
            })
            .collect()
    }
}

/// Preserve meaningful line breaks but expose terminal controls and bidi text.
/// The output and command panes each retain at most 256 KiB of display text.
fn display_text(text: &str) -> String {
    let mut shown = String::new();
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            shown.push('\n');
        }
        let remaining = (256usize * 1024).saturating_sub(shown.len());
        if remaining == 0 {
            break;
        }
        shown.push_str(&crate::review_text::visible_bounded(line, remaining));
        if shown.len() >= 256 * 1024 {
            break;
        }
    }
    shown
}

#[derive(Default)]
pub(crate) struct ReadingHistory {
    seen: std::collections::HashMap<String, u64>,
}

impl ReadingHistory {
    pub fn retain_sessions<'a>(&mut self, sessions: impl Iterator<Item = &'a str>) {
        let live = sessions.collect::<std::collections::HashSet<_>>();
        self.seen
            .retain(|session, _| live.contains(session.as_str()));
    }

    pub fn update(
        &mut self,
        session_id: &str,
        records: &std::collections::VecDeque<CommandRecord>,
        reading: bool,
    ) -> usize {
        let newest = records
            .iter()
            .filter(|r| r.complete)
            .map(|r| r.sequence)
            .max()
            .unwrap_or(0);
        let seen = self.seen.entry(session_id.to_owned()).or_insert(newest);
        if !reading {
            *seen = newest;
        }
        records
            .iter()
            .filter(|r| {
                r.complete
                    && r.sequence > *seen
                    && (r.command_truncated
                        || r.command.as_ref().is_some_and(|c| !c.trim().is_empty()))
            })
            .count()
    }
}

#[derive(Default)]
struct ReviewView {
    rows: Vec<String>,
    summary: String,
    command: String,
    cwd: String,
    status: String,
    provenance: String,
    output: String,
    notice: String,
    ready: bool,
    insertion_issue: Option<String>,
    unavailable: bool,
}

fn review_view(review: &BlockReview, terminal: &TerminalState, writable: bool) -> ReviewView {
    let Some(records) = review.resolve(terminal) else {
        return ReviewView {
            unavailable: true,
            ..Default::default()
        };
    };
    let record = records[review.active.min(records.len() - 1)];
    let output = record.captured_output.as_ref();
    let notice = match output {
        None => {
            "Output snapshot unavailable or evicted. No output has been reconstructed.".to_owned()
        }
        Some(output) if output.truncated => format!(
            "Bounded snapshot: {} bytes including omission markers; original normalized output: {} bytes. Some output was not retained.",
            output.text.len(),
            output.total_bytes
        ),
        Some(output) => format!("Retained output snapshot · {} bytes", output.text.len()),
    };
    let exact = if record.command_truncated {
        "Command text was truncated; insertion is unavailable."
    } else if record.command_exact {
        "Exact shell-reported command"
    } else {
        "Screen-derived or unavailable command; insertion requires exact shell metadata."
    };
    let insertion_issue = super::commands::review_replay_error(&records);
    ReviewView {
        summary: format!(
            "{} commands · {} failures · {} background blocks",
            records
                .iter()
                .filter(|r| r.command.as_ref().is_some_and(|c| !c.trim().is_empty()))
                .count(),
            records
                .iter()
                .filter(|r| matches!(
                    crate::block_mode::classify_outcome(
                        r.command.as_deref(),
                        r.command_truncated,
                        r.exit_code,
                        r.state,
                        r.complete,
                        false
                    ),
                    crate::block_mode::BlockOutcome::Failed(_)
                ))
                .count(),
            records
                .iter()
                .filter(|r| r.command.as_ref().is_none_or(|c| c.trim().is_empty())
                    && !r.command_truncated)
                .count()
        ),
        rows: records
            .iter()
            .enumerate()
            .map(|(index, r)| {
                format!(
                    "{}. {}",
                    index + 1,
                    crate::review_text::visible_bounded(
                        r.command.as_deref().unwrap_or("Background output"),
                        160
                    )
                )
            })
            .collect(),
        command: display_text(
            record
                .command
                .as_deref()
                .unwrap_or("No command text retained"),
        ),
        cwd: display_text(
            record
                .cwd
                .as_deref()
                .unwrap_or("Working directory not reported"),
        ),
        status: super::rendering::block_workspace_status(record, false).0,
        provenance: format!(
            "{}\n{}\nStable block #{}",
            exact,
            crate::block_mode::lifecycle_detail(
                record.start_mark_seen,
                record.completion_provenance
            ),
            record.sequence
        ),
        output: output
            .map(|output| display_text(&output.text))
            .unwrap_or_default(),
        notice,
        ready: writable
            && !terminal.is_alt_buffer_active()
            && terminal.shell_is_prompt_ready()
            && terminal.prompt_input_is_empty()
            && terminal.is_bracketed_paste_enabled()
            && insertion_issue.is_none(),
        insertion_issue,
        unavailable: false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReviewAction {
    Close,
    Insert,
}

fn draw_review(
    ctx: &egui::Context,
    review: &mut BlockReview,
    view: &ReviewView,
) -> Option<ReviewAction> {
    let mut action = None;
    let displayed_index = review.active;
    let armed = review.armed;
    let size = ctx.content_rect().size();
    let response = egui::Modal::new(egui::Id::new("block-review")).show(ctx, |ui| {
        ui.set_width((size.x - 48.0).clamp(80.0, 760.0));
        ui.heading("Review command blocks");
        ui.weak(format!("{} selected · terminal order · nothing runs here", review.sequences.len()));
        if !view.unavailable { ui.label(&view.summary); }
        ui.separator();
        if view.unavailable {
            ui.colored_label(ui.visuals().warn_fg_color, "A reviewed block is no longer retained. Close and select the available blocks again.");
        } else {
            egui::ScrollArea::vertical().id_salt("review-body").max_height((size.y - 220.0).max(80.0)).show(ui, |ui| {
                if view.rows.len() > 1 {
                    ui.strong("Insertion order");
                    egui::ScrollArea::vertical().id_salt("review-order").max_height(120.0).show_rows(ui, 24.0, view.rows.len(), |ui, range| {
                        for index in range {
                            if ui.add_sized([ui.available_width(), 24.0], egui::Button::new(&view.rows[index]).selected(index == review.active).truncate()).clicked() {
                                review.active = index;
                            }
                        }
                    });
                    ui.separator();
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(displayed_index > 0, egui::Button::new("Previous block")).clicked() { review.active = displayed_index - 1; }
                    if ui.add_enabled(displayed_index + 1 < view.rows.len(), egui::Button::new("Next block")).clicked() { review.active = displayed_index + 1; }
                    ui.strong(format!("{} / {}", displayed_index + 1, view.rows.len()));
                });
                ui.label(&view.status);
                ui.add(egui::Label::new(egui::RichText::new(&view.command).monospace()).selectable(true).wrap());
                ui.add(egui::Label::new(egui::RichText::new(&view.cwd).weak()).selectable(true).wrap());
                ui.collapsing("Command provenance", |ui| { ui.label(&view.provenance); });
                ui.separator();
                ui.strong("Captured output");
                ui.weak(&view.notice);
                ui.small("Display capped at 256 KiB; control and bidi characters are shown escaped.");
                if !view.output.is_empty() {
                    ui.add(egui::Label::new(egui::RichText::new(&view.output).monospace()).selectable(true).wrap());
                }
            });
        }
        ui.separator();
        if let Some(error) = &review.error { ui.colored_label(ui.visuals().warn_fg_color, error); }
        if !view.ready && !view.unavailable { ui.weak(view.insertion_issue.as_deref().unwrap_or("Insertion needs an empty, idle bracketed-paste prompt with no pending input.")); }
        ui.horizontal_wrapped(|ui| {
            if ui.button("Close review").clicked() && armed { action = Some(ReviewAction::Close); }
            if ui.add_enabled(armed && view.ready && !view.unavailable, egui::Button::new("Insert at prompt")).clicked() { action = Some(ReviewAction::Insert); }
        });
        ui.weak("Insertion fills the prompt only. Review it there before pressing Enter. Esc closes review.");
    });
    if armed && response.should_close() {
        action = Some(ReviewAction::Close);
    }
    review.armed = true;
    action
}

impl TerminalApp {
    pub(crate) fn open_block_review(&mut self) {
        let session = self.session_manager.get_active_session_mut();
        let Some(selection) = self
            .block_selection
            .as_ref()
            .filter(|s| s.session_id == session.metadata.session_id)
        else {
            return;
        };
        self.block_review = BlockReview::capture(
            selection.session_id.clone(),
            &selection.selected_ids,
            &session.terminal.lock(),
        );
    }

    pub(crate) fn render_block_review(&mut self, ctx: &egui::Context) {
        let Some(mut review) = self.block_review.take() else {
            return;
        };
        let active = self.session_manager.active_index();
        let Some(session) = self
            .session_manager
            .sessions()
            .get(active)
            .filter(|s| s.metadata.session_id == review.session_id && self.config.block_mode)
        else {
            self.return_focus_to_terminal(ctx);
            return;
        };
        let view = review_view(
            &review,
            &session.terminal.lock(),
            session.purpose != crate::session::SessionPurpose::RetainedCommand
                && session.pending_input.is_empty()
                && !self.direct_input_is_blocked_for_session(&review.session_id),
        );
        match draw_review(ctx, &mut review, &view) {
            Some(ReviewAction::Close) => self.return_focus_to_terminal(ctx),
            Some(ReviewAction::Insert) => {
                // Resolve every sequence again at action time. Never use a stale
                // display snapshot or a shell-controlled id to authorize insertion.
                let ids = review
                    .resolve(&session.terminal.lock())
                    .map(|records| records.iter().map(|r| r.id.clone()).collect::<Vec<_>>());
                if let Some(ids) = ids {
                    let old = self.block_selection.take();
                    self.block_selection =
                        crate::block_mode::BlockSelection::all(review.session_id.clone(), ids);
                    match self.try_reinput_selected_commands() {
                        Ok(_) => {
                            self.set_status(
                                "Reviewed commands inserted at prompt; nothing executed",
                            );
                            self.return_focus_to_terminal(ctx);
                            return;
                        }
                        Err(error) => {
                            self.block_selection = old;
                            review.error =
                                Some(super::commands::selected_replay_error_message(&error));
                        }
                    }
                } else {
                    review.error =
                        Some("A reviewed block was evicted. Nothing was inserted.".into());
                }
                self.block_review = Some(review);
            }
            None => self.block_review = Some(review),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finish(terminal: &mut TerminalState, id: &str, command: &str, exit: i32) {
        let output = if exit == 0 {
            "Build finished successfully\r\n"
        } else {
            "running 42 tests\r\ntest block_layout ... ok\r\ntest narrow_toolbar ... FAILED\r\nExpected controls inside the pane\r\n41 passed; 1 failed\r\n"
        };
        let displayed = command.replace("%20", " ");
        terminal.process_input(format!("\x1b]133;A\x07$ \x1b]133;B;jsh_id={id}\x07{displayed}\r\n\x1b]133;C;jsh_id={id};cmdline_url={command};cwd_url=%2Fhome%2Fdev%2Fember\x07{output}\x1b]133;D;{exit};jsh_id={id}\x07\r\n").as_bytes());
    }

    fn fixture() -> (TerminalState, BlockReview) {
        let mut terminal = TerminalState::new(90, 30);
        finish(&mut terminal, "first", "cargo%20check", 0);
        finish(&mut terminal, "second", "cargo%20test", 101);
        terminal.process_input(b"\x1b[?2004h\x1b]133;A\x07$ \x1b]133;B;jsh_id=prompt\x07");
        let review =
            BlockReview::capture("pane".into(), &["second".into(), "first".into()], &terminal)
                .unwrap();
        (terminal, review)
    }

    #[test]
    fn review_resolves_stable_terminal_order_without_changing_viewport() {
        let (mut terminal, review) = fixture();
        terminal.scroll_offset = 4;
        let view = review_view(&review, &terminal, true);
        assert!(view.rows[0].contains("cargo check"));
        assert!(view.rows[1].contains("cargo test"));
        assert!(view.provenance.contains("Exact shell-reported"));
        assert_eq!(terminal.scroll_offset, 4);
        assert!(view.ready);
        assert!(BlockReview::capture("pane".into(), &["missing".into()], &terminal).is_none());
    }

    #[test]
    fn review_rejects_evicted_identity_even_when_shell_id_is_reused() {
        let (mut terminal, review) = fixture();
        terminal.clear_completed_blocks();
        finish(&mut terminal, "first", "different", 0);
        assert!(review.resolve(&terminal).is_none());
        assert!(review_view(&review, &terminal, true).unavailable);
    }

    #[test]
    fn review_is_read_only_for_dirty_busy_alt_or_retained_prompt() {
        let (mut terminal, review) = fixture();
        assert!(!review_view(&review, &terminal, false).ready);
        terminal.note_user_input(b"x");
        assert!(!review_view(&review, &terminal, true).ready);
        let (mut terminal, review) = fixture();
        terminal.process_input(b"\x1b]133;C;jsh_id=prompt;cmdline_url=sleep\x07");
        assert!(!review_view(&review, &terminal, true).ready);
        let (mut terminal, review) = fixture();
        terminal.process_input(b"\x1b[?1049h");
        assert!(!review_view(&review, &terminal, true).ready);
    }

    #[test]
    fn display_text_preserves_newlines_and_literal_escapes_without_bidi() {
        assert_eq!(
            display_text("line\n\\n literal\u{202e}"),
            "line\n\\n literal\\u{202E}"
        );
        assert!(display_text(&"界".repeat(100_000)).len() <= 256 * 1024);
    }

    #[test]
    fn reading_awareness_only_counts_retained_new_completions_and_never_scrolls() {
        let (mut terminal, _) = fixture();
        let mut state = ReadingHistory::default();
        assert_eq!(state.update("pane", terminal.command_records(), false), 0);
        assert_eq!(state.update("pane", terminal.command_records(), true), 0);
        finish(&mut terminal, "third", "pwd", 0);
        assert_eq!(state.update("pane", terminal.command_records(), true), 1);
        assert_eq!(state.update("pane", terminal.command_records(), true), 1);
        assert_eq!(state.update("pane", terminal.command_records(), false), 0);
        finish(&mut terminal, "fourth", "ls", 0);
        assert_eq!(
            state.update("other-pane", terminal.command_records(), true),
            0
        );
    }

    #[test]
    fn reading_awareness_survives_pane_switch_and_prunes_closed_sessions() {
        let (mut a, _) = fixture();
        let (b, _) = fixture();
        let mut state = ReadingHistory::default();
        state.update("a", a.command_records(), false);
        state.update("b", b.command_records(), false);
        finish(&mut a, "later", "pwd", 0);
        assert_eq!(state.update("a", a.command_records(), true), 1);
        state.retain_sessions(["a"].into_iter());
        assert_eq!(state.seen.len(), 1);
        assert!(state.seen.contains_key("a"));
    }

    #[test]
    fn review_uses_real_replay_validation_for_unsafe_inexact_and_oversized_commands() {
        let (terminal, _) = fixture();
        let mut record = terminal.command_record("first").unwrap().clone();
        record.command = Some("echo \u{202e}unsafe".into());
        assert!(super::super::commands::review_replay_error(&[&record])
            .unwrap()
            .contains("formatting"));
        record.command = Some("pwd".into());
        record.command_exact = false;
        assert!(super::super::commands::review_replay_error(&[&record])
            .unwrap()
            .contains("Exact"));
        record.command_exact = true;
        record.command = Some("x".repeat(60_000));
        let records = (0..5)
            .map(|index| {
                let mut r = record.clone();
                r.id = index.to_string();
                r
            })
            .collect::<Vec<_>>();
        let refs = records.iter().collect::<Vec<_>>();
        assert!(super::super::commands::review_replay_error(&refs)
            .unwrap()
            .contains("262144"));
    }

    #[test]
    fn block_review_offscreen_visual_and_escape_reopen() {
        let destination = std::env::var_os("EMBER_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        if let Some(path) = &destination {
            std::fs::create_dir_all(path).unwrap();
        }
        for (name, width, height, evicted) in [
            ("review-wide", 1000, 800, false),
            ("review-narrow", 360, 640, false),
            ("review-evicted", 800, 640, true),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            crate::apply_theme_visuals(&ctx, &crate::theme::Theme::default());
            let (mut terminal, mut review) = fixture();
            let mut renderer = crate::ui::TerminalRenderer::new(
                14.0,
                8.0,
                1.35,
                crate::config::ScrollbarVisibility::Always,
                crate::theme::Theme::default(),
            );
            renderer.gpu_rendering = false;
            review.active = 1;
            if evicted {
                terminal.clear_completed_blocks();
            }
            let view = review_view(&review, &terminal, true);
            let mut capture = super::super::visual_test_support::OffscreenCapture::default();
            for pass in 0..3 {
                let mut result = None;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        time: Some(pass as f64 * 0.25),
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width as f32, height as f32),
                        )),
                        events: if pass == 2 {
                            vec![egui::Event::Key {
                                key: egui::Key::Escape,
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers: egui::Modifiers::NONE,
                            }]
                        } else {
                            Vec::new()
                        },
                        ..Default::default()
                    },
                    |ui| {
                        renderer.render(
                            ui,
                            &mut terminal,
                            false,
                            false,
                            &crate::search::SearchState::default(),
                            &[],
                            &None,
                        );
                        result = draw_review(&ctx, &mut review, &view);
                    },
                );
                if pass == 2 {
                    assert!(result == Some(ReviewAction::Close));
                } else {
                    assert!(result.is_none());
                }
                if let Some(path) = &destination {
                    capture.save(
                        &ctx,
                        &mut output,
                        [width, height],
                        &path.join(format!("{name}.png")),
                    );
                } else {
                    output.textures_delta.clear();
                }
            }
            let (_, reopened) = fixture();
            assert!(
                !reopened.armed,
                "a new review never inherits its predecessor's armed state"
            );
        }
    }
}
