//! Renderer-independent groundwork for the shared ASCII organism contract.
//!
//! This library module is not wired into Ember's live application. It owns no
//! timers, input handlers, PTY writers, renderer, wall-clock source, or persistence. Hosts
//! must supply authoritative paired command events and implement the pending
//! focused-session lifecycle adapter before this is a visible feature.

use std::time::Duration;

use crate::config::OrganismMotion;
pub use jterm_core::organism::{
    classify_command, sprite_frame_with_context, sticky_glyph_with_context, CircadianPhase, CommandKind,
    LifeState, NativeOrganism, Reaction, RenderContext,
};
pub use jterm_core::organism_daily::PreviewPose;
use jterm_core::organism_daily::GentleInteraction;

pub const INPUT_RETREAT: Duration = Duration::from_millis(900);
pub const FULL_FRAME_INTERVAL: Duration = Duration::from_millis(100);
pub const CALM_HEARTBEAT_INTERVAL: Duration = Duration::from_millis(900);

/// Automatic is an explicit Calm fallback until the platform adapter exists.
pub fn resolved_motion(value: Option<OrganismMotion>) -> OrganismMotion {
    value.unwrap_or(OrganismMotion::Calm)
}

/// Immutable input from the future owner adapter. Defaults fail closed.
/// `focused_owner` must identify the single chosen local pane, not merely a
/// visible background pane. Input observation never consumes the input event.
#[derive(Debug, Clone, Copy, Default)]
pub struct PresentationPolicy {
    pub enabled: bool,
    pub focused_owner: bool,
    pub local: bool,
    pub alternate_screen: bool,
    pub motion: Option<OrganismMotion>,
    pub last_input: Option<Duration>,
}

impl PresentationPolicy {
    fn eligible(self) -> bool {
        self.enabled
            && self.focused_owner
            && self.local
            && !self.alternate_screen
    }

    /// Static still permits a non-interactive inline/status representation.
    /// This is a host capability hint, not an implemented Ember status widget.
    pub fn inline_visible(self, now: Duration) -> bool {
        self.eligible()
            && self
                .last_input
                .is_none_or(|last| now.saturating_sub(last) >= INPUT_RETREAT)
    }

    pub fn body_visible(self, now: Duration) -> bool {
        self.inline_visible(now) && resolved_motion(self.motion) != OrganismMotion::Static
    }

    /// Complete scheduling hint, including one bounded retreat-expiry wake.
    /// A host using only `body_interval` would never wake a hidden organism
    /// after typing stops. Re-evaluate this hint after each accepted input and
    /// ownership change, cancelling previous wakes; disabled owners return None.
    /// No timer is allocated here, and Static has no repeating animation wake.
    pub fn next_wake_after(self, now: Duration) -> Option<Duration> {
        if !self.eligible() {
            return None;
        }
        if let Some(last) = self.last_input {
            let elapsed = now.saturating_sub(last);
            if elapsed < INPUT_RETREAT {
                return Some(INPUT_RETREAT - elapsed);
            }
        }
        self.body_interval(now)
    }

    /// Scheduling hint only. The host must cancel existing sources whenever
    /// ownership, visibility or enablement changes; this module creates none.
    pub fn body_interval(self, now: Duration) -> Option<Duration> {
        if !self.body_visible(now) {
            return None;
        }
        Some(match resolved_motion(self.motion) {
            OrganismMotion::Full => FULL_FRAME_INTERVAL,
            OrganismMotion::Calm => CALM_HEARTBEAT_INTERVAL,
            OrganismMotion::Static => return None,
        })
    }
}

/// Isolated settings preview. Uses the core's eight poses and greeting rules;
/// never fabricates command events, changes life state, or saves memory.
pub struct OrganismPreview {
    open: bool,
    pose: PreviewPose,
    greeting: GentleInteraction,
}

impl Default for OrganismPreview {
    fn default() -> Self {
        Self {
            open: false,
            pose: PreviewPose::Calm,
            greeting: GentleInteraction::default(),
        }
    }
}

impl OrganismPreview {
    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.greeting.cancel();
    }

    pub fn select_pose(&mut self, pose: PreviewPose) {
        if self.pose != pose {
            self.pose = pose;
            self.greeting.cancel();
        }
    }

    pub fn say_hello(&mut self, now: Duration) -> bool {
        self.open && self.greeting.request(now, self.pose.context())
    }

    pub fn context(&mut self, now: Duration) -> Option<RenderContext> {
        self.open
            .then(|| self.greeting.apply(now, self.pose.context()))
    }
}

/// One volatile physiology clock per window, never one per pane or frame.
/// Timestamps are injected, monotonic durations; no wall-clock or I/O policy.
pub struct WindowLife {
    state: LifeState,
    anchor: Duration,
    last_input: Option<Duration>,
    last_activity: Duration,
    eligible: bool,
}

impl WindowLife {
    pub fn new_at(now: Duration) -> Self {
        Self {
            state: LifeState::default(),
            anchor: now,
            last_input: None,
            last_activity: now,
            eligible: false,
        }
    }

    pub fn state(&self) -> LifeState {
        self.state
    }

    pub fn replace_state(&mut self, state: LifeState) {
        self.state = NativeOrganism::from_persisted_state(state).state();
    }

    pub fn note_input(&mut self, now: Duration) {
        if now >= self.anchor && now >= self.last_activity {
            self.last_input = Some(now);
            self.last_activity = now;
        }
    }

    pub fn note_output(&mut self, now: Duration) {
        if now >= self.anchor {
            self.last_activity = self.last_activity.max(now);
        }
    }

    pub fn idle_for(&self, now: Duration) -> Duration {
        now.saturating_sub(self.last_activity)
    }

    pub fn advance(
        &mut self,
        now: Duration,
        eligible_live: bool,
        any_running: bool,
        phase: CircadianPhase,
    ) -> f32 {
        if !eligible_live {
            self.anchor = self.anchor.max(now);
            self.eligible = false;
            return 0.0;
        }
        if now < self.anchor {
            return 0.0;
        }
        if any_running {
            self.last_activity = self.last_activity.max(now);
        }
        if !self.eligible {
            self.anchor = now;
            self.eligible = eligible_live;
            return 0.0;
        }
        let elapsed = now.saturating_sub(self.anchor).min(Duration::from_secs(1));
        self.anchor = now;
        if elapsed.is_zero() {
            return 0.0;
        }
        let resting = !any_running && self.idle_for(now) >= Duration::from_secs(60);
        // Account for input over the consumed interval, not only its endpoint.
        // A 900ms dormant tick therefore cannot miss the 900ms input window.
        let start = now.saturating_sub(elapsed);
        let active = self.last_input.map_or(Duration::ZERO, |input| {
            let end = input.saturating_add(Duration::from_millis(900)).min(now);
            end.saturating_sub(input.max(start)).min(elapsed)
        });
        let inactive = elapsed.saturating_sub(active);
        if !active.is_zero() {
            self.state.tick(active.as_secs_f32(), true, resting, phase);
        }
        if !inactive.is_zero() {
            self.state.tick(inactive.as_secs_f32(), false, resting, phase);
        }
        elapsed.as_secs_f32()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn life_values(state: LifeState) -> [f32; 8] {
        [
            state.energy,
            state.mood,
            state.curiosity,
            state.boredom,
            state.stress,
            state.social_need,
            state.attachment,
            state.confidence,
        ]
    }

    #[test]
    fn window_life_consumes_time_once_and_never_catches_up_after_suspend() {
        let mut life = WindowLife::new_at(Duration::ZERO);
        let phase = CircadianPhase::Unlearned;
        assert_eq!(life.advance(Duration::ZERO, true, false, phase), 0.0);
        assert_eq!(life.advance(Duration::from_millis(100), true, false, phase), 0.1);
        let once = life_values(life.state());
        assert_eq!(life.advance(Duration::from_millis(100), true, false, phase), 0.0);
        assert_eq!(life.advance(Duration::ZERO, true, false, phase), 0.0);
        assert_eq!(life_values(life.state()), once);
        assert_eq!(life.advance(Duration::from_secs(3600), true, false, phase), 1.0);
    }

    #[test]
    fn backward_disable_still_pauses_and_resume_starts_without_catchup() {
        let mut life = WindowLife::new_at(Duration::ZERO);
        let phase = CircadianPhase::Unlearned;
        life.advance(Duration::ZERO, true, false, phase);
        life.advance(Duration::from_secs(1), true, false, phase);
        let before = life_values(life.state());
        assert_eq!(life.advance(Duration::ZERO, false, true, phase), 0.0);
        assert_eq!(life_values(life.state()), before);
        assert_eq!(life.advance(Duration::from_secs(2), true, false, phase), 0.0);
        assert_eq!(life_values(life.state()), before);
        assert_eq!(life.advance(Duration::from_secs(3), true, false, phase), 1.0);
    }

    #[test]
    fn input_overlap_output_and_quiet_rest_share_the_same_window_clock() {
        let phase = CircadianPhase::Unlearned;
        let mut life = WindowLife::new_at(Duration::ZERO);
        life.advance(Duration::ZERO, true, false, phase);
        life.note_input(Duration::ZERO);
        let before = life.state();
        life.advance(Duration::from_millis(900), true, false, phase);
        assert!(life.state().boredom < before.boredom);
        assert!(life.state().social_need < before.social_need);
        assert!(life.state().curiosity > before.curiosity);
        life.note_output(Duration::from_secs(1));
        assert_eq!(life.idle_for(Duration::from_secs(1)), Duration::ZERO);

        let mut resting = WindowLife::new_at(Duration::ZERO);
        let mut running = WindowLife::new_at(Duration::ZERO);
        resting.advance(Duration::ZERO, true, false, phase);
        running.advance(Duration::ZERO, true, false, phase);
        resting.advance(Duration::from_secs(60), true, false, phase);
        running.advance(Duration::from_secs(60), true, true, phase);
        assert!(resting.state().energy > LifeState::default().energy);
        assert!(running.state().energy < LifeState::default().energy);
    }

    #[test]
    fn life_replacement_uses_core_normalization_and_authoritative_completion_state() {
        let mut life = WindowLife::new_at(Duration::ZERO);
        life.replace_state(LifeState {
            energy: f32::NAN,
            mood: f32::INFINITY,
            curiosity: -2.0,
            boredom: 2.0,
            stress: f32::NEG_INFINITY,
            social_need: 2.0,
            attachment: -1.0,
            confidence: f32::NAN,
        });
        assert!(life_values(life.state())
            .iter()
            .all(|value| value.is_finite() && (0.0..=1.0).contains(value)));
        let mut native = NativeOrganism::from_persisted_state(life.state());
        native.command_finished(classify_command("cargo test"), Some(1), Some(1_000));
        life.replace_state(native.state());
        assert_eq!(life_values(life.state()), life_values(native.state()));
        let before = life_values(life.state());
        let mut preview = OrganismPreview::default();
        preview.open();
        assert!(preview.say_hello(Duration::ZERO));
        let _ = preview.context(Duration::from_secs(1));
        assert_eq!(life_values(life.state()), before);
    }

    fn visible_policy() -> PresentationPolicy {
        PresentationPolicy {
            enabled: true,
            focused_owner: true,
            local: true,
            motion: Some(OrganismMotion::Full),
            ..Default::default()
        }
    }

    #[test]
    fn ownership_remote_alt_screen_and_disable_suppress_body_and_timers() {
        let now = Duration::from_secs(10);
        assert_eq!(
            visible_policy().body_interval(now),
            Some(FULL_FRAME_INTERVAL)
        );
        for policy in [
            PresentationPolicy::default(),
            PresentationPolicy {
                enabled: false,
                ..visible_policy()
            },
            PresentationPolicy {
                focused_owner: false,
                ..visible_policy()
            },
            PresentationPolicy {
                local: false,
                ..visible_policy()
            },
            PresentationPolicy {
                alternate_screen: true,
                ..visible_policy()
            },
            PresentationPolicy {
                motion: Some(OrganismMotion::Static),
                ..visible_policy()
            },
        ] {
            assert!(!policy.body_visible(now));
            assert_eq!(policy.body_interval(now), None);
        }
    }

    #[test]
    fn input_retreat_is_bounded_and_automatic_explicitly_falls_back_to_calm() {
        let policy = PresentationPolicy {
            motion: None,
            last_input: Some(Duration::from_secs(1)),
            ..visible_policy()
        };
        assert!(!policy.body_visible(Duration::ZERO));
        assert!(!policy.body_visible(Duration::from_millis(1899)));
        assert_eq!(
            policy.next_wake_after(Duration::from_millis(1899)),
            Some(Duration::from_millis(1))
        );
        assert_eq!(
            policy.body_interval(Duration::from_millis(1900)),
            Some(CALM_HEARTBEAT_INTERVAL)
        );
    }

    #[test]
    fn static_keeps_inline_status_without_repeating_body_wakes() {
        let policy = PresentationPolicy {
            motion: Some(OrganismMotion::Static),
            ..visible_policy()
        };
        assert!(policy.inline_visible(Duration::ZERO));
        assert!(!policy.body_visible(Duration::ZERO));
        assert_eq!(policy.next_wake_after(Duration::ZERO), None);
        let disabled = PresentationPolicy {
            enabled: false,
            last_input: Some(Duration::ZERO),
            ..policy
        };
        assert!(!disabled.inline_visible(Duration::ZERO));
        assert_eq!(disabled.next_wake_after(Duration::ZERO), None);
    }

    #[test]
    fn preview_is_isolated_and_closing_preserves_the_core_cooldown() {
        let mut preview = OrganismPreview::default();
        assert!(preview.context(Duration::ZERO).is_none());
        assert!(!preview.say_hello(Duration::ZERO));
        preview.open();
        assert!(preview.say_hello(Duration::ZERO));
        preview.close();
        preview.open();
        assert!(!preview.say_hello(Duration::from_secs(1)));
        assert!(preview.say_hello(Duration::from_secs(8)));
        preview.select_pose(PreviewPose::Working);
        assert!(!preview.say_hello(Duration::from_secs(16)));
        assert_eq!(PreviewPose::ALL.len(), 8);
    }
}
