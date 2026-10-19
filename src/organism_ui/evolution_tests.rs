//! Parser and CPU-paint fixtures for the native companion's work continuity.
//! These do not claim that a GPU frame or a desktop input path was exercised.

use super::*;

fn observed_command(
    host: &mut OrganismHost,
    terminal: &mut crate::terminal::TerminalState,
    command: &str,
    cwd: &str,
    exit: Option<i32>,
) {
    terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    terminal.process_input(
        format!(
            "{command}\r\n\x1b]133;C;cmdline_url={};cwd={cwd}\x07",
            command.replace(' ', "%20")
        )
        .as_bytes(),
    );
    host.batch("local", terminal.command_records(), &[], false, false);
    let end = exit.map_or_else(
        || "\x1b]133;D\x07".to_owned(),
        |exit| format!("\x1b]133;D;{exit};duration_ms=100\x07"),
    );
    terminal.process_input(end.as_bytes());
    let completed = terminal.take_completed_command_events();
    assert_eq!(completed.len(), 1);
    host.batch(
        "local",
        terminal.command_records(),
        &completed,
        false,
        false,
    );
}

fn observed_host() -> OrganismHost {
    let mut host = OrganismHost::default();
    host.acquire(Some("local"), None, false);
    host.quarantine_batch = false;
    host.presentable = true;
    host
}

fn full_config() -> Config {
    Config {
        block_mode: true,
        ascii_organism_enabled: true,
        ascii_organism_motion: Some(OrganismMotion::Full),
        ..Config::default()
    }
}

fn safe_region() -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(100.0, 100.0), egui::pos2(600.0, 240.0))
}

fn body_frame(
    host: &mut OrganismHost,
    ctx: &egui::Context,
    config: &Config,
    block: &str,
    region: egui::Rect,
) -> Vec<String> {
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
                config,
                Some((
                    "local".into(),
                    block.into(),
                    crate::terminal::TerminalState::new(80, 12)
                        .projected_viewport(crate::terminal::HistoryProjection::identity(), true)
                        .key(),
                    region,
                )),
                true,
            );
        },
    );
    let text = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => {
                assert!(region.contains_rect(shape.clip_rect));
                Some(text.galley.text().to_owned())
            }
            _ => None,
        })
        .collect();
    output.textures_delta.clear();
    text
}

#[test]
fn real_build_events_keep_the_work_loop_after_the_reaction_expires() {
    let mut host = observed_host();
    let mut terminal = crate::terminal::TerminalState::new(80, 24);
    for (step, (exit, settled)) in [
        (1, Behavior::GuardFailure),
        (1, Behavior::GuardFailure),
        (1, Behavior::GuardStuck),
        (0, Behavior::GuardRecovery),
    ]
    .into_iter()
    .enumerate()
    {
        host.born = Instant::now() - Duration::from_secs(step as u64 * 10);
        observed_command(
            &mut host,
            &mut terminal,
            "cargo test",
            "/project",
            Some(exit),
        );
        let reaction = host.context.behavior;
        let until = host.reaction_until;
        host.advance_ambient(until - Duration::from_millis(1), true, 1.0);
        assert_eq!(host.context.behavior, reaction);
        host.advance_ambient(until, false, 1.0);
        assert_eq!(
            host.context.behavior, reaction,
            "hidden observers stay paused"
        );
        host.advance_ambient(until, true, 1.0);
        assert_eq!(host.context.behavior, settled);
        assert_ne!(
            host.status_description(until, host.context, true),
            "quiet · between commands"
        );
    }
    host.born = Instant::now() - Duration::from_secs(50);
    observed_command(&mut host, &mut terminal, "git push", "/project", Some(0));
    assert_eq!(host.context.behavior, Behavior::RestAfterPush);
    assert_eq!(host.native.repo_vigil(), RepoVigil::None);
    host.advance_ambient(host.reaction_until, true, 1.0);
    assert!(!matches!(
        host.context.behavior,
        Behavior::GuardFailure
            | Behavior::GuardStuck
            | Behavior::GuardRecovery
            | Behavior::GuardCautious
    ));
}

#[test]
fn unknown_outcomes_do_not_resolve_work_and_another_directory_does_not_inherit_it() {
    let mut host = observed_host();
    let mut terminal = crate::terminal::TerminalState::new(80, 24);
    observed_command(&mut host, &mut terminal, "cargo test", "/one", Some(1));
    observed_command(&mut host, &mut terminal, "cargo test", "/one", None);
    assert_eq!(host.context.behavior, Behavior::UnknownOutcome);
    host.advance_ambient(host.reaction_until, true, 1.0);
    assert_eq!(host.context.behavior, Behavior::GuardFailure);
    observed_command(&mut host, &mut terminal, "echo ready", "/one", Some(0));
    host.advance_ambient(host.reaction_until, true, 1.0);
    assert_eq!(host.context.behavior, Behavior::GuardFailure);
    observed_command(&mut host, &mut terminal, "cargo test", "/two", Some(0));
    host.advance_ambient(host.reaction_until, true, 1.0);
    assert_eq!(host.native.repo_vigil(), RepoVigil::None);
    assert!(!matches!(
        host.context.behavior,
        Behavior::GuardFailure | Behavior::GuardStuck
    ));
    host.acquire(Some("another-pane"), None, false);
    assert_eq!(host.native.repo_vigil(), RepoVigil::None);
}

#[test]
fn a_changed_idle_prompt_directory_retires_the_old_vigil_before_another_start() {
    let mut host = observed_host();
    let mut terminal = crate::terminal::TerminalState::new(80, 24);
    terminal.process_input(b"\x1b]7;file:///one\x07");
    host.observe_directory(terminal.organism_working_dir());
    observed_command(&mut host, &mut terminal, "cargo test", "/one", Some(1));
    host.advance_ambient(host.reaction_until, true, 1.0);
    assert_eq!(host.context.behavior, Behavior::GuardFailure);

    observed_command(&mut host, &mut terminal, "cd /two", "/one", Some(0));
    // Shell prompt metadata can arrive separately from the previous command's
    // end; no new command has run in the new directory.
    terminal.process_input(b"\x1b]7;file:///two\x07\x1b]133;A\x07$ \x1b]133;B\x07");
    host.observe_directory(terminal.organism_working_dir());
    assert_eq!(host.work_context.as_deref(), Some("/two"));
    assert_eq!(host.native.repo_vigil(), RepoVigil::None);
    host.advance_ambient(host.reaction_until, true, 1.0);
    assert!(!matches!(
        host.context.behavior,
        Behavior::GuardFailure | Behavior::GuardStuck
    ));
}

#[test]
fn unchanged_directory_metadata_cannot_override_a_fresher_command_start() {
    let mut host = observed_host();
    let mut terminal = crate::terminal::TerminalState::new(80, 24);
    terminal.process_input(b"\x1b]7;file:///one\x07");
    host.observe_directory(terminal.organism_working_dir());
    observed_command(&mut host, &mut terminal, "cargo test", "/two", Some(1));
    host.observe_directory(terminal.organism_working_dir());
    assert_eq!(host.work_context.as_deref(), Some("/two"));
    assert_eq!(host.native.repo_vigil(), RepoVigil::Failure);
    // Catching up to the Start's directory confirms the scope without erasing
    // a failure that was actually observed there.
    terminal.process_input(b"\x1b]7;file:///two\x07");
    host.observe_directory(terminal.organism_working_dir());
    assert_eq!(host.native.repo_vigil(), RepoVigil::Failure);
}

#[test]
fn directory_and_start_evidence_follow_wire_order_within_one_completion_batch() {
    for newer_directory in [false, true] {
        let mut host = observed_host();
        let mut terminal = crate::terminal::TerminalState::new(80, 24);
        terminal.process_input(b"\x1b]7;file:///a\x07");
        host.observe_directory(terminal.organism_working_dir());
        let command = "\x1b]133;A\x07$ \x1b]133;B\x07cargo test\r\n\x1b]133;C;cmdline_url=cargo%20test;cwd=/c\x07\x1b]133;D;1\x07";
        let report = "\x1b]7;file:///b\x07";
        let batch = if newer_directory {
            format!("{command}{report}")
        } else {
            format!("{report}{command}")
        };
        terminal.process_input(batch.as_bytes());
        let completed = terminal.take_completed_command_events();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].cwd.as_deref(), Some("/c"));
        // The shell's ordinary OSC 7 cache deliberately retains /b in both
        // cases. Only the creature's separate channel captures the ordering.
        assert_eq!(terminal.current_working_dir.as_deref(), Some("/b"));
        assert_eq!(
            terminal.organism_working_dir(),
            Some(if newer_directory { "/b" } else { "/c" })
        );
        host.batch(
            "local",
            terminal.command_records(),
            &completed,
            false,
            false,
        );
        assert_eq!(host.native.repo_vigil(), RepoVigil::Failure);
        host.observe_directory(terminal.organism_working_dir());
        host.advance_ambient(host.reaction_until, true, 1.0);
        if newer_directory {
            assert_eq!(host.work_context.as_deref(), Some("/b"));
            assert_eq!(host.native.repo_vigil(), RepoVigil::None);
            assert_ne!(host.context.behavior, Behavior::GuardFailure);
        } else {
            assert_eq!(host.work_context.as_deref(), Some("/c"));
            assert_eq!(host.native.repo_vigil(), RepoVigil::Failure);
            assert_eq!(host.context.behavior, Behavior::GuardFailure);
        }
    }
}

#[test]
fn first_idle_directory_observation_retires_mismatched_or_unknown_work() {
    for cwd in [Some("/other"), None] {
        let mut host = observed_host();
        let mut terminal = crate::terminal::TerminalState::new(80, 24);
        observed_command(&mut host, &mut terminal, "cargo test", "/source", Some(1));
        assert_eq!(host.native.repo_vigil(), RepoVigil::Failure);
        host.observe_directory(cwd);
        assert_eq!(host.native.repo_vigil(), RepoVigil::None);
    }
}

#[test]
fn repeated_unknown_directory_does_not_restart_ambient_or_clear_painted_origins() {
    let mut host = observed_host();
    host.advance_ambient(Duration::from_secs(1), true, 0.1);
    host.bridge.last_settled = Some(host.context.behavior);
    host.native_bridge.last_settled = Some(host.context.behavior);
    let ambient = format!("{:?}", host.ambient);
    let pose = host.context.behavior;
    for _ in 0..3 {
        host.observe_directory(None);
        assert_eq!(format!("{:?}", host.ambient), ambient);
        assert_eq!(host.context.behavior, pose);
        assert_eq!(host.bridge.last_settled, Some(pose));
        assert_eq!(host.native_bridge.last_settled, Some(pose));
    }
}

#[test]
fn creature_directory_rejects_rebinding_stale_ends_and_unusable_locations() {
    let mut terminal = crate::terminal::TerminalState::new(80, 24);
    terminal.process_input(
        b"\x1b]133;A\x07$ \x1b]133;B\x07run\r\n\x1b]133;C;id=current;cwd=/source\x07",
    );
    terminal.process_input(b"\x1b]133;C;id=current;cwd=/repeat\x07");
    assert_eq!(terminal.organism_working_dir(), Some("/source"));
    terminal.process_input(b"\x1b]133;D;0;id=stale;cwd=/wrong\x07");
    assert_eq!(terminal.organism_working_dir(), Some("/source"));
    terminal.process_input(b"\x1b]133;D;0;id=current;cwd=/destination\x07");
    assert_eq!(terminal.organism_working_dir(), Some("/destination"));
    for invalid in [
        "file://definitely-remote.invalid/remote".to_owned(),
        "relative".to_owned(),
        "/bad%0Apath".to_owned(),
        format!("/{}", "x".repeat(2048)),
    ] {
        terminal.process_input(b"\x1b]7;file:///valid\x07");
        terminal.process_input(format!("\x1b]7;{invalid}\x07").as_bytes());
        assert_eq!(terminal.organism_working_dir(), None);
    }
    terminal.process_input(b"\x1b]7;file:///valid\x07\x1b[?1049h\x1b]7;file:///alt\x07\x1b[?1049l");
    assert_eq!(terminal.organism_working_dir(), None);
}

#[test]
fn exhaustion_shows_rest_and_keeps_unfinished_work_as_a_separate_intention() {
    let mut host = observed_host();
    let mut terminal = crate::terminal::TerminalState::new(80, 24);
    observed_command(&mut host, &mut terminal, "cargo test", "/project", Some(1));
    host.life.replace_state(crate::organism::LifeState {
        energy: 0.05,
        ..host.life.state()
    });
    let now = host.reaction_until;
    host.advance_ambient(now, true, 1.0);
    assert_eq!(host.context.behavior, Behavior::Sleep);
    assert_eq!(host.status_description(now, host.context, true), "Resting");
    let inspection = host.inspect_text(host.context, true);
    assert!(inspection.starts_with("Resting\n"));
    assert!(inspection.contains("Unfinished work: Keeping watch on a failure"));
    host.life.replace_state(crate::organism::LifeState {
        energy: 0.3,
        ..host.life.state()
    });
    host.advance_ambient(now + Duration::from_secs(1), true, 1.0);
    assert_eq!(host.context.behavior, Behavior::GuardFailure);
}

#[test]
fn native_transition_uses_its_own_submitted_pose_and_stable_envelope() {
    let ctx = egui::Context::default();
    let config = full_config();
    let mut host = observed_host();
    host.context = PreviewPose::Concerned.context();
    host.reaction_until = Duration::from_secs(120);
    host.bridge.last_settled = Some(Behavior::Sleep);
    let first = body_frame(&mut host, &ctx, &config, "active", safe_region());
    assert!(!first.is_empty());
    let size = host.native_surface_rect.unwrap().size();
    assert_eq!(
        host.native_bridge.last_settled,
        Some(Behavior::InspectError)
    );

    host.context = RenderContext::new(Behavior::GuardFailure, BodyLanguage::default(), false);
    let second = body_frame(&mut host, &ctx, &config, "active", safe_region());
    assert!(!second.is_empty());
    assert_eq!(host.native_surface_rect.unwrap().size(), size);
    let arc = host.native_bridge.active.unwrap();
    assert_eq!(arc.transition, VisualTransition::InspectErrorToGuardFailure);
    assert_eq!(host.bridge.last_settled, Some(Behavior::Sleep));
    let now = arc.started + Duration::from_millis(u64::from(arc.transition.frame_count()) * 100);
    host.born = Instant::now() - now;
    body_frame(&mut host, &ctx, &config, "active", safe_region());
    assert!(host.native_bridge.active.is_none());
    assert_eq!(
        host.native_bridge.last_settled,
        Some(Behavior::GuardFailure)
    );
}

#[test]
fn native_transition_retires_on_source_change_input_retreat_and_calm() {
    let ctx = egui::Context::default();
    let mut config = full_config();
    let mut host = observed_host();
    host.context = PreviewPose::Concerned.context();
    host.reaction_until = Duration::from_secs(120);
    body_frame(&mut host, &ctx, &config, "old-block", safe_region());
    host.context = RenderContext::new(Behavior::GuardFailure, BodyLanguage::default(), false);
    body_frame(&mut host, &ctx, &config, "new-block", safe_region());
    assert!(
        host.native_bridge.active.is_none(),
        "a different source has no painted origin"
    );

    host.context = PreviewPose::Success.context();
    body_frame(&mut host, &ctx, &config, "new-block", safe_region());
    host.accepted_input("local");
    assert!(host.native_bridge.last_settled.is_none());
    assert!(body_frame(&mut host, &ctx, &config, "new-block", safe_region()).is_empty());
    assert!(host.native_surface_rect.is_none());

    host.born = Instant::now() - Duration::from_secs(2);
    config.ascii_organism_motion = Some(OrganismMotion::Calm);
    assert!(!body_frame(&mut host, &ctx, &config, "new-block", safe_region()).is_empty());
    assert!(host.native_bridge.last_settled.is_none());
    assert!(host.native_bridge.active.is_none());
}

#[test]
fn an_unfitting_body_retires_its_painted_origin_and_its_animation_wake() {
    let ctx = egui::Context::default();
    let config = full_config();
    let mut host = observed_host();
    host.context = PreviewPose::Concerned.context();
    body_frame(&mut host, &ctx, &config, "active", safe_region());
    assert!(host.native_bridge.last_settled.is_some());
    let small = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(20.0, 10.0));
    assert!(body_frame(&mut host, &ctx, &config, "active", small).is_empty());
    assert!(host.native_surface_rect.is_none());
    assert!(host.native_bridge.last_settled.is_none());
    let policy = PresentationPolicy {
        enabled: true,
        focused_owner: true,
        local: true,
        motion: Some(OrganismMotion::Full),
        ..Default::default()
    };
    assert_eq!(
        host.native_body_wake(policy, host.born.elapsed(), None),
        None
    );
}

#[test]
fn inspecting_the_creature_is_content_free_and_does_not_spend_attention() {
    let mut host = observed_host();
    host.work_context = Some("/private/checkout".into());
    host.reaction_status = "unrelated internal fixture".into();
    host.life.replace_state(crate::organism::LifeState {
        energy: 0.2,
        ..crate::organism::LifeState::default()
    });
    let before = format!("{:?}", host.life.state());
    let ticket = host.attention.entry_offer();
    let display = PreviewPose::Waiting
        .context()
        .with_watch_rhythm(WatchRhythm::Waiting);
    let text = host.inspect_text(display, true);
    assert!(text.contains(behavior_label(display.behavior)));
    assert!(text.contains("the command has not finished"));
    assert!(text.contains("Low energy; ready for a rest"));
    assert!(!text.contains("/private/checkout"));
    assert!(!text.contains("unrelated internal fixture"));
    assert_eq!(host.attention.entry_offer(), ticket);
    assert_eq!(format!("{:?}", host.life.state()), before);
    let paused = host.inspect_text(display, false);
    assert!(paused.contains("Live reactions are paused"));
    assert!(!paused.contains("Low energy"));
}

#[test]
fn compact_chrome_names_the_current_behavior_instead_of_only_memory_mode() {
    for behavior in [
        Behavior::Sleep,
        Behavior::WatchAgent,
        Behavior::UnknownOutcome,
        Behavior::GuardFailure,
        Behavior::GuardRecovery,
    ] {
        let context = RenderContext::new(behavior, BodyLanguage::default(), false);
        let text = live_status_text(context, 0, 220.0);
        assert!(text.contains(behavior_label(behavior)));
        assert!(!text.contains('\n'));
    }
}

#[test]
fn quiet_completion_color_is_finite_live_and_specific_to_its_final_pose() {
    let mut host = observed_host();
    let now = Duration::from_secs(60);
    host.native.command_started(CommandKind::Other);
    let reaction = host
        .native
        .command_finished(CommandKind::Other, Some(0), Some(60_000));
    assert_eq!(
        (reaction.behavior, reaction.tone),
        (Behavior::Celebrate, Tone::Quiet)
    );
    host.react(reaction, now);
    let display = host.context;
    assert_eq!(
        host.presentation_color(now, display, true),
        host.palette.quiet
    );
    assert_ne!(host.palette.quiet, host.palette.success);
    assert_eq!(
        host.presentation_color(now, display, false),
        host.palette.success,
        "a preview does not inherit the live command's quiet tone"
    );
    assert_eq!(
        host.presentation_color(host.reaction_until, display, true),
        host.palette.success,
        "an expired result cannot color a later occurrence of the same pose"
    );
    host.running = true;
    assert_eq!(
        host.presentation_color(now, display, true),
        host.palette.success,
        "new work immediately retires the old reaction's color"
    );
    host.running = false;
    let guard = RenderContext::new(Behavior::GuardCautious, BodyLanguage::default(), false);
    assert_eq!(
        host.presentation_color(now, guard, true),
        host.palette.warning
    );
    host.context = guard;
    assert_eq!(
        host.presentation_color(now, display, true),
        host.palette.success,
        "matching an old result is insufficient once the current context changed"
    );

    host.native.command_started(CommandKind::BuildOrTest);
    let failed = host
        .native
        .command_finished(CommandKind::BuildOrTest, Some(1), Some(20));
    host.react(failed, now);
    assert_eq!(
        host.presentation_color(now, host.context, true),
        host.palette.error
    );
    let greeting = RenderContext::new(Behavior::Approach, BodyLanguage::default(), false);
    assert_eq!(
        host.presentation_color(now, greeting, true),
        host.palette.quiet,
        "greeting priority cannot inherit the failure's red tone"
    );
}

#[test]
fn repeated_clean_builds_use_the_reducers_quieter_completion_color() {
    let mut host = observed_host();
    let now = Duration::from_secs(30);
    for pass in 0..7 {
        host.native.command_started(CommandKind::BuildOrTest);
        let reaction = host
            .native
            .command_finished(CommandKind::BuildOrTest, Some(0), Some(100));
        let expected = if pass < 6 {
            host.palette.success
        } else {
            host.palette.quiet
        };
        host.react(reaction, now);
        assert_eq!(host.context.behavior, Behavior::Celebrate);
        assert_eq!(
            host.presentation_color(now, host.context, true),
            expected,
            "build pass {}",
            pass + 1
        );
    }
}
