//! Opt-in, volatile organism in existing egui chrome. No terminal drawing or I/O.
//!
//! A single focused local session owns observation. Only authoritative Start
//! generations admitted after a conservative acquisition quarantine can pair
//! with the existing completion drain. This is not a parser-arrival epoch.
//! The bounded record tail is read only after a parser batch, never per frame.
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use egui::{RichText, Ui};
use jterm_core::organism::{AmbientMind, Behavior, BodyLanguage, RepoVigil, Tone};
use jterm_core::organism_daily::GentleInteraction;

use crate::config::{Config, OrganismMotion};
use crate::organism::{
    classify_command, resolved_motion, sprite_frame_with_context, sticky_glyph_with_context,
    CircadianPhase, CommandKind, GreetingAvailability, NativeOrganism, OrganismPreview,
    PresentationPolicy, PreviewPose, Reaction, RenderContext, WindowLife,
};
use crate::terminal::{CommandRecord, CompletedCommandEvent};

const BATCH_RECORD_LIMIT: usize = 32;

// The fixed strip is still allocated when hidden, so terminal grid size stays
// stable. Geometry only controls ownership, physiology and repaint scheduling.
fn host_geometry_allows(width: f32) -> bool {
    width.is_finite() && width >= 120.0
}

pub struct OrganismHost {
    born: Instant,
    life: WindowLife,
    native: NativeOrganism,
    owner: Option<String>,
    presentable: bool,
    cursor: Option<(u64, bool)>,
    quarantine_batch: bool,
    running: bool,
    pending: Option<PendingCommand>,
    work_context: Option<String>,
    remote_checked: Option<(String, Instant, bool)>,
    last_input: Option<Duration>,
    ambient: AmbientMind,
    context: RenderContext,
    reaction_until: Duration,
}

impl Default for OrganismHost {
    fn default() -> Self {
        Self {
            born: Instant::now(),
            life: WindowLife::new_at(Duration::ZERO),
            native: NativeOrganism::from_persisted_state(crate::organism::LifeState::default()),
            owner: None,
            presentable: false,
            cursor: None,
            quarantine_batch: false,
            running: false,
            pending: None,
            work_context: None,
            remote_checked: None,
            last_input: None,
            ambient: AmbientMind::seeded(0x656d626572),
            context: PreviewPose::Calm.context(),
            reaction_until: Duration::ZERO,
        }
    }
}

impl OrganismHost {
    fn set_available_width(&mut self, width: f32) {
        self.presentable = host_geometry_allows(width);
        if !self.presentable {
            self.acquire(None, None, false);
        }
    }

    /// Revoke the old session before accepting a new one. Acquiring a running
    /// command quarantines it; it must not manufacture a Start or completion.
    pub fn acquire(&mut self, owner: Option<&str>, tail: Option<(u64, bool)>, running: bool) {
        if self.owner.as_deref() == owner {
            self.running = owner.is_some() && running;
            return;
        }
        let now = self.born.elapsed();
        self.life
            .advance(now, false, false, CircadianPhase::Unlearned);
        self.owner = owner.map(str::to_owned);
        self.cursor = tail;
        self.quarantine_batch = owner.is_some();
        self.running = owner.is_some() && running;
        self.pending = None;
        self.work_context = None;
        self.native = NativeOrganism::from_persisted_state(self.life.state());
        self.ambient.interrupt();
        self.context = PreviewPose::Calm.context();
        self.reaction_until = now;
    }

    pub fn accepted_input(&mut self, session: &str) {
        if self.owner.as_deref() == Some(session) {
            let now = self.born.elapsed();
            self.last_input = Some(now);
            self.life.note_input(now);
            self.ambient.interrupt();
        }
    }

    pub fn batch(
        &mut self,
        session: &str,
        records: &VecDeque<CommandRecord>,
        completions: &[CompletedCommandEvent],
        alternate_screen: bool,
        backlogged: bool,
    ) {
        if self.owner.as_deref() != Some(session) {
            return;
        }
        if alternate_screen {
            self.acquire(None, None, false);
            return;
        }
        let now = self.born.elapsed();
        self.life.note_output(now);
        self.ambient.interrupt();
        let running = records.back().is_some_and(|record| {
            record.state == crate::terminal::CommandState::Running && record.start_mark_seen
        });
        self.running = running;
        if self.quarantine(
            records
                .back()
                .map(|record| (record.sequence, record.start_mark_seen)),
            running,
            backlogged,
        ) {
            return;
        }
        if let Some(pending) = self.pending.take() {
            if let Some(completed) = completions
                .iter()
                .rev()
                .take(BATCH_RECORD_LIMIT)
                .find(|completed| pending.matches(completed))
            {
                self.finish(&pending, completed, now);
            } else {
                self.pending = Some(pending);
            }
        }
        // Losing an exceptionally large batch is preferable to scanning all
        // history. Already observed/quarantined generations cannot be replayed;
        // partial control markers are not assigned a parser-arrival epoch.
        let count = records.len().min(BATCH_RECORD_LIMIT);
        for record in records.iter().skip(records.len() - count) {
            let fresh_start = fresh_start(self.cursor, record.sequence, record.start_mark_seen);
            if !fresh_start {
                continue;
            }
            self.cursor = Some((record.sequence, true));
            let kind = classify_command(record.command.as_deref().unwrap_or(""));
            self.set_work_context(record.cwd.as_deref());
            self.native.sync_state(self.life.state());
            let start = self.native.command_started(kind);
            self.life.replace_state(self.native.state());
            self.react(start, now);
            let pending = PendingCommand::from_record(record, kind);
            if let Some(completed) = completions
                .iter()
                .rev()
                .take(BATCH_RECORD_LIMIT)
                .find(|completed| pending.matches(completed))
            {
                self.finish(&pending, completed, now);
                self.pending = None;
            } else if !record.complete {
                self.pending = Some(pending);
            } else {
                self.pending = None;
            }
        }
    }

    fn quarantine(&mut self, tail: Option<(u64, bool)>, running: bool, backlogged: bool) -> bool {
        if !self.quarantine_batch {
            return false;
        }
        self.cursor = tail;
        self.quarantine_batch = backlogged;
        self.running = running;
        self.pending = None;
        true
    }

    fn set_work_context(&mut self, cwd: Option<&str>) {
        let bounded = cwd.filter(|cwd| cwd.len() <= 2048);
        if bounded.is_none() || bounded != self.work_context.as_deref() {
            self.native = NativeOrganism::from_persisted_state(self.life.state());
            self.work_context = bounded.map(str::to_owned);
        }
    }

    fn finish(
        &mut self,
        pending: &PendingCommand,
        completed: &CompletedCommandEvent,
        now: Duration,
    ) {
        let exit = (completed.completion_provenance
            == crate::block_mode::CompletionProvenance::ShellReported)
            .then_some(completed.exit_code)
            .flatten();
        self.native.sync_state(self.life.state());
        let reaction = self
            .native
            .command_finished(pending.kind, exit, completed.duration_ms);
        self.life.replace_state(self.native.state());
        self.react(reaction, now);
    }

    fn react(&mut self, reaction: Reaction, now: Duration) {
        self.ambient.interrupt();
        self.context = RenderContext::new(
            reaction.behavior,
            BodyLanguage::from_state(self.life.state()),
            false,
        );
        self.reaction_until = now + reaction_duration(&reaction);
    }

    pub fn draw(&mut self, ui: &mut Ui, config: &Config, focused: bool) {
        let now = self.born.elapsed();
        let presentable = self.presentable;
        if !presentable {
            self.acquire(None, None, false);
        }
        let eligible =
            config.ascii_organism_enabled && focused && presentable && self.owner.is_some();
        let dt = self
            .life
            .advance(now, eligible, self.running, CircadianPhase::Unlearned);
        if !config.ascii_organism_enabled {
            self.acquire(None, None, false);
            return;
        }
        let policy = PresentationPolicy {
            enabled: true,
            focused_owner: eligible,
            local: true,
            alternate_screen: false,
            motion: config.ascii_organism_motion,
            last_input: self.last_input,
        };
        if eligible && now >= self.reaction_until && !self.running {
            let behavior = self.ambient.step(
                self.life.state(),
                self.life.idle_for(now).as_secs_f32(),
                dt,
                RepoVigil::None,
            );
            self.context = RenderContext::new(
                behavior.display(),
                BodyLanguage::from_state(self.life.state()),
                false,
            );
        }
        let full = resolved_motion(config.ascii_organism_motion) == OrganismMotion::Full;
        let frame = if full {
            now.as_millis() as u64 / 100
        } else {
            0
        };
        let context = if self.running {
            RenderContext::new(
                Behavior::WatchCommand,
                BodyLanguage::from_state(self.life.state()),
                false,
            )
        } else {
            self.context
        };
        // Allocate exactly the same strip while suppressed. Typing, switching
        // panes and alternate screen must not change the terminal's grid size.
        egui::Panel::bottom("ascii_organism_chrome")
            .exact_size(24.0)
            .frame(egui::Frame::NONE.inner_margin(0.0))
            .resizable(false)
            .show(ui, |ui| {
                if policy.inline_visible(now) {
                    let rect = ui
                        .available_rect_before_wrap()
                        .shrink2(egui::vec2(8.0, 0.0));
                    // Paint only: no widget ID, focus, click or hover target.
                    ui.painter().with_clip_rect(rect).text(
                        rect.left_center(),
                        egui::Align2::LEFT_CENTER,
                        format!(
                            "{:<12}  Volatile",
                            sticky_glyph_with_context(context, frame)
                        ),
                        egui::TextStyle::Monospace.resolve(ui.style()),
                        ui.visuals().text_color(),
                    );
                }
            });
        let dormant = self.life.idle_for(now) >= Duration::from_secs(60) && !self.running;
        if let Some(delay) = next_host_wake(policy, now, self.reaction_until, dormant) {
            ui.ctx().request_repaint_after(delay);
        }
    }
}

struct PendingCommand {
    id: String,
    shell_session: Option<String>,
    seq: Option<u64>,
    started_at_ms: Option<u64>,
    kind: CommandKind,
}

impl PendingCommand {
    fn from_record(record: &CommandRecord, kind: CommandKind) -> Self {
        Self {
            id: record.id.clone(),
            shell_session: record.session_id.clone(),
            seq: record.seq,
            started_at_ms: record.started_at_ms,
            kind,
        }
    }

    fn matches(&self, completed: &CompletedCommandEvent) -> bool {
        completed.start_mark_seen
            && completed.id == self.id
            && completed.session_id == self.shell_session
            && completed.seq == self.seq
            && completed.started_at_ms == self.started_at_ms
    }
}

impl crate::TerminalApp {
    pub(crate) fn prepare_organism(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|input| input.viewport().focused.unwrap_or(false))
            && !self.terminal_input_blocked(ctx)
            && !self.active_terminal_is_read_only();
        // Admission uses the last actual root-UI geometry, not a potentially
        // different viewport width. Rendering refreshes it on resize.
        let presentable = self.organism.presentable;
        if !self.config.ascii_organism_enabled || !focused || !presentable {
            self.organism.acquire(None, None, false);
            return;
        }
        let Some(session) = self
            .session_manager
            .sessions()
            .get(self.session_manager.active_index())
        else {
            self.organism.acquire(None, None, false);
            return;
        };
        if session.purpose != crate::session::SessionPurpose::Interactive {
            self.organism.acquire(None, None, false);
            return;
        }
        let id = &session.metadata.session_id;
        let now = Instant::now();
        let cached = self
            .organism
            .remote_checked
            .as_ref()
            .filter(|(session_id, at, _)| {
                session_id == id && now.duration_since(*at) < Duration::from_millis(900)
            })
            .map(|(_, _, local)| *local);
        let local = cached.unwrap_or_else(|| {
            matches!(
                crate::ssh_files_follow::observe_session(session),
                crate::ssh_files_follow::Observation::None
            ) && !matches!(
                crate::session_manager::get_foreground_command(session.get_shell_pid()).as_deref(),
                Some("ssh" | "mosh" | "telnet")
            )
        });
        if cached.is_none() {
            self.organism.remote_checked = Some((id.clone(), now, local));
        }
        let terminal = session.terminal.lock();
        let allowed = local && !terminal.is_alt_buffer_active();
        self.organism.acquire(
            allowed.then_some(id.as_str()),
            terminal
                .command_records()
                .back()
                .map(|record| (record.sequence, record.start_mark_seen)),
            terminal.command_records().back().is_some_and(|record| {
                record.state == crate::terminal::CommandState::Running && record.start_mark_seen
            }),
        );
    }

    pub(crate) fn render_organism(&mut self, ui: &mut Ui) {
        // Resize events wake the host naturally. One root-UI width decision
        // drives admission, painting and scheduling, avoiding threshold churn.
        self.organism.set_available_width(ui.available_width());
        // Top-bar navigation may have changed the owner since event ingestion.
        self.prepare_organism(ui.ctx());
        let focused = ui
            .ctx()
            .input(|input| input.viewport().focused.unwrap_or(false));
        self.organism.draw(ui, &self.config, focused);
    }
}

/// Static has no heartbeat, but a transient reaction still needs one expiry
/// repaint. Choose the earliest deadline, including while input hides it.
fn next_host_wake(
    policy: PresentationPolicy,
    now: Duration,
    reaction_until: Duration,
    dormant: bool,
) -> Option<Duration> {
    if !policy.enabled || !policy.focused_owner || !policy.local || policy.alternate_screen {
        return None;
    }
    let presentation = policy.next_wake_after(now).map(|delay| {
        if dormant && policy.inline_visible(now) {
            delay.max(Duration::from_millis(900))
        } else {
            delay
        }
    });
    let reaction = (reaction_until > now).then(|| reaction_until - now);
    match (presentation, reaction) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(delay), None) | (None, Some(delay)) => Some(delay),
        (None, None) => None,
    }
}

fn fresh_start(cursor: Option<(u64, bool)>, sequence: u64, started: bool) -> bool {
    started
        && cursor
            .is_none_or(|(seen, was_started)| sequence > seen || (sequence == seen && !was_started))
}

fn reaction_duration(reaction: &Reaction) -> Duration {
    Duration::from_millis(match reaction.behavior {
        Behavior::Idle => 1500,
        Behavior::Celebrate if reaction.tone == Tone::Quiet => 1800,
        Behavior::Celebrate => 2500,
        Behavior::GlanceAside => 1400,
        Behavior::InspectError => 5000,
        Behavior::SitNearError => 10000,
        Behavior::CelebrateBig => 7000,
        Behavior::RestAfterPush => 5000,
        Behavior::UnknownOutcome => 4500,
        _ => 2500,
    })
}

pub struct PreviewUi {
    model: OrganismPreview,
    pose: PreviewPose,
    born: Instant,
    greeting_until: Duration,
    hello_ready_at: Duration,
}

impl Default for PreviewUi {
    fn default() -> Self {
        Self {
            model: OrganismPreview::default(),
            pose: PreviewPose::Calm,
            born: Instant::now(),
            greeting_until: Duration::ZERO,
            hello_ready_at: Duration::ZERO,
        }
    }
}

impl PreviewUi {
    pub fn close(&mut self) {
        self.model.close();
        self.greeting_until = Duration::ZERO;
    }

    fn sync_viewport(&mut self, focused: bool, occluded: bool) {
        if focused && !occluded {
            self.model.open();
        } else {
            self.close();
        }
    }

    pub fn show(&mut self, ui: &mut Ui, motion: Option<OrganismMotion>) {
        let (focused, occluded) = ui.ctx().input(|input| {
            let viewport = input.viewport();
            (
                viewport.focused.unwrap_or(false),
                viewport.occluded.unwrap_or(false),
            )
        });
        self.sync_viewport(focused, occluded);
        ui.label("Organism Preview");
        ui.label("Pose");
        egui::ComboBox::from_id_salt("organism_preview_pose")
            .selected_text(self.pose.label())
            .show_ui(ui, |ui| {
                for pose in PreviewPose::ALL {
                    if ui
                        .selectable_value(&mut self.pose, pose, pose.label())
                        .changed()
                    {
                        self.model.select_pose(pose);
                        self.greeting_until = Duration::ZERO;
                    }
                }
            });
        let now = self.born.elapsed();
        let availability = self.model.greeting_availability(now);
        let button = ui.add_enabled(
            availability == GreetingAvailability::Available,
            egui::Button::new("Say hello"),
        );
        match availability {
            GreetingAvailability::Closed => {
                ui.small("Preview paused while window is inactive");
            }
            GreetingAvailability::Busy => {
                ui.small("Unavailable for this pose");
            }
            GreetingAvailability::CoolingDown => {
                ui.small("Wait for the greeting cooldown");
            }
            _ => {}
        }
        if button.clicked() && self.model.say_hello(now) {
            self.greeting_until = now.saturating_add(GentleInteraction::HOLD);
            self.hello_ready_at = now.saturating_add(GentleInteraction::COOLDOWN);
        }
        {
            // Keep the same settings layout while inactive, without replaying
            // a canceled greeting or requesting background animation frames.
            let context = self
                .model
                .context(now)
                .unwrap_or_else(|| self.pose.context());
            let full = availability != GreetingAvailability::Closed
                && resolved_motion(motion) == OrganismMotion::Full;
            let frame = if full {
                now.as_millis() as u64 / 100
            } else {
                0
            };
            ui.add_sized(
                [ui.available_width(), 64.0],
                egui::Label::new(
                    RichText::new(sprite_frame_with_context(context, frame)).monospace(),
                ),
            );
            if let Some(delay) = preview_next_wake(
                now,
                full,
                self.greeting_until,
                self.hello_ready_at,
                self.model.greeting_availability(now),
            ) {
                ui.ctx().request_repaint_after(delay);
            }
        }
    }
}

// No repeating Static/Calm heartbeat. Closing the preview stops scheduling;
// pose changes retain the cooldown but only eligible poses need its wake.
fn preview_next_wake(
    now: Duration,
    full: bool,
    greeting_until: Duration,
    hello_ready_at: Duration,
    availability: GreetingAvailability,
) -> Option<Duration> {
    if availability == GreetingAvailability::Closed {
        return None;
    }
    let mut next = full.then_some(Duration::from_millis(100));
    for deadline in [
        Some(greeting_until),
        (availability == GreetingAvailability::CoolingDown).then_some(hello_ready_at),
    ]
    .into_iter()
    .flatten()
    {
        if deadline > now {
            let delay = deadline - now;
            next = Some(next.map_or(delay, |current| current.min(delay)));
        }
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_mode::CompletionProvenance;
    use crate::terminal::CompletedCommandOutput;

    #[test]
    fn host_geometry_is_fail_closed_at_the_paint_boundary() {
        for width in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0, 0.0, 119.9] {
            assert!(!host_geometry_allows(width));
        }
        assert!(host_geometry_allows(120.0));
        assert!(host_geometry_allows(800.0));
    }

    #[test]
    fn hidden_geometry_revokes_owner_and_resize_reacquires_without_replay() {
        let mut host = OrganismHost::default();
        host.set_available_width(120.0);
        host.acquire(Some("local"), Some((7, true)), true);
        host.pending = Some(pending());
        host.set_available_width(119.9);
        assert!(host.owner.is_none());
        assert!(host.pending.is_none());
        assert!(!host.running);
        let mut policy = static_policy();
        policy.focused_owner = false;
        policy.motion = Some(OrganismMotion::Full);
        assert_eq!(
            next_host_wake(
                policy,
                Duration::ZERO,
                Duration::from_secs(5),
                false
            ),
            None
        );
        host.set_available_width(120.0);
        host.acquire(Some("local"), Some((9, true)), true);
        assert!(host.quarantine_batch);
        assert!(host.pending.is_none());
        assert_eq!(host.cursor, Some((9, true)));
        assert!(host.running);
    }

    #[test]
    fn hidden_geometry_pauses_physiology_and_resume_has_no_catchup() {
        let mut life = WindowLife::new_at(Duration::ZERO);
        let phase = CircadianPhase::Unlearned;
        life.advance(Duration::ZERO, true, false, phase);
        let before = format!("{:?}", life.state());
        assert_eq!(
            life.advance(
                Duration::from_secs(60),
                host_geometry_allows(119.0),
                false,
                phase,
            ),
            0.0
        );
        assert_eq!(format!("{:?}", life.state()), before);
        assert_eq!(
            life.advance(
                Duration::from_secs(120),
                host_geometry_allows(120.0),
                false,
                phase,
            ),
            0.0
        );
        assert_eq!(format!("{:?}", life.state()), before);
    }

    #[test]
    fn preview_blur_preserves_cooldown_without_replaying_greeting() {
        let mut preview = PreviewUi::default();
        preview.sync_viewport(true, false);
        assert!(preview.model.say_hello(Duration::ZERO));
        preview.greeting_until = GentleInteraction::HOLD;
        preview.hello_ready_at = GentleInteraction::COOLDOWN;
        preview.sync_viewport(false, false);
        assert_eq!(preview.greeting_until, Duration::ZERO);
        assert_eq!(preview.hello_ready_at, GentleInteraction::COOLDOWN);
        assert_eq!(preview.pose, PreviewPose::Calm);
        let now = Duration::from_secs(1);
        assert_eq!(
            preview_next_wake(
                now,
                true,
                preview.greeting_until,
                preview.hello_ready_at,
                preview.model.greeting_availability(now),
            ),
            None
        );
        preview.sync_viewport(true, false);
        assert_eq!(
            preview.model.context(now),
            Some(PreviewPose::Calm.context())
        );
        assert_eq!(
            preview.model.greeting_availability(now),
            GreetingAvailability::CoolingDown
        );
        assert_eq!(
            preview_next_wake(
                now,
                false,
                preview.greeting_until,
                preview.hello_ready_at,
                preview.model.greeting_availability(now),
            ),
            Some(Duration::from_secs(7))
        );
        let expired = Duration::from_secs(20);
        assert_eq!(
            preview_next_wake(
                expired,
                false,
                preview.greeting_until,
                preview.hello_ready_at,
                preview.model.greeting_availability(expired),
            ),
            None
        );
    }

    #[test]
    fn preview_occlusion_suspends_even_with_retained_focus() {
        let mut preview = PreviewUi::default();
        preview.sync_viewport(true, false);
        preview.sync_viewport(true, true);
        assert_eq!(
            preview.model.greeting_availability(Duration::ZERO),
            GreetingAvailability::Closed
        );
        preview.sync_viewport(true, false);
        assert_eq!(
            preview.model.greeting_availability(Duration::ZERO),
            GreetingAvailability::Available
        );
    }

    #[test]
    fn preview_query_never_spends_attention_and_explains_busy_or_cooldown() {
        let mut preview = OrganismPreview::default();
        assert_eq!(
            preview.greeting_availability(Duration::ZERO),
            GreetingAvailability::Closed
        );
        preview.open();
        for _ in 0..3 {
            assert_eq!(
                preview.greeting_availability(Duration::ZERO),
                GreetingAvailability::Available
            );
        }
        assert!(preview.say_hello(Duration::ZERO));
        assert_eq!(
            preview.greeting_availability(Duration::from_secs(1)),
            GreetingAvailability::CoolingDown
        );
        // Querying a future time must not move the real core clock.
        assert_eq!(
            preview.greeting_availability(Duration::from_secs(8)),
            GreetingAvailability::Available
        );
        assert!(!preview.say_hello(Duration::from_secs(1)));
        preview.select_pose(PreviewPose::Working);
        assert_eq!(
            preview.greeting_availability(Duration::from_secs(8)),
            GreetingAvailability::Busy
        );
        preview.select_pose(PreviewPose::Calm);
        preview.close();
        preview.open();
        assert_eq!(
            preview.greeting_availability(Duration::from_secs(7)),
            GreetingAvailability::CoolingDown
        );
        assert!(preview.say_hello(Duration::from_secs(8)));
    }

    #[test]
    fn preview_static_wakes_at_greeting_then_cooldown_without_heartbeat() {
        let greeting = Duration::from_secs(2);
        let ready = Duration::from_secs(8);
        assert_eq!(
            preview_next_wake(
                Duration::ZERO,
                false,
                greeting,
                ready,
                GreetingAvailability::CoolingDown,
            ),
            Some(greeting)
        );
        assert_eq!(
            preview_next_wake(
                greeting,
                false,
                greeting,
                ready,
                GreetingAvailability::CoolingDown,
            ),
            Some(Duration::from_secs(6))
        );
        assert_eq!(
            preview_next_wake(
                ready,
                false,
                greeting,
                ready,
                GreetingAvailability::Available
            ),
            None
        );
    }

    #[test]
    fn preview_closed_or_busy_does_not_schedule_cooldown() {
        for availability in [GreetingAvailability::Closed, GreetingAvailability::Busy] {
            assert_eq!(
                preview_next_wake(
                    Duration::ZERO,
                    false,
                    Duration::ZERO,
                    Duration::from_secs(8),
                    availability,
                ),
                None
            );
        }
        assert_eq!(
            preview_next_wake(
                Duration::ZERO,
                true,
                Duration::from_secs(2),
                Duration::from_secs(8),
                GreetingAvailability::Closed,
            ),
            None
        );
    }

    #[test]
    fn preview_full_motion_keeps_nearest_deadline() {
        assert_eq!(
            preview_next_wake(
                Duration::ZERO,
                true,
                Duration::from_millis(50),
                Duration::from_secs(8),
                GreetingAvailability::CoolingDown,
            ),
            Some(Duration::from_millis(50))
        );
        assert_eq!(
            preview_next_wake(
                Duration::from_secs(2),
                true,
                Duration::ZERO,
                Duration::from_secs(8),
                GreetingAvailability::CoolingDown,
            ),
            Some(Duration::from_millis(100))
        );
    }

    // Synthetic data only: these tests never parse terminal text, construct a
    // renderer, open a file, or spawn a shell.
    fn completion(exit_code: Option<i32>) -> CompletedCommandEvent {
        CompletedCommandEvent {
            completed: CompletedCommandOutput {
                id: "command-7".to_owned(),
                session_id: Some("shell-1".to_owned()),
                seq: Some(7),
                started_at_ms: Some(100),
                command: None,
                cwd: None,
                exit_code,
                duration_ms: Some(20),
                output: String::new(),
                output_available: false,
                truncated: false,
                total_bytes: 0,
                agent_generation: None,
            },
            start_mark_seen: true,
            completion_provenance: CompletionProvenance::ShellReported,
        }
    }

    fn pending() -> PendingCommand {
        PendingCommand {
            id: "command-7".to_owned(),
            shell_session: Some("shell-1".to_owned()),
            seq: Some(7),
            started_at_ms: Some(100),
            kind: CommandKind::Other,
        }
    }

    #[test]
    fn watermark_quarantines_existing_starts_but_accepts_a_new_prompt_start() {
        assert!(!fresh_start(Some((7, true)), 7, true));
        assert!(!fresh_start(Some((7, false)), 6, true));
        assert!(!fresh_start(Some((7, false)), 7, false));
        assert!(fresh_start(Some((7, false)), 7, true));
        assert!(fresh_start(Some((7, true)), 8, true));
    }

    #[test]
    fn completion_requires_the_exact_observed_start_generation() {
        let start = pending();
        let mut done = completion(Some(0));
        assert!(start.matches(&done));
        done.started_at_ms = Some(101);
        assert!(!start.matches(&done));
        done.started_at_ms = Some(100);
        done.seq = Some(8);
        assert!(!start.matches(&done));
        done.seq = Some(7);
        done.session_id = Some("different-shell".to_owned());
        assert!(!start.matches(&done));
        done.session_id = Some("shell-1".to_owned());
        done.start_mark_seen = false;
        assert!(!start.matches(&done));
    }

    #[test]
    fn missing_and_inferred_exit_are_unknown_never_success() {
        for provenance in [
            CompletionProvenance::ShellReported,
            CompletionProvenance::BoundaryInferred,
        ] {
            let mut host = OrganismHost::default();
            let mut done = completion(None);
            done.completion_provenance = provenance;
            if provenance == CompletionProvenance::BoundaryInferred {
                done.exit_code = Some(0);
            }
            host.finish(&pending(), &done, Duration::ZERO);
            assert_eq!(host.context.behavior, Behavior::UnknownOutcome);
        }
    }

    #[test]
    fn ownership_handoff_and_disable_revoke_pending_completion() {
        let mut host = OrganismHost::default();
        host.acquire(Some("local-a"), Some((4, true)), true);
        host.pending = Some(pending());
        host.running = true;
        host.acquire(Some("local-b"), Some((9, false)), false);
        assert!(host.pending.is_none());
        assert!(!host.running);
        assert_eq!(host.cursor, Some((9, false)));
        host.acquire(None, None, false);
        assert!(host.owner.is_none());
        assert!(host.cursor.is_none());
        host.accepted_input("local-a");
        assert!(host.last_input.is_none());
    }

    #[test]
    fn input_from_another_session_cannot_change_the_owner() {
        let mut host = OrganismHost::default();
        host.acquire(Some("local-a"), None, false);
        host.accepted_input("local-b");
        assert!(host.last_input.is_none());
        host.accepted_input("local-a");
        assert!(host.last_input.is_some());
    }

    #[test]
    fn retained_reducer_recovers_after_repeated_build_failures() {
        let mut host = OrganismHost::default();
        host.set_work_context(Some("/workspace/project-a"));
        let mut start = pending();
        start.kind = CommandKind::BuildOrTest;
        for exit in [101, 101, 101, 0] {
            host.set_work_context(Some("/workspace/project-a"));
            host.native.sync_state(host.life.state());
            host.native.command_started(start.kind);
            host.life.replace_state(host.native.state());
            host.finish(&start, &completion(Some(exit)), Duration::ZERO);
        }
        assert_eq!(host.context.behavior, Behavior::CelebrateBig);
        host.set_work_context(Some("/workspace/project-b"));
        assert_eq!(host.native.repo_vigil(), RepoVigil::None);
    }

    #[test]
    fn first_batch_and_backlog_quarantine_keep_running_without_reducing_events() {
        let mut host = OrganismHost::default();
        host.acquire(Some("local-a"), Some((7, true)), true);
        assert!(
            host.running,
            "a silent pre-existing command still keeps watch"
        );
        host.pending = Some(pending());
        let before = host.context;
        assert!(host.quarantine(Some((8, true)), true, true));
        assert!(host.quarantine_batch);
        assert!(host.running);
        assert!(host.pending.is_none());
        assert!(host.quarantine(Some((9, true)), true, false));
        assert!(!host.quarantine_batch);
        assert!(!fresh_start(host.cursor, 9, true));
        // A paired-looking stale completion has no pending generation and a
        // quarantined Start cannot be admitted later by its sequence.
        let stale = completion(Some(0));
        assert!(host.pending.as_ref().is_none_or(|p| !p.matches(&stale)));
        assert_eq!(host.context, before);
        assert!(!host.quarantine(Some((10, true)), true, false));
    }

    #[test]
    fn window_input_retreat_survives_owner_handoff() {
        let mut host = OrganismHost::default();
        host.acquire(Some("local-a"), None, false);
        host.accepted_input("local-a");
        let input_at = host.last_input;
        host.acquire(Some("local-b"), None, false);
        assert_eq!(host.last_input, input_at);
        host.acquire(None, None, false);
        assert_eq!(host.last_input, input_at);
        assert!(!host.running);
    }

    fn static_policy() -> PresentationPolicy {
        PresentationPolicy {
            enabled: true,
            focused_owner: true,
            local: true,
            alternate_screen: false,
            motion: Some(OrganismMotion::Static),
            last_input: None,
        }
    }

    #[test]
    fn static_reaction_gets_one_expiry_wake_and_no_heartbeat() {
        let now = Duration::from_secs(10);
        let deadline = now + Duration::from_secs(2);
        assert_eq!(
            next_host_wake(static_policy(), now, deadline, true),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            next_host_wake(static_policy(), deadline, deadline, true),
            None
        );
        assert_eq!(
            next_host_wake(
                static_policy(),
                deadline + Duration::from_secs(1),
                deadline,
                false,
            ),
            None
        );
    }

    #[test]
    fn reaction_and_input_retreat_choose_the_earlier_wake() {
        let now = Duration::from_secs(10);
        let policy = PresentationPolicy {
            last_input: Some(now),
            ..static_policy()
        };
        assert_eq!(
            next_host_wake(policy, now, now + Duration::from_millis(200), true),
            Some(Duration::from_millis(200))
        );
        assert_eq!(
            next_host_wake(policy, now, now + Duration::from_secs(2), true),
            Some(Duration::from_millis(900))
        );
    }

    #[test]
    fn ineligible_host_never_schedules_even_with_a_future_reaction() {
        let now = Duration::from_secs(10);
        for policy in [
            PresentationPolicy {
                enabled: false,
                ..static_policy()
            },
            PresentationPolicy {
                focused_owner: false,
                ..static_policy()
            },
            PresentationPolicy {
                local: false,
                ..static_policy()
            },
            PresentationPolicy {
                alternate_screen: true,
                ..static_policy()
            },
        ] {
            assert_eq!(
                next_host_wake(policy, now, now + Duration::from_secs(2), false),
                None
            );
        }
    }

    #[test]
    fn full_frame_and_dormant_budgets_keep_earlier_reaction_deadline() {
        let now = Duration::from_secs(10);
        let full = PresentationPolicy {
            motion: Some(OrganismMotion::Full),
            ..static_policy()
        };
        assert_eq!(
            next_host_wake(full, now, now, false),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            next_host_wake(full, now, now, true),
            Some(Duration::from_millis(900))
        );
        assert_eq!(
            next_host_wake(full, now, now + Duration::from_millis(50), true),
            Some(Duration::from_millis(50))
        );
    }
}
