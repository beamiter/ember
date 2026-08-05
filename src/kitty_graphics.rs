//! Kitty graphics protocol for ember.
//!
//! The *structural* half of the protocol — control-data parsing, chunk
//! assembly across `m=1` continuations, base64 decoding, raw-format length
//! validation and the pre-decode PNG sniff — lives in
//! [`jterm_core::kitty_graphics`] and is shared with the other jterm
//! terminals. This module owns everything that needs a decoded image or a
//! reply on the wire: the image store and its LRU, placements and their
//! screen/scrollback lifecycle, deletion, the process-global revision counter,
//! the PNG decode (`image` crate, with its own `Limits`) and the protocol
//! responder with its `q=`/`i=`/`I=`/`p=` rules.
//!
//! The responder is deliberately *not* hoisted: its replies take the image
//! dimensions and the error text from whichever decoder produced them, so it
//! has to sit next to the decoder.

use jterm_core::kitty_graphics as kitty;
use kitty::{Action, Assembled, Assembler, Caps, Command, Format, Step};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_KITTY_IMAGES: usize = 100;
const MAX_KITTY_CACHE_MB: u64 = 256;
const MAX_KITTY_PLACEMENTS: usize = 4096;
const MAX_PENDING_RESPONSE_BYTES: usize = 64 * 1024;
/// ember keeps a live screen image store, so a single image may legitimately
/// cover a whole window: the shared `SCREEN` budget is ember's historical
/// 64 MiB / 16384 px limits.
const CAPS: Caps = Caps::SCREEN;
static NEXT_IMAGE_REVISION: AtomicU64 = AtomicU64::new(1);

/// Render a structural failure from [`jterm_core::kitty_graphics`] as this
/// module's error text. The responder classifies its own messages by
/// substring, and none of these contain an `ENOENT`/`ENOSPC` marker, so every
/// structural rejection is answered with `EINVAL` exactly as before.
fn describe(error: kitty::Error) -> String {
    error.to_string()
}

/// Kitty 图像
#[derive(Debug, Clone)]
pub struct KittyImage {
    /// Renderer-facing storage id. For named images this is the protocol id;
    /// anonymous (`i=0`) images receive an internal non-zero id so multiple
    /// anonymous transmit-and-place commands can coexist.
    pub id: u32,
    /// Id visible on the wire. Zero denotes an anonymous image.
    pub protocol_id: u32,
    /// Optional, non-unique image number supplied through `I`.
    pub image_number: Option<u32>,
    /// Monotonic content revision. Image ids may be reused by clients, so UI
    /// texture caches must not treat the id alone as immutable content.
    pub revision: u64,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>, // 原始或解码后的图像数据
}

/// Kitty 图像放置
#[derive(Debug, Clone)]
pub struct KittyPlacement {
    pub image_id: u32,
    pub placement_id: Option<u32>,
    pub x: u32,
    /// Row relative to the live viewport. Negative rows live in scrollback;
    /// adding the terminal's current scroll offset yields the viewport row.
    pub y: i64,
    /// Current visible cell extent. Unlike the protocol's optional `c`/`r`
    /// controls these values are always resolved, including natural-size and
    /// one-sided placements.
    pub width: u32,
    pub height: u32,
    pub requested_columns: Option<u32>,
    pub requested_rows: Option<u32>,
    /// Rows clipped from the original display rectangle by margin scrolling.
    pub clip_top_rows: u32,
    pub clip_bottom_rows: u32,
    pub z_index: i32,
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    /// Pixel offsets within the placement's first terminal cell (`X`, `Y`).
    #[allow(dead_code)] // Parsed now; renderer integration needs per-pane cell pixel metrics.
    pub cell_x_offset: u32,
    #[allow(dead_code)] // Parsed now; renderer integration needs per-pane cell pixel metrics.
    pub cell_y_offset: u32,
}

impl KittyPlacement {
    pub fn viewport_row(&self, scroll_offset: usize) -> i64 {
        self.y
            .saturating_add(i64::try_from(scroll_offset).unwrap_or(i64::MAX))
    }

    fn bottom_row(&self) -> i64 {
        self.y
            .saturating_add(i64::from(self.height))
            .saturating_sub(1)
    }
}

/// The parts of a graphics command this module owns.
///
/// [`jterm_core::kitty_graphics::Command`] models the structural controls
/// (`a`, `f`, `t`, `i`, `I`, `p`, `s`, `v`, `m`, `q`). What is left is the
/// placement geometry and the delete selector — controls that only mean
/// something to an app that actually draws — plus the identity the responder
/// echoes back. Those are read out of the core command through
/// `Command::get`/`u32_value`/`i32_value`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KittyGraphicsParams {
    pub image_id: Option<u32>,        // i
    pub image_number: Option<u32>,    // I
    pub placement_id: Option<u32>,    // p
    pub delete: Option<char>,         // d: delete selector (case controls data lifetime)
    pub x: Option<u32>,               // x: source-image crop offset (not screen position)
    pub y: Option<u32>,               // y: source-image crop offset (not screen position)
    pub crop_width: Option<u32>,      // w: source crop width in pixels
    pub crop_height: Option<u32>,     // h: source crop height in pixels
    pub cell_x_offset: Option<u32>,   // X: horizontal pixel offset in first cell
    pub cell_y_offset: Option<u32>,   // Y: vertical pixel offset in first cell
    pub display_columns: Option<u32>, // c: displayed width in terminal cells
    pub display_rows: Option<u32>,    // r: displayed height in terminal cells
    pub cursor_policy: Option<u32>,   // C: 1 keeps the cursor in place
    pub z: Option<i32>,               // z: z-order
    quiet: Option<u8>,
    resolved_storage_id: Option<u32>,
}

/// Kitty 图像协议状态管理
pub struct KittyGraphicsState {
    images: HashMap<u32, KittyImage>,
    placements: Vec<KittyPlacement>,
    hidden_screen_placements: Vec<KittyPlacement>,
    /// Chunk assembly, base64 and every structural cap, shared with the other
    /// jterm terminals.
    assembler: Assembler,
    /// The app-owned half of the first chunk of the transfer the assembler is
    /// currently assembling. The final chunk of a chunked transfer carries no
    /// metadata at all, so `a=T` placement geometry has to be held here for
    /// the life of the transfer; the responder also falls back to this
    /// identity when a continuation chunk is rejected.
    ///
    /// The assembler keys in-flight transfers per image id plus one anonymous
    /// slot, but an id-less continuation always lands in the most recently
    /// started chunked transfer, so a single slot mirrors it exactly.
    current_transfer: Option<KittyGraphicsParams>,
    total_bytes_processed: u64,
    total_image_memory: u64,
    access_order: std::collections::VecDeque<u32>,
    /// Protocol responses (e.g. query replies) awaiting transmission to the PTY.
    pending_responses: Vec<u8>,
    next_generated_image_id: u32,
    response_image_id: Option<u32>,
    pending_cursor_movement: Option<(u32, u32)>,
    cell_width_pixels: u32,
    cell_height_pixels: u32,
    screen_rows: u32,
    max_scrollback_rows: u32,
}

impl KittyGraphicsState {
    pub fn new() -> Self {
        Self {
            images: HashMap::new(),
            placements: Vec::new(),
            hidden_screen_placements: Vec::new(),
            assembler: Assembler::new(CAPS),
            current_transfer: None,
            total_bytes_processed: 0,
            total_image_memory: 0,
            access_order: std::collections::VecDeque::new(),
            pending_responses: Vec::new(),
            next_generated_image_id: u32::MAX,
            response_image_id: None,
            pending_cursor_movement: None,
            // A safe fallback until the renderer reports exact pane metrics.
            // Common terminal cell geometry also makes headless protocol tests
            // deterministic.
            cell_width_pixels: 8,
            cell_height_pixels: 16,
            screen_rows: 24,
            max_scrollback_rows: 10_000,
        }
    }

    /// Drain any protocol responses (query replies) for transmission to the PTY.
    pub fn take_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending_responses)
    }

    /// Return the resolved cell delta requested by the most recent successful
    /// placement. Natural and one-sided placements use the most recently
    /// reported pane cell metrics, just like placement hit-testing and scroll
    /// lifecycle tracking.
    pub fn take_cursor_movement(&mut self) -> Option<(u32, u32)> {
        self.pending_cursor_movement.take()
    }

    fn ceil_div(numerator: u64, denominator: u64) -> u32 {
        let value = numerator.saturating_add(denominator.saturating_sub(1)) / denominator.max(1);
        u32::try_from(value.max(1)).unwrap_or(u32::MAX)
    }

    fn resolved_extent(
        source_width: u32,
        source_height: u32,
        requested_columns: Option<u32>,
        requested_rows: Option<u32>,
        cell_width: u32,
        cell_height: u32,
    ) -> (u32, u32) {
        let columns = requested_columns.filter(|value| *value != 0);
        let rows = requested_rows.filter(|value| *value != 0);
        let cell_width = cell_width.max(1);
        let cell_height = cell_height.max(1);
        match (columns, rows) {
            (Some(columns), Some(rows)) => (columns, rows),
            (Some(columns), None) => {
                let numerator = u64::from(columns)
                    .saturating_mul(u64::from(cell_width))
                    .saturating_mul(u64::from(source_height));
                let denominator =
                    u64::from(source_width.max(1)).saturating_mul(u64::from(cell_height));
                (columns, Self::ceil_div(numerator, denominator))
            }
            (None, Some(rows)) => {
                let numerator = u64::from(rows)
                    .saturating_mul(u64::from(cell_height))
                    .saturating_mul(u64::from(source_width));
                let denominator =
                    u64::from(source_height.max(1)).saturating_mul(u64::from(cell_width));
                (Self::ceil_div(numerator, denominator), rows)
            }
            (None, None) => (
                Self::ceil_div(u64::from(source_width), u64::from(cell_width)),
                Self::ceil_div(u64::from(source_height), u64::from(cell_height)),
            ),
        }
    }

    fn recompute_placement_extent(
        placement: &mut KittyPlacement,
        cell_width: u32,
        cell_height: u32,
    ) -> bool {
        let (width, full_height) = Self::resolved_extent(
            placement.source_width,
            placement.source_height,
            placement.requested_columns,
            placement.requested_rows,
            cell_width,
            cell_height,
        );
        let clipped = placement
            .clip_top_rows
            .saturating_add(placement.clip_bottom_rows);
        placement.width = width;
        placement.height = full_height.saturating_sub(clipped);
        placement.height != 0
    }

    /// Update pane cell metrics used by natural-size placement geometry. The
    /// renderer calls this before painting; output parsed between frames uses
    /// the most recently reported metrics.
    pub fn set_cell_size_pixels(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if self.cell_width_pixels == width && self.cell_height_pixels == height {
            return;
        }
        self.cell_width_pixels = width;
        self.cell_height_pixels = height;
        self.placements
            .retain_mut(|placement| Self::recompute_placement_extent(placement, width, height));
        self.hidden_screen_placements
            .retain_mut(|placement| Self::recompute_placement_extent(placement, width, height));
    }

    pub fn cell_size_pixels(&self) -> (u32, u32) {
        (self.cell_width_pixels, self.cell_height_pixels)
    }

    pub fn set_max_scrollback_rows(&mut self, rows: usize) {
        self.max_scrollback_rows = u32::try_from(rows).unwrap_or(u32::MAX).max(1);
        self.prune_scrollback();
    }

    fn enforce_image_limits(&mut self) {
        while self.images.len() > MAX_KITTY_IMAGES
            || self.total_image_memory > MAX_KITTY_CACHE_MB * 1024 * 1024
        {
            if let Some(oldest_id) = self.access_order.pop_front() {
                if let Some(img) = self.images.remove(&oldest_id) {
                    self.total_image_memory -= img.data.len() as u64;
                    self.placements.retain(|p| p.image_id != oldest_id);
                    self.hidden_screen_placements
                        .retain(|p| p.image_id != oldest_id);
                }
            } else {
                break;
            }
        }
    }

    /// Parse a graphics APC without terminal-position context. Tests and
    /// non-rendering callers use an origin of (0, 0); the terminal parser uses
    /// [`Self::parse_graphics_payload_at`] so placements use the cursor at the
    /// time the final chunk arrives.
    #[allow(dead_code)] // Public compatibility API; the binary uses the cursor-aware variant.
    pub fn parse_graphics_payload(&mut self, payload: &str) -> Result<(), String> {
        self.parse_graphics_payload_at(payload, 0, 0)
    }

    pub fn parse_graphics_payload_at(
        &mut self,
        payload: &str,
        cursor_col: u32,
        cursor_row: u32,
    ) -> Result<(), String> {
        self.response_image_id = None;
        self.pending_cursor_movement = None;
        let bytes = payload.as_bytes();
        let recovered_response = Self::recover_response_controls(payload);

        // Read the controls `jterm_core` does not model. The assembler parses
        // the structural half again for itself; both passes borrow `payload`
        // and allocate nothing, and both reject exactly the same commands.
        let command = match kitty::parse_command(bytes, &CAPS) {
            Ok(command) => command,
            // Rejecting drops every in-flight transfer, which is what the
            // assembler would have done with an unparseable packet too.
            Err(error) => {
                return Err(self.reject_with_identity(recovered_response, describe(error)))
            }
        };
        let params = match Self::app_params(&command) {
            Ok(params) => params,
            Err(error) => return Err(self.reject_with_identity(recovered_response, error)),
        };

        // The assembler owns chunk assembly, base64 and every structural cap.
        // It aborts the transfer a rejected packet could have continued, so
        // its error already leaves the structural state consistent.
        let step = match self.assembler.feed(bytes) {
            Ok(step) => step,
            Err(error) => {
                return Err(self.reject_with_identity(recovered_response, describe(error)))
            }
        };

        let (mut response_params, result) = match step {
            // Unreachable: `parse_command` already rejected a payload that is
            // not a graphics command.
            Step::NotOurs => {
                let error = "Kitty graphics command is missing its G prefix".to_string();
                return Err(self.reject_with_identity(recovered_response, error));
            }
            // A buffered chunk is acknowledged only once its transfer
            // completes, so there is nothing to answer yet. The protocol sends
            // metadata on the first chunk only, so the app-owned controls have
            // to be held until the last chunk arrives.
            Step::NeedMore => {
                match (self.current_transfer.as_mut(), command.is_continuation()) {
                    (Some(pending), true) if command.get("q").is_some() => {
                        pending.quiet = Some(command.quiet);
                    }
                    (_, false) => self.current_transfer = Some(params),
                    _ => {}
                }
                return Ok(());
            }
            Step::Ready(assembled) => {
                let params = if command.is_continuation() {
                    self.current_transfer.take().unwrap_or(params)
                } else {
                    params
                };
                let response_params = KittyGraphicsParams {
                    image_id: assembled.id,
                    image_number: assembled.number,
                    placement_id: assembled.placement,
                    quiet: Some(assembled.quiet),
                    ..KittyGraphicsParams::default()
                };
                let result = self.finish_transfer(params, assembled, cursor_col, cursor_row);
                (response_params, result)
            }
            Step::Other { interrupted, .. } => {
                self.current_transfer = None;
                // A delete aborts an incomplete upload and then still executes.
                // Every other action is a protocol error until the chunk chain
                // is complete.
                if interrupted && command.action != Action::Delete {
                    let error = "Kitty chunked transfer interrupted by another action".to_string();
                    self.queue_command_response(&params, Some(&error));
                    return Err(error);
                }
                match command.action {
                    Action::Placement => {
                        let result = self.handle_placement(params.clone(), cursor_col, cursor_row);
                        (params, result)
                    }
                    Action::Delete => {
                        let result = self.handle_delete(params.clone(), cursor_col, cursor_row);
                        (params, result)
                    }
                    // The query responder is self-contained: it answers with
                    // the identifier the client used whether the probe
                    // succeeded or not.
                    Action::Query => return self.handle_query(params, &command),
                    _ => (params, Err("Unknown action".to_string())),
                }
            }
        };

        if response_params.image_id.is_none() {
            response_params.image_id = self.response_image_id;
        }
        self.queue_command_response(&response_params, result.as_ref().err().map(String::as_str));
        result
    }

    /// Answer a rejected command with whichever identifier could be recovered
    /// from it, falling back to the in-flight transfer's. A malformed command
    /// cannot safely continue any transfer, because its `m=` flag and payload
    /// boundary are unknown and the byte stream is no longer trusted to be
    /// aligned.
    fn reject_with_identity(&mut self, recovered: KittyGraphicsParams, error: String) -> String {
        let mut response_params =
            if recovered.image_id.is_some() || recovered.image_number.is_some() {
                recovered
            } else {
                self.current_transfer.clone().unwrap_or(recovered)
            };
        if response_params.quiet.is_none() {
            response_params.quiet = self
                .current_transfer
                .as_ref()
                .and_then(|params| params.quiet);
        }
        self.abort_transfers();
        self.queue_command_response(&response_params, Some(&error));
        error
    }

    fn abort_transfers(&mut self) {
        self.assembler.reset();
        self.current_transfer = None;
    }

    /// Reject an APC that could not reach the UTF-8/control parser (for
    /// example an oversized or non-UTF-8 packet), while still honoring the
    /// command's recoverable identifier and quiet level. This keeps malformed
    /// input observable to well-behaved clients without allowing either the
    /// response or the in-flight transfer to grow without bound.
    pub(crate) fn reject_graphics_payload(&mut self, payload: &[u8], error: &str) {
        self.response_image_id = None;
        self.pending_cursor_movement = None;
        let mut response_params = Self::recover_response_controls_bytes(payload);
        if response_params.image_id.is_none() && response_params.image_number.is_none() {
            if let Some(pending) = &self.current_transfer {
                response_params = pending.clone();
            }
        } else if response_params.quiet.is_none() {
            response_params.quiet = self
                .current_transfer
                .as_ref()
                .and_then(|pending| pending.quiet);
        }
        self.abort_transfers();
        self.queue_command_response(&response_params, Some(error));
    }

    fn queue_command_response(&mut self, params: &KittyGraphicsParams, error: Option<&str>) {
        let mut response_fields = Vec::with_capacity(3);
        if let Some(id) = params.image_id {
            response_fields.push(format!("i={id}"));
        }
        if let Some(number) = params.image_number {
            response_fields.push(format!("I={number}"));
        }
        if response_fields.is_empty() {
            return;
        }
        if let Some(placement_id) = params.placement_id.filter(|id| *id != 0) {
            response_fields.push(format!("p={placement_id}"));
        }
        let quiet = params.quiet.unwrap_or(0);
        let body = match error {
            None if quiet >= 1 => return,
            None => "OK".to_string(),
            Some(_) if quiet >= 2 => return,
            Some(error) => {
                let code = if error.contains("does not exist") {
                    "ENOENT"
                } else if error.contains("Too many Kitty placements")
                    || error.contains("No Kitty image ids available")
                {
                    "ENOSPC"
                } else {
                    "EINVAL"
                };
                let message: String = error
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .take(160)
                    .collect();
                format!("{code}:{message}")
            }
        };
        let response = format!("\x1b_G{};{body}\x1b\\", response_fields.join(","));
        if self.pending_responses.len().saturating_add(response.len()) <= MAX_PENDING_RESPONSE_BYTES
        {
            self.pending_responses
                .extend_from_slice(response.as_bytes());
        } else {
            log::warn!(
                "[KITTY_GRAPHICS] Dropping protocol response: pending response buffer reached {} bytes",
                MAX_PENDING_RESPONSE_BYTES
            );
        }
    }

    /// Parse the contents of a Kitty graphics APC.
    ///
    /// The wire format is `G<comma-separated control data>;<base64 payload>`.
    /// Splitting exactly once at `;` is important: base64 padding (`=`) belongs
    /// to the payload and must never be interpreted as another control pair.
    fn recover_response_controls(payload: &str) -> KittyGraphicsParams {
        let payload = payload.strip_prefix('G').unwrap_or(payload);
        let control = payload
            .split_once(';')
            .map_or(payload, |(control, _)| control);
        let mut end = control.len().min(kitty::MAX_CONTROL_BYTES);
        while !control.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        Self::recover_response_controls_from_str(&control[..end])
    }

    fn recover_response_controls_bytes(payload: &[u8]) -> KittyGraphicsParams {
        let payload = payload.strip_prefix(b"G").unwrap_or(payload);
        let end = payload
            .iter()
            .take(kitty::MAX_CONTROL_BYTES)
            .position(|byte| *byte == b';')
            .unwrap_or_else(|| payload.len().min(kitty::MAX_CONTROL_BYTES));
        std::str::from_utf8(&payload[..end]).map_or_else(
            |_| KittyGraphicsParams::default(),
            Self::recover_response_controls_from_str,
        )
    }

    fn recover_response_controls_from_str(control: &str) -> KittyGraphicsParams {
        let mut params = KittyGraphicsParams::default();
        for pair in control.split(',') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            match key {
                "i" => params.image_id = value.parse().ok(),
                "I" => params.image_number = value.parse().ok(),
                "p" => params.placement_id = value.parse().ok(),
                "q" => params.quiet = value.parse::<u8>().ok().filter(|value| *value <= 2),
                _ => {}
            }
        }
        params
    }

    /// Read the controls `jterm_core` does not model out of a parsed command.
    ///
    /// The structural half (`a`, `f`, `t`, `i`, `I`, `p`, `s`, `v`, `m`, `q`,
    /// plus the explicitly unsupported `U`/`P`/`Q`/`H`/`V`/`o`/`S`/`O`) has
    /// already been validated by [`jterm_core::kitty_graphics::parse_command`]
    /// by the time this runs.
    fn app_params(command: &Command<'_>) -> Result<KittyGraphicsParams, String> {
        let mut params = KittyGraphicsParams {
            image_id: command.id,
            image_number: command.number,
            placement_id: command.placement,
            x: Self::u32_control(command, "x")?,
            y: Self::u32_control(command, "y")?,
            crop_width: Self::u32_control(command, "w")?,
            crop_height: Self::u32_control(command, "h")?,
            cell_x_offset: Self::u32_control(command, "X")?,
            cell_y_offset: Self::u32_control(command, "Y")?,
            display_columns: Self::u32_control(command, "c")?,
            display_rows: Self::u32_control(command, "r")?,
            cursor_policy: Self::u32_control(command, "C")?,
            z: Self::i32_control(command, "z")?,
            quiet: command.get("q").map(|_| command.quiet),
            ..KittyGraphicsParams::default()
        };

        if let Some(policy) = params.cursor_policy {
            if policy > 1 {
                return Err(format!("Invalid Kitty cursor policy: {policy}"));
            }
        }
        if let Some(value) = command.get("d") {
            let mut chars = value.chars();
            let selector = chars
                .next()
                .filter(|_| chars.next().is_none())
                .ok_or_else(|| format!("Invalid Kitty delete selector: {value}"))?;
            if !matches!(
                selector,
                'a' | 'A'
                    | 'i'
                    | 'I'
                    | 'n'
                    | 'N'
                    | 'c'
                    | 'C'
                    | 'f'
                    | 'F'
                    | 'p'
                    | 'P'
                    | 'q'
                    | 'Q'
                    | 'r'
                    | 'R'
                    | 'x'
                    | 'X'
                    | 'y'
                    | 'Y'
                    | 'z'
                    | 'Z'
            ) {
                return Err(format!("Unsupported Kitty delete selector: {value}"));
            }
            params.delete = Some(selector);
        }
        Ok(params)
    }

    fn u32_control(command: &Command<'_>, key: &str) -> Result<Option<u32>, String> {
        command
            .u32_value(key)
            .map_err(|_| Self::control_error(command, key))
    }

    fn i32_control(command: &Command<'_>, key: &str) -> Result<Option<i32>, String> {
        command
            .i32_value(key)
            .map_err(|_| Self::control_error(command, key))
    }

    fn control_error(command: &Command<'_>, key: &str) -> String {
        format!(
            "Invalid Kitty {key} value: {}",
            command.get(key).unwrap_or_default()
        )
    }

    fn allocate_generated_image_id(&mut self) -> Result<u32, String> {
        let start = self.next_generated_image_id;
        loop {
            let candidate = self.next_generated_image_id;
            self.next_generated_image_id = self.next_generated_image_id.wrapping_sub(1);
            if candidate != 0 && !self.images.contains_key(&candidate) {
                return Ok(candidate);
            }
            if self.next_generated_image_id == start {
                return Err("No Kitty image ids available".to_string());
            }
        }
    }

    fn newest_storage_id_for_number(&self, number: u32) -> Option<u32> {
        self.access_order.iter().rev().copied().find(|storage_id| {
            self.images
                .get(storage_id)
                .is_some_and(|image| image.image_number == Some(number))
        })
    }

    fn newest_storage_id_for_protocol_id(&self, protocol_id: u32) -> Option<u32> {
        if protocol_id != 0
            && self
                .images
                .get(&protocol_id)
                .is_some_and(|image| image.protocol_id == protocol_id)
        {
            return Some(protocol_id);
        }
        self.access_order.iter().rev().copied().find(|storage_id| {
            self.images
                .get(storage_id)
                .is_some_and(|image| image.protocol_id == protocol_id)
        })
    }

    /// `i=` and `I=` are mutually exclusive; the core parser rejects a command
    /// that carries both, so only one branch below can ever be taken.
    fn resolve_image_reference(&self, params: &KittyGraphicsParams) -> Result<u32, String> {
        if let Some(image_id) = params.image_id {
            if image_id == 0 {
                return Err("Anonymous Kitty images cannot be referenced by i=0".to_string());
            }
            return self
                .newest_storage_id_for_protocol_id(image_id)
                .ok_or_else(|| format!("Image {image_id} does not exist"));
        }
        if let Some(image_number) = params.image_number {
            return self
                .newest_storage_id_for_number(image_number)
                .ok_or_else(|| format!("Image number {image_number} does not exist"));
        }
        Err("Missing image ID or image number".to_string())
    }

    fn remove_image_data(&mut self, storage_id: u32) {
        if let Some(image) = self.images.remove(&storage_id) {
            self.total_image_memory = self
                .total_image_memory
                .saturating_sub(image.data.len() as u64);
        }
        self.access_order.retain(|id| *id != storage_id);
    }

    fn relocate_anonymous_storage_collision(&mut self, requested_id: u32) -> Result<(), String> {
        let is_anonymous = self
            .images
            .get(&requested_id)
            .is_some_and(|image| image.protocol_id == 0);
        if !is_anonymous {
            return Ok(());
        }

        let replacement = self.allocate_generated_image_id()?;
        let mut image = self
            .images
            .remove(&requested_id)
            .ok_or("Anonymous Kitty image disappeared during relocation")?;
        image.id = replacement;
        self.images.insert(replacement, image);
        for placement in self
            .placements
            .iter_mut()
            .chain(&mut self.hidden_screen_placements)
        {
            if placement.image_id == requested_id {
                placement.image_id = replacement;
            }
        }
        for storage_id in &mut self.access_order {
            if *storage_id == requested_id {
                *storage_id = replacement;
            }
        }
        Ok(())
    }

    fn reclaim_unplaced_images(&mut self, candidates: impl IntoIterator<Item = u32>) {
        for storage_id in candidates {
            let referenced = self
                .placements
                .iter()
                .chain(&self.hidden_screen_placements)
                .any(|placement| placement.image_id == storage_id);
            if !referenced {
                self.remove_image_data(storage_id);
            }
        }
    }

    fn finish_transfer(
        &mut self,
        params: KittyGraphicsParams,
        assembled: Assembled,
        cursor_col: u32,
        cursor_row: u32,
    ) -> Result<(), String> {
        // Direct-only transmission, i/I exclusivity and the raw geometry were
        // all checked by the assembler before a byte was decoded.
        let format = assembled.format;
        let display = assembled.display;
        let placement_id = assembled.placement;
        // Image numbers get a terminal-assigned protocol id. Anonymous images
        // use a private storage id while remaining i=0 on the wire, allowing
        // multiple anonymous a=T commands to coexist safely.
        let (protocol_id, image_number, image_id) = if let Some(number) = assembled.number {
            let generated = self.allocate_generated_image_id()?;
            self.response_image_id = Some(generated);
            (generated, Some(number), generated)
        } else if let Some(protocol_id) = assembled.id.filter(|id| *id != 0) {
            self.relocate_anonymous_storage_collision(protocol_id)?;
            (protocol_id, None, protocol_id)
        } else {
            (0, None, self.allocate_generated_image_id()?)
        };

        // Normalize all supported wire formats to RGBA for the renderer. The
        // raw formats need no decoder: the assembler already checked that the
        // payload is exactly s*v*channels bytes.
        let (final_data, width, height) = if format == Format::Png {
            self.decode_png(assembled.bytes)?
        } else {
            assembled.into_rgba8().map_err(describe)?
        };

        let data_size = final_data.len() as u64;
        self.total_bytes_processed = self.total_bytes_processed.saturating_add(data_size);

        // Retransmission replaces the image and invalidates all its placements.
        if let Some(old) = self.images.remove(&image_id) {
            self.total_image_memory = self
                .total_image_memory
                .saturating_sub(old.data.len() as u64);
            self.access_order.retain(|&id| id != image_id);
            self.placements
                .retain(|placement| placement.image_id != image_id);
            self.hidden_screen_placements
                .retain(|placement| placement.image_id != image_id);
        }

        self.total_image_memory = self.total_image_memory.saturating_add(data_size);
        self.access_order.push_back(image_id);
        let revision = NEXT_IMAGE_REVISION.fetch_add(1, Ordering::Relaxed);
        self.images.insert(
            image_id,
            KittyImage {
                id: image_id,
                protocol_id,
                image_number,
                revision,
                width,
                height,
                data: final_data,
            },
        );
        self.enforce_image_limits();

        if display {
            let mut placement_params = params;
            placement_params.image_id = Some(protocol_id);
            placement_params.image_number = None;
            placement_params.placement_id = placement_id;
            placement_params.resolved_storage_id = Some(image_id);
            self.handle_placement(placement_params, cursor_col, cursor_row)?;
        }

        log::info!(
            "[KITTY_GRAPHICS] Stored image {} ({}x{}) format: {:?} | Stats: {} images, {}MB total",
            image_id,
            width,
            height,
            format,
            self.images.len(),
            self.total_bytes_processed / 1_000_000
        );
        Ok(())
    }

    /// Decode f=100 strictly as PNG and return normalized RGBA pixels.
    ///
    /// The payload's signature and IHDR have already been sniffed against
    /// [`CAPS`] by the assembler, so a hundred-byte packet can no longer make
    /// the decoder reserve a gigapixel canvas. The post-decode check stays
    /// here because only the decoder knows what the file really contained.
    fn decode_png(&self, data: Vec<u8>) -> Result<(Vec<u8>, u32, u32), String> {
        let mut reader =
            image::ImageReader::with_format(std::io::Cursor::new(data), image::ImageFormat::Png);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(CAPS.max_dimension);
        limits.max_image_height = Some(CAPS.max_dimension);
        limits.max_alloc = Some(CAPS.max_decoded_bytes as u64);
        reader.limits(limits);
        let img = reader
            .decode()
            .map_err(|e| format!("Failed to load image: {}", e))?;

        let width = img.width();
        let height = img.height();
        kitty::raw_layout(width, height, Format::Png, &CAPS).map_err(describe)?;
        let rgba_image = img.to_rgba8();

        log::debug!(
            "[KITTY_GRAPHICS] Decoded PNG image {}x{} -> RGBA {}B",
            width,
            height,
            rgba_image.len()
        );

        Ok((rgba_image.into_raw(), width, height))
    }

    /// 处理放置操作 (a=p)
    fn handle_placement(
        &mut self,
        params: KittyGraphicsParams,
        cursor_col: u32,
        cursor_row: u32,
    ) -> Result<(), String> {
        let image_id = match params.resolved_storage_id {
            Some(storage_id) => storage_id,
            None => self.resolve_image_reference(&params)?,
        };
        let image = self
            .images
            .get(&image_id)
            .ok_or_else(|| format!("Image storage {image_id} does not exist"))?;
        let protocol_id = image.protocol_id;
        let image_width = image.width;
        let image_height = image.height;
        self.response_image_id = (params.image_number.is_some()).then_some(protocol_id);

        // The protocol displays the intersection of the requested source
        // rectangle and the actual image. Oversized rectangles are clipped,
        // not rejected; a wholly disjoint rectangle is a successful no-op.
        let requested_x = params.x.unwrap_or(0);
        let requested_y = params.y.unwrap_or(0);
        let requested_width = params
            .crop_width
            .filter(|value| *value != 0)
            .unwrap_or(image_width);
        let requested_height = params
            .crop_height
            .filter(|value| *value != 0)
            .unwrap_or(image_height);
        let source_x = requested_x.min(image_width);
        let source_y = requested_y.min(image_height);
        let source_end_x = requested_x.saturating_add(requested_width).min(image_width);
        let source_end_y = requested_y
            .saturating_add(requested_height)
            .min(image_height);
        let source_width = source_end_x.saturating_sub(source_x);
        let source_height = source_end_y.saturating_sub(source_y);
        // The requested source rectangle does not intersect the image. There
        // is no placement rectangle and therefore no cursor movement.
        if source_width == 0 || source_height == 0 {
            return Ok(());
        }
        let x = cursor_col;
        let y = i64::from(cursor_row);
        let requested_columns = params.display_columns.filter(|value| *value != 0);
        let requested_rows = params.display_rows.filter(|value| *value != 0);
        let (width, height) = Self::resolved_extent(
            source_width,
            source_height,
            requested_columns,
            requested_rows,
            self.cell_width_pixels,
            self.cell_height_pixels,
        );
        let cell_x_offset = params.cell_x_offset.unwrap_or(0);
        let cell_y_offset = params.cell_y_offset.unwrap_or(0);
        if cell_x_offset >= self.cell_width_pixels {
            return Err(format!(
                "Kitty X offset {cell_x_offset} is outside a {}px cell",
                self.cell_width_pixels
            ));
        }
        if cell_y_offset >= self.cell_height_pixels {
            return Err(format!(
                "Kitty Y offset {cell_y_offset} is outside a {}px cell",
                self.cell_height_pixels
            ));
        }
        let z = params.z.unwrap_or(0);
        if params.cursor_policy.unwrap_or(0) == 0 {
            self.pending_cursor_movement = Some((width, height));
        }

        // A missing placement id (and the protocol's p=0 sentinel) means the
        // placement is deliberately anonymous; repeated puts must coexist.
        let placement_id = params
            .placement_id
            .filter(|id| protocol_id != 0 && *id != 0);

        let placement = KittyPlacement {
            image_id,
            placement_id,
            x,
            y,
            width,
            height,
            requested_columns,
            requested_rows,
            clip_top_rows: 0,
            clip_bottom_rows: 0,
            z_index: z,
            source_x,
            source_y,
            source_width,
            source_height,
            cell_x_offset,
            cell_y_offset,
        };

        // Reusing a placement id updates it instead of growing the list. New
        // ids are capped so untrusted PTY output cannot cause unbounded memory
        // growth and increasingly expensive full-vector sorts.
        if let Some(id) = placement_id {
            if let Some(existing) = self.placements.iter().position(|candidate| {
                candidate.image_id == image_id && candidate.placement_id == Some(id)
            }) {
                self.placements.remove(existing);
            } else if self.placements.len() >= MAX_KITTY_PLACEMENTS {
                return Err(format!(
                    "Too many Kitty placements (limit {})",
                    MAX_KITTY_PLACEMENTS
                ));
            }
        } else if self.placements.len() >= MAX_KITTY_PLACEMENTS {
            return Err(format!(
                "Too many Kitty placements (limit {})",
                MAX_KITTY_PLACEMENTS
            ));
        }
        let insert_at = self
            .placements
            .partition_point(|candidate| candidate.z_index <= placement.z_index);
        self.placements.insert(insert_at, placement);

        log::info!(
            "[KITTY_GRAPHICS] Placed image {} at ({},{}) size {}x{} z={}",
            image_id,
            x,
            y,
            width,
            height,
            z
        );

        Ok(())
    }

    fn placement_intersects_cell(placement: &KittyPlacement, x: u32, y: i64) -> bool {
        let last_x = placement
            .x
            .saturating_add(placement.width.max(1).saturating_sub(1));
        let last_y = placement.bottom_row();
        x >= placement.x && x <= last_x && y >= placement.y && y <= last_y
    }

    fn placement_is_live_visible(placement: &KittyPlacement, screen_rows: u32) -> bool {
        placement.bottom_row() >= 0 && placement.y < i64::from(screen_rows)
    }

    fn delete_matching_placements(
        placements: &mut Vec<KittyPlacement>,
        affected: &mut HashSet<u32>,
        mut predicate: impl FnMut(&KittyPlacement) -> bool,
    ) {
        placements.retain(|placement| {
            if predicate(placement) {
                affected.insert(placement.image_id);
                false
            } else {
                true
            }
        });
    }

    fn one_based_cell(key: &str, value: Option<u32>) -> Result<u32, String> {
        value
            .filter(|value| *value != 0)
            .map(|value| value - 1)
            .ok_or_else(|| format!("Kitty delete selector requires non-zero {key}"))
    }

    /// Handle the standard `a=d,d=<selector>` operation. Lowercase selectors
    /// remove placements but retain image data; uppercase selectors also free
    /// data once no placement in either screen buffer references it.
    fn handle_delete(
        &mut self,
        params: KittyGraphicsParams,
        cursor_col: u32,
        cursor_row: u32,
    ) -> Result<(), String> {
        if params.image_id.is_some() && params.image_number.is_some() {
            return Err("Kitty commands cannot specify both i and I".to_string());
        }

        let selector = params.delete.unwrap_or('a');
        let release_data = selector.is_ascii_uppercase();
        let selector = selector.to_ascii_lowercase();
        let mut affected = HashSet::new();

        match selector {
            // With no d key, a=d defaults to deleting only placements on the
            // currently visible screen. The alternate screen stays intact.
            'a' => {
                let screen_rows = self.screen_rows;
                Self::delete_matching_placements(&mut self.placements, &mut affected, |placement| {
                    Self::placement_is_live_visible(placement, screen_rows)
                })
            }
            'i' => {
                let protocol_id = params
                    .image_id
                    .ok_or("Kitty d=i requires an image id in i")?;
                let storage_ids: HashSet<u32> = self
                    .images
                    .iter()
                    .filter_map(|(storage_id, image)| {
                        (image.protocol_id == protocol_id).then_some(*storage_id)
                    })
                    .collect();
                if storage_ids.is_empty() {
                    return Err(format!("Image {protocol_id} does not exist"));
                }
                affected.extend(&storage_ids);
                let placement_id = params.placement_id.filter(|id| *id != 0);
                let matches = |placement: &KittyPlacement| {
                    storage_ids.contains(&placement.image_id)
                        && placement_id.is_none_or(|id| placement.placement_id == Some(id))
                };
                Self::delete_matching_placements(&mut self.placements, &mut affected, matches);
                Self::delete_matching_placements(
                    &mut self.hidden_screen_placements,
                    &mut affected,
                    matches,
                );
            }
            'n' => {
                let number = params
                    .image_number
                    .ok_or("Kitty d=n requires an image number in I")?;
                let storage_id = self
                    .newest_storage_id_for_number(number)
                    .ok_or_else(|| format!("Image number {number} does not exist"))?;
                self.response_image_id = Some(self.images[&storage_id].protocol_id);
                affected.insert(storage_id);
                let placement_id = params.placement_id.filter(|id| *id != 0);
                let matches = |placement: &KittyPlacement| {
                    placement.image_id == storage_id
                        && placement_id.is_none_or(|id| placement.placement_id == Some(id))
                };
                Self::delete_matching_placements(&mut self.placements, &mut affected, matches);
                Self::delete_matching_placements(
                    &mut self.hidden_screen_placements,
                    &mut affected,
                    matches,
                );
            }
            'c' => {
                Self::delete_matching_placements(&mut self.placements, &mut affected, |placement| {
                    Self::placement_intersects_cell(placement, cursor_col, i64::from(cursor_row))
                })
            }
            'p' | 'q' => {
                let x = Self::one_based_cell("x", params.x)?;
                let y = i64::from(Self::one_based_cell("y", params.y)?);
                let z = if selector == 'q' {
                    Some(params.z.ok_or("Kitty d=q requires z")?)
                } else {
                    None
                };
                Self::delete_matching_placements(
                    &mut self.placements,
                    &mut affected,
                    |placement| {
                        Self::placement_intersects_cell(placement, x, y)
                            && z.is_none_or(|z| placement.z_index == z)
                    },
                );
            }
            'r' => {
                let first = params.x.ok_or("Kitty d=r requires x")?;
                let last = params.y.ok_or("Kitty d=r requires y")?;
                if first > last {
                    return Err("Kitty d=r image id range is reversed".to_string());
                }
                let storage_ids: HashSet<u32> = self
                    .images
                    .iter()
                    .filter_map(|(storage_id, image)| {
                        (image.protocol_id >= first && image.protocol_id <= last)
                            .then_some(*storage_id)
                    })
                    .collect();
                affected.extend(&storage_ids);
                Self::delete_matching_placements(
                    &mut self.placements,
                    &mut affected,
                    |placement| storage_ids.contains(&placement.image_id),
                );
                Self::delete_matching_placements(
                    &mut self.hidden_screen_placements,
                    &mut affected,
                    |placement| storage_ids.contains(&placement.image_id),
                );
            }
            'x' => {
                let x = Self::one_based_cell("x", params.x)?;
                let screen_rows = self.screen_rows;
                Self::delete_matching_placements(
                    &mut self.placements,
                    &mut affected,
                    |placement| {
                        let last_x = placement
                            .x
                            .saturating_add(placement.width.max(1).saturating_sub(1));
                        Self::placement_is_live_visible(placement, screen_rows)
                            && x >= placement.x
                            && x <= last_x
                    },
                );
            }
            'y' => {
                let y = i64::from(Self::one_based_cell("y", params.y)?);
                let screen_rows = self.screen_rows;
                Self::delete_matching_placements(
                    &mut self.placements,
                    &mut affected,
                    |placement| {
                        Self::placement_is_live_visible(placement, screen_rows)
                            && y >= placement.y
                            && y <= placement.bottom_row()
                    },
                );
            }
            'z' => {
                let z = params.z.ok_or("Kitty d=z requires z")?;
                let screen_rows = self.screen_rows;
                Self::delete_matching_placements(
                    &mut self.placements,
                    &mut affected,
                    |placement| {
                        placement.z_index == z
                            && Self::placement_is_live_visible(placement, screen_rows)
                    },
                );
            }
            'f' => return Err("Kitty animation frame deletion is not supported".to_string()),
            _ => return Err(format!("Unsupported Kitty delete selector: {selector}")),
        }

        if release_data {
            self.reclaim_unplaced_images(affected);
        }
        Ok(())
    }

    /// 处理查询操作 (a=q)
    ///
    /// Apps probe protocol support by sending a query with an image id/number;
    /// the terminal must answer with an APC `OK` response (and must NOT store
    /// the image). The response echoes back whichever identifier the app used.
    fn handle_query(
        &mut self,
        params: KittyGraphicsParams,
        command: &Command<'_>,
    ) -> Result<(), String> {
        let result = (|| {
            if params.image_id.filter(|id| *id != 0).is_none()
                && params.image_number.filter(|number| *number != 0).is_none()
            {
                return Err("Kitty query requires a non-zero i or I identifier".to_string());
            }
            // A query carries no image data for the assembler to buffer, so
            // its transport is checked here rather than in `Assembler::feed`.
            command.require_direct_transport().map_err(describe)?;

            if command.payload_b64.is_empty() {
                return Err("No image data provided for Kitty query".to_string());
            }
            let data = kitty::decode_base64(command.payload_b64.as_bytes(), CAPS.max_decoded_bytes)
                .map_err(describe)?;
            if command.format == Format::Png {
                self.decode_png(data)?;
                return Ok(());
            }
            let (width, height) = command
                .declared()
                .ok_or_else(|| "Missing width or height for raw image format".to_string())?;
            let layout =
                kitty::raw_layout(width, height, command.format, &CAPS).map_err(describe)?;
            if data.len() != layout.source_bytes {
                return Err(format!(
                    "Image data has {} bytes, expected {}",
                    data.len(),
                    layout.source_bytes
                ));
            }
            Ok(())
        })();
        self.queue_command_response(&params, result.as_ref().err().map(String::as_str));
        result
    }

    /// 获取所有放置
    pub fn get_placements(&self) -> &[KittyPlacement] {
        &self.placements
    }

    fn clip_top_rows(placement: &mut KittyPlacement, rows: u32) -> bool {
        let rows = rows.min(placement.height);
        placement.clip_top_rows = placement.clip_top_rows.saturating_add(rows);
        placement.height = placement.height.saturating_sub(rows);
        placement.height != 0
    }

    fn clip_bottom_rows(placement: &mut KittyPlacement, rows: u32) -> bool {
        let rows = rows.min(placement.height);
        placement.clip_bottom_rows = placement.clip_bottom_rows.saturating_add(rows);
        placement.height = placement.height.saturating_sub(rows);
        placement.height != 0
    }

    fn prune_placements_to_scrollback(placements: &mut Vec<KittyPlacement>, first_kept_row: i64) {
        placements.retain_mut(|placement| {
            if placement.bottom_row() < first_kept_row {
                return false;
            }
            if placement.y < first_kept_row {
                let clipped = u32::try_from(first_kept_row - placement.y).unwrap_or(u32::MAX);
                placement.y = first_kept_row;
                return Self::clip_top_rows(placement, clipped);
            }
            true
        });
    }

    fn prune_scrollback(&mut self) {
        let first_kept_row = -i64::from(self.max_scrollback_rows);
        Self::prune_placements_to_scrollback(&mut self.placements, first_kept_row);
        // The primary screen can be hidden while an alternate-screen app is
        // active. Its historical placements must obey the same scrollback cap
        // as the text buffer even while they are not being rendered.
        Self::prune_placements_to_scrollback(&mut self.hidden_screen_placements, first_kept_row);
    }

    /// Scroll placements with a text region. `preserve_scrollback` is true
    /// only when the text row leaving the top is actually archived by the
    /// terminal (normally the primary screen with a top margin of zero).
    pub fn scroll_region_up(
        &mut self,
        top: usize,
        bottom: usize,
        lines: usize,
        preserve_scrollback: bool,
    ) {
        let top = i64::try_from(top).unwrap_or(i64::MAX);
        let bottom = i64::try_from(bottom).unwrap_or(i64::MAX);
        let lines = u32::try_from(lines).unwrap_or(u32::MAX);
        let delta = i64::from(lines);
        self.placements.retain_mut(|placement| {
            // Historical placements advance with the history origin whenever
            // another row is archived.
            if preserve_scrollback && placement.y < top {
                placement.y = placement.y.saturating_sub(delta);
                return true;
            }
            // The protocol scrolls only placements wholly inside the margins.
            if placement.y < top || placement.bottom_row() > bottom {
                return true;
            }
            let new_y = placement.y.saturating_sub(delta);
            if new_y >= top {
                placement.y = new_y;
                return true;
            }
            if preserve_scrollback && top == 0 {
                placement.y = new_y;
                return true;
            }
            let clipped = u32::try_from(top - new_y).unwrap_or(u32::MAX);
            placement.y = top;
            Self::clip_top_rows(placement, clipped)
        });
        if preserve_scrollback {
            self.prune_scrollback();
        }
    }

    pub fn scroll_region_down(&mut self, top: usize, bottom: usize, lines: usize) {
        let top = i64::try_from(top).unwrap_or(i64::MAX);
        let bottom = i64::try_from(bottom).unwrap_or(i64::MAX);
        let lines = u32::try_from(lines).unwrap_or(u32::MAX);
        let delta = i64::from(lines);
        self.placements.retain_mut(|placement| {
            if placement.y < top || placement.bottom_row() > bottom {
                return true;
            }
            let new_y = placement.y.saturating_add(delta);
            let new_bottom = new_y
                .saturating_add(i64::from(placement.height))
                .saturating_sub(1);
            placement.y = new_y;
            if new_bottom <= bottom {
                return true;
            }
            let clipped = u32::try_from(new_bottom - bottom).unwrap_or(u32::MAX);
            Self::clip_bottom_rows(placement, clipped)
        });
    }

    #[cfg(test)]
    pub fn clear_rows(&mut self, first: usize, last: usize) {
        let first = i64::try_from(first).unwrap_or(i64::MAX);
        let last = i64::try_from(last).unwrap_or(i64::MAX);
        self.placements
            .retain(|placement| placement.bottom_row() < first || placement.y > last);
    }

    pub fn clear_placements(&mut self) {
        let screen_rows = self.screen_rows;
        self.placements
            .retain(|placement| !Self::placement_is_live_visible(placement, screen_rows));
    }

    /// Clear every placement belonging to the active screen buffer. Alternate
    /// screen entry uses this after swapping buffers: unlike ED2, a newly
    /// entered alternate screen must not retain an old off-screen placement.
    pub fn clear_current_screen_placements(&mut self) {
        self.placements.clear();
    }

    pub fn clear_scrollback_placements(&mut self) {
        self.placements.retain_mut(|placement| {
            if placement.bottom_row() < 0 {
                return false;
            }
            if placement.y < 0 {
                let clipped = u32::try_from(-placement.y).unwrap_or(u32::MAX);
                placement.y = 0;
                return Self::clip_top_rows(placement, clipped);
            }
            true
        });
    }

    pub fn switch_screen(&mut self) {
        std::mem::swap(&mut self.placements, &mut self.hidden_screen_placements);
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = u32::try_from(cols).unwrap_or(u32::MAX);
        let rows = u32::try_from(rows).unwrap_or(u32::MAX);
        self.screen_rows = rows;
        Self::clip_placements_to_screen(&mut self.placements, cols, rows);
        Self::clip_placements_to_screen(&mut self.hidden_screen_placements, cols, rows);
        self.prune_scrollback();
    }

    fn clip_placements_to_screen(placements: &mut Vec<KittyPlacement>, cols: u32, rows: u32) {
        if cols == 0 || rows == 0 {
            placements.clear();
            return;
        }
        let last_row = i64::from(rows - 1);
        placements.retain_mut(|placement| {
            if placement.x >= cols || placement.y > last_row {
                return false;
            }
            let overflow = placement.bottom_row().saturating_sub(last_row);
            if overflow <= 0 {
                return true;
            }
            Self::clip_bottom_rows(placement, u32::try_from(overflow).unwrap_or(u32::MAX))
        });
    }

    /// 获取图像
    pub fn get_image(&self, id: u32) -> Option<&KittyImage> {
        if let Some(image) = self.images.get(&id) {
            return Some(image);
        }
        if id != 0 {
            return None;
        }
        self.access_order.iter().rev().find_map(|storage_id| {
            self.images
                .get(storage_id)
                .filter(|image| image.protocol_id == 0)
        })
    }

    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    pub fn image_memory_mb(&self) -> u64 {
        self.total_image_memory / 1_000_000
    }
}

impl Default for KittyGraphicsState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use image::ImageEncoder;

    fn encode(data: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(data)
    }

    fn transfer_rgba(state: &mut KittyGraphicsState, id: u32, rgba: &[u8]) {
        state
            .parse_graphics_payload(&format!("Gf=32,i={id},s=1,v=1;{}", encode(rgba)))
            .unwrap();
    }

    fn transfer_solid_rgba(state: &mut KittyGraphicsState, id: u32, width: u32, height: u32) {
        let data = vec![255; width as usize * height as usize * 4];
        state
            .parse_graphics_payload(&format!(
                "Gf=32,i={id},s={width},v={height};{}",
                encode(&data)
            ))
            .unwrap();
        state.take_responses();
    }

    fn one_pixel_png() -> Vec<u8> {
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[10, 20, 30, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        png
    }

    fn app_params(payload: &str) -> Result<KittyGraphicsParams, String> {
        let command = kitty::parse_command(payload.as_bytes(), &CAPS).map_err(describe)?;
        KittyGraphicsState::app_params(&command)
    }

    /// The structural controls are parsed by `jterm_core` (and pinned by its
    /// own tests); what this module still parses is the placement geometry and
    /// the delete selector, read back out of the shared command.
    #[test]
    fn app_controls_are_read_out_of_the_core_command() {
        let params = app_params("Ga=p,i=1,p=7,x=4,y=5,w=6,h=7,X=8,Y=9,c=2,r=3,z=-9,C=1").unwrap();
        assert_eq!(params.image_id, Some(1));
        assert_eq!(params.placement_id, Some(7));
        assert_eq!((params.x, params.y), (Some(4), Some(5)));
        assert_eq!((params.crop_width, params.crop_height), (Some(6), Some(7)));
        assert_eq!(
            (params.cell_x_offset, params.cell_y_offset),
            (Some(8), Some(9))
        );
        assert_eq!(
            (params.display_columns, params.display_rows),
            (Some(2), Some(3))
        );
        assert_eq!(params.z, Some(-9));
        assert_eq!(params.cursor_policy, Some(1));
        assert_eq!(app_params("Ga=d,d=I,i=22").unwrap().delete, Some('I'));

        assert_eq!(
            app_params("Ga=p,i=1,x=nope"),
            Err("Invalid Kitty x value: nope".to_string())
        );
        assert_eq!(
            app_params("Ga=p,i=1,z=nope"),
            Err("Invalid Kitty z value: nope".to_string())
        );
        assert_eq!(
            app_params("Ga=p,i=1,C=2"),
            Err("Invalid Kitty cursor policy: 2".to_string())
        );
        assert_eq!(
            app_params("Ga=d,d=w,i=1"),
            Err("Unsupported Kitty delete selector: w".to_string())
        );
    }

    /// Behaviours the shared module standardized. ember already agreed with
    /// the exact-length and continuation rules; the rest are pinned here so a
    /// future core change cannot loosen them silently.
    #[test]
    fn shared_structural_rules_are_enforced() {
        let mut state = KittyGraphicsState::new();
        // f= accepts only the three numeric protocol values.
        for alias in ["png", "jpeg", "rgba", "0"] {
            assert!(state
                .parse_graphics_payload(&format!("Gf={alias},i=1,s=1,v=1;AQIDBA=="))
                .is_err());
        }
        // i= and I= are mutually exclusive at parse time.
        assert!(state
            .parse_graphics_payload("Gf=32,i=1,I=2,s=1,v=1;AQIDBA==")
            .is_err());
        // base64: an impossible length and interior padding are rejected,
        // whitespace and sloppy padding are not.
        assert!(state
            .parse_graphics_payload("Gf=32,i=1,s=1,v=1;AQIDBAU")
            .is_err());
        assert!(state
            .parse_graphics_payload("Gf=32,i=1,s=1,v=1;AQ=DBA==")
            .is_err());
        state
            .parse_graphics_payload("Gf=32,i=1,s=1,v=1; AQID BA \n")
            .unwrap();
        assert_eq!(state.get_image(1).unwrap().data, [1, 2, 3, 4]);
        assert_eq!(state.image_count(), 1);
    }

    /// The shared assembler keys in-flight transfers per image id plus one
    /// anonymous slot, so an unrelated single-shot upload no longer destroys a
    /// chunked one. A non-transmit action still aborts everything, which
    /// `delete_aborts_partial_transfer_and_still_executes` covers.
    #[test]
    fn a_chunked_upload_survives_an_unrelated_single_shot_transfer() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload("Ga=T,f=32,i=60,s=1,v=1,c=5,r=6,m=1;AQID")
            .unwrap();
        transfer_rgba(&mut state, 61, &[5, 6, 7, 8]);
        assert!(state.assembler.has_pending());

        // The id-less final chunk still lands in the chunked transfer, and
        // still carries the placement geometry of its own first chunk.
        state.parse_graphics_payload("Gm=0;BA==").unwrap();
        assert_eq!(state.get_image(60).unwrap().data, [1, 2, 3, 4]);
        assert_eq!(state.get_image(61).unwrap().data, [5, 6, 7, 8]);
        let placement = &state.get_placements()[0];
        assert_eq!(placement.image_id, 60);
        assert_eq!((placement.width, placement.height), (5, 6));
        assert!(!state.assembler.has_pending());
    }

    #[test]
    fn defaults_to_transmit_and_f32_rgba() {
        let mut state = KittyGraphicsState::new();
        let rgba = [1, 2, 3, 4];
        state
            .parse_graphics_payload(&format!("Gi=5,s=1,v=1;{}", encode(&rgba)))
            .unwrap();

        let image = state.get_image(5).unwrap();
        assert_eq!((image.width, image.height), (1, 1));
        assert_eq!(image.data, rgba);
    }

    #[test]
    fn f24_rgb_is_expanded_to_rgba() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload(&format!("Gf=24,i=6,s=1,v=1;{}", encode(&[9, 8, 7])))
            .unwrap();
        assert_eq!(state.get_image(6).unwrap().data, [9, 8, 7, 255]);
    }

    #[test]
    fn f100_decodes_png_but_rejects_non_png() {
        let mut state = KittyGraphicsState::new();
        let png = one_pixel_png();
        state
            .parse_graphics_payload(&format!("Gf=100,i=7;{}", encode(&png)))
            .unwrap();
        let image = state.get_image(7).unwrap();
        assert_eq!((image.width, image.height), (1, 1));
        assert_eq!(image.data, [10, 20, 30, 255]);

        // The shared PNG sniff rejects the payload structurally, before the
        // `image` decoder is ever handed a buffer.
        let error = state
            .parse_graphics_payload(&format!("Gf=100,i=8;{}", encode(b"not a PNG")))
            .unwrap_err();
        assert!(error.contains("PNG header is truncated"), "{error}");
        assert!(state.get_image(8).is_none());
    }

    #[test]
    fn chunked_transfer_uses_first_chunk_metadata() {
        let mut state = KittyGraphicsState::new();
        let rgba = [1, 2, 3, 4, 5, 6, 7, 8];

        state
            .parse_graphics_payload(&format!("Gf=32,i=9,s=2,v=1,m=1;{}", encode(&rgba[..3])))
            .unwrap();
        state
            .parse_graphics_payload(&format!("Gm=1,q=1;{}", encode(&rgba[3..6])))
            .unwrap();
        state
            .parse_graphics_payload(&format!("Gm=0;{}", encode(&rgba[6..])))
            .unwrap();

        let image = state.get_image(9).unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.data, rgba);
        assert!(!state.assembler.has_pending());
    }

    #[test]
    fn chunked_transfer_accepts_an_empty_final_chunk() {
        let mut state = KittyGraphicsState::new();
        let rgba = [20, 30, 40, 255];
        state
            .parse_graphics_payload(&format!("Gf=32,i=10,s=1,v=1,m=1;{}", encode(&rgba)))
            .unwrap();
        state.parse_graphics_payload("Gm=0;").unwrap();
        assert_eq!(state.get_image(10).unwrap().data, rgba);
    }

    #[test]
    fn transmit_and_place_uses_final_cursor_and_cell_dimensions() {
        let mut state = KittyGraphicsState::new();
        let rgba = [20, 30, 40, 255];
        state
            .parse_graphics_payload_at(
                &format!(
                    "Ga=T,f=32,i=20,s=1,v=1,p=7,c=3,r=2,x=0,y=0;{}",
                    encode(&rgba)
                ),
                11,
                12,
            )
            .unwrap();

        let placement = &state.get_placements()[0];
        assert_eq!((placement.x, placement.y), (11, 12));
        assert_eq!((placement.width, placement.height), (3, 2));
        assert_eq!(placement.placement_id, Some(7));
    }

    #[test]
    fn transmit_and_place_supports_anonymous_image_zero() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload_at("Ga=T,f=32,s=1,v=1;AQIDBA==", 5, 6)
            .unwrap();

        assert!(state.get_image(0).is_some());
        let placement = &state.get_placements()[0];
        assert_ne!(placement.image_id, 0);
        assert_eq!(state.get_image(placement.image_id).unwrap().protocol_id, 0);
        assert_eq!(placement.placement_id, None);
        assert_eq!((placement.x, placement.y), (5, 6));
    }

    #[test]
    fn chunked_transmit_and_place_uses_cursor_at_final_chunk() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload_at("Ga=T,f=32,i=21,s=1,v=1,c=2,r=4,m=1;AQID", 1, 2)
            .unwrap();
        state
            .parse_graphics_payload_at("Gm=0;BA==", 30, 40)
            .unwrap();

        let placement = &state.get_placements()[0];
        assert_eq!((placement.x, placement.y), (30, 40));
        assert_eq!((placement.width, placement.height), (2, 4));
    }

    #[test]
    fn continuation_metadata_is_rejected_and_aborts_transfer() {
        let mut state = KittyGraphicsState::new();
        let error = state
            .parse_graphics_payload("Gf=32,i=11,s=1,v=1,m=1;AQID")
            .and_then(|()| state.parse_graphics_payload("Gi=11,m=0;BA=="))
            .unwrap_err();
        assert!(error.contains("only m= and an optional q="), "{error}");
        assert!(!state.assembler.has_pending());
        assert_eq!(state.image_count(), 0);
    }

    #[test]
    fn invalid_final_base64_aborts_transfer() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload("Gf=32,i=12,s=1,v=1,m=1;AQID")
            .unwrap();
        assert!(state.parse_graphics_payload("Gm=0;%%%=").is_err());
        assert!(!state.assembler.has_pending());
        assert!(state.get_image(12).is_none());
    }

    #[test]
    fn delete_aborts_partial_transfer_and_still_executes() {
        let mut state = KittyGraphicsState::new();
        transfer_rgba(&mut state, 22, &[1, 2, 3, 4]);
        state
            .parse_graphics_payload("Gf=32,i=23,s=1,v=1,m=1;AQID")
            .unwrap();

        state.parse_graphics_payload("Ga=d,d=I,i=22").unwrap();
        assert!(!state.assembler.has_pending());
        assert!(state.get_image(22).is_none());
        assert!(state.get_image(23).is_none());
    }

    #[test]
    fn final_chunk_cannot_bypass_total_transfer_limit() {
        // The decoded length is computed and checked before a byte is
        // reserved, so a zero budget cannot be bypassed by allocating first.
        assert_eq!(
            kitty::decode_base64(b"AAAA", 0),
            Err(kitty::Error::TooLarge)
        );
        assert_eq!(CAPS, Caps::SCREEN);
        assert_eq!(CAPS.max_decoded_bytes, 64 * 1024 * 1024);
        assert_eq!(CAPS.max_dimension, 16_384);
    }

    #[test]
    fn raw_formats_require_an_exact_decoded_length() {
        let mut state = KittyGraphicsState::new();
        for rgba in [&[1, 2, 3][..], &[1, 2, 3, 4, 5][..]] {
            let error = state
                .parse_graphics_payload(&format!("Gf=32,i=13,s=1,v=1;{}", encode(rgba)))
                .unwrap_err();
            assert!(
                error.contains("raw image length does not match s= and v="),
                "{error}"
            );
        }
        assert!(state.get_image(13).is_none());
    }

    #[test]
    fn oversized_raw_image_is_rejected_before_allocation() {
        let mut state = KittyGraphicsState::new();
        let payload = format!("Gf=32,i=1,s={0},v={0};AAAA", CAPS.max_dimension);
        let error = state.parse_graphics_payload(&payload).unwrap_err();
        assert!(error.contains("exceeds the configured limits"), "{error}");
        assert_eq!(state.image_count(), 0);
    }

    #[test]
    fn non_direct_transmission_is_rejected() {
        let mut state = KittyGraphicsState::new();
        for medium in ["f", "t", "s"] {
            let error = state
                .parse_graphics_payload(&format!("Gf=32,t={medium},i=1,s=1,v=1;L3RtcC9pbWFnZQ=="))
                .unwrap_err();
            assert!(
                error.contains("unsupported kitty graphics transport"),
                "{error}"
            );
        }
        assert!(!state.assembler.has_pending());
    }

    #[test]
    fn query_uses_standard_action_and_does_not_store_an_image() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload("Ga=q,i=31,f=32,s=1,v=1;AQIDBA==")
            .unwrap();
        assert_eq!(state.image_count(), 0);
        assert_eq!(state.take_responses(), b"\x1b_Gi=31;OK\x1b\\");
    }

    #[test]
    fn commands_acknowledge_ids_and_honor_quiet_levels() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload("Gf=32,i=41,s=1,v=1;AQIDBA==")
            .unwrap();
        assert_eq!(state.take_responses(), b"\x1b_Gi=41;OK\x1b\\");

        state
            .parse_graphics_payload("Gf=32,i=42,s=1,v=1,q=1;AQIDBA==")
            .unwrap();
        assert!(state.take_responses().is_empty());

        assert!(state.parse_graphics_payload("Ga=p,i=99").is_err());
        let error = String::from_utf8(state.take_responses()).unwrap();
        assert!(error.starts_with("\x1b_Gi=99;ENOENT:"));

        assert!(state.parse_graphics_payload("Ga=p,i=100,q=2").is_err());
        assert!(state.take_responses().is_empty());
    }

    #[test]
    fn pending_protocol_responses_are_bounded() {
        let mut state = KittyGraphicsState::new();
        state
            .pending_responses
            .resize(MAX_PENDING_RESPONSE_BYTES - 1, b'x');
        let params = KittyGraphicsParams {
            image_id: Some(1),
            ..KittyGraphicsParams::default()
        };

        state.queue_command_response(&params, None);

        assert_eq!(
            state.pending_responses.len(),
            MAX_PENDING_RESPONSE_BYTES - 1
        );
    }

    #[test]
    fn query_rejects_an_unsupported_medium() {
        let mut state = KittyGraphicsState::new();
        let error = state.parse_graphics_payload("Ga=q,t=f,i=43;").unwrap_err();
        assert!(
            error.contains("unsupported kitty graphics transport"),
            "{error}"
        );
        let response = String::from_utf8(state.take_responses()).unwrap();
        assert!(response.starts_with("\x1b_Gi=43;EINVAL:"));
    }

    #[test]
    fn placement_crop_natural_size_and_screen_lifecycle_are_tracked() {
        let mut state = KittyGraphicsState::new();
        let rgba = vec![255; 4 * 4 * 4];
        state
            .parse_graphics_payload(&format!("Gf=32,i=44,s=4,v=4;{}", encode(&rgba)))
            .unwrap();
        state.take_responses();
        state
            .parse_graphics_payload_at("Ga=p,i=44,x=1,y=1,w=2,h=3", 2, 5)
            .unwrap();
        let placement = &state.get_placements()[0];
        assert_eq!((placement.width, placement.height), (1, 1));
        assert_eq!(
            (
                placement.source_x,
                placement.source_y,
                placement.source_width,
                placement.source_height
            ),
            (1, 1, 2, 3)
        );

        state.scroll_region_up(0, 9, 2, true);
        assert_eq!(state.get_placements()[0].y, 3);
        state.switch_screen();
        assert!(state.get_placements().is_empty());
        state.switch_screen();
        assert_eq!(state.get_placements().len(), 1);
        state.clear_rows(3, 3);
        assert!(state.get_placements().is_empty());
    }

    #[test]
    fn placement_extents_resolve_natural_and_one_sided_rectangles() {
        let mut state = KittyGraphicsState::new();
        state.set_cell_size_pixels(10, 20);
        transfer_solid_rgba(&mut state, 45, 40, 20);

        state
            .parse_graphics_payload_at("Ga=p,i=45,p=1", 0, 0)
            .unwrap();
        assert_eq!(state.take_cursor_movement(), Some((4, 1)));
        let natural = state
            .get_placements()
            .iter()
            .find(|placement| placement.placement_id == Some(1))
            .unwrap();
        assert_eq!((natural.width, natural.height), (4, 1));

        state
            .parse_graphics_payload_at("Ga=p,i=45,p=2,c=3", 0, 0)
            .unwrap();
        assert_eq!(state.take_cursor_movement(), Some((3, 1)));
        let columns_only = state
            .get_placements()
            .iter()
            .find(|placement| placement.placement_id == Some(2))
            .unwrap();
        assert_eq!((columns_only.width, columns_only.height), (3, 1));

        state
            .parse_graphics_payload_at("Ga=p,i=45,p=3,r=3", 0, 0)
            .unwrap();
        assert_eq!(state.take_cursor_movement(), Some((12, 3)));
        let rows_only = state
            .get_placements()
            .iter()
            .find(|placement| placement.placement_id == Some(3))
            .unwrap();
        assert_eq!((rows_only.width, rows_only.height), (12, 3));

        // Changing cell geometry keeps natural and aspect-preserving placement
        // hit regions in sync with what the renderer will paint.
        state.set_cell_size_pixels(20, 10);
        let extents: Vec<_> = state
            .get_placements()
            .iter()
            .map(|placement| (placement.placement_id, placement.width, placement.height))
            .collect();
        assert!(extents.contains(&(Some(1), 2, 2)));
        assert!(extents.contains(&(Some(2), 3, 3)));
        assert!(extents.contains(&(Some(3), 3, 3)));
    }

    #[test]
    fn scrolling_tracks_history_and_clips_at_page_margins() {
        let mut state = KittyGraphicsState::new();
        state.resize(20, 6);
        state.set_max_scrollback_rows(2);
        transfer_rgba(&mut state, 46, &[1, 2, 3, 4]);
        state
            .parse_graphics_payload_at("Ga=p,i=46,c=2,r=4,C=1", 0, 0)
            .unwrap();

        state.scroll_region_up(0, 5, 1, true);
        let placement = &state.get_placements()[0];
        assert_eq!((placement.y, placement.height), (-1, 4));
        assert_eq!(placement.viewport_row(1), 0);

        // Only the two rows retained by text scrollback remain renderable.
        for _ in 0..4 {
            state.scroll_region_up(0, 5, 1, true);
        }
        let placement = &state.get_placements()[0];
        assert_eq!((placement.y, placement.height), (-2, 1));
        assert_eq!(placement.clip_top_rows, 3);
        state.scroll_region_up(0, 5, 1, true);
        assert!(state.get_placements().is_empty());

        state
            .parse_graphics_payload_at("Ga=p,i=46,c=2,r=3,C=1", 0, 2)
            .unwrap();
        state.scroll_region_up(2, 5, 1, false);
        let placement = &state.get_placements()[0];
        assert_eq!((placement.y, placement.height), (2, 2));
        assert_eq!(placement.clip_top_rows, 1);

        state.clear_current_screen_placements();
        state
            .parse_graphics_payload_at("Ga=p,i=46,c=2,r=3,C=1", 0, 3)
            .unwrap();
        state.scroll_region_down(2, 5, 1);
        let placement = &state.get_placements()[0];
        assert_eq!((placement.y, placement.height), (4, 2));
        assert_eq!(placement.clip_bottom_rows, 1);
    }

    #[test]
    fn resize_permanently_clips_rows_discarded_from_both_screens() {
        let mut state = KittyGraphicsState::new();
        state.resize(20, 7);
        transfer_rgba(&mut state, 47, &[1, 2, 3, 4]);
        state
            .parse_graphics_payload_at("Ga=p,i=47,c=2,r=4,C=1", 0, 3)
            .unwrap();
        state.switch_screen();
        state
            .parse_graphics_payload_at("Ga=p,i=47,c=2,r=4,C=1", 0, 3)
            .unwrap();

        state.resize(20, 5);
        assert_eq!(
            (
                state.get_placements()[0].height,
                state.get_placements()[0].clip_bottom_rows
            ),
            (2, 2)
        );
        state.switch_screen();
        assert_eq!(
            (
                state.get_placements()[0].height,
                state.get_placements()[0].clip_bottom_rows
            ),
            (2, 2)
        );
    }

    #[test]
    fn unsupported_placement_controls_are_rejected_with_bounded_acknowledgements() {
        let mut state = KittyGraphicsState::new();
        transfer_rgba(&mut state, 48, &[1, 2, 3, 4]);
        state.take_responses();

        for control in ["U=1", "P=1", "Q=1", "H=-1", "V=1"] {
            let error = state
                .parse_graphics_payload(&format!("Ga=p,i=48,{control}"))
                .unwrap_err();
            assert!(
                error.contains("unsupported kitty graphics placement control"),
                "{error}"
            );
            let response = String::from_utf8(state.take_responses()).unwrap();
            assert!(response.starts_with("\x1b_Gi=48;EINVAL:"));
            assert!(response.len() < 256);
        }
        assert!(state.get_placements().is_empty());

        assert!(state.parse_graphics_payload("Ga=p,i=48,U=1,q=2").is_err());
        assert!(state.take_responses().is_empty());
    }

    #[test]
    fn malformed_control_and_non_utf8_rejections_honor_quiet_and_response_bounds() {
        let mut state = KittyGraphicsState::new();
        assert!(state.parse_graphics_payload("Ga=p,i=49,bad").is_err());
        let response = state.take_responses();
        assert!(response.starts_with(b"\x1b_Gi=49;EINVAL:"));
        assert!(response.len() < 256);

        assert!(state.parse_graphics_payload("Ga=p,i=49,bad,q=2").is_err());
        assert!(state.take_responses().is_empty());

        state.reject_graphics_payload(
            b"Ga=p,i=49,q=0;\xff",
            "Kitty graphics command is not valid UTF-8",
        );
        let response = state.take_responses();
        assert!(response.starts_with(b"\x1b_Gi=49;EINVAL:"));
        assert!(response.len() < 256);

        let oversized = format!("Gi=49,{}", "x".repeat(kitty::MAX_CONTROL_BYTES));
        assert!(state.parse_graphics_payload(&oversized).is_err());
        let response = state.take_responses();
        assert!(response.starts_with(b"\x1b_Gi=49;EINVAL:"));
        assert!(response.len() < 256);
    }

    #[test]
    fn revisions_are_unique_across_terminal_states() {
        let mut first_state = KittyGraphicsState::new();
        let mut second_state = KittyGraphicsState::new();
        transfer_rgba(&mut first_state, 1, &[1, 2, 3, 4]);
        transfer_rgba(&mut second_state, 1, &[5, 6, 7, 8]);

        let first_revision = first_state.get_image(1).unwrap().revision;
        let second_revision = second_state.get_image(1).unwrap().revision;
        assert_ne!(first_revision, second_revision);
    }

    #[test]
    fn retransmitting_an_image_invalidates_placements_and_changes_revision() {
        let mut state = KittyGraphicsState::new();
        transfer_rgba(&mut state, 7, &[255, 0, 0, 255]);
        let first_revision = state.get_image(7).unwrap().revision;
        state
            .parse_graphics_payload("Ga=p,i=7,p=2,x=0,y=0")
            .unwrap();
        assert_eq!(state.get_placements().len(), 1);

        transfer_rgba(&mut state, 7, &[0, 255, 0, 255]);
        assert_ne!(state.get_image(7).unwrap().revision, first_revision);
        assert!(state.get_placements().is_empty());
    }

    #[test]
    fn placement_identity_is_scoped_to_image_and_anonymous_puts_coexist() {
        let mut state = KittyGraphicsState::new();
        transfer_rgba(&mut state, 1, &[1, 2, 3, 4]);
        transfer_rgba(&mut state, 2, &[5, 6, 7, 8]);
        state
            .parse_graphics_payload_at("Ga=p,i=1,p=7,c=2,r=3,z=4", 1, 2)
            .unwrap();
        state
            .parse_graphics_payload_at("Ga=p,i=2,p=7,z=-1", 9, 2)
            .unwrap();
        state
            .parse_graphics_payload_at("Ga=p,i=1,p=0", 3, 0)
            .unwrap();
        state.parse_graphics_payload_at("Ga=p,i=1", 4, 0).unwrap();

        assert_eq!(state.get_placements().len(), 4);
        assert!(state
            .get_placements()
            .iter()
            .any(|placement| placement.image_id == 1 && placement.x == 1));
        assert!(state
            .get_placements()
            .iter()
            .any(|placement| placement.image_id == 2 && placement.x == 9));
        assert_eq!(
            state
                .get_placements()
                .iter()
                .filter(|placement| placement.placement_id.is_none())
                .count(),
            2
        );
    }

    #[test]
    fn placement_ids_update_in_place_and_count_is_bounded() {
        let mut state = KittyGraphicsState::new();
        transfer_rgba(&mut state, 1, &[1, 2, 3, 4]);
        transfer_rgba(&mut state, 2, &[5, 6, 7, 8]);
        state
            .parse_graphics_payload_at("Ga=p,i=1,p=7,z=4", 1, 2)
            .unwrap();
        state
            .parse_graphics_payload_at("Ga=p,i=1,p=7,z=-1", 9, 2)
            .unwrap();
        assert_eq!(state.get_placements().len(), 1);
        assert_eq!(state.get_placements()[0].x, 9);

        let template = state.get_placements()[0].clone();
        state.placements = vec![template; MAX_KITTY_PLACEMENTS];
        let error = state
            .parse_graphics_payload("Ga=p,i=2,p=999999,x=0,y=0")
            .unwrap_err();
        assert!(error.contains("Too many Kitty placements"));
        assert_eq!(state.get_placements().len(), MAX_KITTY_PLACEMENTS);
    }

    #[test]
    fn delete_with_image_and_placement_id_removes_only_that_pair() {
        let mut state = KittyGraphicsState::new();
        transfer_rgba(&mut state, 1, &[1, 2, 3, 4]);
        state.parse_graphics_payload("Ga=p,i=1,p=7").unwrap();
        state.parse_graphics_payload("Ga=p,i=1,p=8").unwrap();

        state.parse_graphics_payload("Ga=d,d=i,i=1,p=7").unwrap();
        assert!(state.get_image(1).is_some());
        assert_eq!(state.get_placements().len(), 1);
        assert_eq!(state.get_placements()[0].placement_id, Some(8));
    }

    #[test]
    fn placement_requires_an_existing_image() {
        let mut state = KittyGraphicsState::new();
        let error = state.parse_graphics_payload("Ga=p,i=99").unwrap_err();
        assert!(error.contains("does not exist"));
        assert!(state.get_placements().is_empty());
    }

    #[test]
    fn delete_defaults_to_current_screen_and_case_controls_data_lifetime() {
        let mut state = KittyGraphicsState::new();
        transfer_rgba(&mut state, 1, &[1, 2, 3, 4]);
        transfer_rgba(&mut state, 2, &[5, 6, 7, 8]);
        state.parse_graphics_payload("Ga=p,i=1,C=1").unwrap();
        state.switch_screen();
        state.parse_graphics_payload("Ga=p,i=2,C=1").unwrap();
        state.switch_screen();

        state.parse_graphics_payload("Ga=d").unwrap();
        assert!(state.get_placements().is_empty());
        assert!(state.get_image(1).is_some(), "lowercase keeps image data");
        state.switch_screen();
        assert_eq!(
            state.get_placements().len(),
            1,
            "hidden screen is untouched"
        );

        state.parse_graphics_payload("Ga=d,d=A").unwrap();
        assert!(state.get_placements().is_empty());
        assert!(
            state.get_image(2).is_none(),
            "uppercase reclaims unreferenced data"
        );
        assert!(state.get_image(1).is_some());
    }

    #[test]
    fn delete_image_selector_preserves_or_reclaims_data_by_case() {
        let mut state = KittyGraphicsState::new();
        transfer_rgba(&mut state, 3, &[1, 2, 3, 4]);
        state.parse_graphics_payload("Ga=p,i=3,p=9,C=1").unwrap();

        state.parse_graphics_payload("Ga=d,d=i,i=3,p=9").unwrap();
        assert!(state.get_placements().is_empty());
        assert!(state.get_image(3).is_some());

        state.parse_graphics_payload("Ga=p,i=3,C=1").unwrap();
        state.parse_graphics_payload("Ga=d,d=I,i=3").unwrap();
        assert!(state.get_placements().is_empty());
        assert!(state.get_image(3).is_none());
    }

    #[test]
    fn delete_cell_selector_uses_one_based_intersection_coordinates() {
        let mut state = KittyGraphicsState::new();
        transfer_rgba(&mut state, 4, &[1, 2, 3, 4]);
        state
            .parse_graphics_payload_at("Ga=p,i=4,c=3,r=2,z=-7,C=1", 0, 0)
            .unwrap();

        state
            .parse_graphics_payload("Ga=d,d=q,x=2,y=2,z=-7")
            .unwrap();
        assert!(state.get_placements().is_empty());
        assert!(state.get_image(4).is_some());
    }

    #[test]
    fn placement_ack_echoes_placement_id() {
        let mut state = KittyGraphicsState::new();
        transfer_rgba(&mut state, 5, &[1, 2, 3, 4]);
        state.take_responses();
        state.parse_graphics_payload("Ga=p,i=5,p=17,C=1").unwrap();
        assert_eq!(state.take_responses(), b"\x1b_Gi=5,p=17;OK\x1b\\");

        state
            .parse_graphics_payload("Ga=T,f=32,s=1,v=1,p=99,C=1;AQIDBA==")
            .unwrap();
        assert!(
            state.take_responses().is_empty(),
            "an anonymous image must not emit a p-only acknowledgement"
        );
    }

    #[test]
    fn image_numbers_allocate_unique_ids_and_resolve_to_the_newest_image() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload("Gf=32,I=13,s=1,v=1;AQIDBA==")
            .unwrap();
        let first_id = state
            .images
            .values()
            .find(|image| image.image_number == Some(13))
            .unwrap()
            .protocol_id;
        assert_eq!(
            state.take_responses(),
            format!("\x1b_Gi={first_id},I=13;OK\x1b\\").as_bytes()
        );

        state
            .parse_graphics_payload("Gf=32,I=13,s=1,v=1;BQYHCA==")
            .unwrap();
        let newest_id = state.newest_storage_id_for_number(13).unwrap();
        assert_ne!(newest_id, first_id);
        state.take_responses();
        state.parse_graphics_payload("Ga=p,I=13,p=4,C=1").unwrap();
        assert_eq!(state.get_placements()[0].image_id, newest_id);
        assert_eq!(
            state.take_responses(),
            format!("\x1b_Gi={newest_id},I=13,p=4;OK\x1b\\").as_bytes()
        );
    }

    #[test]
    fn anonymous_images_have_distinct_internal_storage_and_survive_id_collision() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload("Ga=T,f=32,s=1,v=1,C=1;AQIDBA==")
            .unwrap();
        state
            .parse_graphics_payload("Ga=T,f=32,s=1,v=1,C=1;BQYHCA==")
            .unwrap();
        assert_eq!(state.image_count(), 2);
        assert_eq!(state.get_placements().len(), 2);
        assert_ne!(
            state.get_placements()[0].image_id,
            state.get_placements()[1].image_id
        );

        // The first generated storage id is u32::MAX. A later client-named
        // image with that id relocates the anonymous image instead of deleting it.
        state
            .parse_graphics_payload("Gf=32,i=4294967295,s=1,v=1;CQoLDA==")
            .unwrap();
        assert_eq!(state.image_count(), 3);
        assert_eq!(state.get_placements().len(), 2);
        assert_eq!(state.get_image(u32::MAX).unwrap().protocol_id, u32::MAX);

        assert!(state.parse_graphics_payload("Ga=p,i=0,C=1").is_err());
        state.parse_graphics_payload("Ga=d,d=i,i=0").unwrap();
        assert!(state.get_placements().is_empty());
        assert_eq!(state.image_count(), 3, "lowercase retains anonymous data");
        state.parse_graphics_payload("Ga=d,d=I,i=0").unwrap();
        assert_eq!(state.image_count(), 1, "uppercase reclaims all i=0 data");
    }

    #[test]
    fn crop_is_intersected_with_source_image_and_disjoint_crop_is_a_noop() {
        let mut state = KittyGraphicsState::new();
        let rgba = vec![255; 4 * 4 * 4];
        state
            .parse_graphics_payload(&format!("Gf=32,i=6,s=4,v=4;{}", encode(&rgba)))
            .unwrap();
        state
            .parse_graphics_payload("Ga=p,i=6,x=3,y=2,w=10,h=10,C=1")
            .unwrap();
        let placement = &state.get_placements()[0];
        assert_eq!(
            (
                placement.source_x,
                placement.source_y,
                placement.source_width,
                placement.source_height
            ),
            (3, 2, 1, 2)
        );

        state
            .parse_graphics_payload("Ga=p,i=6,x=9,y=9,w=2,h=2,C=1")
            .unwrap();
        assert_eq!(state.get_placements().len(), 1);
    }

    #[test]
    fn placement_parses_cell_offsets_and_cursor_policy() {
        let mut state = KittyGraphicsState::new();
        transfer_rgba(&mut state, 7, &[1, 2, 3, 4]);
        state
            .parse_graphics_payload("Ga=p,i=7,X=3,Y=4,c=2,r=3,C=1")
            .unwrap();
        let placement = &state.get_placements()[0];
        assert_eq!((placement.cell_x_offset, placement.cell_y_offset), (3, 4));
        assert_eq!(state.take_cursor_movement(), None);

        state.parse_graphics_payload("Ga=p,i=7,c=2,r=3").unwrap();
        assert_eq!(state.take_cursor_movement(), Some((2, 3)));
    }

    #[test]
    fn query_requires_decodable_image_data() {
        let mut state = KittyGraphicsState::new();
        let error = state.parse_graphics_payload("Ga=q,i=8;").unwrap_err();
        assert!(error.contains("No image data"));
        assert!(String::from_utf8(state.take_responses())
            .unwrap()
            .starts_with("\x1b_Gi=8;EINVAL:"));
    }
}
