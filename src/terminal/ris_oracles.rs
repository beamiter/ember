//! Frozen host RIS and constructor before shared reset extraction.
use super::*;
use jterm_engine::protocol_state::TerminalModes;
impl TerminalState {
    fn frozen_ris_constructor(
        cols: usize,
        rows: usize,
        host_context: TerminalHostContext,
        sync_output_clock: SyncOutputClock,
    ) -> Self {
        let (cols, rows) = clamp_terminal_dimensions(cols, rows);
        let mut grid = TerminalGrid::new(rows, cols);
        let mut alt_grid = TerminalGrid::new(rows, cols);
        let mut next_raw_row_id = 1u64;
        for id in grid
            .row_ids_mut()
            .iter_mut()
            .chain(alt_grid.row_ids_mut().iter_mut())
        {
            *id = allocate_raw_row_id_from(&mut next_raw_row_id);
        }

        let mut modes = TerminalModes::default();
        modes.insert(25);
        modes.insert(7);
        // xterm's alternateScroll resource defaults on: wheel in less/vim
        // (alt screen, no mouse reporting) is cursor keys, not a no-op.
        modes.insert(1007);
        modes.insert(1036);

        let mut dirty_region = DirtyRegion::new();
        // Mark all rows as dirty on initialization to ensure first frame renders correctly
        dirty_region.mark_all(rows);
        let mut kitty_graphics = KittyGraphicsState::new();
        kitty_graphics.resize(cols, rows);

        TerminalState {
            vt: jterm_engine::native_terminal::NativeTerminal::adopt(jterm_engine::native_terminal::NativeProfile::Ember, ScreenState::with_grids(grid, alt_grid), ProtocolState::with_modes(modes), jterm_engine::parser::Parser::with_bounded_profile(
                jterm_engine::parser::ParserLimits {
                    pending_bytes: MAX_PENDING_ESCAPE,
                    kitty_recovery_bytes: jterm_core::kitty_graphics::MAX_CONTROL_BYTES + 1,
                },
                jterm_engine::parser::ParserProfile::EmberV1,
            )
            .expect("fixed Ember parser limits are valid")),

            host_context,
            scrollback: RetainedHistory::default(),
            selection: None,
            projected_selection: None,
            inline_text_selection: None,
            selection_link_text: None,
            selection_revision: 1,
            #[cfg(test)]
            search_source_epoch: search_chunk::next_source_epoch(),
            search_source_revision: 1,
            search_scrollback_epoch: search_chunk::next_source_epoch(),
            scroll_offset: 0,
            max_scrollback: 10000,
            titles: jterm_engine::titles::TitleState::default(),
            announced_jsh_session_id: None,
            current_working_dir: None,
            organism_working_dir: None,
            global_bg: Color::Default,

            ime_enabled: false,
            preedit_text: String::new(),
            preedit_cursor: 0,

            host_window_focused: false,
            output_buffer: Vec::new(),
            osc5522: Default::default(),
            kitty_graphics,
            dirty_region,
            grid_version: 1,
            // IMPORTANT: row_versions must match grid.rows(), not the parameter 'rows'
            // This ensures dirty tracking works correctly even with scrollback
            row_versions: vec![1; rows], // Use 'rows' here since grid.rows() == rows at init
            chat_pending_submit: None,
            chat_native_takeover_sequence: None,
            inline_cli_launch_floor: None,
            inline_cli_command_scope: None,
            visible_cells_cache: None,
            projected_viewport_cache: None,
            projection_plan_cache: None,
            projection_failure: None,
            projection_last_policy: None,
            projection_frame_source: None,
            projection_budget_cache: std::cell::RefCell::new(None),
            projection_query_cache: std::cell::Cell::new(None),
            projection_raw_budget_cache: std::cell::Cell::new(None),
            projection_materialization_failure: None,
            #[cfg(test)]
            projection_query_evaluations: std::cell::Cell::new(0),
            #[cfg(test)]
            projection_raw_evaluations: std::cell::Cell::new(0),
            projection_frame_valid: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            #[cfg(test)]
            projection_test_budget: None,
            transformed_viewport_cache: None,
            next_projection_plan_revision: 1,
            viewport_mapping_exact_cache: std::cell::Cell::new(None),
            hyperlinks: hyperlink::HyperlinkTable::default(),

            sync_output_clock,
            last_archived_screen_snapshot: Vec::new(),
            last_synced_primary_screen_snapshot: Vec::new(),
            osc52: jterm_engine::clipboard::Osc52Clipboard::default(),

            osc_reply_terminator: b"\x1b\\",
            colors: ColorState::new(TerminalDefaultColors::default()),
            pending_notifications: Vec::new(),
            pending_bell: false,
            total_lines_scrolled: 0,
            next_raw_row_id,
            row_identity_revision: 1,
            full_screen_scroll_revision: 1,
            command_marks: VecDeque::new(),
            watch: super::watch::ParserWatch::default(),
            command_records: VecDeque::new(),
            shell_marker_clock: ShellMarkerClock::default(),
            output_stream: None,
            next_command_sequence: 1,
            completed_history_revision: 0,
            consumed_command_ids: VecDeque::new(),
            finished_output_provenance: HashMap::new(),
            finished_output_owners: HashMap::new(),
            finished_output_revision: 1,
            active_output_provenance: None,
            pending_completed_command_outputs: VecDeque::new(),
            captured_command_output_bytes: 0,
            cleared_blocks_stash: None,
            agent_prompt_input_tainted: false,
            armed_agent_execution: None,
        }
    }
    fn frozen_ris(&mut self) {
        self.record_reset_interrupted_agent_command();
        // Completion events are application-owned once produced. RIS clears
        // terminal history, but must not erase an already-finished event or
        // the Agent interruption emitted immediately above before the app can
        // drain it.
        let pending_completed_command_outputs =
            std::mem::take(&mut self.pending_completed_command_outputs);
        let consumed_command_ids = std::mem::take(&mut self.consumed_command_ids);
        let shell_marker_clock = std::mem::take(&mut self.shell_marker_clock);
        let output_stream = self.output_stream;
        let cols = self.vt.screen.grid.row_len();
        let rows = self.vt.screen.grid.rows();
        let max_scrollback = self.max_scrollback;
        let cell_size = self.kitty_graphics.cell_size_pixels();
        // Theme colours belong to the app, not to the program that sent RIS.
        let default_colors = self.colors.defaults();
        let next_raw_row_id = self.next_raw_row_id;
        let row_identity_revision = self.row_identity_revision;
        let next_command_sequence = self.next_command_sequence;
        let completed_history_revision = self.completed_history_revision;
        let had_completed_history = self
            .command_records
            .iter()
            .any(|record| record.output_source.is_some());
        let direct_inline_cli = self.inline_cli_launch_floor.is_some();
        self.watch.prompt();
        let watch = std::mem::take(&mut self.watch);
        *self = Self::frozen_ris_constructor(
            cols,
            rows,
            self.host_context.clone(),
            self.sync_output_clock.clone(),
        );
        self.watch = watch;
        self.pending_completed_command_outputs = pending_completed_command_outputs;
        self.consumed_command_ids = consumed_command_ids;
        self.shell_marker_clock = shell_marker_clock;
        self.output_stream = output_stream;
        self.colors = ColorState::new(default_colors);
        // A reset replaces every physical row but must never restart the
        // allocator and let an old external origin retarget into the new grid.
        self.next_raw_row_id = next_raw_row_id;
        self.row_identity_revision = row_identity_revision;
        self.next_command_sequence = next_command_sequence;
        self.completed_history_revision = completed_history_revision;
        if had_completed_history {
            self.mark_completed_history_changed();
        }
        self.set_inline_cli_launch(direct_inline_cli);
        self.vt.screen.grid.row_ids_mut().fill(RawRowId::UNTRACKED);
        self.vt.screen.alt_grid.row_ids_mut().fill(RawRowId::UNTRACKED);
        self.fill_untracked_grid_row_ids();
        self.mark_row_identity_changed();
        self.set_max_scrollback(max_scrollback);
        self.kitty_graphics
            .set_cell_size_pixels(cell_size.0, cell_size.1);
    }
}

#[test]
fn ris_matches_frozen_host_reset() {
    for seed in 0..16 {
        let make = || {
            let mut t = TerminalState::new(7, 3);
            t.process_input(
                b"old\r\nhistory\r\nmore\r\nlast\x1b[31;44m\x1b[2;3r\x1b[4h\x1b[?6h\x1b[=17u",
            );
            if seed & 1 != 0 {
                t.process_input(b"\x1b[?1049hALT");
            }
            if seed & 2 != 0 {
                t.process_input(b"\x1b[?2026h");
            }
            if seed & 4 != 0 {
                t.process_input(b"\x1b]8;;https://example.test\x1b\\link");
            }
            if seed & 8 != 0 {
                t.set_inline_cli_launch(true);
            }
            t
        };
        let mut actual = make();
        let mut reference = make();
        actual.hard_reset();
        reference.frozen_ris();
        assert_eq!(
            super::super::alternate_parity_tests::snapshot(&actual),
            super::super::alternate_parity_tests::snapshot(&reference),
            "seed={seed}"
        );
        assert_eq!(
            format!("{:?}", actual.vt.protocol),
            format!("{:?}", reference.vt.protocol)
        );
    }
}
