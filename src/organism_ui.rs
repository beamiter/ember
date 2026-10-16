//! Opt-in, volatile organism in native egui chrome and proven empty Block space.
//! Decoration never mutates terminal cells, selection origins or terminal I/O.
//!
//! A single focused local session owns observation. Only authoritative Start
//! generations admitted after a conservative acquisition quarantine can pair
//! with the existing completion drain. This is not a parser-arrival epoch.
//! The bounded record tail is read only after a parser batch, never per frame.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use egui::{RichText, Ui};
use jterm_core::organism::{
    AmbientMind, Behavior, BodyLanguage, RepoVigil, VisualTransition, WatchRhythm,
};
use jterm_core::organism_daily::{behavior_explanation, GentleInteraction};
use jterm_core::organism_live::{
    LiveActivityKnowledge, LiveAttention, LiveAttentionEvent, LiveAttentionInput,
    LiveAttentionOutcome, LiveAttentionRejection, LiveAttentionSnapshot,
};

use crate::config::{Config, OrganismMotion};
use crate::organism::{
    classify_command, resolved_motion, sprite_frame_with_context, sticky_glyph_with_context,
    CircadianPhase, CommandKind, GreetingAvailability, NativeOrganism, OrganismPreview,
    PresentationPolicy, PreviewPose, Reaction, RenderContext, WindowLife,
};
use crate::terminal::{CommandRecord, CompletedCommandEvent};
use crate::theme::ThemeExt;
use crate::watch_adapter::WatchAdapter;

const BATCH_RECORD_LIMIT: usize = 32;

// The fixed strip is still allocated when hidden, so terminal grid size stays
// stable. Geometry only controls ownership, physiology and repaint scheduling.
fn host_geometry_allows(width: f32) -> bool {
    width.is_finite() && width >= 120.0
}

fn host_geometry_presentable(available: egui::Rect, clip: egui::Rect) -> bool {
    if !available.is_finite() || !clip.is_finite() {
        return false;
    }
    let visible = available.intersect(clip);
    visible.is_positive() && host_geometry_allows(visible.width())
}

/// Evidence only: a dirty draft or uncertain submission is not a running job.
fn live_activity_knowledge(
    running: bool,
    clean_prompt: bool,
    pending_input: bool,
    input_barrier: bool,
    agent_uncertain: bool,
) -> LiveActivityKnowledge {
    if running {
        LiveActivityKnowledge::Busy
    } else if clean_prompt && !pending_input && !input_barrier && !agent_uncertain {
        LiveActivityKnowledge::KnownIdle
    } else {
        LiveActivityKnowledge::Unknown
    }
}

/// Local interactive-shell identity comes from launch, never terminal text.
/// A busy lock or missing running observation provides no positive evidence.
fn known_local_shell_running(
    purpose: crate::session::SessionPurpose,
    has_source_shell: bool,
    running: Option<bool>,
) -> bool {
    purpose == crate::session::SessionPurpose::Interactive
        && has_source_shell
        && running == Some(true)
}

fn window_has_known_local_running(
    facts: impl IntoIterator<Item = (crate::session::SessionPurpose, bool, Option<bool>)>,
) -> bool {
    facts.into_iter().any(|(purpose, source_shell, running)| {
        known_local_shell_running(purpose, source_shell, running)
    })
}

fn terminal_has_running_command(terminal: &crate::terminal::TerminalState) -> bool {
    terminal.command_records().back().is_some_and(|record| {
        record.state == crate::terminal::CommandState::Running
            && record.start_mark_seen
            && !record.complete
    })
}

/// Native pointer observation only; all dwell, hold, cooldown and source policy
/// lives in jterm_core. Layout changes synchronize position without an entry.
fn observe_live_attention(
    attention: &mut LiveAttention<String>,
    pointer_inside: &mut bool,
    now: Duration,
    owner: &String,
    mut input: LiveAttentionInput,
    target: egui::Rect,
    events: &[egui::Event],
) -> Option<LiveAttentionSnapshot> {
    if attention.owner() != Some(owner) {
        return None;
    }
    let final_inside = input.pointer_inside;
    for event in events {
        let inside = match event {
            egui::Event::PointerMoved(position) => target.contains(*position),
            egui::Event::PointerGone => false,
            _ => continue,
        };
        input.pointer_inside = inside;
        let event = if !inside {
            attention
                .entry_ticket()
                .map(|ticket| LiveAttentionEvent::PointerLeft { ticket })
        } else if !*pointer_inside {
            attention
                .entry_offer()
                .map(|ticket| LiveAttentionEvent::PointerEntered { ticket })
        } else {
            None
        };
        if let Some(event) = event {
            attention.advance(now, owner, input, event).ok()?;
        }
        *pointer_inside = inside;
    }
    // The final native position may change because layout moved, without a
    // pointer event. It is a fact for cancellation, never permission to rearm.
    input.pointer_inside = final_inside;
    let snapshot = attention
        .advance(now, owner, input, LiveAttentionEvent::Reconcile)
        .ok()?;
    *pointer_inside = final_inside;
    Some(snapshot)
}

/// A static attention cue, not a focus ring or a measure of work progress.
fn hover_cue_rect(target: egui::Rect, clip: egui::Rect) -> Option<egui::Rect> {
    if !target.is_finite() || !clip.is_finite() || target.width() < 28.0 || target.height() < 8.0 {
        return None;
    }
    let cue = egui::Rect::from_min_size(
        egui::pos2(target.min.x + 8.0, target.max.y - 4.0),
        egui::vec2(12.0, 1.0),
    );
    let visible = cue.intersect(clip);
    visible.is_positive().then_some(visible)
}

fn native_body_rect(region: egui::Rect, size: egui::Vec2) -> Option<egui::Rect> {
    if !region.is_finite()
        || !region.is_positive()
        || !size.is_finite()
        || size.x <= 0.0
        || size.y <= 0.0
        || size.x > region.width()
        || size.y > region.height()
    {
        return None;
    }
    Some(egui::Rect::from_min_size(region.max - size, size))
}

/// Native theme tokens match the reference GTK card; no hard-coded theme.
#[derive(Clone, Copy)]
struct NativePalette {
    background: egui::Color32,
    foreground: egui::Color32,
    quiet: egui::Color32,
    active: egui::Color32,
    success: egui::Color32,
    error: egui::Color32,
    warning: egui::Color32,
}

impl Default for NativePalette {
    fn default() -> Self {
        Self::from_theme(&crate::theme::Theme::default())
    }
}

impl NativePalette {
    fn from_theme(theme: &crate::theme::Theme) -> Self {
        Self {
            background: theme.terminal_background(),
            foreground: theme.terminal_foreground(),
            quiet: theme.ansi_color(4),
            active: crate::theme::Theme::rgb_to_color32(theme.tabbar.active_border),
            success: theme.ansi_color(2),
            error: theme.ansi_color(1),
            warning: theme.ansi_color(3),
        }
    }

    fn body(self, behavior: Behavior) -> egui::Color32 {
        match behavior {
            Behavior::WatchCommand | Behavior::WatchSettled | Behavior::WatchAgent => self.active,
            Behavior::Celebrate | Behavior::CelebrateBig | Behavior::RestAfterPush => self.success,
            Behavior::InspectError | Behavior::SitNearError => self.error,
            Behavior::UnknownOutcome => self.warning,
            _ => self.quiet,
        }
    }
}

fn alpha(color: egui::Color32, opacity: u8) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), opacity)
}

fn organism_font(ui: &Ui, size: f32) -> egui::FontId {
    let family = egui::FontFamily::Name("organism-monospace-bold".into());
    let registered = ui.fonts(|fonts| fonts.definitions().families.contains_key(&family));
    egui::FontId::new(
        size,
        if registered {
            family
        } else {
            egui::FontFamily::Monospace
        },
    )
}

/// Same restrained 80-second edge-to-edge route as Anvil/Forge. Geometry is
/// presentation only; no new life state or attention controller lives here.
fn native_wander_phase(frame: u64, language: BodyLanguage) -> (f32, bool) {
    if language.drowsy {
        return (0.0, false);
    }
    let walk = if language.listless { 120 } else { 40 };
    let phase = frame % 800;
    let sit = (800 - 2 * walk) / 2;
    if phase < sit {
        (0.0, false)
    } else if phase < sit + walk {
        (((phase - sit) * 40 / walk) as f32 / 40.0, true)
    } else if phase < 2 * sit + walk {
        (1.0, false)
    } else {
        (
            (40 - (phase - 2 * sit - walk) * 40 / walk) as f32 / 40.0,
            true,
        )
    }
}

fn native_placement(
    region: egui::Rect,
    size: egui::Vec2,
    context: RenderContext,
    frame: u64,
    wandering: bool,
) -> Option<egui::Rect> {
    let fitting = native_body_rect(region, size)?;
    let fraction = if wandering {
        native_wander_phase(
            frame,
            BodyLanguage {
                listless: context.body_language.listless || context.behavior == Behavior::Explore,
                ..context.body_language
            },
        )
        .0
    } else if matches!(
        context.behavior,
        Behavior::WatchCommand | Behavior::WatchSettled | Behavior::WatchAgent
    ) {
        1.0
    } else {
        0.0
    };
    Some(egui::Rect::from_min_size(
        egui::pos2(
            region.left() + (region.width() - size.x) * fraction,
            fitting.top(),
        ),
        size,
    ))
}

/// The GTK reference approaches its route target by one cell per presentation
/// beat. Missed beats never catch up, and unrelated redraws never accelerate it.
fn native_walk_step(
    previous: egui::Rect,
    target: egui::Rect,
    cell_width: f32,
    new_frame: bool,
) -> egui::Rect {
    if previous.size() != target.size() || !cell_width.is_finite() || cell_width <= 0.0 {
        return target;
    }
    let x = if new_frame {
        previous.left() + (target.left() - previous.left()).clamp(-cell_width, cell_width)
    } else {
        previous.left()
    };
    egui::Rect::from_min_size(egui::pos2(x, target.top()), target.size())
}

fn explicit_window_available(
    focused: Option<bool>,
    visible: Option<bool>,
    buttons_down: bool,
    buttons_pressed: bool,
) -> bool {
    focused.unwrap_or(false) && visible.unwrap_or(true) && !buttons_down && !buttons_pressed
}

fn live_greeting_allowed(
    policy: PresentationPolicy,
    now: Duration,
    running: bool,
    buttons_down: bool,
) -> bool {
    policy.inline_visible(now)
        && resolved_motion(policy.motion) != OrganismMotion::Static
        && !running
        && !buttons_down
}

/// Endpoints are committed only after a visible body paint command is submitted.
/// This is not evidence that a GPU frame has reached the screen.
#[derive(Clone, Copy, Default)]
struct PaintedBridge {
    last_settled: Option<Behavior>,
    active: Option<PaintedArc>,
}

#[derive(Clone, Copy)]
struct PaintedArc {
    transition: VisualTransition,
    started: Duration,
}

struct BodyPaintPlan {
    context: RenderContext,
    frame: u64,
    after_submit: PaintedBridge,
}

impl PaintedBridge {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn direct(target: RenderContext, frame: u64) -> BodyPaintPlan {
        BodyPaintPlan {
            context: target,
            frame,
            after_submit: Self {
                last_settled: Some(target.behavior),
                active: None,
            },
        }
    }

    fn plan_at_cadence(
        &self,
        now: Duration,
        target: RenderContext,
        frame: u64,
        cadence_active: bool,
    ) -> BodyPaintPlan {
        if cadence_active {
            self.plan(now, target, frame)
        } else {
            Self::direct(target, frame)
        }
    }

    fn plan(&self, now: Duration, target: RenderContext, frame: u64) -> BodyPaintPlan {
        let direct = || Self::direct(target, frame);
        if let Some(arc) = self.active {
            // A newer authoritative target interrupts immediately. Never queue
            // an arc or infer a settled source from an intermediate bridge pose.
            if arc.transition.target() != target.behavior {
                return direct();
            }
            let relative = now.saturating_sub(arc.started).as_millis() / 100;
            if relative >= u128::from(arc.transition.frame_count()) {
                return direct();
            }
            return BodyPaintPlan {
                context: target.with_transition(Some(arc.transition)),
                frame: relative as u64,
                after_submit: *self,
            };
        }
        let Some(transition) = self
            .last_settled
            .and_then(|source| VisualTransition::between(source, target.behavior))
        else {
            return direct();
        };
        BodyPaintPlan {
            context: target.with_transition(Some(transition)),
            frame: 0,
            after_submit: Self {
                last_settled: self.last_settled,
                active: Some(PaintedArc {
                    transition,
                    started: now,
                }),
            },
        }
    }

    fn submitted(&mut self, plan: BodyPaintPlan) {
        *self = plan.after_submit;
    }
}

fn submitted_text_visible(text_rect: egui::Rect, clip: egui::Rect) -> bool {
    text_rect.intersect(clip).is_positive()
}

fn bridge_allowed(expanded: bool, full: bool, visible_clip: bool, greeting: bool) -> bool {
    expanded && full && visible_clip && !greeting
}

fn host_height(enabled: bool, expanded: bool) -> f32 {
    if !enabled {
        0.0
    } else if expanded {
        96.0
    } else {
        24.0
    }
}

/// All content is clipped to reserved chrome; controls never overlap the body.
#[derive(Clone, Copy)]
struct CompanionLayout {
    card: egui::Rect,
    body: egui::Rect,
    details: egui::Rect,
    control: egui::Rect,
}

fn companion_layout(rect: egui::Rect, expanded: bool) -> CompanionLayout {
    if !rect.is_finite() || !rect.is_positive() {
        return CompanionLayout {
            card: egui::Rect::NOTHING,
            body: egui::Rect::NOTHING,
            details: egui::Rect::NOTHING,
            control: egui::Rect::NOTHING,
        };
    }
    let card = rect.shrink2(egui::vec2(
        0.0,
        if expanded {
            4.0_f32.min(rect.height() / 2.0)
        } else {
            0.0
        },
    ));
    let inner = card.shrink2(egui::vec2(
        12.0_f32.min(card.width() / 2.0),
        if expanded {
            8.0_f32.min(card.height() / 2.0)
        } else {
            0.0
        },
    ));
    let control = egui::Rect::from_min_max(
        egui::pos2((inner.max.x - 28.0).max(inner.min.x), inner.min.y),
        inner.max,
    );
    let body = egui::Rect::from_min_max(
        inner.min,
        egui::pos2((inner.min.x + 104.0).min(control.min.x), inner.max.y),
    );
    let details = egui::Rect::from_min_max(
        egui::pos2((body.max.x + 12.0).min(control.min.x), inner.min.y),
        egui::pos2((control.min.x - 8.0).max(body.max.x), inner.max.y),
    );
    CompanionLayout {
        card,
        body,
        details,
        control,
    }
}

fn companion_line(
    ui: &Ui,
    rect: egui::Rect,
    text: impl Into<String>,
    size: f32,
    color: egui::Color32,
) {
    companion_text(ui, rect, text, size, color, 1);
}

fn companion_text(
    ui: &Ui,
    rect: egui::Rect,
    text: impl Into<String>,
    size: f32,
    color: egui::Color32,
    max_rows: usize,
) {
    let clip = rect.intersect(ui.clip_rect());
    if !clip.is_positive() || rect.width() < 8.0 {
        return;
    }
    let mut job = egui::text::LayoutJob::simple(
        text.into(),
        egui::FontId::proportional(size),
        color,
        rect.width(),
    );
    job.wrap.max_rows = max_rows;
    job.wrap.break_anywhere = true;
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    ui.painter()
        .with_clip_rect(clip)
        .galley(rect.min, galley, color);
}

/// Preserve the body/control slots; details alone adapt to the available width.
fn companion_details(ui: &Ui, rect: egui::Rect, status: impl Into<String>) {
    let row = |x: f32, y: f32, width: f32, height: f32| {
        egui::Rect::from_min_size(rect.min + egui::vec2(x, y), egui::vec2(width, height))
            .intersect(rect)
    };
    let title = "ASCII organism";
    let badge = "Volatile memory · no LLM";
    let measure = |text: &str, size| {
        ui.painter()
            .layout_no_wrap(
                text.to_owned(),
                egui::FontId::proportional(size),
                ui.visuals().text_color(),
            )
            .size()
            .x
    };
    let title_width = measure(title, 14.0);
    let badge_width = measure(badge, 11.0);
    let wide = rect.width() >= title_width + 16.0 + badge_width;
    companion_line(
        ui,
        row(
            0.0,
            0.0,
            if wide { title_width } else { rect.width() },
            20.0,
        ),
        title,
        14.0,
        ui.visuals().strong_text_color(),
    );
    companion_line(
        ui,
        if wide {
            row(rect.width() - badge_width, 2.0, badge_width, 18.0)
        } else {
            row(0.0, 54.0, rect.width(), 18.0)
        },
        badge,
        11.0,
        ui.visuals().weak_text_color(),
    );
    companion_text(
        ui,
        row(0.0, 22.0, rect.width(), 32.0),
        status,
        12.0,
        ui.visuals().text_color(),
        2,
    );
}

/// A pointer-only control never requests keyboard focus or sends terminal input.
fn companion_toggle(ui: &mut Ui, rect: egui::Rect, expanded: bool, interactive: bool) -> bool {
    let id = ui.make_persistent_id("ascii_organism_expand");
    let press_id = id.with("press_expanded");
    let rect = rect.intersect(ui.clip_rect());
    if !rect.is_positive() {
        ui.data_mut(|data| data.remove::<bool>(press_id));
        return false;
    }
    let interactive = interactive && ui.is_enabled();
    let sense = if interactive {
        egui::Sense::CLICK
    } else {
        egui::Sense::hover()
    };
    let response = ui.interact(rect, id, sense);
    // egui retains click ownership when a widget temporarily becomes hover-only.
    // An old press must not cross disabled frames or a change of toggle action.
    let (pressed, down, released) = ui.input(|input| {
        let button = egui::PointerButton::Primary;
        (
            input.pointer.button_pressed(button),
            input.pointer.button_down(button),
            input.pointer.button_released(button),
        )
    });
    let native_clicked = response.clicked();
    let pointer_down_on = response.is_pointer_button_down_on();
    let clicked = ui.data_mut(|data| {
        let mut armed = data.get_temp::<bool>(press_id);
        if !interactive || armed.is_some_and(|action| action != expanded) {
            armed = None;
        }
        if interactive && pressed && (pointer_down_on || native_clicked) {
            armed = Some(expanded);
        }
        let clicked = interactive && released && native_clicked && armed == Some(expanded);
        if down {
            if let Some(action) = armed {
                data.insert_temp(press_id, action);
            } else {
                data.remove::<bool>(press_id);
            }
        } else {
            data.remove::<bool>(press_id);
        }
        clicked
    });
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            interactive,
            if expanded {
                "Collapse ASCII organism"
            } else {
                "Expand ASCII organism"
            },
        )
    });
    let visuals = if interactive {
        ui.style().interact(&response)
    } else {
        &ui.visuals().widgets.noninteractive
    };
    let painter = ui.painter().with_clip_rect(rect);
    if interactive && (response.hovered() || response.is_pointer_button_down_on()) {
        painter.rect(
            rect,
            4.0,
            visuals.bg_fill,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
    }
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        if expanded { "⌄" } else { "⌃" },
        egui::FontId::proportional(16.0),
        visuals.fg_stroke.color,
    );
    if interactive {
        response
            .clone()
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(if expanded {
                "Collapse ASCII organism"
            } else {
                "Expand ASCII organism"
            });
    }
    clicked
}

fn host_text(
    context: RenderContext,
    frame: u64,
    width: f32,
    expanded: bool,
) -> (Option<std::borrow::Cow<'static, str>>, String) {
    (
        expanded.then(|| sprite_frame_with_context(context, frame)),
        live_status_text(context, frame, width),
    )
}

/// Explain only the final observed watch rhythm, never command progress.
fn live_behavior_explanation(context: RenderContext) -> std::borrow::Cow<'static, str> {
    let base = behavior_explanation(context.behavior);
    let activity = if matches!(
        context.behavior,
        Behavior::WatchCommand | Behavior::WatchSettled
    ) {
        match context.watch_rhythm {
            WatchRhythm::Busy => Some("Recent output activity."),
            WatchRhythm::Waiting => Some("No new output observed recently."),
            WatchRhythm::Resumed => Some("Output activity resumed."),
            WatchRhythm::Steady => None,
        }
    } else {
        None
    };
    activity.map_or_else(
        || base.into(),
        |activity| format!("{base} {activity}").into(),
    )
}

fn live_status_text(context: RenderContext, frame: u64, width: f32) -> String {
    let glyph = sticky_glyph_with_context(context, frame);
    if width.is_finite() && width >= 640.0 {
        format!(
            "{:<12}  {}  | Volatile",
            glyph,
            live_behavior_explanation(context)
        )
    } else {
        format!("{:<12}  Volatile", glyph)
    }
}

fn live_semantic_text(context: RenderContext) -> String {
    // No sprite/frame/transition text: accessibility describes the current
    // authoritative behavior, without a live-region announcement every tick.
    format!(
        "ASCII Organism: {} Memory is volatile.",
        live_behavior_explanation(context)
    )
}

fn live_semantic_rect(rect: egui::Rect, clip: egui::Rect) -> Option<egui::Rect> {
    if !rect.is_finite() || !clip.is_finite() {
        return None;
    }
    let visible = rect.intersect(clip);
    visible.is_positive().then_some(visible)
}

fn live_semantic_response(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    context: RenderContext,
) -> Option<egui::Response> {
    let rect = live_semantic_rect(rect, ui.clip_rect())?;
    let response = ui.interact(
        rect,
        ui.make_persistent_id("ascii_organism_semantics"),
        egui::Sense::hover(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Label,
            ui.is_enabled(),
            live_semantic_text(context),
        )
    });
    Some(response)
}

fn live_tooltip_allowed(running: bool, buttons_down: bool, keys_down: bool) -> bool {
    !running && !buttons_down && !keys_down
}

/// Content-free lifecycle edge detector. Disabled observers still advance their
/// baseline, so enabling a companion never replays an old Agent reward.
#[derive(Debug, Default)]
struct AgentPulseTracker {
    present: bool,
    last: Option<jterm_core::organism::AgentPulse>,
}

impl AgentPulseTracker {
    fn observe(
        &mut self,
        state: Option<jterm_core::agent::AgentState>,
        busy: bool,
        enabled: bool,
    ) -> Option<jterm_core::organism::AgentPulse> {
        let present = state.is_some();
        let pulse = state.and_then(|state| jterm_core::organism::agent_state_pulse(state, busy));
        let pulse = if !present && self.present {
            Some(jterm_core::organism::AgentPulse::Gone)
        } else {
            pulse
        };
        let changed = pulse != self.last;
        self.present = present;
        // Preserve Gone across an absent session to avoid replaying a close.
        if present || pulse.is_some() {
            self.last = pulse;
        }
        if enabled && changed {
            pulse
        } else {
            None
        }
    }
}

fn system_motion_id() -> egui::Id {
    egui::Id::new("organism_system_reduced_motion")
}

fn publish_system_motion(state: &std::sync::atomic::AtomicBool, next: bool) -> bool {
    state.swap(next, Ordering::AcqRel) != next
}

/// One subscription per window, created on its UI thread and canceled on drop.
/// The callback coalesces state; it never touches command/life/owner state.
#[derive(Default)]
struct SystemMotion {
    reduced: std::sync::Arc<std::sync::atomic::AtomicBool>,
    subscription: Option<mundy::Subscription>,
}

impl SystemMotion {
    fn reduced(&self) -> bool {
        self.reduced.load(Ordering::Acquire)
    }

    fn connect(&mut self, ctx: &egui::Context) {
        if self.subscription.is_some() {
            return;
        }
        let reduced = self.reduced.clone();
        let ctx = ctx.clone();
        self.subscription = Some(mundy::Preferences::subscribe(
            mundy::Interest::ReducedMotion,
            move |preferences| {
                let next = preferences.reduced_motion.is_reduce();
                if publish_system_motion(&reduced, next) {
                    // eframe may deliver one old delayed repaint. This immediate
                    // frame re-evaluates existing policy and never rearms animation
                    // for reduced Automatic; no cancellation API is assumed.
                    ctx.request_repaint();
                }
            },
        ));
    }
}

pub struct OrganismHost {
    system_motion: SystemMotion,

    agent_pulses: AgentPulseTracker,
    born: Instant,
    life: WindowLife,
    native: NativeOrganism,
    owner: Option<String>,
    presentable: bool,
    cursor: Option<(u64, bool)>,
    quarantine_batch: bool,
    running: bool,
    window_running: bool,
    owner_activity_at: Duration,
    activity: LiveActivityKnowledge,
    pending: Option<PendingCommand>,
    work_context: Option<String>,
    remote_checked: Option<(String, Instant, bool)>,
    last_input: Option<Duration>,
    ambient: AmbientMind,
    context: RenderContext,
    reaction_until: Duration,
    attention: LiveAttention<String>,
    pointer_inside: bool,
    native_surface_identity: Option<(String, String, crate::terminal::ProjectionCacheKey)>,
    native_surface_rect: Option<egui::Rect>,
    native_surface_region: Option<egui::Rect>,
    native_surface_frame: Option<u64>,
    native_surface_walking: bool,
    palette: NativePalette,
    reaction_status: String,
    watch: WatchAdapter,
    bridge: PaintedBridge,
    bridge_mode: bool,
    rhythm_enabled: bool,
}

impl Default for OrganismHost {
    fn default() -> Self {
        Self {
            system_motion: SystemMotion::default(),
            agent_pulses: Default::default(),
            born: Instant::now(),
            life: WindowLife::new_at(Duration::ZERO),
            native: NativeOrganism::from_persisted_state(crate::organism::LifeState::default()),
            owner: None,
            presentable: false,
            cursor: None,
            quarantine_batch: false,
            running: false,
            window_running: false,
            owner_activity_at: Duration::ZERO,
            activity: LiveActivityKnowledge::Unknown,
            pending: None,
            work_context: None,
            remote_checked: None,
            last_input: None,
            ambient: AmbientMind::seeded(0x656d626572),
            context: PreviewPose::Calm.context(),
            reaction_until: Duration::ZERO,
            attention: LiveAttention::default(),
            pointer_inside: false,
            native_surface_identity: None,
            native_surface_rect: None,
            native_surface_region: None,
            native_surface_frame: None,
            native_surface_walking: false,
            palette: NativePalette::default(),
            reaction_status: "quiet · waiting for a real Block event".into(),
            watch: WatchAdapter::default(),
            bridge: PaintedBridge::default(),
            bridge_mode: false,
            rhythm_enabled: false,
        }
    }
}

impl OrganismHost {
    fn motion(&self, choice: Option<OrganismMotion>) -> OrganismMotion {
        crate::organism::resolved_motion_with_system(choice, self.system_motion.reduced())
    }

    fn configure_bridge(&mut self, active: bool) {
        if self.bridge_mode != active {
            self.bridge.reset();
        }
        self.bridge_mode = active;
    }

    fn set_available_geometry(&mut self, available: egui::Rect, clip: egui::Rect) {
        self.presentable = host_geometry_presentable(available, clip);
        if !self.presentable {
            self.acquire(None, None, false);
        }
    }

    /// Revoke the old session before accepting a new one. Acquiring a running
    /// command quarantines it; it must not manufacture a Start or completion.
    pub fn acquire(&mut self, owner: Option<&str>, tail: Option<(u64, bool)>, running: bool) {
        let now = self.born.elapsed();
        if self.owner.as_deref() == owner {
            self.running = owner.is_some() && running;
            if self.running {
                self.activity = LiveActivityKnowledge::Busy;
            } else if self.activity == LiveActivityKnowledge::Busy {
                self.activity = LiveActivityKnowledge::Unknown;
            }
            if !self.running {
                self.watch.reset();
            }
            return;
        }
        self.watch.reset();
        self.bridge.reset();
        self.life
            .advance(now, false, false, CircadianPhase::Unlearned);
        self.attention.set_owner(owner.map(str::to_owned));
        self.owner = owner.map(str::to_owned);
        self.cursor = tail;
        self.quarantine_batch = owner.is_some();
        self.running = owner.is_some() && running;
        self.activity = if self.running {
            LiveActivityKnowledge::Busy
        } else {
            LiveActivityKnowledge::Unknown
        };
        self.pending = None;
        self.work_context = None;
        self.native = NativeOrganism::from_persisted_state(self.life.state());
        self.ambient.interrupt();
        self.context = PreviewPose::Calm.context();
        self.reaction_until = now;
    }

    fn cancel_attention(&mut self) {
        if let Some(owner) = self.owner.as_ref() {
            self.attention.cancel(owner);
        }
    }

    fn request_hello(
        &mut self,
        available: bool,
        target: Option<&str>,
        config: &Config,
    ) -> Result<(), &'static str> {
        self.request_hello_available_at(self.born.elapsed(), available, target, config)
    }

    fn request_hello_available_at(
        &mut self,
        now: Duration,
        available: bool,
        target: Option<&str>,
        config: &Config,
    ) -> Result<(), &'static str> {
        if !available {
            return Err("ASCII Organism is giving the window or pointer priority");
        }
        self.request_hello_at(now, target, config)
    }

    fn request_hello_at(
        &mut self,
        now: Duration,
        target: Option<&str>,
        config: &Config,
    ) -> Result<(), &'static str> {
        if !config.ascii_organism_enabled {
            return Err("Enable ASCII Organism before saying hello");
        }
        if self.motion(config.ascii_organism_motion) == OrganismMotion::Static {
            return Err("Choose Full or Calm to say hello");
        }
        if target.is_none() || target != self.owner.as_deref() || !self.presentable {
            return Err("ASCII Organism is unavailable for the original terminal");
        }
        let policy = PresentationPolicy {
            enabled: true,
            focused_owner: true,
            local: true,
            alternate_screen: false,
            motion: Some(self.motion(config.ascii_organism_motion)),
            last_input: self.last_input,
        };
        if self.running || !policy.inline_visible(now) {
            return Err("ASCII Organism is giving terminal activity priority");
        }
        let owner = self
            .owner
            .as_ref()
            .ok_or("ASCII Organism is unavailable for the original terminal")?;
        let input = LiveAttentionInput {
            base_context: self.context,
            activity: self.activity,
            hard_eligible: true,
            hover_available: self.presentable,
            pointer_inside: self.pointer_inside,
        };
        let snapshot = self
            .attention
            .advance(now, owner, input, LiveAttentionEvent::Explicit)
            .map_err(|_| "ASCII Organism is unavailable for the original terminal")?;
        match snapshot.outcome {
            LiveAttentionOutcome::Accepted(_) => Ok(()),
            LiveAttentionOutcome::Rejected(LiveAttentionRejection::BusyContext) => {
                Err("ASCII Organism is busy with its current reaction")
            }
            LiveAttentionOutcome::Rejected(LiveAttentionRejection::Cooldown) => {
                Err("ASCII Organism is cooling down; try again later")
            }
            _ => Err("ASCII Organism is unavailable for the original terminal"),
        }
    }

    pub fn accepted_input(&mut self, session: &str) {
        if self.owner.as_deref() == Some(session) {
            let now = self.born.elapsed();
            self.cancel_attention();
            self.activity = if self.running {
                LiveActivityKnowledge::Busy
            } else {
                LiveActivityKnowledge::Unknown
            };
            self.watch.reset();
            self.bridge.reset();
            self.last_input = Some(now);
            self.owner_activity_at = now;
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
        self.cancel_attention();
        self.owner_activity_at = now;
        self.life.note_output(now);
        self.ambient.interrupt();
        let running = records.back().is_some_and(|record| {
            record.state == crate::terminal::CommandState::Running && record.start_mark_seen
        });
        self.running = running;
        // This parsed batch may have changed the prompt. Re-establish clean
        // idle evidence in prepare; lack of a Running record is insufficient.
        self.activity = if running {
            LiveActivityKnowledge::Busy
        } else {
            LiveActivityKnowledge::Unknown
        };
        if !running {
            self.watch.reset();
        }
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
            self.native
                .set_agent_command(record.agent_generation.is_some());
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

    fn watch_visible(&self, now: Duration) -> bool {
        self.owner.is_some()
            && !self
                .last_input
                .is_some_and(|last| now.saturating_sub(last) < crate::organism::INPUT_RETREAT)
    }

    pub fn watch_parked(&mut self, session: &str) {
        if self.owner.as_deref() == Some(session) {
            self.watch.quarantine(self.born.elapsed());
        }
    }

    pub fn watch_parsed(
        &mut self,
        session: &str,
        update: crate::terminal::WatchParsed,
        backlogged: bool,
    ) {
        if self.owner.as_deref() != Some(session) {
            return;
        }
        let now = self.born.elapsed();
        if !self.watch_visible(now) {
            self.watch.reset();
            return;
        }
        if !self.rhythm_enabled {
            self.watch.attach(now, update.command);
            return;
        }
        let read_at = update
            .origin
            .map(|origin| origin.read_at)
            .map(|at| at.saturating_duration_since(self.born))
            .unwrap_or(Duration::ZERO);
        self.watch
            .parsed(now, read_at, update, backlogged || self.quarantine_batch);
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
        self.native
            .set_agent_command(completed.agent_generation.is_some());
        let reaction = self
            .native
            .command_finished(pending.kind, exit, completed.duration_ms);
        self.life.replace_state(self.native.state());
        self.react(reaction, now);
    }

    fn react(&mut self, reaction: Reaction, now: Duration) {
        self.cancel_attention();
        self.ambient.interrupt();
        self.context = RenderContext::new(
            reaction.behavior,
            BodyLanguage::from_state(self.life.state()),
            false,
        );
        self.reaction_status = reaction.description.clone();
        self.reaction_until = now + reaction.hold_duration();
    }

    /// Paint-only native overlay. The renderer supplies same-frame, proven
    /// empty active-card space. No widget, focus request, PTY write or timer.
    fn draw_native_body(
        &mut self,
        ui: &mut Ui,
        config: &Config,
        surface: Option<(
            String,
            String,
            crate::terminal::ProjectionCacheKey,
            egui::Rect,
        )>,
        interactive: bool,
    ) {
        if !config.command_cards_enabled() {
            if self.native_surface_identity.take().is_some() {
                self.cancel_attention();
            }
            self.native_surface_rect = None;
            self.native_surface_region = None;
            self.native_surface_frame = None;
            self.native_surface_walking = false;
            return;
        }
        let now = self.born.elapsed();
        let policy = PresentationPolicy {
            enabled: config.ascii_organism_enabled,
            focused_owner: interactive && self.owner.is_some(),
            local: true,
            alternate_screen: false,
            motion: Some(self.motion(config.ascii_organism_motion)),
            last_input: self.last_input,
        };
        let full = self.motion(config.ascii_organism_motion) == OrganismMotion::Full;
        let frame = if full {
            now.as_millis() as u64 / 100
        } else {
            0
        };
        let context = if self.running {
            self.watch.context(
                now,
                BodyLanguage::from_state(self.life.state()),
                self.rhythm_enabled,
            )
        } else {
            self.context
        };
        let surface = surface.filter(|(session, _, _, region)| {
            config.command_cards_enabled()
                && policy.body_visible(now)
                && self.owner.as_deref() == Some(session.as_str())
                && region.is_finite()
                && region.is_positive()
                && ui.clip_rect().contains_rect(*region)
        });
        if surface.is_none() {
            self.cancel_attention();
            self.native_surface_identity = None;
            self.native_surface_rect = None;
            self.native_surface_region = None;
            self.native_surface_frame = None;
            self.native_surface_walking = false;
            self.pointer_inside = false;
            return;
        }
        let (pointer, buttons_down, events) = ui.ctx().input(|input| {
            (
                input.pointer.hover_pos(),
                input.pointer.any_down() || input.pointer.any_pressed(),
                input
                    .events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            egui::Event::PointerMoved(_) | egui::Event::PointerGone
                        )
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        });
        let palette = self.palette;
        let layout = |context: RenderContext| {
            ui.painter().layout_no_wrap(
                sprite_frame_with_context(context, frame).into_owned(),
                organism_font(ui, config.font_size),
                palette.body(context.behavior),
            )
        };
        // Measure native text; neither terminal font scaling nor a narrower
        // pane may silently clip a body over the input row.
        let base_galley = layout(context);
        // Keep a measured twelve-column envelope across all shared ASCII poses.
        // This includes the reference 6x3 padding and 1px border on every side.
        let envelope = ui.painter().layout_no_wrap(
            "MMMMMMMMMMMM\nMMMMMMMMMMMM\nMMMMMMMMMMMM".into(),
            organism_font(ui, config.font_size),
            self.palette.quiet,
        );
        let size = envelope.size().max(base_galley.size()) + egui::vec2(14.0, 8.0);
        let identity = surface
            .as_ref()
            .map(|(session, block, projection, _)| (session.clone(), block.clone(), *projection));
        let region = surface.as_ref().map(|(_, _, _, region)| *region);
        let geometry_changed =
            self.native_surface_identity != identity || self.native_surface_region != region;
        let previous = (!geometry_changed)
            .then_some(self.native_surface_rect)
            .flatten();
        // Hover freezes even during cooldown: a visit never has to chase the cat.
        let pointer_over_previous =
            previous.is_some_and(|rect| pointer.is_some_and(|point| rect.contains(point)));
        let wandering = full
            && !self.running
            && now >= self.reaction_until
            && !context.body_language.drowsy
            && matches!(context.behavior, Behavior::Idle | Behavior::Explore);
        let mut target = region
            .and_then(|region| {
                if pointer_over_previous
                    && previous
                        .is_some_and(|rect| region.contains_rect(rect) && rect.size() == size)
                {
                    previous
                } else {
                    native_placement(region, size, context, frame, wandering)
                }
            })
            .filter(|_| !buttons_down);
        let new_frame = self.native_surface_frame != Some(frame);
        if full && !pointer_over_previous {
            if let Some((previous, desired)) = previous.zip(target) {
                let cell_width = envelope.size().x / 12.0;
                target = Some(native_walk_step(previous, desired, cell_width, new_frame));
            }
        }
        let changed = geometry_changed || self.native_surface_rect != target;
        if changed {
            self.cancel_attention();
            // Neither wandering nor a resized surface can manufacture entry
            // underneath a stationary pointer. A physical exit/re-entry is required.
            self.pointer_inside =
                target.is_some_and(|rect| pointer.is_some_and(|point| rect.contains(point)));
        }
        let walking = full
            && wandering
            && !pointer_over_previous
            && !geometry_changed
            && if new_frame {
                previous
                    .zip(target)
                    .is_some_and(|(old, new)| old.min != new.min)
            } else {
                self.native_surface_walking
            };
        self.native_surface_identity = identity;
        self.native_surface_region = region;
        self.native_surface_rect = target;
        self.native_surface_frame = target.map(|_| frame);
        self.native_surface_walking = walking;
        let Some(owner) = self.owner.as_ref() else {
            return;
        };
        let target = target.unwrap_or(egui::Rect::NOTHING);
        let snapshot = observe_live_attention(
            &mut self.attention,
            &mut self.pointer_inside,
            now,
            owner,
            LiveAttentionInput {
                base_context: context,
                activity: self.activity,
                hard_eligible: policy.body_visible(now) && !buttons_down && target.is_positive(),
                hover_available: target.is_positive()
                    && resolved_motion(policy.motion) != OrganismMotion::Static,
                pointer_inside: pointer.is_some_and(|p| target.contains(p)),
            },
            target,
            if changed { &[] } else { &events },
        );
        if target.is_positive() {
            let mut display = snapshot.map_or(context, |value| value.presentation);
            display.walking = walking;
            let galley = layout(display);
            let inner = target.shrink2(egui::vec2(7.0, 4.0));
            if let Some((_, _, _, region)) = surface {
                if galley.size().x <= inner.width() && galley.size().y <= inner.height() {
                    let painter = ui.painter().with_clip_rect(region);
                    let color = self.palette.body(display.behavior);
                    let border = if snapshot.is_some_and(|value| value.candidate_visible)
                        || pointer.is_some_and(|point| target.contains(point))
                    {
                        alpha(self.palette.foreground, 115)
                    } else {
                        alpha(
                            color,
                            if matches!(
                                display.behavior,
                                Behavior::InspectError | Behavior::SitNearError
                            ) {
                                140 // Reference error border: .55.
                            } else if color == self.palette.quiet {
                                82
                            } else {
                                128
                            },
                        )
                    };
                    // Immediate-mode decoration: each frame submits only its
                    // current rectangle. Neither terminal cells nor GPU grid
                    // caches contain the old body, so movement leaves no trail.
                    painter.rect(
                        target,
                        6.0,
                        alpha(self.palette.background, 204),
                        egui::Stroke::new(1.0, border),
                        egui::StrokeKind::Inside,
                    );
                    painter.galley(inner.min, galley, color);
                } else {
                    self.cancel_attention();
                    self.native_surface_rect = None;
                }
            }
        }
        // Reconcile the same shared cadence as traditional chrome after this
        // native body has accepted/expired attention. A dormant Block/Chat
        // owner needs Full frames during a visit, not only its hold deadline.
        if let Some(delay) = self.native_body_wake(policy, now, snapshot) {
            ui.ctx().request_repaint_after(delay);
        }
    }

    fn native_body_wake(
        &self,
        policy: PresentationPolicy,
        now: Duration,
        snapshot: Option<LiveAttentionSnapshot>,
    ) -> Option<Duration> {
        if !policy.body_visible(now) {
            return None;
        }
        next_live_wake(
            policy,
            now,
            self.reaction_until,
            self.owner_dormant(now),
            snapshot,
        )
    }

    /// Persistent status is part of the projected Block document, between
    /// history and the active prompt. The host supplies origin-free rows; this
    /// method creates no terminal text, selection origins or focus ownership.
    fn draw_block_status(
        &self,
        ui: &mut Ui,
        config: &Config,
        surface: Option<(String, egui::Rect)>,
        interactive: bool,
    ) -> bool {
        if !config.command_cards_enabled() || !config.ascii_organism_enabled {
            return false;
        }
        let Some((session, region)) =
            surface.filter(|(_, rect)| rect.is_finite() && rect.is_positive())
        else {
            return false;
        };
        let now = self.born.elapsed();
        let live = self.owner.as_deref() == Some(session.as_str());
        let context = if live && self.running {
            self.watch.context(
                now,
                BodyLanguage::from_state(self.life.state()),
                self.rhythm_enabled,
            )
        } else if live {
            self.context
        } else {
            PreviewPose::Calm.context()
        };
        let display = if live {
            self.attention
                .snapshot(
                    now,
                    &session,
                    LiveAttentionInput {
                        base_context: context,
                        activity: self.activity,
                        hard_eligible: interactive
                            && self.motion(config.ascii_organism_motion) != OrganismMotion::Static,
                        hover_available: self.native_surface_rect.is_some(),
                        pointer_inside: self.pointer_inside,
                    },
                )
                .ok()
                .map_or(context, |snapshot| snapshot.presentation)
        } else {
            context
        };
        let palette = self.palette;
        let color = palette.body(display.behavior);
        let expanded = config.ascii_organism_expanded;
        let card = region.shrink2(egui::vec2(0.0, if expanded { 3.0 } else { 1.0 }));
        let clip = card.intersect(ui.clip_rect());
        if !clip.is_positive() {
            return false;
        }
        let painter = ui.painter().with_clip_rect(clip);
        let (fill_alpha, border_alpha) = match display.behavior {
            Behavior::WatchCommand | Behavior::WatchSettled | Behavior::WatchAgent => (18, 230),
            Behavior::Celebrate | Behavior::CelebrateBig | Behavior::RestAfterPush => (20, 140),
            Behavior::InspectError | Behavior::SitNearError => (26, 178),
            Behavior::UnknownOutcome => (18, 158),
            _ => (9, 178),
        };
        painter.rect(
            card,
            6.0,
            alpha(color, fill_alpha),
            egui::Stroke::new(1.0, alpha(color, border_alpha)),
            egui::StrokeKind::Inside,
        );
        let stripe =
            egui::Rect::from_min_max(card.min, egui::pos2(card.left() + 2.0, card.bottom()));
        painter.rect_filled(stripe, 1.0, alpha(color, 178));
        let inner = card.shrink2(egui::vec2(12.0, if expanded { 8.0 } else { 0.0 }));
        if !inner.is_positive() {
            return false;
        }
        let control = egui::Rect::from_min_max(
            egui::pos2((inner.right() - 24.0).max(inner.left()), inner.top()),
            inner.max,
        );
        // A press belongs to this inline card at this exact allocation. It
        // cannot migrate to the footer, another session, or a resized slot.
        let toggle = ui
            .push_id(
                (
                    "ascii-organism-block-status",
                    &session,
                    region.min.x.to_bits(),
                    region.min.y.to_bits(),
                    region.max.x.to_bits(),
                    region.max.y.to_bits(),
                ),
                |ui| companion_toggle(ui, control, expanded, interactive),
            )
            .inner;
        let text_right = (control.left() - 8.0).max(inner.left());
        // The upper status card uses the reference's quieter 0.82 scale;
        // only the independent live body uses the full terminal font size.
        let font_size = Config::clamp_font_size(config.font_size) * 0.82;
        let font_size = if expanded {
            font_size
        } else {
            font_size.min(inner.height() / 1.2)
        };
        let full = self.motion(config.ascii_organism_motion) == OrganismMotion::Full;
        let frame = if live && full {
            now.as_millis() as u64 / 100
        } else {
            0
        };
        let line_height = font_size * 1.2;
        let line = |rect: egui::Rect, text: String, size: f32, color, mono: bool| {
            let clip = rect.intersect(clip);
            if !clip.is_positive() || rect.width() < 8.0 {
                return;
            }
            let mut job = egui::text::LayoutJob::simple(
                text,
                if mono {
                    egui::FontId::monospace(size)
                } else {
                    egui::FontId::proportional(size)
                },
                color,
                rect.width(),
            );
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
            painter.with_clip_rect(clip).galley(rect.min, galley, color);
        };
        if !expanded {
            line(
                egui::Rect::from_min_max(inner.min, egui::pos2(text_right, inner.bottom())),
                format!(
                    "{}  ASCII organism · {}",
                    sticky_glyph_with_context(display, frame),
                    live_behavior_explanation(display)
                ),
                font_size,
                color,
                true,
            );
            return toggle;
        }
        let sprite = ui.painter().layout_no_wrap(
            sprite_frame_with_context(display, frame).into_owned(),
            organism_font(ui, font_size),
            color,
        );
        let body_width = ui
            .painter()
            .layout_no_wrap("MMMMMMMMMMMM".into(), organism_font(ui, font_size), color)
            .size()
            .x;
        let body = egui::Rect::from_min_max(
            inner.min,
            egui::pos2((inner.left() + body_width).min(text_right), inner.bottom()),
        );
        if sprite.size().x <= body.width() && sprite.size().y <= body.height() {
            painter.with_clip_rect(body.intersect(clip)).galley(
                egui::pos2(body.left(), body.center().y - sprite.size().y * 0.5),
                sprite,
                color,
            );
        }
        let details = egui::Rect::from_min_max(
            egui::pos2((body.right() + 12.0).min(text_right), inner.top()),
            egui::pos2(text_right, inner.bottom()),
        );
        let row = |index: f32| {
            egui::Rect::from_min_size(
                details.min + egui::vec2(0.0, index * line_height),
                egui::vec2(details.width(), line_height),
            )
            .intersect(details)
        };
        let badge = "volatile · no LLM";
        let badge_size = font_size * 0.82;
        let badge_width = painter
            .layout_no_wrap(
                badge.into(),
                egui::FontId::monospace(badge_size),
                palette.foreground,
            )
            .size()
            .x;
        let title_width = painter
            .layout_no_wrap(
                "ASCII organism".into(),
                egui::FontId::proportional(font_size),
                palette.foreground,
            )
            .size()
            .x;
        let wide = details.width() >= title_width + badge_width + 16.0;
        let mut title = row(0.0);
        if wide {
            title.max.x = title.min.x + title_width;
            let badge_rect = egui::Rect::from_min_max(
                egui::pos2(details.right() - badge_width, details.top()),
                egui::pos2(details.right(), details.top() + line_height),
            );
            line(
                badge_rect,
                badge.into(),
                badge_size,
                alpha(palette.foreground, 140),
                true,
            );
        }
        line(
            title,
            "ASCII organism".into(),
            font_size,
            palette.foreground,
            false,
        );
        let status = if !live {
            "quiet · live reactions paused".to_owned()
        } else if self.running {
            "watching · command active".to_owned()
        } else if now < self.reaction_until {
            self.reaction_status.clone()
        } else if self.cursor.is_none() {
            "quiet · waiting for a real Block event".to_owned()
        } else {
            "quiet · between commands".to_owned()
        };
        let status_color = match display.behavior {
            Behavior::Celebrate
            | Behavior::CelebrateBig
            | Behavior::RestAfterPush
            | Behavior::InspectError
            | Behavior::SitNearError => color,
            _ => palette.foreground,
        };
        line(row(1.0), status, font_size, status_color, false);
        let mut caption = live_behavior_explanation(display).into_owned();
        if display.body_language.drowsy {
            caption.push_str(" · sleepy");
        }
        if display.body_language.tense {
            caption.push_str(" · subdued");
        }
        if display.body_language.listless {
            caption.push_str(" · curious");
        }
        if !wide {
            caption.push_str(" · volatile");
        }
        line(
            row(2.0),
            caption,
            badge_size,
            alpha(palette.foreground, 140),
            true,
        );
        // Passive semantic surface only. The live body's bounded rectangle is
        // the sole shared greeting target and never captures terminal input.
        let semantic = egui::Rect::from_min_max(inner.min, egui::pos2(text_right, inner.bottom()));
        if let Some(response) = live_semantic_response(ui, semantic, display) {
            response.on_hover_text("ASCII organism · volatile memory · no LLM");
        }
        toggle
    }

    fn advance_life(&mut self, now: Duration, eligible: bool) -> f32 {
        if self.running {
            self.owner_activity_at = now;
        }
        self.life.advance(
            now,
            eligible,
            self.window_running || self.running,
            CircadianPhase::Unlearned,
        )
    }

    fn owner_idle_for(&self, now: Duration) -> Duration {
        now.saturating_sub(self.owner_activity_at)
    }

    fn owner_dormant(&self, now: Duration) -> bool {
        !self.running && self.owner_idle_for(now) >= Duration::from_secs(60)
    }

    pub fn draw(&mut self, ui: &mut Ui, config: &Config, focused: bool, interactive: bool) -> bool {
        let now = self.born.elapsed();
        let presentable = self.presentable;
        if !presentable {
            self.acquire(None, None, false);
        }
        let eligible =
            config.ascii_organism_enabled && focused && presentable && self.owner.is_some();
        let dt = self.advance_life(now, eligible);
        if !config.ascii_organism_enabled {
            self.acquire(None, None, false);
            return false;
        }
        let policy = PresentationPolicy {
            enabled: true,
            focused_owner: eligible,
            local: true,
            alternate_screen: false,
            motion: Some(self.motion(config.ascii_organism_motion)),
            last_input: self.last_input,
        };
        if !policy.inline_visible(now) {
            self.bridge.reset();
        }
        if eligible && now >= self.reaction_until && !self.running {
            let behavior = self.ambient.step(
                self.life.state(),
                self.owner_idle_for(now).as_secs_f32(),
                dt,
                RepoVigil::None,
            );
            self.context = RenderContext::new(
                behavior.display(),
                BodyLanguage::from_state(self.life.state()),
                false,
            );
        }
        let full = self.motion(config.ascii_organism_motion) == OrganismMotion::Full;
        if !config.ascii_organism_expanded || !full {
            self.bridge.reset();
        }
        let frame = if full {
            now.as_millis() as u64 / 100
        } else {
            0
        };
        let context = if self.running {
            self.watch.context(
                now,
                BodyLanguage::from_state(self.life.state()),
                self.rhythm_enabled,
            )
        } else {
            self.context
        };
        if config.command_cards_enabled() {
            // Block mode owns real display-only rows immediately above input.
            // Do not reserve a duplicate bottom panel or make footer hover the
            // only way to see the organism. Physiology still advances above.
            let dormant = self.owner_dormant(now);
            let next = next_host_wake(policy, now, self.reaction_until, dormant);
            let watch_wake = self
                .rhythm_enabled
                .then(|| self.watch.wake_after(now))
                .flatten();
            if let Some(delay) = next.into_iter().chain(watch_wake).min() {
                ui.ctx().request_repaint_after(delay);
            }
            return false;
        }
        let (pointer, buttons_down, pointer_events) = ui.ctx().input(|input| {
            (
                input.pointer.hover_pos(),
                input.pointer.any_down() || input.pointer.any_pressed(),
                input
                    .events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            egui::Event::PointerMoved(_) | egui::Event::PointerGone
                        )
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        });
        let greeting_allowed = live_greeting_allowed(policy, now, self.running, buttons_down);
        let dormant = self.owner_dormant(now);
        let mut attention_snapshot = None;
        let mut toggle = false;
        // Allocate exactly the same strip while suppressed. Typing, switching
        // panes and alternate screen must not change the terminal's grid size.
        egui::Panel::bottom("ascii_organism_chrome")
            .exact_size(host_height(
                config.ascii_organism_enabled,
                config.ascii_organism_expanded,
            ))
            .frame(egui::Frame::NONE.inner_margin(0.0))
            .resizable(false)
            .show(ui, |ui| {
                let rect = ui
                    .available_rect_before_wrap()
                    .shrink2(egui::vec2(8.0, 0.0));
                // Observe only the bounded left body for greetings. The separate
                // right control never requests focus or sends terminal input.
                let expanded = config.ascii_organism_expanded;
                let layout = companion_layout(rect, expanded);
                let body_rect = layout.body;
                let status_rect = if expanded {
                    layout.details
                } else {
                    egui::Rect::from_min_max(
                        layout.body.min,
                        egui::pos2(layout.control.min.x - 8.0, layout.body.max.y),
                    )
                };
                let hover_rect = body_rect;
                if expanded && layout.card.is_positive() {
                    ui.painter().with_clip_rect(ui.clip_rect()).rect(
                        layout.card,
                        6.0,
                        ui.visuals().faint_bg_color,
                        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
                        egui::StrokeKind::Inside,
                    );
                }
                toggle |= companion_toggle(ui, layout.control, expanded, interactive);
                let near = pointer.is_some_and(|point| hover_rect.contains(point));
                if let Some(owner) = self.owner.as_ref() {
                    attention_snapshot = if config.command_cards_enabled() {
                        // Native Block owns passive attention. Chrome reads the
                        // one shared model without fabricating another visit.
                        self.attention.snapshot(now, owner, LiveAttentionInput {
                            base_context: context,
                            activity: self.activity,
                            hard_eligible: greeting_allowed,
                            hover_available: self.native_surface_rect.is_some(),
                            pointer_inside: self.pointer_inside,
                        }).ok()
                    } else { observe_live_attention(
                        &mut self.attention,
                        &mut self.pointer_inside,
                        now,
                        owner,
                        LiveAttentionInput {
                            base_context: context,
                            activity: self.activity,
                            hard_eligible: greeting_allowed,
                            hover_available: hover_rect.intersect(ui.clip_rect()).is_positive(),
                            pointer_inside: near,
                        },
                        hover_rect,
                        &pointer_events,
                    ) };
                } else {
                    self.pointer_inside = near;
                }
                if policy.inline_visible(now) {
                    let display =
                        attention_snapshot.map_or(context, |snapshot| snapshot.presentation);
                    if attention_snapshot.is_some_and(|snapshot| snapshot.candidate_visible) {
                        if let Some(cue) = hover_cue_rect(hover_rect, ui.clip_rect()) {
                            ui.painter()
                                .rect_filled(cue, 0.0, ui.visuals().weak_text_color());
                        }
                    }
                    let (sprite, status) = host_text(display, frame, rect.width(), expanded);
                    let rect = egui::Rect::from_min_max(layout.body.min, egui::pos2(layout.control.min.x - 8.0, layout.body.max.y));
                    if let Some(response) = live_semantic_response(ui, rect, display) {
                        if live_tooltip_allowed(
                            self.running,
                            buttons_down,
                            ui.ctx().input(|input| !input.keys_down.is_empty()),
                        ) {
                            // Native hover-delay redraw only; no organism timer.
                            response.on_hover_text(live_semantic_text(display));
                        }
                    }
                    if let Some(sprite) = sprite {
                        let clip = body_rect.intersect(ui.clip_rect());
                        let visible = clip.is_positive();
                        let greeting =
                            attention_snapshot.is_some_and(|snapshot| snapshot.active.is_some());
                        let plan = if bridge_allowed(expanded, full, visible, greeting) {
                            Some(self.bridge.plan_at_cadence(now, display, frame, !dormant))
                        } else {
                            self.bridge.reset();
                            None
                        };
                        if visible {
                            let sprite = plan.as_ref().map_or(sprite, |plan| {
                                sprite_frame_with_context(plan.context, plan.frame)
                            });
                            let text_rect = ui.painter().with_clip_rect(clip).text(
                                body_rect.min,
                                egui::Align2::LEFT_TOP,
                                sprite,
                                egui::FontId::monospace(14.0),
                                ui.visuals().text_color(),
                            );
                            if submitted_text_visible(text_rect, clip) {
                                if let Some(plan) = plan {
                                    self.bridge.submitted(plan);
                                }
                            } else {
                                self.bridge.reset();
                            }
                        }
                    }
                    if expanded {
                        companion_details(ui, status_rect, live_behavior_explanation(display).into_owned());
                    } else {
                        ui.painter()
                            .with_clip_rect(status_rect.intersect(ui.clip_rect()))
                            .text(
                                status_rect.left_center(),
                                egui::Align2::LEFT_CENTER,
                                status,
                                egui::TextStyle::Monospace.resolve(ui.style()),
                                ui.visuals().text_color(),
                            );
                    }
                } else {
                    // Paint-only companion: never an owner, idle fact or greeting.
                    // It does not consult physiology or start a new repaint timer.
                    let semantic_rect = egui::Rect::from_min_max(layout.body.min, egui::pos2(layout.control.min.x - 8.0, layout.body.max.y));
                    if let Some(rect) = live_semantic_rect(semantic_rect, ui.clip_rect()) {
                        let response = ui.interact(rect, ui.make_persistent_id("ascii_organism_passive_semantics"), egui::Sense::hover());
                        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, false, "ASCII organism. Quietly here. Live reactions are paused. Memory is volatile."));
                    }
                    let context = PreviewPose::Calm.context();
                    let text = if expanded {
                        sprite_frame_with_context(context, 0)
                    } else {
                        sticky_glyph_with_context(context, 0)
                    };
                    let clip = body_rect.intersect(ui.clip_rect());
                    if clip.is_positive() {
                        ui.painter().with_clip_rect(clip).text(
                            if expanded { body_rect.min } else { body_rect.left_center() },
                            if expanded { egui::Align2::LEFT_TOP } else { egui::Align2::LEFT_CENTER },
                            text,
                            egui::FontId::monospace(14.0),
                            ui.visuals().weak_text_color(),
                        );
                    }
                    if expanded {
                        companion_details(ui, status_rect, "Quietly here. Live reactions are paused.");
                    }
                }
            });
        let next = next_live_wake(
            policy,
            self.born.elapsed(),
            self.reaction_until,
            dormant,
            attention_snapshot,
        );
        let watch_wake = self
            .rhythm_enabled
            .then(|| self.watch.wake_after(self.born.elapsed()))
            .flatten();
        if let Some(delay) = next.into_iter().chain(watch_wake).min() {
            ui.ctx().request_repaint_after(delay);
        }
        toggle
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

fn hidden_root_viewport(root: bool, focused: Option<bool>, visible: Option<bool>) -> bool {
    root && (!focused.unwrap_or(false) || !visible.unwrap_or(true))
}

impl crate::TerminalApp {
    /// Recompute from current sessions after parsing and close handling. This
    /// never blocks on a renderer's terminal lock or adds a process probe.
    pub(crate) fn sync_organism_window_activity(&mut self) {
        self.organism.window_running = self.config.ascii_organism_enabled
            && window_has_known_local_running(self.session_manager.sessions().iter().map(
                |session| {
                    let has_source_shell = session.shell.shell_program().is_some();
                    let running = (session.purpose == crate::session::SessionPurpose::Interactive
                        && has_source_shell)
                        .then(|| {
                            session
                                .terminal
                                .try_lock()
                                .map(|terminal| terminal_has_running_command(&terminal))
                        })
                        .flatten();
                    (session.purpose, has_source_shell, running)
                },
            ));
    }

    /// eframe skips App::ui in an observed hidden root pass, but still calls
    /// App::logic. Revoke only organism state here; never draw or request wakes.
    /// Rapid hide/restore transitions without a hidden pass remain unobserved.
    pub(crate) fn observe_organism_visibility(&mut self, ctx: &egui::Context) {
        let root = ctx.viewport_id() == egui::ViewportId::ROOT;
        let hidden = ctx.input(|input| {
            let viewport = input.viewport();
            hidden_root_viewport(root, viewport.focused, viewport.visible())
        });
        if hidden {
            self.organism.acquire(None, None, false);
            self.config_panel.suspend_organism_preview();
        }
    }

    pub(crate) fn say_hello_to_organism(&mut self, ctx: &egui::Context, target: Option<&str>) {
        // Admission is a single current-state check, never a deferred request.
        let available = ctx.input(|input| {
            let viewport = input.viewport();
            explicit_window_available(
                viewport.focused,
                viewport.visible(),
                input.pointer.any_down(),
                input.pointer.any_pressed(),
            )
        });
        if available {
            self.prepare_organism(ctx);
        }
        match self.organism.request_hello(available, target, &self.config) {
            // Pointer activation may occur after this frame's companion paint.
            // One immediate UI repaint also covers a dormant Calm host.
            Ok(()) => ctx.request_repaint(),
            Err(message) => self.set_status_for(message, Duration::from_secs(3)),
        }
    }

    pub(crate) fn sync_organism_agent(&mut self) {
        let (state, busy) = self.agent_panel.organism_state();
        if let Some(pulse) =
            self.organism
                .agent_pulses
                .observe(state, busy, self.config.ascii_organism_enabled)
        {
            self.organism
                .life
                .replace_state(jterm_core::organism::agent_pulse(
                    self.organism.life.state(),
                    pulse,
                ));
        }
    }

    pub(crate) fn prepare_organism(&mut self, ctx: &egui::Context) {
        self.organism.system_motion.connect(ctx);
        ctx.data_mut(|data| {
            data.insert_temp(system_motion_id(), self.organism.system_motion.reduced())
        });
        self.sync_organism_agent();
        self.organism.configure_bridge(
            self.config.ascii_organism_expanded
                && self.organism.motion(self.config.ascii_organism_motion) == OrganismMotion::Full,
        );
        let rhythm_enabled =
            self.organism.motion(self.config.ascii_organism_motion) != OrganismMotion::Static;
        if self.organism.rhythm_enabled != rhythm_enabled {
            self.organism.watch.reset();
        }
        self.organism.rhythm_enabled = rhythm_enabled;
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
        let input_barrier = self.direct_input_is_blocked_for_session(id);
        let agent_uncertain = self.agent_panel.session_active()
            && self.agent_panel.bound_session_id() == Some(id.as_str());
        let terminal = session.terminal.lock();
        let allowed = local && !terminal.is_alt_buffer_active();
        let running = terminal.command_records().back().is_some_and(|record| {
            record.state == crate::terminal::CommandState::Running && record.start_mark_seen
        });
        let activity = live_activity_knowledge(
            running,
            !running && terminal.prompt_input_is_empty(),
            !session.pending_input.is_empty(),
            input_barrier,
            agent_uncertain,
        );
        self.organism.acquire(
            allowed.then_some(id.as_str()),
            terminal
                .command_records()
                .back()
                .map(|record| (record.sequence, record.start_mark_seen)),
            running,
        );
        if allowed {
            self.organism.activity = activity;
            let now = self.organism.born.elapsed();
            if self.organism.watch_visible(now) {
                self.organism.watch.attach(now, terminal.watch_command());
            } else {
                self.organism.watch.reset();
            }
        }
    }

    pub(crate) fn render_native_organism(
        &mut self,
        ui: &mut Ui,
        surface: Option<(
            String,
            String,
            crate::terminal::ProjectionCacheKey,
            egui::Rect,
        )>,
        status_surface: Option<(String, egui::Rect)>,
        frame_interactive: bool,
    ) {
        // Pane focus may have changed during this same terminal frame.
        self.prepare_organism(ui.ctx());
        let interactive = frame_interactive
            && !self.terminal_input_blocked(ui.ctx())
            && !self.active_terminal_is_read_only();
        self.organism.palette = NativePalette::from_theme(&self.current_theme);
        self.organism
            .draw_native_body(ui, &self.config, surface, interactive);
        if self
            .organism
            .draw_block_status(ui, &self.config, status_surface, interactive)
        {
            self.config.ascii_organism_expanded = !self.config.ascii_organism_expanded;
            self.config_panel.sync_from_config(&self.config);
            self.force_resize_session = true;
            self.schedule_config_save();
            ui.ctx().request_repaint();
        }
    }

    pub(crate) fn render_organism(&mut self, ui: &mut Ui) {
        // Resize events wake the host naturally. Use the actual remaining root
        // layout intersected with its clip, including vertically exhausted UI.
        // Before this layout is observed, input admission still uses the last
        // observed geometry; it does not defer or replay a rejected request.
        self.organism
            .set_available_geometry(ui.available_rect_before_wrap(), ui.clip_rect());
        // Top-bar navigation may have changed the owner since event ingestion.
        self.prepare_organism(ui.ctx());
        // This is after active/background source ingestion. A repaint is only
        // a wake hint; it never carries or revives an old owner/command token.
        if self.organism.rhythm_enabled {
            self.organism.watch.advance(self.organism.born.elapsed());
        }
        let focused = ui
            .ctx()
            .input(|input| input.viewport().focused.unwrap_or(false));
        let interactive = focused
            && !self.terminal_input_blocked(ui.ctx())
            && !self.active_terminal_is_read_only();
        if self.organism.draw(ui, &self.config, focused, interactive) {
            self.config.ascii_organism_expanded = !self.config.ascii_organism_expanded;
            self.config_panel.sync_from_config(&self.config);
            self.force_resize_session = true;
            self.schedule_config_save();
            ui.ctx().request_repaint();
        }
    }
}

/// Accepted Full greetings briefly use the existing frame cadence even after
/// a quiet minute. Candidate attention and cooldown never leave dormant mode.
fn next_live_wake(
    policy: PresentationPolicy,
    now: Duration,
    reaction_until: Duration,
    dormant: bool,
    attention: Option<LiveAttentionSnapshot>,
) -> Option<Duration> {
    let active_full_greeting = policy.inline_visible(now)
        && resolved_motion(policy.motion) == OrganismMotion::Full
        && attention.is_some_and(|snapshot| snapshot.active.is_some());
    [
        next_host_wake(
            policy,
            now,
            reaction_until,
            dormant && !active_full_greeting,
        ),
        attention
            .and_then(|snapshot| snapshot.wake_after(now))
            .map(|delay| delay.max(Duration::from_millis(1))),
    ]
    .into_iter()
    .flatten()
    .min()
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

static NEXT_PREVIEW_REVISION: AtomicU64 = AtomicU64::new(1);

fn allocate_preview_revision(counter: &AtomicU64) -> Option<u64> {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current.checked_add(1)?;
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Some(current),
            Err(observed) => current = observed,
        }
    }
}

struct PreviewSequence {
    started: Option<Duration>,
    paused_at: Option<Duration>,
    paused_total: Duration,
    revision: Option<u64>,
}

impl Default for PreviewSequence {
    fn default() -> Self {
        Self {
            started: None,
            paused_at: None,
            paused_total: Duration::ZERO,
            revision: allocate_preview_revision(&NEXT_PREVIEW_REVISION),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PreviewAction {
    Play,
    Stop,
    Pause,
    Resume,
    Hello,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PreviewTicket {
    revision: Option<u64>,
    action: PreviewAction,
}

impl PreviewTicket {
    fn id(self) -> egui::Id {
        egui::Id::new(("organism_preview_control", self))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DemoStep {
    index: usize,
    pose: PreviewPose,
    next_in: Duration,
    animation_time: Duration,
    paused: bool,
}

impl DemoStep {
    fn next_wake(self, full: bool) -> Option<Duration> {
        if self.paused {
            None
        } else if full {
            Some(self.next_in.min(Duration::from_millis(100)))
        } else {
            Some(self.next_in)
        }
    }

    fn frame(self, full: bool) -> u64 {
        if full {
            self.animation_time.as_millis() as u64 / 100
        } else {
            0
        }
    }
}

impl PreviewSequence {
    const POSES: [PreviewPose; 5] = [
        PreviewPose::Calm,
        PreviewPose::Working,
        PreviewPose::Concerned,
        PreviewPose::Success,
        PreviewPose::Sleeping,
    ];

    fn ticket(&self, action: PreviewAction) -> PreviewTicket {
        PreviewTicket {
            revision: self.revision,
            action,
        }
    }

    fn retire(&mut self) {
        self.started = None;
        self.paused_at = None;
        self.paused_total = Duration::ZERO;
        self.revision = allocate_preview_revision(&NEXT_PREVIEW_REVISION);
    }

    fn start(&mut self, now: Duration) {
        self.retire();
        if self.revision.is_some() {
            self.started = Some(now);
        }
    }

    fn stop(&mut self) {
        if self.started.is_some() {
            self.retire();
        }
    }

    fn pause(&mut self, now: Duration) -> bool {
        if self.sample(now).is_none() || self.paused_at.is_some() {
            return false;
        }
        self.paused_at = Some(now.max(self.started.unwrap()));
        self.revision = allocate_preview_revision(&NEXT_PREVIEW_REVISION);
        if self.revision.is_none() {
            self.started = None;
            self.paused_at = None;
            return false;
        }
        true
    }

    fn resume(&mut self, now: Duration) -> bool {
        if self.sample(now).is_none() {
            return false;
        }
        let Some(paused_at) = self.paused_at.take() else {
            return false;
        };
        self.paused_total = self
            .paused_total
            .saturating_add(now.saturating_sub(paused_at));
        self.revision = allocate_preview_revision(&NEXT_PREVIEW_REVISION);
        if self.revision.is_none() {
            self.started = None;
            self.paused_at = None;
            return false;
        }
        true
    }

    fn sample(&mut self, now: Duration) -> Option<DemoStep> {
        let started = self.started?;
        let clock = self.paused_at.unwrap_or(now).max(started);
        let elapsed = clock
            .saturating_sub(started)
            .saturating_sub(self.paused_total);
        if elapsed >= Duration::from_secs(10) {
            self.stop();
            return None;
        }
        let index = elapsed.as_secs() as usize / 2;
        Some(DemoStep {
            index,
            pose: Self::POSES[index],
            next_in: Duration::from_secs((index as u64 + 1) * 2) - elapsed,
            animation_time: clock.saturating_sub(self.paused_total),
            paused: self.paused_at.is_some(),
        })
    }
}

pub struct PreviewUi {
    model: OrganismPreview,
    demo: PreviewSequence,
    pose: PreviewPose,
    born: Instant,
    greeting_until: Duration,
    hello_ready_at: Duration,
}

impl Default for PreviewUi {
    fn default() -> Self {
        Self {
            model: OrganismPreview::default(),
            demo: PreviewSequence::default(),
            pose: PreviewPose::Calm,
            born: Instant::now(),
            greeting_until: Duration::ZERO,
            hello_ready_at: Duration::ZERO,
        }
    }
}

impl PreviewUi {
    pub fn close(&mut self) {
        if self.model.greeting_availability(self.born.elapsed()) != GreetingAvailability::Closed {
            self.demo.retire();
        }
        self.model.close();
        self.greeting_until = Duration::ZERO;
    }

    fn start_demo(&mut self, now: Duration) {
        // Cancel only the isolated greeting, retaining its real cooldown and
        // the manually selected pose. No live host/model is accessed.
        self.model.close();
        self.model.open();
        self.greeting_until = Duration::ZERO;
        self.demo.start(now);
    }

    fn select_pose(&mut self, pose: PreviewPose) {
        self.demo.retire();
        self.pose = pose;
        self.model.select_pose(pose);
        self.greeting_until = Duration::ZERO;
    }

    fn apply_control(&mut self, ticket: PreviewTicket, now: Duration) -> bool {
        let step = self.demo.sample(now);
        if ticket.revision.is_none()
            || ticket.revision != self.demo.revision
            || self.model.greeting_availability(now) == GreetingAvailability::Closed
        {
            return false;
        }
        match ticket.action {
            PreviewAction::Play if step.is_none() => {
                self.start_demo(now);
                self.demo.started.is_some()
            }
            PreviewAction::Stop if step.is_some() => {
                self.demo.stop();
                true
            }
            PreviewAction::Pause if step.is_some_and(|step| !step.paused) => self.demo.pause(now),
            PreviewAction::Resume if step.is_some_and(|step| step.paused) => self.demo.resume(now),
            PreviewAction::Hello if step.is_none() && self.model.say_hello(now) => {
                self.greeting_until = now.saturating_add(GentleInteraction::HOLD);
                self.hello_ready_at = now.saturating_add(GentleInteraction::COOLDOWN);
                self.demo.retire();
                true
            }
            _ => false,
        }
    }

    fn display_context(&mut self, now: Duration, demo: Option<DemoStep>) -> RenderContext {
        demo.map_or_else(
            || {
                self.model
                    .context(now)
                    .unwrap_or_else(|| self.pose.context())
            },
            |step| step.pose.context(),
        )
    }

    fn sync_viewport(&mut self, focused: bool, occluded: bool) {
        if focused && !occluded {
            self.model.open();
        } else {
            self.close();
        }
    }

    pub fn show(&mut self, ui: &mut Ui, motion: Option<OrganismMotion>) {
        let reduced = ui
            .ctx()
            .data(|data| data.get_temp::<bool>(system_motion_id()).unwrap_or(false));
        let motion = Some(crate::organism::resolved_motion_with_system(
            motion, reduced,
        ));
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
                        self.select_pose(pose);
                    }
                }
            });
        let now = self.born.elapsed();
        let availability = self.model.greeting_availability(now);
        let initial_demo = self.demo.sample(now);
        ui.horizontal_wrapped(|ui| {
            let action = if initial_demo.is_some() {
                PreviewAction::Stop
            } else {
                PreviewAction::Play
            };
            let ticket = self.demo.ticket(action);
            let label = if initial_demo.is_some() {
                "Stop demo"
            } else {
                "Play demo"
            };
            let clicked = ui
                .push_id(ticket.id(), |ui| {
                    ui.add_enabled(
                        ticket.revision.is_some() && availability != GreetingAvailability::Closed,
                        egui::Button::new(label),
                    )
                    .clicked()
                })
                .inner;
            if clicked {
                self.apply_control(ticket, now);
            }
            if let Some(step) = self.demo.sample(now) {
                let action = if step.paused {
                    PreviewAction::Resume
                } else {
                    PreviewAction::Pause
                };
                let ticket = self.demo.ticket(action);
                let label = if step.paused {
                    "Resume demo"
                } else {
                    "Pause demo"
                };
                let clicked = ui
                    .push_id(ticket.id(), |ui| {
                        ui.add_enabled(
                            ticket.revision.is_some()
                                && availability != GreetingAvailability::Closed,
                            egui::Button::new(label),
                        )
                        .clicked()
                    })
                    .inner;
                if clicked {
                    self.apply_control(ticket, now);
                }
            }
        });
        let demo = self.demo.sample(now);
        let hello_ticket = self.demo.ticket(PreviewAction::Hello);
        let hello_clicked = ui
            .push_id(hello_ticket.id(), |ui| {
                ui.add_enabled(
                    hello_ticket.revision.is_some()
                        && demo.is_none()
                        && availability == GreetingAvailability::Available,
                    egui::Button::new("Say hello"),
                )
                .clicked()
            })
            .inner;
        if let Some(step) = demo {
            ui.small(format!(
                "Demo {}/5: {} (example{})",
                step.index + 1,
                step.pose.label(),
                if step.paused { ", paused" } else { "" }
            ));
            ui.small("No command is run. Say hello is paused during the demo.");
        } else {
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
        }
        if hello_clicked {
            self.apply_control(hello_ticket, now);
        }
        {
            // Keep the same settings layout while inactive, without replaying
            // a canceled greeting or requesting background animation frames.
            let context = self.display_context(now, demo);
            let full = availability != GreetingAvailability::Closed
                && resolved_motion(motion) == OrganismMotion::Full;
            let frame = demo.map_or_else(
                || {
                    if full {
                        now.as_millis() as u64 / 100
                    } else {
                        0
                    }
                },
                |step| step.frame(full),
            );
            ui.small(format!(
                "Example: {}",
                behavior_explanation(context.behavior)
            ));
            ui.add_sized(
                [ui.available_width(), 64.0],
                egui::Label::new(
                    RichText::new(sprite_frame_with_context(context, frame)).monospace(),
                ),
            );
            let next = if availability == GreetingAvailability::Closed {
                None
            } else if let Some(step) = demo {
                step.next_wake(full)
            } else {
                preview_next_wake(
                    now,
                    full,
                    self.greeting_until,
                    self.hello_ready_at,
                    self.model.greeting_availability(now),
                )
            };
            if let Some(delay) = next {
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

    #[test]
    fn system_motion_coalesces_duplicates_and_keeps_explicit_choices() {
        let host = OrganismHost::default();
        assert_eq!(host.motion(None), OrganismMotion::Calm);
        assert!(!publish_system_motion(&host.system_motion.reduced, false));
        assert!(publish_system_motion(&host.system_motion.reduced, true));
        assert!(!publish_system_motion(&host.system_motion.reduced, true));
        assert_eq!(host.motion(None), OrganismMotion::Static);
        for choice in [
            OrganismMotion::Full,
            OrganismMotion::Calm,
            OrganismMotion::Static,
        ] {
            assert_eq!(host.motion(Some(choice)), choice);
        }
        assert!(publish_system_motion(&host.system_motion.reduced, false));
        assert_eq!(host.motion(None), OrganismMotion::Calm);
    }

    #[test]
    fn system_motion_transition_preserves_completion_and_stops_animation_wakes() {
        let mut host = OrganismHost::default();
        host.owner = Some("local".into());
        host.cursor = Some((42, false));
        host.reaction_status = "completed successfully".into();
        host.reaction_until = Duration::from_secs(12);
        let now = Duration::from_secs(10);
        let policy = |motion| PresentationPolicy {
            enabled: true,
            focused_owner: true,
            local: true,
            motion: Some(motion),
            ..Default::default()
        };
        assert_eq!(
            next_host_wake(policy(host.motion(None)), now, host.reaction_until, false),
            Some(Duration::from_millis(900))
        );
        assert!(publish_system_motion(&host.system_motion.reduced, true));
        // Completion is still shown until its existing bounded expiry; no heartbeat.
        assert_eq!(
            next_host_wake(policy(host.motion(None)), now, host.reaction_until, false),
            Some(Duration::from_secs(2))
        );
        for elapsed in [12, 13, 60, 3600] {
            assert_eq!(
                next_host_wake(
                    policy(host.motion(None)),
                    Duration::from_secs(elapsed),
                    host.reaction_until,
                    false
                ),
                None
            );
        }
        assert_eq!(host.cursor, Some((42, false)));
        assert_eq!(host.owner.as_deref(), Some("local"));
        assert_eq!(host.reaction_status, "completed successfully");
        for mut hidden in [policy(host.motion(None)); 4].into_iter().enumerate() {
            match hidden.0 {
                0 => hidden.1.enabled = false,
                1 => hidden.1.focused_owner = false,
                2 => hidden.1.local = false,
                _ => hidden.1.alternate_screen = true,
            }
            assert_eq!(
                next_host_wake(hidden.1, now, host.reaction_until, false),
                None
            );
        }
        publish_system_motion(&host.system_motion.reduced, false);
        assert_eq!(host.cursor, Some((42, false)));
        assert_eq!(
            next_host_wake(policy(host.motion(None)), now, Duration::ZERO, false),
            Some(Duration::from_millis(900))
        );
    }

    #[test]
    fn agent_lifecycle_edges_are_deduplicated_and_disabled_edges_are_consumed() {
        use jterm_core::agent::AgentState;
        use jterm_core::organism::AgentPulse;
        let mut tracker = AgentPulseTracker::default();
        assert_eq!(tracker.observe(None, false, true), None);
        assert_eq!(tracker.observe(Some(AgentState::Ready), false, true), None);
        assert_eq!(
            tracker.observe(Some(AgentState::Completed), true, true),
            Some(AgentPulse::Working)
        );
        assert_eq!(
            tracker.observe(Some(AgentState::Completed), true, true),
            None
        );
        assert_eq!(
            tracker.observe(Some(AgentState::Completed), false, true),
            Some(AgentPulse::Finished)
        );
        assert_eq!(tracker.observe(Some(AgentState::Ready), false, true), None);
        assert_eq!(
            tracker.observe(Some(AgentState::Completed), false, true),
            Some(AgentPulse::Finished)
        );
        assert_eq!(
            tracker.observe(Some(AgentState::Cancelled), false, true),
            Some(AgentPulse::Gone)
        );
        assert_eq!(tracker.observe(None, false, true), None);
        assert_eq!(tracker.observe(None, false, true), None);
        assert_eq!(
            tracker.observe(Some(AgentState::AwaitingModel), false, false),
            None
        );
        assert_eq!(
            tracker.observe(Some(AgentState::AwaitingModel), false, true),
            None
        );
        assert_eq!(tracker.observe(None, false, true), Some(AgentPulse::Gone));
        assert_eq!(tracker.observe(None, false, true), None);
        let mut first = AgentPulseTracker::default();
        assert_eq!(
            first.observe(Some(AgentState::Cancelled), false, true),
            Some(AgentPulse::Gone)
        );
    }

    use crate::block_mode::CompletionProvenance;
    use crate::terminal::CompletedCommandOutput;

    #[test]
    fn window_running_snapshot_tracks_non_owner_start_finish_close_and_switch() {
        use crate::session::SessionPurpose;
        let mut owner = crate::terminal::TerminalState::new(24, 5);
        let mut background = crate::terminal::TerminalState::new(24, 5);
        for terminal in [&mut owner, &mut background] {
            terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        }
        let snapshot = |terminals: &[&crate::terminal::TerminalState]| {
            window_has_known_local_running(terminals.iter().map(|terminal| {
                (
                    SessionPurpose::Interactive,
                    true,
                    Some(terminal_has_running_command(terminal)),
                )
            }))
        };
        let mut host = OrganismHost::default();
        host.acquire(Some("owner"), None, false);
        host.window_running = snapshot(&[&owner, &background]);
        assert!(!host.window_running);

        background.process_input(b"sleep 120\r\n\x1b]133;C;cmdline_url=sleep%20120\x07");
        host.window_running = snapshot(&[&owner, &background]);
        assert!(host.window_running);
        assert!(!host.running, "background work cannot claim the owner pose");
        host.acquire(Some("background"), None, true);
        host.acquire(None, None, false);
        host.acquire(Some("owner"), None, false);
        assert!(
            host.window_running,
            "owner changes do not clear window facts"
        );

        background.process_input(b"\x1b]133;D;0\x07");
        host.window_running = snapshot(&[&owner, &background]);
        assert!(!host.window_running);
        background.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 120\r\n\x1b]133;C;cmdline_url=sleep%20120\x07",
        );
        assert!(snapshot(&[&owner, &background]));
        host.window_running = snapshot(&[&owner]);
        assert!(
            !host.window_running,
            "closing a running session leaves no busy latch"
        );
        assert!(!window_has_known_local_running([]));
    }

    #[test]
    fn window_running_requires_known_interactive_local_shell_facts() {
        use crate::session::SessionPurpose::{EphemeralCommand, Interactive, RetainedCommand};
        for purpose in [Interactive, EphemeralCommand, RetainedCommand] {
            for source_shell in [false, true] {
                for running in [None, Some(false), Some(true)] {
                    assert_eq!(
                        known_local_shell_running(purpose, source_shell, running),
                        purpose == Interactive && source_shell && running == Some(true),
                    );
                }
            }
        }
        let mut unmarked = crate::terminal::TerminalState::new(24, 5);
        unmarked.process_input(b"compiling and running\r\n");
        assert!(!terminal_has_running_command(&unmarked));
    }

    #[test]
    fn background_work_prevents_rest_without_changing_owner_pose_or_cadence() {
        let mut quiet = OrganismHost::default();
        let mut working = OrganismHost::default();
        quiet.acquire(Some("owner"), None, false);
        working.acquire(Some("owner"), None, false);
        working.window_running = true;
        let context = working.context;
        for second in 0..=61 {
            let now = Duration::from_secs(second);
            quiet.advance_life(now, true);
            working.advance_life(now, true);
        }
        let now = Duration::from_secs(61);
        assert!(working.life.state().energy < quiet.life.state().energy);
        assert_eq!(working.context, context);
        assert!(!working.running);
        assert_eq!(working.activity, LiveActivityKnowledge::Unknown);
        assert_eq!(working.owner_idle_for(now), quiet.owner_idle_for(now));
        assert!(working.owner_dormant(now));
        for motion in [
            OrganismMotion::Full,
            OrganismMotion::Calm,
            OrganismMotion::Static,
        ] {
            let policy = PresentationPolicy {
                enabled: true,
                focused_owner: true,
                local: true,
                motion: Some(motion),
                ..Default::default()
            };
            let next = next_host_wake(policy, now, Duration::ZERO, working.owner_dormant(now));
            assert_eq!(
                next,
                if motion == OrganismMotion::Static {
                    None
                } else {
                    Some(Duration::from_millis(900))
                }
            );
        }
        let before = working.life.state().energy;
        working.advance_life(now + Duration::from_secs(1), false);
        assert_eq!(working.life.state().energy, before);
    }

    fn companion_sprite_samples() -> std::collections::BTreeSet<String> {
        use jterm_core::organism::{
            Behavior, BodyLanguage, RenderContext, VisualGrowthStage, VisualTransition, WatchRhythm,
        };
        let behaviors = [
            Behavior::Idle,
            Behavior::WatchCommand,
            Behavior::InspectError,
            Behavior::SitNearError,
            Behavior::Celebrate,
            Behavior::CelebrateBig,
            Behavior::RestAfterPush,
            Behavior::UnknownOutcome,
            Behavior::GlanceAside,
            Behavior::Sleep,
            Behavior::Explore,
            Behavior::Approach,
            Behavior::WatchAgent,
            Behavior::WatchSettled,
            Behavior::GuardFailure,
            Behavior::GuardStuck,
            Behavior::GuardRecovery,
            Behavior::GuardCautious,
        ];
        let mut samples = std::collections::BTreeSet::new();
        let mut check = |context, frame| {
            let sprite = sprite_frame_with_context(context, frame);
            samples.insert(sprite.into_owned());
        };
        for behavior in behaviors {
            for bits in 0..8 {
                let language = BodyLanguage {
                    drowsy: bits & 1 != 0,
                    tense: bits & 2 != 0,
                    listless: bits & 4 != 0,
                };
                for walking in [false, true] {
                    for rhythm in [
                        WatchRhythm::Steady,
                        WatchRhythm::Busy,
                        WatchRhythm::Waiting,
                        WatchRhythm::Resumed,
                    ] {
                        for stage in [
                            VisualGrowthStage::Juvenile,
                            VisualGrowthStage::Adult,
                            VisualGrowthStage::Seasoned,
                        ] {
                            let context = RenderContext::new(behavior, language, walking)
                                .with_watch_rhythm(rhythm)
                                .with_growth_stage(stage);
                            for frame in 0..130 {
                                check(context, frame);
                            }
                        }
                    }
                }
            }
            for target in behaviors {
                if let Some(transition) = VisualTransition::between(behavior, target) {
                    for stage in [
                        VisualGrowthStage::Juvenile,
                        VisualGrowthStage::Adult,
                        VisualGrowthStage::Seasoned,
                    ] {
                        let context = RenderContext::new(target, BodyLanguage::default(), false)
                            .with_growth_stage(stage)
                            .with_transition(Some(transition));
                        for frame in 0..transition.frame_count() {
                            check(context, frame);
                        }
                    }
                }
            }
        }
        samples
    }
    fn native_test_frame(
        host: &mut OrganismHost,
        ctx: &egui::Context,
        config: &Config,
        block: &str,
        region: egui::Rect,
        events: Vec<egui::Event>,
        interactive: bool,
    ) {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 480.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                host.draw_native_body(
                    ui,
                    config,
                    Some((
                        "local".into(),
                        block.into(),
                        crate::terminal::TerminalState::new(80, 12)
                            .projected_viewport(
                                crate::terminal::HistoryProjection::identity(),
                                true,
                            )
                            .key(),
                        region,
                    )),
                    interactive,
                )
            },
        );
        output.textures_delta.clear();
    }

    #[test]
    fn native_wander_matches_reference_cycle_and_never_leaves_safe_space() {
        let ordinary = BodyLanguage::default();
        assert_eq!(native_wander_phase(0, ordinary), (0.0, false));
        assert_eq!(native_wander_phase(359, ordinary), (0.0, false));
        assert_eq!(native_wander_phase(380, ordinary), (0.5, true));
        assert_eq!(native_wander_phase(400, ordinary), (1.0, false));
        assert_eq!(native_wander_phase(780, ordinary), (0.5, true));
        assert_eq!(native_wander_phase(800, ordinary), (0.0, false));
        for scale in [0.75, 1.0, 1.25, 1.5, 2.0, 3.0] {
            let region = egui::Rect::from_min_size(
                egui::pos2(20.0, 100.0),
                egui::vec2(640.0, 160.0) * scale,
            );
            let size = egui::vec2(116.0, 62.0) * scale;
            let context = PreviewPose::Calm.context();
            for frame in 0..1600 {
                let rect = native_placement(region, size, context, frame, true).unwrap();
                assert!(region.contains_rect(rect), "scale {scale}, frame {frame}");
                assert_eq!(rect.bottom(), region.bottom());
                assert_eq!(
                    native_placement(region, size, context, frame, false),
                    native_placement(region, size, context, 0, false)
                );
                assert_eq!(
                    native_wander_phase(
                        frame,
                        BodyLanguage {
                            drowsy: true,
                            ..ordinary
                        }
                    ),
                    (0.0, false)
                );
            }
        }
    }

    #[test]
    fn native_walk_speed_matches_cell_cadence_without_redraw_or_resume_catchup() {
        let old = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(120.0, 60.0));
        let far = old.translate(egui::vec2(1200.0, 0.0));
        assert_eq!(native_walk_step(old, far, 8.0, false), old);
        let step = native_walk_step(old, far, 8.0, true);
        assert_eq!(step.left(), old.left() + 8.0);
        assert_eq!(step.size(), old.size());
        assert_eq!(native_walk_step(step, old, 8.0, true), old);
        let nearby = old.translate(egui::vec2(3.0, 0.0));
        assert_eq!(native_walk_step(old, nearby, 8.0, true), nearby);
        // Safety-critical output/resize y snaps immediately; horizontal motion
        // still cannot catch up after a hidden/suspended interval.
        let below = far.translate(egui::vec2(0.0, 40.0));
        assert_eq!(native_walk_step(old, below, 8.0, true).top(), below.top());
    }

    #[test]
    fn native_hover_freezes_wander_without_resizing_or_capturing_input() {
        let ctx = egui::Context::default();
        let mut host = OrganismHost::default();
        host.acquire(Some("local"), None, false);
        host.context = PreviewPose::Calm.context();
        host.activity = LiveActivityKnowledge::KnownIdle;
        let config = Config {
            block_mode: true,
            ascii_organism_enabled: true,
            ascii_organism_motion: Some(OrganismMotion::Full),
            ..Config::default()
        };
        let region = egui::Rect::from_min_max(egui::pos2(100.0, 100.0), egui::pos2(600.0, 240.0));
        native_test_frame(
            &mut host,
            &ctx,
            &config,
            "active",
            region,
            vec![egui::Event::PointerMoved(egui::pos2(20.0, 20.0))],
            true,
        );
        let nest = host.native_surface_rect.unwrap();
        native_test_frame(
            &mut host,
            &ctx,
            &config,
            "active",
            region,
            vec![egui::Event::PointerMoved(nest.center())],
            true,
        );
        host.born = Instant::now() - Duration::from_secs(39);
        native_test_frame(&mut host, &ctx, &config, "active", region, vec![], true);
        assert_eq!(
            host.native_surface_rect,
            Some(nest),
            "hover holds the complete target"
        );
        host.born = Instant::now() - Duration::from_millis(39_100);
        native_test_frame(
            &mut host,
            &ctx,
            &config,
            "active",
            region,
            vec![egui::Event::PointerMoved(egui::pos2(20.0, 20.0))],
            true,
        );
        let moved = host.native_surface_rect.unwrap();
        assert!(moved.left() > nest.left());
        assert_eq!(moved.size(), nest.size());
        assert!(region.contains_rect(moved));
    }

    #[test]
    fn block_status_is_visible_without_hover_and_all_annotation_paint_is_clipped() {
        for expanded in [false, true] {
            for width in [32.0, 120.0, 220.0, 640.0, 1800.0] {
                for font_size in [8.0, 14.0, 24.0, 40.0] {
                    let ctx = egui::Context::default();
                    let host = OrganismHost::default();
                    let config = Config {
                        block_mode: true,
                        ascii_organism_enabled: true,
                        ascii_organism_expanded: expanded,
                        font_size,
                        ascii_organism_motion: Some(OrganismMotion::Static),
                        ..Config::default()
                    };
                    let height = if expanded {
                        font_size * 0.82 * 1.2 * 3.0 + 22.0
                    } else {
                        font_size
                    };
                    let region = egui::Rect::from_min_size(
                        egui::pos2(20.0, 40.0),
                        egui::vec2(width, height),
                    );
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(2048.0, 600.0),
                            )),
                            ..Default::default()
                        },
                        |ui| {
                            assert!(!host.draw_block_status(
                                ui,
                                &config,
                                Some(("local".into(), region)),
                                false
                            ));
                        },
                    );
                    let text: Vec<_> = output
                        .shapes
                        .iter()
                        .filter_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) => Some((shape.clip_rect, text)),
                            _ => None,
                        })
                        .collect();
                    if width >= 640.0 && font_size <= 24.0 {
                        assert!(
                            text.iter()
                                .any(|(_, text)| text.galley.text().contains("ASCII organism")),
                            "resting status must not require pointer hover"
                        );
                    }
                    for (clip, text) in text {
                        assert!(region.contains_rect(clip), "{clip:?} escapes {region:?}");
                        assert!(text.galley.rows.len() <= 3);
                    }
                    output.textures_delta.clear();
                }
            }
        }
    }

    #[test]
    fn block_status_resize_does_not_migrate_an_armed_toggle_press() {
        for resize in [false, true] {
            let ctx = egui::Context::default();
            let host = OrganismHost::default();
            let config = Config {
                block_mode: true,
                ascii_organism_enabled: true,
                ascii_organism_expanded: true,
                ..Config::default()
            };
            let point = egui::pos2(636.0, 75.0);
            let mut clicks = 0;
            for frame in 0..3 {
                let rect = egui::Rect::from_min_size(
                    egui::pos2(20.0, 40.0),
                    egui::vec2(if resize && frame == 2 { 642.0 } else { 640.0 }, 84.0),
                );
                let events = match frame {
                    1 => vec![
                        egui::Event::PointerMoved(point),
                        egui::Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    2 => vec![egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    _ => vec![],
                };
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(800.0, 600.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        clicks += usize::from(host.draw_block_status(
                            ui,
                            &config,
                            Some(("local".into(), rect)),
                            true,
                        ));
                    },
                );
                output.textures_delta.clear();
            }
            assert_eq!(clicks, usize::from(!resize));
        }
    }

    #[test]
    fn block_mode_advances_host_without_reserving_duplicate_footer() {
        let ctx = egui::Context::default();
        let mut host = OrganismHost::default();
        let config = Config {
            block_mode: true,
            ascii_organism_enabled: true,
            ascii_organism_expanded: true,
            ..Config::default()
        };
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let before = ui.available_rect_before_wrap();
            assert!(!host.draw(ui, &config, true, true));
            assert_eq!(before, ui.available_rect_before_wrap());
        });
        output.textures_delta.clear();
    }

    #[test]
    fn native_body_projection_change_retires_old_core_ticket_at_same_rectangle() {
        let ctx = egui::Context::default();
        let mut host = OrganismHost::default();
        host.acquire(Some("local"), None, false);
        host.activity = LiveActivityKnowledge::KnownIdle;
        let config = Config {
            block_mode: true,
            ascii_organism_enabled: true,
            ascii_organism_motion: Some(OrganismMotion::Full),
            ..Config::default()
        };
        let region = egui::Rect::from_min_max(egui::pos2(100.0, 100.0), egui::pos2(500.0, 220.0));
        native_test_frame(
            &mut host,
            &ctx,
            &config,
            "same",
            region,
            vec![egui::Event::PointerMoved(egui::pos2(20.0, 20.0))],
            true,
        );
        native_test_frame(
            &mut host,
            &ctx,
            &config,
            "same",
            region,
            vec![egui::Event::PointerMoved(egui::pos2(140.0, 200.0))],
            true,
        );
        let owner = String::from("local");
        let input = LiveAttentionInput {
            base_context: host.context,
            activity: LiveActivityKnowledge::KnownIdle,
            hard_eligible: true,
            hover_available: true,
            pointer_inside: true,
        };
        let candidate = host
            .attention
            .snapshot(host.born.elapsed(), &owner, input)
            .unwrap()
            .candidate
            .unwrap();
        let mut projection = host.native_surface_identity.as_ref().unwrap().2;
        projection.projection_revision = projection.projection_revision.wrapping_add(1);
        projection.scroll_offset = projection.scroll_offset.saturating_add(1);
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 480.0),
                )),
                ..Default::default()
            },
            |ui| {
                host.draw_native_body(
                    ui,
                    &config,
                    Some((owner.clone(), "same".into(), projection, region)),
                    true,
                )
            },
        );
        output.textures_delta.clear();
        assert!(host
            .attention
            .snapshot(host.born.elapsed(), &owner, input)
            .unwrap()
            .candidate
            .is_none());
        let delayed = host.attention.advance(
            candidate.deadline,
            &owner,
            input,
            LiveAttentionEvent::CandidateDue {
                ticket: candidate.ticket,
            },
        );
        assert!(
            !delayed.is_ok_and(|value| matches!(value.outcome, LiveAttentionOutcome::Accepted(_)))
        );
    }

    #[test]
    fn native_body_reuses_attention_and_requires_fresh_pointer_after_layout_change() {
        let ctx = egui::Context::default();
        let mut host = OrganismHost::default();
        host.acquire(Some("local"), None, false);
        host.activity = LiveActivityKnowledge::KnownIdle;
        let config = Config {
            block_mode: true,
            ascii_organism_enabled: true,
            ascii_organism_motion: Some(OrganismMotion::Full),
            ..Config::default()
        };
        let region = egui::Rect::from_min_max(egui::pos2(100.0, 100.0), egui::pos2(500.0, 220.0));
        native_test_frame(
            &mut host,
            &ctx,
            &config,
            "block-a",
            region,
            vec![egui::Event::PointerMoved(egui::pos2(20.0, 20.0))],
            true,
        );
        assert!(host.native_surface_rect.is_some());
        native_test_frame(
            &mut host,
            &ctx,
            &config,
            "block-a",
            region,
            vec![egui::Event::PointerMoved(egui::pos2(140.0, 200.0))],
            true,
        );
        let owner = String::from("local");
        let input = LiveAttentionInput {
            base_context: host.context,
            activity: LiveActivityKnowledge::KnownIdle,
            hard_eligible: true,
            hover_available: true,
            pointer_inside: true,
        };
        assert!(host
            .attention
            .snapshot(host.born.elapsed(), &owner, input)
            .unwrap()
            .candidate
            .is_some());
        // New Block under the same stationary pointer is not a fresh visit.
        native_test_frame(&mut host, &ctx, &config, "block-b", region, vec![], true);
        assert!(host
            .attention
            .snapshot(host.born.elapsed(), &owner, input)
            .unwrap()
            .candidate
            .is_none());
        let shifted = region.translate(egui::vec2(-10.0, 0.0));
        native_test_frame(&mut host, &ctx, &config, "block-b", shifted, vec![], true);
        assert!(host
            .attention
            .snapshot(host.born.elapsed(), &owner, input)
            .unwrap()
            .candidate
            .is_none());
        native_test_frame(
            &mut host,
            &ctx,
            &config,
            "block-b",
            shifted,
            vec![egui::Event::PointerMoved(egui::pos2(20.0, 20.0))],
            true,
        );
        native_test_frame(
            &mut host,
            &ctx,
            &config,
            "block-b",
            shifted,
            vec![egui::Event::PointerMoved(egui::pos2(130.0, 200.0))],
            true,
        );
        assert!(host
            .attention
            .snapshot(host.born.elapsed(), &owner, input)
            .unwrap()
            .candidate
            .is_some());
    }

    #[test]
    fn native_body_typing_retreat_restores_current_block_and_hard_gates_hide_it() {
        let ctx = egui::Context::default();
        let mut host = OrganismHost::default();
        host.acquire(Some("local"), None, false);
        let mut config = Config {
            block_mode: true,
            ascii_organism_enabled: true,
            ascii_organism_motion: Some(OrganismMotion::Full),
            ..Config::default()
        };
        let region = egui::Rect::from_min_max(egui::pos2(100.0, 100.0), egui::pos2(500.0, 220.0));
        host.last_input = Some(Duration::ZERO);
        native_test_frame(&mut host, &ctx, &config, "old", region, vec![], true);
        assert!(host.native_surface_rect.is_none());
        host.born = Instant::now() - crate::organism::INPUT_RETREAT - Duration::from_millis(10);
        native_test_frame(&mut host, &ctx, &config, "current", region, vec![], true);
        assert!(host.native_surface_rect.is_some());
        assert_eq!(host.native_surface_identity.as_ref().unwrap().1, "current");
        native_test_frame(&mut host, &ctx, &config, "current", region, vec![], false);
        assert!(host.native_surface_rect.is_none());
        config.ascii_organism_motion = Some(OrganismMotion::Static);
        native_test_frame(&mut host, &ctx, &config, "current", region, vec![], true);
        assert!(
            host.native_surface_rect.is_none(),
            "Static keeps only the status card"
        );
        config.ascii_organism_motion = Some(OrganismMotion::Calm);
        native_test_frame(
            &mut host,
            &ctx,
            &config,
            "current",
            egui::Rect::from_min_size(region.min, egui::vec2(8.0, 8.0)),
            vec![],
            true,
        );
        assert!(host.native_surface_rect.is_none());
    }

    #[test]
    fn native_body_fit_is_complete_at_narrow_width_and_font_scale() {
        let region = egui::Rect::from_min_max(egui::pos2(40.0, 50.0), egui::pos2(220.0, 150.0));
        for size in [egui::vec2(120.0, 70.0), egui::vec2(180.0, 100.0)] {
            let body = native_body_rect(region, size).unwrap();
            assert!(region.contains_rect(body));
            assert_eq!(body.max, region.max);
        }
        for size in [
            egui::vec2(181.0, 70.0),
            egui::vec2(120.0, 101.0),
            egui::vec2(f32::NAN, 10.0),
            egui::Vec2::ZERO,
        ] {
            assert!(native_body_rect(region, size).is_none());
        }
    }

    #[test]
    fn native_body_reuses_live_wake_without_focus_or_additional_timer() {
        let source = include_str!("organism_ui.rs");
        let body = source
            .split("    fn draw_native_body(")
            .nth(1)
            .unwrap()
            .split("    fn draw_block_status(")
            .next()
            .unwrap();
        assert!(body.contains("self.native_body_wake(policy, now, snapshot)"));
        assert!(body.contains("next_live_wake("));
        for forbidden in [
            "ui.interact(",
            "request_focus(",
            "shell.write(",
            "thread::",
            "spawn(",
            "request_repaint_after(Duration::from_millis(100))",
        ] {
            assert!(
                !body.contains(forbidden),
                "native body must be passive: {forbidden}"
            );
        }
        assert!(body.contains("policy.body_visible(now)"));
        assert!(body.contains("self.owner.as_deref() == Some(session.as_str())"));
        assert!(body.contains("if changed { &[] } else { &events }"));
    }

    #[test]
    fn companion_every_shared_frame_has_three_rows_and_bounded_columns() {
        for sprite in companion_sprite_samples() {
            assert_eq!(sprite.lines().count(), 3);
            assert!(sprite.lines().all(|line| line.chars().count() <= 12));
        }
    }

    #[test]
    fn companion_actual_galleys_fit_body_and_clip_long_unicode_details() {
        let samples = companion_sprite_samples();
        let ctx = egui::Context::default();
        for _ in 0..2 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(host_rect(640.0, 480.0)),
                    ..Default::default()
                },
                |ui| {
                    for sprite in &samples {
                        let galley = ui.painter().layout_no_wrap(
                            sprite.clone(),
                            egui::FontId::monospace(14.0),
                            egui::Color32::WHITE,
                        );
                        assert_eq!(galley.rows.len(), 3);
                        assert!(galley.size().x <= 104.0, "{}: {}", galley.size().x, sprite);
                        assert!(galley.size().y <= 72.0);
                    }
                    let layout = companion_layout(host_rect(360.0, 96.0), true);
                    let row = egui::Rect::from_min_size(
                        layout.details.min,
                        egui::vec2(layout.details.width(), 20.0),
                    );
                    companion_line(
                        ui,
                        row,
                        "状态 λ output 活动 🦊 ".repeat(1000),
                        12.0,
                        egui::Color32::WHITE,
                    );
                },
            );
            output.textures_delta.clear();
            let layout = companion_layout(host_rect(360.0, 96.0), true);
            let text = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some((shape.clip_rect, text)),
                    _ => None,
                })
                .unwrap();
            assert_eq!(text.1.galley.rows.len(), 1);
            assert!(layout.details.contains_rect(text.0));
            assert!(!text.0.intersect(layout.control).is_positive());
            assert!(!text.0.intersect(layout.body).is_positive());
        }
    }

    #[test]
    fn companion_details_use_two_bounded_rows_and_responsive_badge() {
        for width in [32.0, 120.0, 220.0, 480.0] {
            for status in [
                "Quietly here. Live reactions are paused.".to_owned(),
                "状态 λ output activity resumed 🦊 ".repeat(80),
            ] {
                let ctx = egui::Context::default();
                let details = host_rect(width, 72.0);
                for _ in 0..2 {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(host_rect(640.0, 480.0)),
                            ..Default::default()
                        },
                        |ui| companion_details(ui, details, status.clone()),
                    );
                    output.textures_delta.clear();
                    let text: Vec<_> = output
                        .shapes
                        .iter()
                        .filter_map(|shape| {
                            if let egui::Shape::Text(text) = &shape.shape {
                                Some((shape.clip_rect, text))
                            } else {
                                None
                            }
                        })
                        .collect();
                    assert_eq!(text.len(), 3);
                    for (clip, shape) in &text {
                        assert!(details.contains_rect(*clip));
                        assert!(shape.galley.rows.len() <= 2);
                    }
                    assert_eq!(text[0].1.galley.rows.len(), 1);
                    assert_eq!(text[1].1.galley.rows.len(), 1);
                    assert_eq!(text[2].1.pos.y, 22.0);
                    assert!(text[2].1.galley.size().y <= 32.0);
                    if width == 480.0 {
                        assert_eq!(text[1].1.pos.y, 2.0);
                        assert!(text[1].1.pos.x > text[0].1.pos.x + text[0].1.galley.size().x);
                    } else {
                        assert_eq!(text[1].1.pos.y, 54.0);
                    }
                    assert!(!text[1].0.intersect(text[2].0).is_positive());
                }
            }
        }
    }

    #[test]
    fn companion_details_header_boundary_and_short_height_are_clipped() {
        let ctx = egui::Context::default();
        let mut boundary = 0.0;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            boundary = ui
                .painter()
                .layout_no_wrap(
                    "ASCII organism".into(),
                    egui::FontId::proportional(14.0),
                    egui::Color32::WHITE,
                )
                .size()
                .x
                + 16.0
                + ui.painter()
                    .layout_no_wrap(
                        "Volatile memory · no LLM".into(),
                        egui::FontId::proportional(11.0),
                        egui::Color32::WHITE,
                    )
                    .size()
                    .x;
        });
        output.textures_delta.clear();
        for width in [boundary - 0.5, boundary, boundary + 0.5] {
            for height in [1.0, 18.0, 30.0, 54.0, 60.0, 72.0] {
                let rect = host_rect(width, height);
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(host_rect(640.0, 480.0)),
                        ..Default::default()
                    },
                    |ui| companion_details(ui, rect, "Output activity resumed. Watching quietly."),
                );
                output.textures_delta.clear();
                for shape in &output.shapes {
                    if let egui::Shape::Text(text) = &shape.shape {
                        assert!(rect.contains_rect(shape.clip_rect));
                        if text.galley.text() == "Volatile memory · no LLM" {
                            assert_eq!(text.pos.y, if width >= boundary { 2.0 } else { 54.0 });
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn companion_toggle_feedback_is_visible_only_when_interactive() {
        for interactive in [false, true] {
            let ctx = egui::Context::default();
            let rect = egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(28.0, 72.0));
            let mut fills = Vec::new();
            for frame in 0..4 {
                let point = if frame == 0 {
                    egui::pos2(200.0, 200.0)
                } else {
                    rect.center()
                };
                let mut events = vec![egui::Event::PointerMoved(point)];
                if frame == 2 {
                    events.push(egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                if frame == 3 {
                    events.push(egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(host_rect(640.0, 480.0)),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        let clicked = companion_toggle(ui, rect, true, interactive);
                        assert_eq!(clicked, interactive && frame == 3);
                        let response = ui
                            .ctx()
                            .read_response(ui.make_persistent_id("ascii_organism_expand"))
                            .unwrap();
                        assert!(!response.has_focus());
                        assert!(!response.sense.is_focusable());
                    },
                );
                output.textures_delta.clear();
                let fill = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(r) if r.rect == rect => Some(r.fill),
                    _ => None,
                });
                fills.push(fill);
                if interactive && frame > 0 {
                    assert_eq!(
                        output.platform_output.cursor_icon,
                        egui::CursorIcon::PointingHand
                    );
                }
            }
            assert!(fills[0].is_none());
            if interactive {
                assert!(fills[1].is_some());
                assert!(fills[2].is_some());
                assert_ne!(fills[1], fills[2]);
            } else {
                assert!(fills.iter().all(Option::is_none));
            }
        }
    }

    #[test]
    fn companion_geometry_keeps_body_details_and_control_separate() {
        for expanded in [false, true] {
            for width in [1.0, 24.0, 80.0, 120.0, 180.0, 220.0, 360.0, 640.0, 1024.0] {
                for height in [1.0, 8.0, 24.0, 40.0, 96.0] {
                    let rect = host_rect(width, height);
                    let layout = companion_layout(rect, expanded);
                    for part in [layout.card, layout.body, layout.details, layout.control] {
                        assert!(part.is_finite());
                        if part.is_positive() {
                            assert!(rect.contains_rect(part));
                        }
                    }
                    assert!(!layout.body.intersect(layout.control).is_positive());
                    assert!(!layout.details.intersect(layout.control).is_positive());
                    assert!(!layout.body.intersect(layout.details).is_positive());
                }
            }
        }
        for rect in [
            host_rect(0.0, 96.0),
            host_rect(640.0, 0.0),
            host_rect(0.0, 0.0),
        ] {
            let layout = companion_layout(rect, true);
            for part in [layout.card, layout.body, layout.details, layout.control] {
                assert!(!part.is_positive());
            }
        }
        let layout = companion_layout(host_rect(640.0, 96.0), true);
        assert_eq!(layout.body.width(), 104.0);
        assert_eq!(layout.details.min.x - layout.body.max.x, 12.0);
        assert_eq!(layout.body.height(), 72.0);
    }

    #[test]
    fn companion_toggle_is_pointer_only_and_excludes_terminal_on_resize_frame() {
        for interactive in [false, true] {
            let ctx = egui::Context::default();
            let mut expanded = true;
            let mut point = egui::Pos2::ZERO;
            let mut previous_terminal = egui::Rect::NOTHING;
            let mut clicks = 0;
            for frame in 0..4 {
                let events = match frame {
                    1 => vec![
                        egui::Event::PointerMoved(point),
                        egui::Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    2 => vec![egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    _ => vec![],
                };
                if matches!(frame, 1 | 2) {
                    assert!(!previous_terminal.contains(point));
                }
                let mut discarded = false;
                let mut passes = 0;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(host_rect(640.0, 480.0)),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        passes += 1;
                        let has_edge = ui.input(|input| {
                            input
                                .events
                                .iter()
                                .any(|event| matches!(event, egui::Event::PointerButton { .. }))
                        });
                        if frame == 2 && discarded {
                            assert!(!has_edge);
                            assert!(!ui.input(|input| input.pointer.any_released()));
                        }
                        let mut toggle = false;
                        egui::Panel::bottom("card_test")
                            .exact_size(host_height(true, expanded))
                            .resizable(false)
                            .frame(egui::Frame::NONE.inner_margin(0.0))
                            .show(ui, |ui| {
                                let layout = companion_layout(
                                    ui.available_rect_before_wrap()
                                        .shrink2(egui::vec2(8.0, 0.0)),
                                    expanded,
                                );
                                if frame == 0 {
                                    point = layout.control.center();
                                }
                                assert!(!layout.body.contains(point));
                                toggle =
                                    companion_toggle(ui, layout.control, expanded, interactive);
                                let id = ui.make_persistent_id("ascii_organism_expand");
                                let response = ui.ctx().read_response(id).unwrap();
                                assert!(!response.sense.is_focusable());
                                assert!(!response.has_focus());
                            });
                        let terminal = ui.available_rect_before_wrap();
                        if has_edge {
                            assert!(!terminal.contains(point));
                        }
                        if toggle {
                            clicks += 1;
                            expanded = !expanded;
                        }
                        previous_terminal = terminal;
                        if frame == 2 && !discarded {
                            discarded = true;
                            ui.ctx().request_discard(
                                "verify collapse release is not replayed after layout",
                            );
                        }
                    },
                );
                output.textures_delta.clear();
                if frame == 2 {
                    assert!(passes >= 2);
                }
            }
            assert_eq!(clicks, usize::from(interactive));
            assert_eq!(expanded, !interactive);
        }
    }

    #[test]
    fn companion_toggle_cancels_interrupted_press_and_accepts_fresh_click() {
        for interruption in [
            "none",
            "expanded",
            "round_trip",
            "disabled",
            "parent_disabled",
            "hidden",
            "clipped",
            "dragged_out",
            "same_frame",
            "secondary",
        ] {
            let ctx = egui::Context::default();
            let point = egui::pos2(612.0, 462.0);
            let mut clicks = Vec::new();
            for frame in 0..8 {
                let expanded = match interruption {
                    "expanded" => frame < 2,
                    "round_trip" => frame != 2,
                    _ => true,
                };
                let card = egui::Rect::from_min_max(
                    egui::pos2(8.0, 480.0 - host_height(true, expanded)),
                    egui::pos2(632.0, 480.0),
                );
                let rect = companion_layout(card, expanded).control;
                assert!(rect.contains(point));
                let pointer = if interruption == "dragged_out" && frame == 4 {
                    egui::pos2(300.0, 300.0)
                } else {
                    point
                };
                let mut events = vec![egui::Event::PointerMoved(pointer)];
                let button = |pressed| egui::Event::PointerButton {
                    pos: pointer,
                    button: if interruption == "secondary" && frame < 6 {
                        egui::PointerButton::Secondary
                    } else {
                        egui::PointerButton::Primary
                    },
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                };
                if frame == 1 || frame == 6 {
                    events.push(button(true));
                }
                if (frame == 4 && interruption != "same_frame")
                    || frame == 7
                    || (frame == 1 && interruption == "same_frame")
                {
                    events.push(button(false));
                }
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(host_rect(640.0, 480.0)),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        if interruption == "hidden" && frame == 2 {
                            return;
                        }
                        let interactive = !(interruption == "disabled" && frame == 2);
                        if interruption == "parent_disabled" && frame == 2 {
                            ui.disable();
                        }
                        let rect = if interruption == "clipped" && frame == 2 {
                            egui::Rect::NOTHING
                        } else {
                            rect
                        };
                        if companion_toggle(ui, rect, expanded, interactive) {
                            clicks.push(frame);
                        }
                        if rect.is_positive() {
                            let response = ui
                                .ctx()
                                .read_response(ui.make_persistent_id("ascii_organism_expand"))
                                .unwrap();
                            assert!(!response.has_focus());
                            assert!(!response.sense.is_focusable());
                        }
                    },
                );
                output.textures_delta.clear();
            }
            let expected = match interruption {
                "none" => vec![4, 7],
                "same_frame" => vec![1, 7],
                _ => vec![7],
            };
            assert_eq!(clicks, expected, "{interruption}");
        }
    }

    #[test]
    fn companion_fallback_paints_without_owner_or_new_activity_in_every_motion_mode() {
        for motion in [
            Some(OrganismMotion::Full),
            Some(OrganismMotion::Calm),
            Some(OrganismMotion::Static),
            None,
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut host = OrganismHost::default();
            let config = Config {
                block_mode: false,
                ascii_organism_enabled: true,
                ascii_organism_expanded: true,
                ascii_organism_motion: motion,
                ..Config::default()
            };
            for _ in 0..2 {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(host_rect(640.0, 480.0)),
                        ..Default::default()
                    },
                    |ui| {
                        assert!(!host.draw(ui, &config, false, false));
                    },
                );
                output.textures_delta.clear();
                assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == "ASCII organism")));
                assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == "Quietly here. Live reactions are paused.")));
                let accessible = output.platform_output.accesskit_update.as_ref().unwrap();
                assert!(accessible.nodes.iter().any(|(_, node)| node.role()
                    == egui::accesskit::Role::Label
                    && node
                        .value()
                        .is_some_and(|value| value.contains("Live reactions are paused"))));
                assert!(host.owner.is_none());
                assert_eq!(host.activity, LiveActivityKnowledge::Unknown);
                assert!(host.attention.owner().is_none());
                assert!(host.bridge.active.is_none());
            }
        }
    }

    #[test]
    fn companion_is_shared_chrome_before_both_terminal_views_and_preserves_input_bounds() {
        let main = include_str!("main.rs");
        let render = main
            .split("self.render_bottom_bar(root_ui);")
            .nth(1)
            .unwrap()
            .split("self.render_floating_panels(ctx);")
            .next()
            .unwrap();
        assert!(
            render.find("self.render_organism(root_ui);").unwrap()
                < render.find("egui::CentralPanel::default()").unwrap()
        );
        assert!(!render.contains("block_mode"));
        let route = main
            .split("let pointer_routes_to_terminal =")
            .nth(1)
            .unwrap()
            .split("let sequence_reports_to_app")
            .next()
            .unwrap();
        let route: String = route.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert_eq!(
            route,
            "pointer_over_active_terminal||capture_for_route.is_some();"
        );
        let bounds = main
            .split("let pointer_over_active_terminal =")
            .nth(1)
            .unwrap()
            .split("// 获取当前活跃会话")
            .next()
            .unwrap();
        let bounds: String = bounds.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert!(bounds.contains(".is_some_and(|(pointer,rect)|rect.contains(pointer))"));
        let source = include_str!("organism_ui.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        let fallback = production
            .split("// Paint-only companion:")
            .nth(1)
            .unwrap()
            .split("let next = next_live_wake(")
            .next()
            .unwrap();
        assert!(!fallback.contains("request_repaint"));
        assert!(!fallback.contains("acquire("));
        assert!(!fallback.contains("LiveActivityKnowledge::KnownIdle"));
        let render = production
            .split("pub(crate) fn render_organism(")
            .nth(1)
            .unwrap();
        assert!(render.contains("self.schedule_config_save();"));
        assert!(render.contains("!self.terminal_input_blocked(ui.ctx())"));
    }

    #[test]
    fn watch_activity_explanation_matches_final_context_in_both_surfaces() {
        for behavior in [Behavior::WatchCommand, Behavior::WatchSettled] {
            for (rhythm, activity) in [
                (WatchRhythm::Busy, "Recent output activity."),
                (WatchRhythm::Waiting, "No new output observed recently."),
                (WatchRhythm::Resumed, "Output activity resumed."),
            ] {
                let context = RenderContext::new(behavior, BodyLanguage::default(), false)
                    .with_watch_rhythm(rhythm);
                let expected = format!("{} {activity}", behavior_explanation(behavior));
                assert_eq!(live_behavior_explanation(context), expected);
                assert!(live_semantic_text(context).contains(&expected));
                for frame in [0, 1, 9, 1000] {
                    assert!(live_status_text(context, frame, 640.0).contains(&expected));
                }
                // Narrow chrome keeps its compact layout; semantics stay complete.
                assert!(!live_status_text(context, 0, 120.0).contains(activity));
                assert!(live_semantic_text(context).contains(activity));
            }
        }
    }

    #[test]
    fn nonwatch_and_steady_contexts_keep_existing_behavior_explanation() {
        for pose in PreviewPose::ALL {
            for rhythm in [
                WatchRhythm::Steady,
                WatchRhythm::Busy,
                WatchRhythm::Waiting,
                WatchRhythm::Resumed,
            ] {
                let context = pose.context().with_watch_rhythm(rhythm);
                if matches!(
                    context.behavior,
                    Behavior::WatchCommand | Behavior::WatchSettled
                ) && rhythm != WatchRhythm::Steady
                {
                    continue;
                }
                assert_eq!(
                    live_behavior_explanation(context),
                    behavior_explanation(context.behavior)
                );
            }
        }
        let agent = RenderContext::new(Behavior::WatchAgent, BodyLanguage::default(), false)
            .with_watch_rhythm(WatchRhythm::Busy);
        assert_eq!(
            live_behavior_explanation(agent),
            behavior_explanation(Behavior::WatchAgent)
        );
    }

    #[test]
    fn static_watch_description_keeps_steady_even_when_activity_was_observed() {
        let mut watch = WatchAdapter::default();
        watch.attach(
            Duration::ZERO,
            Some(crate::terminal::WatchCommand {
                source: 1,
                generation: 1,
                revision: 1,
            }),
        );
        let now = Duration::from_secs(4);
        assert_eq!(watch.rhythm(now), WatchRhythm::Waiting);
        let context = watch.context(now, BodyLanguage::default(), false);
        assert_eq!(context.watch_rhythm, WatchRhythm::Steady);
        assert_eq!(
            live_behavior_explanation(context),
            behavior_explanation(context.behavior)
        );
        let source = include_str!("organism_ui.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        let draw = production
            .split("pub fn draw(")
            .nth(1)
            .unwrap()
            .split("struct PendingCommand")
            .next()
            .unwrap();
        assert!(draw.contains("host_text(display, frame"));
        assert!(draw.contains("live_semantic_response(ui, rect, display)"));
        assert!(draw.contains("response.on_hover_text(live_semantic_text(display))"));
    }

    #[test]
    fn live_semantics_are_complete_and_independent_of_animation_or_width() {
        for pose in PreviewPose::ALL {
            let context = pose.context();
            let text = live_semantic_text(context);
            assert!(text.contains(behavior_explanation(context.behavior)));
            assert!(text.starts_with("ASCII Organism: "));
            assert!(text.ends_with("Memory is volatile."));
            assert!(!text.contains('\n'));
        }
    }

    #[test]
    fn live_semantics_require_finite_positive_clipped_chrome() {
        let chrome = egui::Rect::from_min_size(egui::pos2(8.0, 200.0), egui::vec2(120.0, 24.0));
        let clip = egui::Rect::from_min_max(egui::pos2(0.0, 210.0), egui::pos2(80.0, 300.0));
        let visible = live_semantic_rect(chrome, clip).unwrap();
        assert_eq!(visible, chrome.intersect(clip));
        assert!(live_semantic_rect(chrome, egui::Rect::NOTHING).is_none());
        assert!(live_semantic_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(f32::NAN, 24.0)),
            clip
        )
        .is_none());
        assert!(live_semantic_rect(chrome, egui::Rect::EVERYTHING).is_none());
    }

    #[test]
    fn live_semantic_label_has_no_focus_click_or_live_announcement() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let context = PreviewPose::Concerned.context();
        let text = live_semantic_text(context);
        let mut identity = None;
        for _ in 0..2 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(320.0, 240.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    let rect =
                        egui::Rect::from_min_size(ui.min_rect().min, egui::vec2(120.0, 24.0));
                    let response = live_semantic_response(ui, rect, context).unwrap();
                    assert!(!response.sense.is_focusable());
                    assert!(!response.sense.senses_click());
                    assert!(!response.sense.senses_drag());
                    if let Some(previous) = identity {
                        assert_eq!(previous, response.id);
                    }
                    identity = Some(response.id);
                },
            );
            // This semantic-only fixture does not submit output to a renderer.
            output.textures_delta.clear();
            let update = output.platform_output.accesskit_update.unwrap();
            let node = update
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.role() == egui::accesskit::Role::Label
                        && node
                            .value()
                            .is_some_and(|value| value.starts_with("ASCII Organism: "))
                })
                .expect("full semantic label is exposed even in narrow chrome");
            assert_eq!(node.1.value(), Some(text.as_str()));
            assert!(!node.1.supports_action(egui::accesskit::Action::Focus));
            assert!(!node.1.supports_action(egui::accesskit::Action::Click));
            assert_eq!(node.1.live(), None);
        }
    }

    #[test]
    fn live_tooltip_defers_to_busy_pointer_and_keyboard_input() {
        assert!(live_tooltip_allowed(false, false, false));
        for state in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            assert!(!live_tooltip_allowed(state.0, state.1, state.2));
        }
    }

    #[test]
    fn live_semantics_follow_visible_final_context_without_new_scheduler() {
        let source = include_str!("organism_ui.rs");
        let draw = source
            .split_once("egui::Panel::bottom(\"ascii_organism_chrome\")")
            .unwrap()
            .1
            .split_once("struct PendingCommand")
            .unwrap()
            .0;
        assert!(
            draw.find("if policy.inline_visible(now)").unwrap()
                < draw
                    .find("live_semantic_response(ui, rect, display)")
                    .unwrap()
        );
        assert!(draw.contains("response.on_hover_text(live_semantic_text(display))"));
        let helper = source
            .split_once("fn live_semantic_response(")
            .unwrap()
            .1
            .split_once("pub struct OrganismHost")
            .unwrap()
            .0;
        assert!(!helper.contains("request_repaint"));
        assert!(!helper.contains("request_focus"));
        assert!(!helper.contains("Sense::click"));
    }

    #[test]
    fn body_bridge_endpoint_requires_submitted_text_inside_the_clip() {
        let text = egui::Rect::from_min_max(egui::pos2(8.0, 8.0), egui::pos2(80.0, 60.0));
        let far_right = egui::Rect::from_min_max(egui::pos2(120.0, 0.0), egui::pos2(160.0, 72.0));
        assert!(far_right.is_positive());
        assert!(!submitted_text_visible(text, far_right));
        let partial = egui::Rect::from_min_max(egui::pos2(50.0, 0.0), egui::pos2(160.0, 72.0));
        assert!(submitted_text_visible(text, partial));
        let edge = egui::Rect::from_min_max(egui::pos2(80.0, 0.0), egui::pos2(160.0, 72.0));
        assert!(!submitted_text_visible(text, edge));
    }

    #[test]
    fn body_bridge_requires_a_submitted_source_and_uses_relative_frames() {
        let mut bridge = PaintedBridge::default();
        let source = PreviewPose::Working.context();
        let target = PreviewPose::Success.context();
        let unsubmitted = bridge.plan(Duration::ZERO, source, 9000);
        assert!(unsubmitted.context.transition.is_none());
        assert!(bridge.last_settled.is_none());
        assert!(bridge
            .plan(Duration::ZERO, target, 9000)
            .context
            .transition
            .is_none());
        bridge.submitted(unsubmitted);
        let start = Duration::from_secs(1);
        let plan = bridge.plan(start, target, 9000);
        assert_eq!(plan.context.behavior, target.behavior);
        assert!(plan.context.transition.is_some());
        assert_eq!(plan.frame, 0);
        bridge.submitted(plan);
        let almost = bridge.plan(Duration::from_millis(1399), target, 9004);
        assert!(almost.context.transition.is_some());
        assert_eq!(almost.frame, 3);
        bridge.submitted(almost);
        let done = bridge.plan(Duration::from_millis(1400), target, 9004);
        assert!(done.context.transition.is_none());
        assert_eq!(done.frame, 9004);
        bridge.submitted(done);
        assert_eq!(bridge.last_settled, Some(target.behavior));
        assert!(bridge.active.is_none());
    }

    #[test]
    fn body_bridge_new_target_interrupts_without_queuing_or_stale_semantics() {
        let mut bridge = PaintedBridge::default();
        let source = PreviewPose::Working.context();
        let success = PreviewPose::Success.context();
        let failure = PreviewPose::Concerned.context();
        let first = bridge.plan(Duration::ZERO, source, 0);
        bridge.submitted(first);
        let arc = bridge.plan(Duration::from_millis(100), success, 1);
        bridge.submitted(arc);
        let interrupted = bridge.plan(Duration::from_millis(200), failure, 2);
        assert_eq!(interrupted.context, failure);
        assert_eq!(interrupted.frame, 2);
        bridge.submitted(interrupted);
        assert_eq!(bridge.last_settled, Some(failure.behavior));
        assert!(bridge.active.is_none());
        // Status is always rendered independently from the true target.
        assert_eq!(
            live_status_text(failure, 2, 640.0),
            host_text(failure, 2, 640.0, true).1
        );
    }

    #[test]
    fn body_bridge_dormant_cadence_paints_target_without_a_new_wake() {
        let mut bridge = PaintedBridge::default();
        let source = PreviewPose::Working.context();
        let target = PreviewPose::Success.context();
        let seed = bridge.plan(Duration::ZERO, source, 0);
        bridge.submitted(seed);
        let dormant = bridge.plan_at_cadence(Duration::from_secs(1), target, 10, false);
        assert_eq!(dormant.context, target);
        assert!(dormant.after_submit.active.is_none());
        assert_eq!(dormant.after_submit.last_settled, Some(target.behavior));
        // Entering slow cadence also retires an in-flight bridge.
        let arc = bridge.plan(Duration::from_secs(1), target, 10);
        bridge.submitted(arc);
        assert!(bridge.active.is_some());
        let stopped = bridge.plan_at_cadence(Duration::from_millis(1100), target, 11, false);
        assert_eq!(stopped.context, target);
        bridge.submitted(stopped);
        assert!(bridge.active.is_none());
    }

    #[test]
    fn body_bridge_is_limited_to_full_expanded_visible_non_greeting_paints() {
        assert!(bridge_allowed(true, true, true, false));
        for (expanded, full, visible, greeting) in [
            (false, true, true, false),
            (true, false, true, false),
            (true, true, false, false),
            (true, true, true, true),
        ] {
            assert!(!bridge_allowed(expanded, full, visible, greeting));
        }
        let mut bridge = PaintedBridge::default();
        let plan = bridge.plan(Duration::ZERO, PreviewPose::Working.context(), 0);
        bridge.submitted(plan);
        bridge.reset();
        assert!(bridge
            .plan(Duration::from_secs(1), PreviewPose::Success.context(), 10)
            .context
            .transition
            .is_none());
    }

    #[test]
    fn body_bridge_input_owner_and_mode_changes_retire_painted_endpoints() {
        let mut host = OrganismHost::default();
        host.acquire(Some("one"), None, false);
        host.configure_bridge(true);
        for event in 0..3 {
            let plan = host
                .bridge
                .plan(Duration::ZERO, PreviewPose::Working.context(), 0);
            host.bridge.submitted(plan);
            match event {
                0 => host.accepted_input("one"),
                1 => host.acquire(None, None, false),
                _ => host.configure_bridge(false),
            }
            assert!(host.bridge.last_settled.is_none());
            assert!(host.bridge.active.is_none());
        }
    }

    #[test]
    fn preview_pause_freezes_stage_frame_and_exact_remaining_time() {
        let mut demo = PreviewSequence::default();
        demo.start(Duration::from_secs(10));
        let pause_at = Duration::from_millis(12345);
        assert!(demo.pause(pause_at));
        let frozen = demo.sample(pause_at).unwrap();
        assert_eq!(frozen.pose, PreviewPose::Working);
        assert_eq!(frozen.next_in, Duration::from_millis(1655));
        assert_eq!(frozen.frame(true), 123);
        let later = Duration::from_secs(3600);
        assert_eq!(demo.sample(later), Some(frozen));
        assert_eq!(frozen.next_wake(true), None);
        assert_eq!(frozen.next_wake(false), None);
        assert!(demo.resume(later));
        let resumed = demo.sample(later).unwrap();
        assert_eq!(resumed.frame(true), frozen.frame(true));
        assert_eq!(resumed.next_in, frozen.next_in);
        assert_eq!(
            demo.sample(later + frozen.next_in).unwrap().pose,
            PreviewPose::Concerned
        );
        assert!(demo.sample(later + Duration::from_millis(7655)).is_none());
    }

    #[test]
    fn paused_preview_respects_motion_preferences_without_resuming() {
        let mut demo = PreviewSequence::default();
        demo.start(Duration::ZERO);
        assert!(demo.pause(Duration::from_millis(1750)));
        let step = demo.sample(Duration::from_secs(100)).unwrap();
        assert_eq!(step.frame(true), 17);
        for motion in [OrganismMotion::Calm, OrganismMotion::Static] {
            assert_eq!(step.frame(motion == OrganismMotion::Full), 0);
            assert_eq!(step.next_wake(motion == OrganismMotion::Full), None);
        }
        assert_eq!(step.frame(true), 17);
        assert!(demo.sample(Duration::from_secs(200)).unwrap().paused);
    }

    #[test]
    fn preview_pause_cannot_revive_expiry_or_repeat_retirement() {
        let mut demo = PreviewSequence::default();
        demo.start(Duration::ZERO);
        assert!(!demo.pause(Duration::from_secs(10)));
        let retired = demo.revision;
        assert!(!demo.resume(Duration::from_secs(11)));
        assert!(demo.sample(Duration::from_secs(12)).is_none());
        assert_eq!(demo.revision, retired);
        demo.start(Duration::from_secs(20));
        assert!(demo.pause(Duration::from_millis(29999)));
        assert!(demo.resume(Duration::from_secs(100)));
        assert_eq!(
            demo.sample(Duration::from_secs(100)).unwrap().next_in,
            Duration::from_millis(1)
        );
        assert!(demo.sample(Duration::from_millis(100001)).is_none());
    }

    #[test]
    fn preview_control_tickets_reject_stale_repeated_and_cross_run_actions() {
        let mut preview = PreviewUi::default();
        preview.sync_viewport(true, false);
        let old_hello = preview.demo.ticket(PreviewAction::Hello);
        let play = preview.demo.ticket(PreviewAction::Play);
        assert!(preview.apply_control(play, Duration::ZERO));
        assert!(!preview.apply_control(play, Duration::ZERO));
        assert!(!preview.apply_control(old_hello, Duration::from_secs(1)));
        let pause = preview.demo.ticket(PreviewAction::Pause);
        assert!(preview.apply_control(pause, Duration::from_secs(1)));
        assert!(!preview.apply_control(pause, Duration::from_secs(2)));
        let old_resume = preview.demo.ticket(PreviewAction::Resume);
        assert!(preview.apply_control(old_resume, Duration::from_secs(3)));
        let pause = preview.demo.ticket(PreviewAction::Pause);
        assert!(preview.apply_control(pause, Duration::from_secs(4)));
        assert!(!preview.apply_control(old_resume, Duration::from_secs(5)));
        let old_stop = preview.demo.ticket(PreviewAction::Stop);
        assert!(preview.apply_control(old_stop, Duration::from_secs(6)));
        assert!(!preview.apply_control(old_hello, Duration::from_secs(6)));
        let play = preview.demo.ticket(PreviewAction::Play);
        assert!(preview.apply_control(play, Duration::from_secs(7)));
        assert!(!preview.apply_control(old_stop, Duration::from_secs(8)));
        assert!(preview.demo.sample(Duration::from_secs(8)).is_some());
        let stop = preview.demo.ticket(PreviewAction::Stop);
        assert!(!preview.apply_control(stop, Duration::from_secs(17)));
        assert!(preview.demo.sample(Duration::from_secs(18)).is_none());
        let hello = preview.demo.ticket(PreviewAction::Hello);
        assert!(preview.apply_control(hello, Duration::from_secs(20)));
        assert!(!preview.apply_control(hello, Duration::from_secs(29)));
        let fresh_hello = preview.demo.ticket(PreviewAction::Hello);
        assert!(preview.apply_control(fresh_hello, Duration::from_secs(29)));
    }

    #[test]
    fn paused_preview_cancel_reopen_preserves_manual_pose_and_retires_hello() {
        for cancel in 0..4 {
            let mut preview = PreviewUi::default();
            preview.sync_viewport(true, false);
            preview.select_pose(PreviewPose::Curious);
            let hello = preview.demo.ticket(PreviewAction::Hello);
            preview.start_demo(Duration::ZERO);
            assert!(preview.demo.pause(Duration::from_secs(1)));
            match cancel {
                0 => preview.close(),
                1 => preview.sync_viewport(false, false),
                2 => preview.sync_viewport(true, true),
                _ => preview.select_pose(PreviewPose::Curious),
            }
            preview.sync_viewport(true, false);
            assert!(preview.demo.sample(Duration::from_secs(100)).is_none());
            assert_eq!(preview.pose, PreviewPose::Curious);
            assert!(!preview.apply_control(hello, Duration::from_secs(100)));
            assert_eq!(
                preview.display_context(Duration::from_secs(100), None),
                PreviewPose::Curious.context()
            );
        }
    }

    #[test]
    fn preview_explanation_tracks_hello_and_demo_without_spending_cooldown() {
        let mut preview = PreviewUi::default();
        preview.sync_viewport(true, false);
        preview.select_pose(PreviewPose::Curious);
        let hello = preview.demo.ticket(PreviewAction::Hello);
        assert!(preview.apply_control(hello, Duration::ZERO));
        let greeting = preview.display_context(Duration::from_secs(1), None);
        assert_eq!(greeting.behavior, Behavior::Approach);
        preview.start_demo(Duration::from_secs(1));
        assert!(preview.demo.pause(Duration::from_secs(4)));
        let step = preview.demo.sample(Duration::from_secs(5));
        assert_eq!(
            preview.display_context(Duration::from_secs(5), step),
            PreviewPose::Working.context()
        );
        let current_hello = preview.demo.ticket(PreviewAction::Hello);
        assert!(!preview.apply_control(current_hello, Duration::from_secs(5)));
        preview.demo.stop();
        assert_eq!(
            preview.model.greeting_availability(Duration::from_secs(6)),
            GreetingAvailability::CoolingDown
        );
        assert_eq!(
            preview.display_context(Duration::from_secs(6), None),
            PreviewPose::Curious.context()
        );
        assert_eq!(
            preview.model.greeting_availability(Duration::from_secs(8)),
            GreetingAvailability::Available
        );
    }

    fn preview_button_frame(
        ctx: &egui::Context,
        ticket: PreviewTicket,
        label: &str,
        events: Vec<egui::Event>,
    ) -> (bool, egui::Rect) {
        let mut clicked = false;
        let mut rect = egui::Rect::NOTHING;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(320.0, 240.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                ui.push_id(ticket.id(), |ui| {
                    let response = ui.add_sized([120.0, 24.0], egui::Button::new(label));
                    clicked = response.clicked();
                    rect = response.rect;
                });
            },
        );
        output.textures_delta.clear();
        (clicked, rect)
    }

    fn preview_button_release(
        old: PreviewTicket,
        old_label: &str,
        new: PreviewTicket,
        new_label: &str,
    ) -> bool {
        let ctx = egui::Context::default();
        let (_, rect) = preview_button_frame(&ctx, old, old_label, vec![]);
        let pos = rect.center();
        preview_button_frame(&ctx, old, old_label, vec![egui::Event::PointerMoved(pos)]);
        preview_button_frame(
            &ctx,
            old,
            old_label,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        preview_button_frame(&ctx, new, new_label, vec![]);
        preview_button_frame(
            &ctx,
            new,
            new_label,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        )
        .0
    }

    #[test]
    fn preview_buttons_do_not_migrate_press_to_new_action_run_or_hello() {
        let mut demo = PreviewSequence::default();
        demo.start(Duration::ZERO);
        let pause = demo.ticket(PreviewAction::Pause);
        assert!(preview_button_release(
            pause,
            "Pause demo",
            pause,
            "Pause demo"
        ));
        assert!(demo.pause(Duration::from_secs(1)));
        assert!(!preview_button_release(
            pause,
            "Pause demo",
            demo.ticket(PreviewAction::Resume),
            "Resume demo"
        ));
        let stop = demo.ticket(PreviewAction::Stop);
        demo.stop();
        assert!(!preview_button_release(
            stop,
            "Stop demo",
            demo.ticket(PreviewAction::Play),
            "Play demo"
        ));
        let hello = demo.ticket(PreviewAction::Hello);
        demo.start(Duration::from_secs(2));
        demo.stop();
        assert!(!preview_button_release(
            hello,
            "Say hello",
            demo.ticket(PreviewAction::Hello),
            "Say hello"
        ));
    }

    #[test]
    fn preview_revision_is_checked_and_controls_share_snapshot_identity() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate_preview_revision(&counter), Some(u64::MAX - 1));
        assert_eq!(allocate_preview_revision(&counter), None);
        let mut exhausted = PreviewUi {
            demo: PreviewSequence {
                revision: None,
                ..Default::default()
            },
            ..Default::default()
        };
        exhausted.sync_viewport(true, false);
        for action in [
            PreviewAction::Play,
            PreviewAction::Pause,
            PreviewAction::Resume,
            PreviewAction::Stop,
            PreviewAction::Hello,
        ] {
            assert!(!exhausted.apply_control(exhausted.demo.ticket(action), Duration::ZERO));
        }
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
        let source = include_str!("organism_ui.rs");
        let show = source
            .split("pub fn show(&mut self, ui: &mut Ui, motion:")
            .nth(1)
            .unwrap()
            .split("fn preview_next_wake")
            .next()
            .unwrap();
        assert!(show.contains(".push_id(hello_ticket.id()"));
        assert_eq!(show.matches(".push_id(ticket.id()").count(), 2);
        assert!(show.contains("self.display_context(now, demo)"));
        assert!(show.contains("Example: {}"));
        assert!(show.contains("step.next_wake(full)"));
    }

    #[test]
    fn preview_demo_runs_once_at_exact_phase_boundaries() {
        let mut demo = PreviewSequence::default();
        assert!(demo.sample(Duration::ZERO).is_none());
        demo.start(Duration::ZERO);
        for (millis, pose, remaining) in [
            (0, PreviewPose::Calm, 2000),
            (1999, PreviewPose::Calm, 1),
            (2000, PreviewPose::Working, 2000),
            (4000, PreviewPose::Concerned, 2000),
            (6000, PreviewPose::Success, 2000),
            (8000, PreviewPose::Sleeping, 2000),
            (9999, PreviewPose::Sleeping, 1),
        ] {
            let step = demo.sample(Duration::from_millis(millis)).unwrap();
            assert_eq!(step.pose, pose);
            assert_eq!(step.next_in, Duration::from_millis(remaining));
        }
        assert!(demo.sample(Duration::from_secs(10)).is_none());
        assert!(demo.sample(Duration::from_secs(11)).is_none());
        demo.start(Duration::from_secs(20));
        assert!(demo.sample(Duration::from_secs(80)).is_none());
        assert!(demo.sample(Duration::from_secs(81)).is_none());
    }

    #[test]
    fn preview_demo_static_has_only_finite_phase_and_end_wakes() {
        let mut demo = PreviewSequence::default();
        demo.start(Duration::ZERO);
        let first = demo.sample(Duration::ZERO).unwrap();
        assert_eq!(first.next_wake(false), Some(Duration::from_secs(2)));
        assert_eq!(first.next_wake(true), Some(Duration::from_millis(100)));
        let last = demo.sample(Duration::from_millis(9999)).unwrap();
        assert_eq!(last.next_wake(false), Some(Duration::from_millis(1)));
        assert!(demo.sample(Duration::from_secs(10)).is_none());
        assert_eq!(
            preview_next_wake(
                Duration::from_secs(10),
                false,
                Duration::ZERO,
                Duration::ZERO,
                GreetingAvailability::Available,
            ),
            None
        );
    }

    #[test]
    fn preview_demo_preserves_manual_pose_and_real_greeting_cooldown() {
        let mut preview = PreviewUi::default();
        preview.sync_viewport(true, false);
        preview.select_pose(PreviewPose::Curious);
        assert!(preview.model.say_hello(Duration::ZERO));
        preview.start_demo(Duration::from_secs(1));
        assert_eq!(preview.pose, PreviewPose::Curious);
        assert_eq!(
            preview.model.greeting_availability(Duration::from_secs(2)),
            GreetingAvailability::CoolingDown
        );
        assert_eq!(
            preview.model.context(Duration::from_secs(2)),
            Some(PreviewPose::Curious.context())
        );
        assert!(preview.demo.sample(Duration::from_secs(11)).is_none());
        assert_eq!(preview.pose, PreviewPose::Curious);
        assert_eq!(
            preview.model.greeting_availability(Duration::from_secs(11)),
            GreetingAvailability::Available
        );
    }

    #[test]
    fn preview_demo_cancels_on_stop_pose_close_and_inactive_viewport() {
        let mut preview = PreviewUi::default();
        preview.sync_viewport(true, false);
        preview.start_demo(Duration::ZERO);
        preview.demo.stop();
        assert!(preview.demo.sample(Duration::from_secs(1)).is_none());
        preview.start_demo(Duration::ZERO);
        preview.select_pose(PreviewPose::Sleeping);
        assert!(preview.demo.sample(Duration::from_secs(1)).is_none());
        assert_eq!(preview.pose, PreviewPose::Sleeping);
        preview.start_demo(Duration::ZERO);
        preview.close();
        assert!(preview.demo.sample(Duration::from_secs(1)).is_none());
        for (focused, occluded) in [(false, false), (true, true)] {
            preview.sync_viewport(true, false);
            preview.start_demo(Duration::ZERO);
            preview.sync_viewport(focused, occluded);
            assert!(preview.demo.sample(Duration::from_secs(1)).is_none());
            assert_eq!(
                preview.model.greeting_availability(Duration::from_secs(1)),
                GreetingAvailability::Closed
            );
            preview.sync_viewport(true, false);
            assert!(preview.demo.sample(Duration::from_secs(2)).is_none());
        }
    }

    #[test]
    fn companion_height_depends_only_on_explicit_preferences() {
        assert_eq!(host_height(false, false), 0.0);
        assert_eq!(host_height(false, true), 0.0);
        assert_eq!(host_height(true, false), 24.0);
        assert_eq!(host_height(true, true), 96.0);
        for width in [0.0, 119.9, 120.0, 640.0] {
            for focused in [false, true] {
                let policy = PresentationPolicy {
                    enabled: true,
                    focused_owner: focused && host_geometry_allows(width),
                    local: true,
                    ..PresentationPolicy::default()
                };
                let _visible = policy.inline_visible(Duration::ZERO);
                assert_eq!(host_height(policy.enabled, true), 96.0);
            }
        }
    }

    #[test]
    fn expanded_body_and_status_share_the_final_context_and_frame() {
        for pose in PreviewPose::ALL {
            for frame in [0, 1, 9] {
                let context = pose.context();
                let (sprite, status) = host_text(context, frame, 640.0, true);
                assert_eq!(sprite.unwrap(), sprite_frame_with_context(context, frame));
                assert_eq!(status, live_status_text(context, frame, 640.0));
                let (compact, compact_status) = host_text(context, frame, 640.0, false);
                assert!(compact.is_none());
                assert_eq!(compact_status, status);
            }
        }
    }

    #[test]
    fn watch_owner_loss_and_nonrunning_batch_clear_observation_without_life_change() {
        let mut host = OrganismHost {
            rhythm_enabled: true,
            ..OrganismHost::default()
        };
        host.acquire(Some("one"), Some((7, true)), true);
        let before = format!("{:?}", host.life.state());
        host.watch.attach(
            Duration::ZERO,
            Some(crate::terminal::WatchCommand {
                source: 1,
                generation: 7,
                revision: 1,
            }),
        );
        host.acquire(None, None, false);
        assert_eq!(
            host.watch.rhythm(Duration::from_secs(100)),
            WatchRhythm::Steady
        );
        host.acquire(Some("two"), Some((8, true)), true);
        host.batch("two", &VecDeque::new(), &[], false, false);
        assert_eq!(
            host.watch.rhythm(Duration::from_secs(100)),
            WatchRhythm::Steady
        );
        assert_eq!(format!("{:?}", host.life.state()), before);
    }

    #[test]
    fn watch_retreat_drops_activity_before_the_next_draw() {
        let mut host = OrganismHost {
            rhythm_enabled: true,
            last_input: Some(Duration::ZERO),
            ..OrganismHost::default()
        };
        host.acquire(Some("one"), Some((7, true)), true);
        let update = crate::terminal::WatchParsed {
            origin: None,
            command: Some(crate::terminal::WatchCommand {
                source: 1,
                generation: 7,
                revision: 1,
            }),
            trusted_start: false,
            batch: None,
            invalidated: false,
        };
        host.watch_parsed("one", update, false);
        assert_eq!(
            host.watch.rhythm(Duration::from_secs(100)),
            WatchRhythm::Steady
        );
        host.last_input = None;
        host.watch_parsed("one", update, false);
        assert_eq!(
            host.watch.rhythm(Duration::from_secs(100)),
            WatchRhythm::Waiting
        );
    }

    #[test]
    fn explicit_hello_wiring_keeps_palette_target_and_admission_one_shot() {
        let input = include_str!("app/input.rs");
        let palette = input
            .split("pub(crate) fn dispatch_palette_command")
            .nth(1)
            .unwrap()
            .split("/// 切换活跃会话")
            .next()
            .unwrap();
        assert!(
            palette.find("organism_target.clone()").unwrap()
                < palette.find("self.command_palette.close()").unwrap()
        );
        assert!(palette.contains("memory.surrender_focus(id)"));
        assert!(palette.contains("self.say_hello_to_organism(ctx, hello_target.as_deref())"));
        let rendering = include_str!("app/rendering.rs");
        let palette_render = rendering
            .split(".hint_text(\"Search commands... (? asks AI)\")")
            .nth(1)
            .unwrap()
            .split("ui.separator()")
            .next()
            .unwrap();
        assert!(palette_render
            .contains("self.command_palette.query_widget_id = Some(search_response.id)"));
        let source = include_str!("organism_ui.rs");
        let admission = source
            .split("pub(crate) fn say_hello_to_organism")
            .nth(1)
            .unwrap()
            .split("pub(crate) fn prepare_organism")
            .next()
            .unwrap();
        assert_eq!(admission.matches("self.prepare_organism(ctx)").count(), 1);
        assert_eq!(admission.matches("self.organism.request_hello").count(), 1);
        assert!(!admission.contains("request_repaint_after"));
    }

    #[test]
    fn explicit_hello_requires_original_owner_idle_motion_and_real_input_quiet() {
        let mut host = OrganismHost::default();
        host.set_available_geometry(host_rect(640.0, 96.0), host_rect(1000.0, 1000.0));
        host.acquire(Some("original"), None, false);
        let before = host.life.state();
        let now = Duration::from_secs(10);
        let mut config = Config {
            ascii_organism_enabled: true,
            ascii_organism_motion: Some(OrganismMotion::Calm),
            ..Default::default()
        };
        assert!(host.request_hello_at(now, None, &config).is_err());
        assert!(host
            .request_hello_at(now, Some("replacement"), &config)
            .is_err());
        config.ascii_organism_motion = Some(OrganismMotion::Static);
        assert!(host
            .request_hello_at(now, Some("original"), &config)
            .is_err());
        config.ascii_organism_motion = Some(OrganismMotion::Full);
        host.running = true;
        assert!(host
            .request_hello_at(now, Some("original"), &config)
            .is_err());
        host.running = false;
        host.last_input = Some(now);
        assert!(host
            .request_hello_at(now, Some("original"), &config)
            .is_err());
        host.last_input = None;
        let pressed = explicit_window_available(Some(true), Some(true), false, true);
        assert!(!pressed);
        assert!(host
            .request_hello_available_at(now, pressed, Some("original"), &config)
            .is_err());
        assert!(host_attention_snapshot(&host, now).active.is_none());
        assert!(host
            .request_hello_available_at(
                now,
                explicit_window_available(Some(true), Some(true), false, false),
                Some("original"),
                &config
            )
            .is_ok());
        assert_eq!(format!("{:?}", host.life.state()), format!("{:?}", before));
        assert!(host.pending.is_none());
    }

    #[test]
    fn hover_cue_geometry_is_fixed_inside_existing_chrome_and_clipped() {
        for height in [24.0, 72.0] {
            let target =
                egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(96.0, height));
            let cue = hover_cue_rect(target, target).unwrap();
            assert_eq!(cue.size(), egui::vec2(12.0, 1.0));
            assert!(target.contains_rect(cue));
            assert_eq!(cue.min.x, target.min.x + 8.0);
            assert!(hover_cue_rect(target, target.translate(egui::vec2(200.0, 0.0))).is_none());
        }
        assert!(hover_cue_rect(egui::Rect::NOTHING, egui::Rect::EVERYTHING).is_none());
        assert!(hover_cue_rect(egui::Rect::EVERYTHING, egui::Rect::EVERYTHING).is_none());
        assert!(hover_cue_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(20.0, 7.0)),
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0))
        )
        .is_none());
    }

    #[test]
    fn live_hover_admission_prioritizes_input_visibility_and_static() {
        let mut policy = static_policy();
        let now = Duration::from_secs(10);
        assert!(!live_greeting_allowed(policy, now, false, false));
        policy.motion = Some(OrganismMotion::Calm);
        assert!(live_greeting_allowed(policy, now, false, false));
        assert!(!live_greeting_allowed(policy, now, true, false));
        assert!(!live_greeting_allowed(policy, now, false, true));
        policy.last_input = Some(now);
        assert!(!live_greeting_allowed(policy, now, false, false));
        policy.last_input = None;
        policy.local = false;
        assert!(!live_greeting_allowed(policy, now, false, false));
        policy.local = true;
        policy.alternate_screen = true;
        assert!(!live_greeting_allowed(policy, now, false, false));
        policy.alternate_screen = false;
        policy.focused_owner = false;
        assert!(!live_greeting_allowed(policy, now, false, false));
    }

    #[test]
    fn live_status_explains_final_behavior_without_expanding_narrow_layout() {
        for behavior in [
            Behavior::WatchCommand,
            Behavior::UnknownOutcome,
            Behavior::InspectError,
            Behavior::CelebrateBig,
            Behavior::Approach,
        ] {
            let context = RenderContext::new(behavior, BodyLanguage::default(), false);
            assert!(live_status_text(context, 0, 640.0).contains(behavior_explanation(behavior)));
            assert!(!live_status_text(context, 0, 120.0).contains(behavior_explanation(behavior)));
        }
    }

    #[test]
    fn hidden_logic_is_root_only_and_uses_current_visibility() {
        assert!(hidden_root_viewport(true, Some(true), Some(false)));
        assert!(hidden_root_viewport(true, Some(false), Some(true)));
        assert!(hidden_root_viewport(true, None, Some(true)));
        assert!(!hidden_root_viewport(true, Some(true), None));
        assert!(!hidden_root_viewport(true, Some(true), Some(true)));
        assert!(!hidden_root_viewport(false, Some(false), Some(false)));
    }

    #[test]
    fn observed_hidden_pass_discards_completion_and_restore_quarantines() {
        let mut host = OrganismHost::default();
        host.acquire(Some("local"), Some((7, true)), true);
        host.pending = Some(pending());
        let before = format!("{:?}", host.life.state());
        // This is the exact state-only operation performed by App::logic.
        host.acquire(None, None, false);
        host.batch(
            "local",
            &VecDeque::new(),
            &[completion(Some(0))],
            false,
            false,
        );
        assert_eq!(format!("{:?}", host.life.state()), before);
        assert!(host.pending.is_none());
        host.acquire(Some("local"), Some((7, true)), false);
        host.batch(
            "local",
            &VecDeque::new(),
            &[completion(Some(0))],
            false,
            false,
        );
        assert_eq!(format!("{:?}", host.life.state()), before);
        assert!(host.pending.is_none());
        assert!(!host.quarantine_batch);
    }

    #[test]
    fn hidden_logic_wiring_keeps_the_hook_state_only() {
        let main = include_str!("main.rs");
        let hook = main
            .split("    fn logic(")
            .nth(1)
            .expect("root App logic hook")
            .split("    fn ui(")
            .next()
            .expect("next UI method");
        assert!(hook.contains("self.observe_organism_visibility(ctx);"));
        assert!(!hook.contains("request_repaint"));
        assert!(!hook.contains("render_"));
        let panel = include_str!("config_panel.rs");
        assert!(panel.contains("self.organism_preview.close();"));
    }

    fn attention_input(inside: bool, eligible: bool) -> LiveAttentionInput {
        LiveAttentionInput {
            base_context: PreviewPose::Calm.context(),
            activity: LiveActivityKnowledge::KnownIdle,
            hard_eligible: eligible,
            hover_available: true,
            pointer_inside: inside,
        }
    }

    fn host_attention_snapshot(host: &OrganismHost, now: Duration) -> LiveAttentionSnapshot {
        host.attention
            .snapshot(
                now,
                host.owner.as_ref().unwrap(),
                attention_input(host.pointer_inside, true),
            )
            .unwrap()
    }

    #[test]
    fn native_activity_requires_positive_clean_prompt_evidence() {
        use LiveActivityKnowledge::{Busy, KnownIdle, Unknown};
        assert_eq!(
            live_activity_knowledge(false, true, false, false, false),
            KnownIdle
        );
        // Startup/no integration, completed command without a new prompt,
        // dirty draft and tainted accepted input all lack clean prompt proof.
        assert_eq!(
            live_activity_knowledge(false, false, false, false, false),
            Unknown
        );
        for (pending, barrier, agent) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            assert_eq!(
                live_activity_knowledge(false, true, pending, barrier, agent),
                Unknown
            );
            assert_eq!(
                live_activity_knowledge(true, true, pending, barrier, agent),
                Busy
            );
        }
        assert_eq!(
            live_activity_knowledge(true, false, false, false, false),
            Busy
        );
        // Clearing uncertainty with fresh positive evidence does not persist
        // an Agent/pending-input block or manufacture a running command.
        assert_eq!(
            live_activity_knowledge(false, true, false, false, false),
            KnownIdle
        );
    }

    #[test]
    fn native_activity_maps_real_prompt_evidence_and_delayed_start() {
        use LiveActivityKnowledge::{Busy, KnownIdle, Unknown};
        let mut terminal = crate::terminal::TerminalState::new(80, 24);
        let classify = |terminal: &crate::terminal::TerminalState| {
            let running = terminal.command_records().back().is_some_and(|record| {
                record.state == crate::terminal::CommandState::Running && record.start_mark_seen
            });
            live_activity_knowledge(
                running,
                terminal.prompt_input_is_empty(),
                false,
                false,
                false,
            )
        };
        assert_eq!(classify(&terminal), Unknown);
        terminal.process_input(b"\x1b]133;A\x07");
        assert_eq!(classify(&terminal), Unknown);
        terminal.process_input(b"$ \x1b]133;B\x07");
        assert_eq!(classify(&terminal), KnownIdle);
        // Local admission is recorded before echo or the command-start mark.
        terminal.note_user_input(b"echo hello\r");
        assert_eq!(classify(&terminal), Unknown);
        terminal.process_input(b"echo hello");
        assert_eq!(classify(&terminal), Unknown);
        terminal.process_input(b"\r\n\x1b]133;C\x07");
        assert_eq!(classify(&terminal), Busy);
        terminal.process_input(b"hello\r\n\x1b]133;D;0\x07");
        assert_eq!(classify(&terminal), Unknown);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        assert_eq!(classify(&terminal), KnownIdle);
    }

    #[test]
    fn native_activity_unknown_requires_explicit_intent_and_existing_gates() {
        let owner = String::from("local");
        let mut attention = LiveAttention::default();
        attention.set_owner(Some(owner.clone()));
        let mut inside = false;
        let now = Duration::from_secs(10);
        let mut input = attention_input(true, true);
        input.activity = live_activity_knowledge(false, false, false, false, false);
        let pointer = observe_live_attention(
            &mut attention,
            &mut inside,
            now,
            &owner,
            input,
            host_rect(96.0, 24.0),
            &[egui::Event::PointerMoved(egui::pos2(12.0, 12.0))],
        )
        .unwrap();
        assert!(pointer.candidate.is_none());
        input.hard_eligible = false;
        let blocked = attention
            .advance(now, &owner, input, LiveAttentionEvent::Explicit)
            .unwrap();
        assert!(blocked.active.is_none());
        input.hard_eligible = true;
        let accepted = attention
            .advance(now, &owner, input, LiveAttentionEvent::Explicit)
            .unwrap();
        assert!(matches!(
            accepted.outcome,
            LiveAttentionOutcome::Accepted(_)
        ));
        input.activity = LiveActivityKnowledge::Busy;
        let busy = attention
            .advance(now, &owner, input, LiveAttentionEvent::Reconcile)
            .unwrap();
        assert!(busy.active.is_none());
    }

    #[test]
    fn native_activity_snapshot_is_invalidated_without_a_submission_ledger() {
        let mut host = OrganismHost::default();
        assert_eq!(host.activity, LiveActivityKnowledge::Unknown);
        host.acquire(Some("local"), None, false);
        host.activity = LiveActivityKnowledge::KnownIdle;
        host.accepted_input("local");
        assert_eq!(host.activity, LiveActivityKnowledge::Unknown);
        // A delayed OSC C has not established Running. A later actual start
        // is positive Busy; completion without new prompt is Unknown again.
        host.acquire(Some("local"), Some((7, true)), true);
        assert_eq!(host.activity, LiveActivityKnowledge::Busy);
        host.acquire(Some("local"), Some((7, true)), false);
        assert_eq!(host.activity, LiveActivityKnowledge::Unknown);
        host.activity = LiveActivityKnowledge::KnownIdle;
        host.batch("local", &VecDeque::new(), &[], false, false);
        assert_eq!(host.activity, LiveActivityKnowledge::Unknown);
        host.activity = LiveActivityKnowledge::KnownIdle;
        host.acquire(Some("other"), None, false);
        assert_eq!(host.activity, LiveActivityKnowledge::Unknown);
    }

    #[test]
    fn native_activity_uses_existing_facts_in_both_shared_inputs() {
        let source = include_str!("organism_ui.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        let prepare = source
            .split("pub(crate) fn prepare_organism(")
            .nth(1)
            .unwrap()
            .split("pub(crate) fn render_organism(")
            .next()
            .unwrap();
        for fact in [
            "terminal.prompt_input_is_empty()",
            "!session.pending_input.is_empty()",
            "direct_input_is_blocked_for_session(id)",
            "self.agent_panel.session_active()",
            "self.agent_panel.bound_session_id() == Some(id.as_str())",
        ] {
            assert!(
                prepare.contains(fact),
                "missing current native fact: {fact}"
            );
        }
        // Every actual shared input, including the native body's observer and
        // the read-only chrome snapshot, must carry the same native evidence.
        // Check bounded surfaces rather than an obsolete global occurrence
        // count: adding a renderer is not a change to activity knowledge.
        let carries_native_activity = |section: &str| {
            let mut seen = false;
            for input in section.split("LiveAttentionInput {").skip(1) {
                seen = true;
                let fields = input.split('}').next().unwrap();
                if !fields
                    .lines()
                    .any(|line| line.trim() == "activity: self.activity,")
                {
                    return false;
                }
            }
            seen
        };
        for (label, start, end) in [
            (
                "explicit greeting",
                "    fn request_hello_at(",
                "    pub fn accepted_input(",
            ),
            (
                "native body",
                "    fn draw_native_body(",
                "    pub fn draw(",
            ),
            (
                "chrome observer and snapshot",
                "    pub fn draw(",
                "struct PendingCommand",
            ),
        ] {
            let section = source
                .split(start)
                .nth(1)
                .unwrap()
                .split(end)
                .next()
                .unwrap();
            assert!(
                carries_native_activity(section),
                "native evidence missing in {label}"
            );
            // A single caller substituting an idle claim must fail even if all
            // other inputs in that surface remain correct.
            let invented_idle = section.replacen(
                "activity: self.activity,",
                "activity: LiveActivityKnowledge::KnownIdle,",
                1,
            );
            assert!(
                !carries_native_activity(&invented_idle),
                "weak contract for {label}"
            );
            let unrelated_decoy = format!("{invented_idle}\n// activity: self.activity,\n");
            assert!(
                !carries_native_activity(&unrelated_decoy),
                "a matching string outside the input cannot establish evidence for {label}"
            );
        }
        assert!(!prepare.contains("shell.write("));
        assert!(!prepare.contains("queue_input("));
    }

    #[test]
    fn native_attention_layout_sync_does_not_manufacture_an_entry() {
        let owner = String::from("local");
        let mut attention = LiveAttention::default();
        attention.set_owner(Some(owner.clone()));
        let mut inside = false;
        let rect = host_rect(96.0, 24.0);
        let now = Duration::from_secs(10);
        let layout = observe_live_attention(
            &mut attention,
            &mut inside,
            now,
            &owner,
            attention_input(true, true),
            rect,
            &[],
        )
        .unwrap();
        assert!(inside);
        assert!(layout.entry.is_none());
        assert!(layout.candidate.is_none());
        let entered = observe_live_attention(
            &mut attention,
            &mut inside,
            now,
            &owner,
            attention_input(true, true),
            rect,
            &[
                egui::Event::PointerMoved(egui::pos2(120.0, 12.0)),
                egui::Event::PointerMoved(egui::pos2(12.0, 12.0)),
            ],
        )
        .unwrap();
        assert!(entered.candidate_visible);
        assert_eq!(
            entered.next_deadline(),
            Some(now + LiveAttention::<String>::DWELL)
        );
    }

    #[test]
    fn native_attention_preserves_same_frame_leave_and_reentry_order() {
        let owner = String::from("local");
        let mut attention = LiveAttention::default();
        attention.set_owner(Some(owner.clone()));
        let mut inside = false;
        let rect = host_rect(96.0, 24.0);
        let now = Duration::from_secs(10);
        let first = observe_live_attention(
            &mut attention,
            &mut inside,
            now,
            &owner,
            attention_input(true, true),
            rect,
            &[egui::Event::PointerMoved(egui::pos2(12.0, 12.0))],
        )
        .unwrap();
        let second = observe_live_attention(
            &mut attention,
            &mut inside,
            now,
            &owner,
            attention_input(true, true),
            rect,
            &[
                egui::Event::PointerGone,
                egui::Event::PointerMoved(egui::pos2(12.0, 12.0)),
            ],
        )
        .unwrap();
        assert_ne!(first.entry, second.entry);
        assert!(second.candidate_visible);
        assert!(inside);
    }

    #[test]
    fn native_attention_discard_pass_does_not_replay_pointer_edges() {
        let ctx = egui::Context::default();
        let owner = String::from("local");
        let mut attention = LiveAttention::default();
        attention.set_owner(Some(owner.clone()));
        let mut inside = false;
        let rect = host_rect(96.0, 24.0);
        let mut snapshots = Vec::new();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(host_rect(320.0, 240.0)),
                events: vec![
                    egui::Event::PointerMoved(egui::pos2(12.0, 12.0)),
                    egui::Event::PointerMoved(egui::pos2(120.0, 12.0)),
                    egui::Event::PointerMoved(egui::pos2(12.0, 12.0)),
                ],
                ..Default::default()
            },
            |ui| {
                let events = ui.input(|input| input.events.clone());
                let now =
                    Duration::from_secs(10) + Duration::from_millis(snapshots.len() as u64 * 100);
                let snapshot = observe_live_attention(
                    &mut attention,
                    &mut inside,
                    now,
                    &owner,
                    attention_input(true, true),
                    rect,
                    &events,
                )
                .unwrap();
                if snapshots.is_empty() {
                    assert_eq!(events.len(), 3);
                    ui.ctx().request_discard("verify native event retirement");
                } else {
                    assert!(events.is_empty());
                }
                snapshots.push(snapshot);
            },
        );
        // CPU-only input fixture deliberately does not render generated output.
        output.textures_delta.clear();
        assert_eq!(snapshots.len(), 2);
        assert!(snapshots[0].candidate_visible);
        assert_eq!(snapshots[0].entry, snapshots[1].entry);
        assert_eq!(snapshots[0].next_deadline(), snapshots[1].next_deadline());
    }

    #[test]
    fn native_attention_retreat_releases_real_leave_without_rearming() {
        let owner = String::from("local");
        let mut attention = LiveAttention::default();
        attention.set_owner(Some(owner.clone()));
        let mut inside = false;
        let rect = host_rect(96.0, 24.0);
        let now = Duration::from_secs(10);
        observe_live_attention(
            &mut attention,
            &mut inside,
            now,
            &owner,
            attention_input(true, true),
            rect,
            &[egui::Event::PointerMoved(egui::pos2(12.0, 12.0))],
        )
        .unwrap();
        let left = observe_live_attention(
            &mut attention,
            &mut inside,
            now,
            &owner,
            attention_input(false, false),
            rect,
            &[egui::Event::PointerGone],
        )
        .unwrap();
        assert!(left.entry.is_none());
        let suppressed = observe_live_attention(
            &mut attention,
            &mut inside,
            now,
            &owner,
            attention_input(true, false),
            rect,
            &[egui::Event::PointerMoved(egui::pos2(12.0, 12.0))],
        )
        .unwrap();
        assert!(suppressed.entry.is_some());
        assert!(suppressed.candidate.is_none());
        let restored = observe_live_attention(
            &mut attention,
            &mut inside,
            now + Duration::from_secs(1),
            &owner,
            attention_input(true, true),
            rect,
            &[],
        )
        .unwrap();
        assert!(restored.candidate.is_none());
        assert!(restored.active.is_none());
    }

    #[test]
    fn native_attention_stale_owner_cannot_replace_pointer_facts() {
        let owner = String::from("local");
        let mut attention = LiveAttention::default();
        attention.set_owner(Some(owner.clone()));
        let mut inside = true;
        let offer = attention.entry_offer();
        assert!(observe_live_attention(
            &mut attention,
            &mut inside,
            Duration::from_secs(10),
            &String::from("old"),
            attention_input(false, false),
            host_rect(96.0, 24.0),
            &[egui::Event::PointerGone]
        )
        .is_none());
        assert!(inside);
        assert_eq!(attention.owner(), Some(&owner));
        assert_eq!(attention.entry_offer(), offer);
    }

    #[test]
    fn shared_attention_snapshot_drives_final_host_deadline() {
        let owner = String::from("local");
        let mut attention = LiveAttention::default();
        attention.set_owner(Some(owner.clone()));
        let now = Duration::from_secs(90);
        let input = attention_input(false, true);
        let accepted = attention
            .advance(now, &owner, input, LiveAttentionEvent::Explicit)
            .unwrap();
        assert!(accepted.active.is_some());
        let full = PresentationPolicy {
            motion: Some(OrganismMotion::Full),
            ..static_policy()
        };
        assert_eq!(
            next_live_wake(full, now, now, true, Some(accepted)),
            Some(Duration::from_millis(100))
        );
        let calm = PresentationPolicy {
            motion: Some(OrganismMotion::Calm),
            ..full
        };
        assert_eq!(
            next_live_wake(calm, now, now, true, Some(accepted)),
            Some(Duration::from_millis(900))
        );
        let expiry = accepted.active.unwrap().deadline;
        assert_eq!(
            next_live_wake(
                full,
                expiry - Duration::from_millis(1),
                now,
                true,
                Some(accepted)
            ),
            Some(Duration::from_millis(1))
        );
        assert_eq!(
            next_live_wake(full, expiry, now, true, Some(accepted)),
            Some(Duration::from_millis(1))
        );
        let expired = attention
            .advance(expiry, &owner, input, LiveAttentionEvent::Reconcile)
            .unwrap();
        assert!(expired.active.is_none());
        assert_eq!(
            next_live_wake(full, expiry, now, true, Some(expired)),
            Some(Duration::from_millis(900))
        );
    }

    #[test]
    fn native_body_greeting_restores_full_frames_then_returns_to_dormant_cadence() {
        let host = OrganismHost::default();
        let owner = String::from("local");
        let mut attention = LiveAttention::default();
        attention.set_owner(Some(owner.clone()));
        let now = Duration::from_secs(90);
        assert!(host.owner_dormant(now));
        let input = attention_input(false, true);
        let accepted = attention
            .advance(now, &owner, input, LiveAttentionEvent::Explicit)
            .unwrap();
        let full = PresentationPolicy {
            motion: Some(OrganismMotion::Full),
            ..static_policy()
        };
        let calm = PresentationPolicy {
            motion: Some(OrganismMotion::Calm),
            ..full
        };
        assert_eq!(
            host.native_body_wake(full, now, Some(accepted)),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            host.native_body_wake(calm, now, Some(accepted)),
            Some(Duration::from_millis(900))
        );
        // Static and withdrawn bodies stay asleep even with a copied old
        // snapshot. Their ordinary host owns any remaining status deadline.
        assert_eq!(
            host.native_body_wake(static_policy(), now, Some(accepted)),
            None
        );
        assert_eq!(
            host.native_body_wake(
                PresentationPolicy {
                    enabled: false,
                    ..full
                },
                now,
                Some(accepted)
            ),
            None
        );
        let expiry = accepted.active.unwrap().deadline;
        let expired = attention
            .advance(expiry, &owner, input, LiveAttentionEvent::Reconcile)
            .unwrap();
        assert!(expired.active.is_none());
        for policy in [full, calm] {
            assert_eq!(
                host.native_body_wake(policy, expiry, Some(expired)),
                Some(Duration::from_millis(900))
            );
        }
    }

    #[test]
    fn shared_attention_motion_changes_keep_the_original_deadline() {
        let owner = String::from("local");
        let mut attention = LiveAttention::default();
        attention.set_owner(Some(owner.clone()));
        let now = Duration::from_secs(10);
        let full = PresentationPolicy {
            motion: Some(OrganismMotion::Full),
            ..static_policy()
        };
        let calm = PresentationPolicy {
            motion: Some(OrganismMotion::Calm),
            ..full
        };
        let mut input = attention_input(false, live_greeting_allowed(full, now, false, false));
        let accepted = attention
            .advance(now, &owner, input, LiveAttentionEvent::Explicit)
            .unwrap();
        input.hard_eligible = live_greeting_allowed(calm, now, false, false);
        let continued = attention
            .advance(
                now + Duration::from_millis(500),
                &owner,
                input,
                LiveAttentionEvent::Reconcile,
            )
            .unwrap();
        assert_eq!(accepted.active, continued.active);
        input.hard_eligible = live_greeting_allowed(static_policy(), now, false, false);
        let cancelled = attention
            .advance(
                now + Duration::from_millis(600),
                &owner,
                input,
                LiveAttentionEvent::Reconcile,
            )
            .unwrap();
        assert!(cancelled.active.is_none());
        input.hard_eligible = live_greeting_allowed(full, now, false, false);
        let rejected = attention
            .advance(
                now + Duration::from_secs(1),
                &owner,
                input,
                LiveAttentionEvent::Explicit,
            )
            .unwrap();
        assert_eq!(
            rejected.outcome,
            LiveAttentionOutcome::Rejected(LiveAttentionRejection::Cooldown)
        );
    }

    #[test]
    fn shared_attention_adapter_has_one_policy_owner_and_snapshot() {
        let source = include_str!("organism_ui.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(source.contains("attention: LiveAttention<String>"));
        assert!(!source.contains("struct LiveGreeting"));
        let draw = source
            .split("pub fn draw(")
            .nth(1)
            .unwrap()
            .split("impl crate::app::TermApp")
            .next()
            .unwrap();
        assert!(draw.contains("observe_live_attention("));
        assert!(draw.contains("snapshot.candidate_visible"));
        assert!(draw.contains("snapshot.presentation"));
        assert!(draw.contains("self.born.elapsed()"));
    }

    fn host_rect(width: f32, height: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height))
    }

    #[test]
    fn observed_geometry_requires_positive_remaining_clipped_height() {
        let available = host_rect(640.0, 96.0);
        assert!(host_geometry_presentable(available, available));
        for height in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(!host_geometry_presentable(
                host_rect(640.0, height),
                available
            ));
            assert!(!host_geometry_presentable(
                available,
                host_rect(640.0, height)
            ));
        }
        assert!(!host_geometry_presentable(
            available,
            available.translate(egui::vec2(0.0, 96.0))
        ));
        assert!(!host_geometry_presentable(
            available,
            host_rect(119.9, 96.0)
        ));
        assert!(host_geometry_presentable(available, host_rect(120.0, 1.0)));
    }

    #[test]
    fn zero_height_retires_live_attention_and_restores_without_replay() {
        let mut host = OrganismHost::default();
        let rect = host_rect(640.0, 96.0);
        host.set_available_geometry(rect, rect);
        host.acquire(Some("local"), Some((7, true)), false);
        host.pending = Some(pending());
        let config = Config {
            ascii_organism_enabled: true,
            ascii_organism_motion: Some(OrganismMotion::Full),
            ..Default::default()
        };
        let now = Duration::from_secs(10);
        assert!(host.request_hello_at(now, Some("local"), &config).is_ok());
        let life = format!("{:?}", host.life.state());
        host.set_available_geometry(host_rect(640.0, 0.0), rect);
        assert!(!host.presentable);
        assert!(host.owner.is_none());
        assert!(host.pending.is_none());
        assert!(
            host.attention.owner().is_none()
                || host_attention_snapshot(&host, now).active.is_none()
        );
        assert!(host.attention.owner().is_none());
        let hidden = PresentationPolicy {
            focused_owner: host.presentable && host.owner.is_some(),
            motion: config.ascii_organism_motion,
            ..static_policy()
        };
        assert_eq!(next_live_wake(hidden, now, now, false, None), None);
        assert_eq!(format!("{:?}", host.life.state()), life);
        host.set_available_geometry(rect, rect);
        host.acquire(Some("local"), Some((9, true)), false);
        assert!(host.quarantine_batch);
        assert!(host.pending.is_none());
        assert!(
            host.attention.owner().is_none()
                || host_attention_snapshot(&host, now).active.is_none()
        );
        assert!(host.request_hello_at(now, Some("local"), &config).is_err());
        assert_eq!(format!("{:?}", host.life.state()), life);
    }

    #[test]
    fn observed_empty_geometry_refuses_hello_without_spending_cooldown() {
        let mut host = OrganismHost::default();
        let rect = host_rect(640.0, 96.0);
        host.set_available_geometry(rect, rect);
        host.acquire(Some("local"), None, false);
        host.set_available_geometry(rect, host_rect(640.0, 0.0));
        let now = Duration::from_secs(10);
        let config = Config {
            ascii_organism_enabled: true,
            ascii_organism_motion: Some(OrganismMotion::Full),
            ..Default::default()
        };
        assert!(host.request_hello_at(now, Some("local"), &config).is_err());
        host.set_available_geometry(rect, rect);
        host.acquire(Some("local"), None, false);
        assert!(host.request_hello_at(now, Some("local"), &config).is_ok());
        let source = include_str!("organism_ui.rs");
        let render = source
            .split("#[cfg(test)]")
            .next()
            .unwrap()
            .split("pub(crate) fn render_organism")
            .nth(1)
            .unwrap()
            .split("/// Accepted Full greetings")
            .next()
            .unwrap();
        assert!(render
            .contains("set_available_geometry(ui.available_rect_before_wrap(), ui.clip_rect())"));
        assert!(
            render.find("set_available_geometry").unwrap()
                < render.find("self.prepare_organism").unwrap()
        );
    }

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
        host.set_available_geometry(host_rect(120.0, 96.0), host_rect(1000.0, 1000.0));
        host.acquire(Some("local"), Some((7, true)), true);
        host.pending = Some(pending());
        host.set_available_geometry(host_rect(119.9, 96.0), host_rect(1000.0, 1000.0));
        assert!(host.owner.is_none());
        assert!(host.pending.is_none());
        assert!(!host.running);
        let mut policy = static_policy();
        policy.focused_owner = false;
        policy.motion = Some(OrganismMotion::Full);
        assert_eq!(
            next_host_wake(policy, Duration::ZERO, Duration::from_secs(5), false),
            None
        );
        host.set_available_geometry(host_rect(120.0, 96.0), host_rect(1000.0, 1000.0));
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

    #[test]
    fn local_agent_starts_and_completions_leave_later_human_work_unclaimed() {
        let mut terminal = crate::terminal::TerminalState::new(80, 24);
        let mut host = OrganismHost::default();
        host.acquire(Some("local"), None, false);
        host.quarantine_batch = false;
        for (step, (generation, exit)) in [
            (Some(1), 101),
            (Some(2), 101),
            (Some(3), 101),
            (Some(4), 0),
            (None, 101),
            (None, 101),
            (None, 101),
            (None, 0),
        ]
        .into_iter()
        .enumerate()
        {
            terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
            if let Some(generation) = generation {
                terminal
                    .arm_agent_execution(generation, "cargo test")
                    .unwrap();
            }
            terminal.process_input(
                b"cargo test\r\n\x1b]133;C;cmdline_url=cargo%20test;cwd=/project\x07",
            );
            host.batch("local", terminal.command_records(), &[], false, false);
            assert_eq!(
                host.context.behavior,
                if generation.is_some() {
                    Behavior::WatchAgent
                } else {
                    Behavior::WatchCommand
                }
            );
            let confidence = host.life.state().confidence;
            terminal.process_input(format!("\x1b]133;D;{exit};duration_ms=100\x07").as_bytes());
            let completed = terminal.take_completed_command_events();
            assert_eq!(completed.len(), 1);
            assert_eq!(completed[0].agent_generation, generation);
            host.batch(
                "local",
                terminal.command_records(),
                &completed,
                false,
                false,
            );
            if generation.is_some() {
                assert!(host.reaction_status.contains("agent-driven"));
                if exit != 0 {
                    assert_eq!(host.life.state().confidence, confidence);
                }
            } else if exit != 0 {
                assert!(host.life.state().confidence < confidence);
            }
            if step == 3 {
                assert_eq!(host.context.behavior, Behavior::Celebrate);
            } else if step == 7 {
                assert_eq!(host.context.behavior, Behavior::CelebrateBig);
            }
        }
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
    fn agent_completion_attribution_uses_accepted_completion_not_previous_flag() {
        let mut host = OrganismHost::default();
        let mut start = pending();
        start.kind = CommandKind::BuildOrTest;
        let mut done = completion(Some(101));
        done.completed.agent_generation = Some(7);
        let confidence = host.life.state().confidence;
        // A completion-only observation still carries its locally validated
        // attribution; it does not need a busy Agent panel or a model request.
        host.finish(&start, &done, Duration::ZERO);
        assert_eq!(host.life.state().confidence, confidence);
        assert!(host.reaction_status.contains("agent-driven"));

        host.native.set_agent_command(true);
        done.completed.agent_generation = None;
        host.finish(&start, &done, Duration::ZERO);
        assert!(host.life.state().confidence < confidence);
        assert!(!host.reaction_status.contains("agent-driven"));

        host.acquire(Some("local"), None, false);
        host.quarantine_batch = false;
        host.pending = Some(pending());
        let before = format!("{:?}", host.life.state());
        let mut stale = completion(Some(0));
        stale.completed.agent_generation = Some(99);
        stale.completed.id = "previous-command".to_owned();
        host.batch("local", &VecDeque::new(), &[stale], false, false);
        assert_eq!(format!("{:?}", host.life.state()), before);
        assert!(host.pending.is_some());

        let mut boundary = completion(Some(0));
        boundary.completed.agent_generation = Some(100);
        boundary.completed.duration_ms = Some(60_000);
        boundary.completion_provenance = CompletionProvenance::BoundaryInferred;
        host.finish(&start, &boundary, Duration::ZERO);
        assert_eq!(host.context.behavior, Behavior::UnknownOutcome);
    }

    #[test]
    fn sustained_completion_batch_is_quiet_consumed_once_and_owner_scoped() {
        let mut host = OrganismHost::default();
        host.acquire(Some("local-a"), Some((7, true)), false);
        host.quarantine_batch = false;
        host.pending = Some(pending());
        let mut done = completion(Some(0));
        done.duration_ms = Some(60_000);
        host.batch("local-b", &VecDeque::new(), &[done.clone()], false, false);
        assert!(host.pending.is_some());
        assert_eq!(host.context.behavior, Behavior::Idle);
        host.batch("local-a", &VecDeque::new(), &[done.clone()], false, false);
        assert!(host.pending.is_none());
        assert_eq!(host.context.behavior, Behavior::Celebrate);
        let deadline = host.reaction_until;
        let state = format!("{:?}", host.life.state());
        host.batch("local-a", &VecDeque::new(), &[done], false, false);
        assert_eq!(host.reaction_until, deadline);
        assert_eq!(format!("{:?}", host.life.state()), state);
        // An owner handoff discards the old reaction, even if the old drain
        // is encountered again. Returning starts a fresh quarantine.
        host.acquire(Some("local-b"), None, false);
        host.acquire(Some("local-a"), Some((7, true)), false);
        assert_eq!(host.context.behavior, Behavior::Idle);
        assert!(host.quarantine_batch);
        assert!(host.pending.is_none());
    }

    #[test]
    fn sustained_completion_uses_finite_static_expiry_and_new_work_wins() {
        let mut host = OrganismHost::default();
        host.acquire(Some("local-a"), None, false);
        let now = Duration::from_secs(60);
        let mut done = completion(Some(0));
        done.duration_ms = Some(60_000);
        host.finish(&pending(), &done, now);
        assert_eq!(host.context.behavior, Behavior::Celebrate);
        assert_eq!(host.reaction_until, now + Duration::from_millis(1800));
        assert_eq!(
            next_host_wake(static_policy(), now, host.reaction_until, false),
            Some(Duration::from_millis(1800))
        );
        assert_eq!(
            next_host_wake(
                static_policy(),
                host.reaction_until,
                host.reaction_until,
                false
            ),
            None
        );
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
                alternate_screen: true,
                ..static_policy()
            },
        ] {
            assert_eq!(
                next_host_wake(policy, now, host.reaction_until, false),
                None
            );
        }
        let started = host.native.command_started(CommandKind::Other);
        host.react(started, now + Duration::from_millis(1));
        assert_eq!(host.context.behavior, Behavior::WatchCommand);
        host.acquire(None, None, false);
        assert_eq!(host.context.behavior, Behavior::Idle);
        assert!(host.pending.is_none());
    }

    #[test]
    fn missing_and_inferred_exit_are_unknown_never_success() {
        for provenance in [
            CompletionProvenance::ShellReported,
            CompletionProvenance::BoundaryInferred,
        ] {
            let mut host = OrganismHost::default();
            let mut done = completion(None);
            done.duration_ms = Some(60_000);
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

#[cfg(test)]
mod chat_offscreen_tests {
    use super::*;
    use crate::terminal::{
        HistoryProjection, ProjectionPolicy, ProjectionViewState, TerminalState,
    };

    #[test]
    fn chat_native_composer_exposes_safe_ascii_surface_and_offscreen_fixture() {
        let ctx = egui::Context::default();
        let config = Config {
            terminal_mode: Some(crate::config::TerminalMode::Chat),
            block_mode: false,
            ascii_organism_enabled: true,
            ascii_organism_motion: Some(OrganismMotion::Calm),
            font_size: 18.0,
            ..Config::default()
        };
        let mut renderer = crate::ui::TerminalRenderer::new(
            config.font_size,
            16.0,
            1.4,
            crate::config::ScrollbarVisibility::Always,
            crate::theme::Theme::default(),
        );
        renderer.block_mode = true;
        renderer.chat_mode = true;
        renderer.chat_native_owner = Some((7, 7)); // Synthetic fixture authority.
        renderer.gpu_rendering = false;
        renderer.companion_enabled = true;
        let mut host = OrganismHost::default();
        host.acquire(Some("chat-fixture"), None, false);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 768.0));
        let mut terminal = None;
        let mut policy = ProjectionPolicy::new();
        policy.set_chat_mode(true);
        policy.set_block_spacing(true);
        policy.set_block_chrome(true, 0);
        let mut state = ProjectionViewState::new();
        let mut accumulated = egui::FullOutput::default();
        for _ in 0..3 {
            let output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| {
                renderer.sync_font_metrics(&ctx);
                let (cols, rows) = renderer.grid_dimensions(screen.size());
                let terminal = terminal.get_or_insert_with(|| {
                    let mut terminal = TerminalState::new(cols, rows);
                    terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07printf 'hello, terminal'\r\n\x1b]133;C;cwd=/workspace/ember\x07hello, terminal\r\n\x1b]133;D;0\x07");
                    terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07cargo test --lib\r\n\x1b]133;C;cwd=/workspace/ember\x07   Compiling ember v0.4.0\r\nrunning fixture tests\r\n\x1b[32mtest result: ok. All synthetic fixture tests passed.\x1b[0m\r\n\x1b]133;D;0\x07");
                    terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07false\r\n\x1b]133;C;cwd=/workspace/ember\x07Deliberate fixture failure for status styling.\r\n\x1b]133;D;7\x07");
                    terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07echo next");
                    terminal
                });
                let viewport = terminal.projected_viewport_with_state(HistoryProjection::identity(), true, &policy, &mut state);
                assert!(viewport.chat_composer_rows().is_some());
                assert_eq!(viewport.chat_command_spans().len(), 3);
                renderer.set_projection_frame(terminal, viewport, &policy);
                renderer.render_in_rect(ui, terminal, true, true, true, &crate::search::SearchState::default(), &[], &None, screen);
                let surface = renderer.companion_surface.clone().map(|(block, key, region)| ("chat-fixture".to_owned(), block, key, region));
                assert!(surface.is_some(), "Chat offers the existing organism a real, proven blank composer region");
                host.draw_native_body(ui, &config, surface, true);
                assert!(host.native_surface_rect.is_some(), "the real native ASCII body fits inside the composer");
            });
            accumulated.append(output);
        }
        if let Ok(path) = std::env::var("EMBER_CHAT_RENDER_DUMP") {
            crate::ui::chat::export_native_frame(
                std::path::Path::new(&path),
                &ctx,
                accumulated,
                [1024, 768],
            );
        } else {
            accumulated.textures_delta.clear();
        }
    }
}
