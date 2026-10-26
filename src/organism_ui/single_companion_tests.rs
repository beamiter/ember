use super::*;

fn render_companion(
    host: &mut OrganismHost,
    ctx: &egui::Context,
    config: &Config,
    surface_session: &str,
    region: egui::Rect,
) -> egui::FullOutput {
    let mut terminal = crate::terminal::TerminalState::new(80, 24);
    let key = terminal
        .projected_viewport(crate::terminal::HistoryProjection::identity(), true)
        .key();
    ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        },
        |ui| {
            host.set_available_geometry(ui.available_rect_before_wrap(), ui.clip_rect());
            let before = ui.available_rect_before_wrap();
            host.draw(ui, config, true, true);
            if config.command_cards_enabled() {
                assert_eq!(before, ui.available_rect_before_wrap());
            }
            host.draw_native_body(
                ui,
                config,
                Some((surface_session.into(), "active-block".into(), key, region)),
                true,
            );
        },
    )
}

fn organism_shapes(output: &egui::FullOutput) -> usize {
    output
        .shapes
        .iter()
        .filter(|shape| match &shape.shape {
            egui::Shape::Text(text) => {
                let text = text.galley.text();
                // Full bodies are one three-line galley; compact bodies keep
                // the shared glyph. Labels/details contain neither.
                text.lines().count() == 3 || text.contains("^-^") || text.contains("o.o")
            }
            _ => false,
        })
        .count()
}

#[test]
fn each_mode_submits_one_body_and_no_duplicate_status_card() {
    for mode in [
        crate::config::TerminalMode::Normal,
        crate::config::TerminalMode::Block,
        crate::config::TerminalMode::Chat,
    ] {
        for motion in [
            OrganismMotion::Full,
            OrganismMotion::Calm,
            OrganismMotion::Static,
        ] {
            let ctx = egui::Context::default();
            let mut host = OrganismHost::default();
            host.acquire(Some("local"), None, false);
            let config = Config {
                terminal_mode: Some(mode),
                ascii_organism_enabled: true,
                ascii_organism_expanded: true,
                ascii_organism_motion: Some(motion),
                ..Config::default()
            };
            let region =
                egui::Rect::from_min_max(egui::pos2(200.0, 100.0), egui::pos2(600.0, 220.0));
            for _ in 0..2 {
                let mut output = render_companion(&mut host, &ctx, &config, "local", region);
                assert_eq!(organism_shapes(&output), 1, "{mode:?} / {motion:?}");
                if config.command_cards_enabled() {
                    assert!(!output.shapes.iter().any(|shape| matches!(
                        &shape.shape,
                        egui::Shape::Text(text) if text.galley.text() == "ASCII organism"
                    )));
                }
                output.textures_delta.clear();
            }
        }
    }
}

#[test]
fn split_handoff_rejects_old_surface_without_resetting_life_or_voice() {
    let ctx = egui::Context::default();
    let mut host = OrganismHost::default();
    host.voice = CompanionVoice::new(17);
    host.acquire(Some("left"), None, false);
    host.presentable = true;
    let config = Config {
        block_mode: true,
        ascii_organism_enabled: true,
        ascii_organism_motion: Some(OrganismMotion::Static),
        ..Config::default()
    };
    let region = egui::Rect::from_min_max(egui::pos2(40.0, 100.0), egui::pos2(400.0, 230.0));
    let reply = host
        .reply_to(true, true, Some("left"), CompanionIntent::Story)
        .unwrap();
    let mood = host.life.state().mood;
    let personality = host.voice.temperament();
    host.acquire(Some("right"), None, false);
    assert_eq!(host.life.state().mood, mood);
    assert_eq!(host.voice.temperament(), personality);
    assert_eq!(host.last_reply, Some(reply));
    assert!(host
        .reply_to(true, true, Some("left"), CompanionIntent::Story)
        .is_err());
    assert_eq!(host.last_reply, Some(reply));
    let mut old = render_companion(&mut host, &ctx, &config, "left", region);
    assert_eq!(organism_shapes(&old), 0);
    assert!(host.native_surface_rect.is_none());
    old.textures_delta.clear();
    let mut current = render_companion(&mut host, &ctx, &config, "right", region);
    assert_eq!(organism_shapes(&current), 1);
    assert_eq!(host.native_surface_identity.as_ref().unwrap().0, "right");
    current.textures_delta.clear();
}

#[test]
fn a_small_safe_region_uses_one_compact_body_without_allocating_rows() {
    let ctx = egui::Context::default();
    let mut host = OrganismHost::default();
    host.acquire(Some("local"), None, false);
    let config = Config {
        block_mode: true,
        ascii_organism_enabled: true,
        ascii_organism_motion: Some(OrganismMotion::Static),
        ..Config::default()
    };
    let region = egui::Rect::from_min_size(egui::pos2(40.0, 100.0), egui::vec2(100.0, 32.0));
    let mut output = render_companion(&mut host, &ctx, &config, "local", region);
    let body = host
        .native_surface_rect
        .expect("compact body fits this safe region");
    assert!(region.contains_rect(body));
    assert_eq!(
        output
            .shapes
            .iter()
            .filter(|shape| matches!(shape.shape, egui::Shape::Text(_)))
            .count(),
        1
    );
    assert!(output.shapes.iter().all(|shape| match &shape.shape {
        egui::Shape::Text(text) => text.galley.text().lines().count() == 1,
        _ => true,
    }));
    output.textures_delta.clear();
}

#[test]
fn conversation_commands_roundtrip_without_shell_bindings() {
    for (name, command) in [
        (
            "organism:company",
            crate::keybindings::Command::OrganismCompany,
        ),
        ("organism:story", crate::keybindings::Command::OrganismStory),
        ("organism:rest", crate::keybindings::Command::OrganismRest),
        (
            "organism:care_toggle",
            crate::keybindings::Command::OrganismCareToggle,
        ),
        (
            "organism:care_snooze",
            crate::keybindings::Command::OrganismCareSnooze,
        ),
    ] {
        assert_eq!(
            name.parse::<crate::keybindings::Command>().unwrap(),
            command
        );
        assert_eq!(command.to_string(), name);
    }
    let mut host = OrganismHost::default();
    host.acquire(Some("local"), None, false);
    host.presentable = true;
    for (enabled, available, target) in [
        (false, true, Some("local")),
        (true, false, Some("local")),
        (true, true, Some("old-pane")),
        (true, true, None),
    ] {
        assert!(host
            .reply_to(enabled, available, target, CompanionIntent::Story)
            .is_err());
        assert_eq!(host.last_reply, None);
    }
    let first = host
        .reply_to(true, true, Some("local"), CompanionIntent::Story)
        .unwrap();
    let second = host
        .reply_to(true, true, Some("local"), CompanionIntent::Story)
        .unwrap();
    assert_ne!(first, second);
}

#[test]
fn optional_care_counts_one_window_and_waits_for_a_quiet_offer() {
    let mut host = OrganismHost::default();
    assert!(!host.care.enabled());
    host.acquire(Some("left"), None, false);
    host.care.set_enabled(true);
    for second in 0..=2700 {
        let now = Duration::from_secs(second);
        host.last_input = Some(now);
        host.observe_care(now, true, false);
    }
    assert!(
        host.pending_care.is_none(),
        "protected work cannot be interrupted"
    );
    // The accumulated care clock belongs to the GUI, not to the pane.
    host.acquire(Some("right"), None, false);
    let now = Duration::from_secs(2701);
    host.last_input = Some(now);
    host.observe_care(now, true, true);
    assert!(host.pending_care.take().is_some());
    host.observe_care(now, true, true);
    assert!(
        host.pending_care.is_none(),
        "one frame cannot duplicate an offer"
    );
}

#[test]
fn care_never_counts_background_or_unobserved_human_activity() {
    for focused in [false, true] {
        let mut host = OrganismHost::default();
        host.care.set_enabled(true);
        for second in 0..=3600 {
            let now = Duration::from_secs(second);
            // A background window has input facts; a foreground window here
            // has none. Neither can establish active human working time.
            host.last_input = (!focused).then_some(now);
            host.observe_care(now, focused, true);
        }
        assert!(host.pending_care.is_none());
    }
}

#[test]
fn temperament_and_opt_in_care_choice_survive_reinstantiation() {
    let first = OrganismHost::default();
    let second = OrganismHost::default();
    assert_eq!(first.voice.temperament(), second.voice.temperament());
    let mut config: Config = toml::from_str("").unwrap();
    assert!(!config.ascii_organism_care_enabled);
    config.ascii_organism_care_enabled = true;
    let persisted = toml::to_string(&config).unwrap();
    let restored: Config = toml::from_str(&persisted).unwrap();
    assert!(restored.ascii_organism_care_enabled);
}

#[test]
fn care_requires_current_owner_paint_and_quiet_terminal_input() {
    let mut host = OrganismHost::default();
    host.acquire(Some("local"), None, false);
    host.care.set_enabled(true);
    host.activity = LiveActivityKnowledge::KnownIdle;
    for second in 0..=2700 {
        let now = Duration::from_secs(second);
        host.last_input = Some(now);
        host.observe_care(now, true, false);
    }
    let now = Duration::from_secs(2701);
    host.presented_body = Some((7, "local".into()));
    for (pass, eligible, terminal_clear, input_clear) in [
        (8, true, true, true),
        (7, false, true, true),
        (7, true, false, true),
        (7, true, true, false),
    ] {
        host.finish_care_frame(now, pass, eligible, terminal_clear, input_clear);
        assert!(host.pending_care.is_none());
    }
    host.presented_body = Some((7, "old-pane".into()));
    host.finish_care_frame(now, 7, true, true, true);
    assert!(host.pending_care.is_none());
    host.presented_body = Some((7, "local".into()));
    host.context = PreviewPose::Concerned.context();
    host.finish_care_frame(now, 7, true, true, true);
    assert!(host.pending_care.is_none());
    host.context = PreviewPose::Calm.context();
    host.finish_care_frame(now, 7, true, true, true);
    assert!(host.pending_care.is_some());
}

#[test]
fn actual_split_renderers_offer_only_the_active_panes_single_body() {
    let ctx = egui::Context::default();
    let config = Config {
        block_mode: true,
        ascii_organism_enabled: true,
        ascii_organism_motion: Some(OrganismMotion::Static),
        ..Config::default()
    };
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 600.0));
    let mut renderers: Vec<_> = (0..2)
        .map(|_| {
            let mut renderer = crate::ui::TerminalRenderer::new(
                14.0,
                8.0,
                1.4,
                crate::config::ScrollbarVisibility::Always,
                crate::theme::Theme::default(),
            );
            renderer.block_mode = true;
            renderer.gpu_rendering = false;
            renderer
        })
        .collect();
    let mut terminals = [None, None];
    let mut views = [
        crate::terminal::ProjectionViewState::new(),
        crate::terminal::ProjectionViewState::new(),
    ];
    let mut host = OrganismHost::default();
    let mut accumulated = egui::FullOutput::default();
    for active in [0, 1, 0, 1] {
        host.acquire(
            Some(if active == 0 { "left" } else { "right" }),
            None,
            false,
        );
        let output = ctx.run_ui(
            egui::RawInput { screen_rect: Some(screen), ..Default::default() },
            |ui| {
                host.set_available_geometry(ui.available_rect_before_wrap(), ui.clip_rect());
                host.draw(ui, &config, true, true);
                let mut chosen = None;
                for pane in 0..2 {
                    let region = egui::Rect::from_min_size(
                        egui::pos2(8.0 + pane as f32 * 492.0, 8.0),
                        egui::vec2(484.0, 580.0),
                    );
                    let renderer = &mut renderers[pane];
                    renderer.sync_font_metrics(&ctx);
                    renderer.companion_enabled = pane == active;
                    let (cols, rows) = renderer.grid_dimensions(region.size());
                    let terminal = terminals[pane].get_or_insert_with(|| {
                        let mut terminal = crate::terminal::TerminalState::new(cols, rows);
                        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07cargo test\r\n\x1b]133;C\x07Tests passed.\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ");
                        terminal
                    });
                    let mut policy = crate::terminal::ProjectionPolicy::new();
                    policy.set_block_chrome(true, 0);
                    let viewport = terminal.projected_viewport_with_state(
                        crate::terminal::HistoryProjection::identity(), true, &policy, &mut views[pane],
                    );
                    assert!(!viewport.row_kinds().iter().any(|kind| matches!(
                        kind,
                        crate::terminal::ProjectedRowKind::CompanionStatus { .. }
                    )));
                    renderer.set_projection_frame(terminal, viewport, &policy);
                    renderer.render_in_rect(
                        ui, terminal, true, pane == active, true,
                        &crate::search::SearchState::default(), &[], &None, region,
                    );
                    if pane == active {
                        chosen = renderer.companion_surface.clone().map(|(block, key, region)| {
                            ((if pane == 0 { "left" } else { "right" }).to_owned(), block, key, region)
                        });
                    } else {
                        assert!(renderer.companion_surface.is_none());
                    }
                }
                assert!(chosen.is_some(), "the active split offers real empty input space");
                host.draw_native_body(ui, &config, chosen, true);
                assert!(host.native_surface_rect.is_some());
            },
        );
        assert_eq!(organism_shapes(&output), 1, "focused pane {active}");
        accumulated.append(output);
    }
    if let Ok(path) = std::env::var("EMBER_COMPANION_RENDER_DUMP") {
        crate::ui::chat::export_native_frame(
            std::path::Path::new(&path),
            &ctx,
            accumulated,
            [1000, 600],
        );
    } else {
        accumulated.textures_delta.clear();
    }
}
