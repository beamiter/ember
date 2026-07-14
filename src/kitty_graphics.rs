use base64::Engine;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_KITTY_IMAGES: usize = 100;
const MAX_KITTY_CACHE_MB: u64 = 256;
const MAX_KITTY_PLACEMENTS: usize = 4096;
const MAX_KITTY_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_KITTY_IMAGE_DIMENSION: u32 = 16_384;
const MAX_PENDING_RESPONSE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_KITTY_CONTROL_BYTES: usize = 16 * 1024;
static NEXT_IMAGE_REVISION: AtomicU64 = AtomicU64::new(1);

/// 图像格式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Rgb,
    Rgba,
}

impl ImageFormat {
    fn from_control(value: &str) -> Option<Self> {
        match value {
            "24" => Some(Self::Rgb),
            "32" => Some(Self::Rgba),
            "100" => Some(Self::Png),
            _ => None,
        }
    }
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

/// Kitty 图像协议参数
#[derive(Debug, Clone)]
pub struct KittyGraphicsParams {
    pub action: Option<String>,    // a: t=transfer, d=delete, p=place, q=query
    pub image_id: Option<u32>,     // i
    pub image_number: Option<u32>, // I
    pub placement_id: Option<u32>, // p
    pub delete: Option<char>,      // d: delete selector (case controls data lifetime)
    pub format: Option<ImageFormat>, // f: 24=RGB, 32=RGBA, 100=PNG
    pub transmission_medium: Option<char>, // t: d=direct (the only supported medium)
    pub width: Option<u32>,        // s
    pub height: Option<u32>,       // v
    pub x: Option<u32>,            // x: source-image crop offset (not screen position)
    pub y: Option<u32>,            // y: source-image crop offset (not screen position)
    pub crop_width: Option<u32>,   // w: source crop width in pixels
    pub crop_height: Option<u32>,  // h: source crop height in pixels
    pub cell_x_offset: Option<u32>, // X: horizontal pixel offset in first cell
    pub cell_y_offset: Option<u32>, // Y: vertical pixel offset in first cell
    pub display_columns: Option<u32>, // c: displayed width in terminal cells
    pub display_rows: Option<u32>, // r: displayed height in terminal cells
    pub cursor_policy: Option<u32>, // C: 1 keeps the cursor in place
    pub z: Option<i32>,            // z: z-order
    pub more: bool,                // m: 1=more data, 0=last
    pub data: Option<String>,      // base64 encoded data
    more_specified: bool,
    quiet: Option<u8>,
    has_first_chunk_metadata: bool,
    resolved_storage_id: Option<u32>,
}

impl Default for KittyGraphicsParams {
    fn default() -> Self {
        Self {
            action: Some("t".to_string()),
            image_id: None,
            image_number: None,
            placement_id: None,
            delete: None,
            format: None,
            transmission_medium: None,
            width: None,
            height: None,
            x: None,
            y: None,
            crop_width: None,
            crop_height: None,
            cell_x_offset: None,
            cell_y_offset: None,
            display_columns: None,
            display_rows: None,
            cursor_policy: None,
            z: None,
            more: false,
            data: None,
            more_specified: false,
            quiet: None,
            has_first_chunk_metadata: false,
            resolved_storage_id: None,
        }
    }
}

/// 待传输的图像数据
struct PendingTransfer {
    metadata: KittyGraphicsParams,
    data: Vec<u8>,
}

/// Hard cap on bytes buffered while waiting for a Kitty graphics
/// chunked-transfer terminator (`m=0`). Without this a peer that keeps
/// sending `m=1` chunks but never closes the transfer would grow
/// `pending_transfer` without bound; the per-image `MAX_KITTY_CACHE_MB`
/// only kicks in after the transfer completes.
const MAX_PENDING_TRANSFER_BYTES: usize = MAX_KITTY_IMAGE_BYTES;

/// Kitty 图像协议状态管理
pub struct KittyGraphicsState {
    images: HashMap<u32, KittyImage>,
    placements: Vec<KittyPlacement>,
    hidden_screen_placements: Vec<KittyPlacement>,
    pending_transfer: Option<PendingTransfer>,
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
            pending_transfer: None,
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
        let recovered_response = Self::recover_response_controls(payload);
        let params = match Self::parse_params(payload) {
            Ok(params) => params,
            Err(error) => {
                // A malformed command cannot safely continue an in-flight
                // transfer because its m flag/payload boundary is unknown.
                let mut response_params = if recovered_response.image_id.is_some()
                    || recovered_response.image_number.is_some()
                {
                    recovered_response
                } else {
                    self.pending_transfer
                        .as_ref()
                        .map(|pending| pending.metadata.clone())
                        .unwrap_or(recovered_response)
                };
                if response_params.quiet.is_none() {
                    response_params.quiet = self
                        .pending_transfer
                        .as_ref()
                        .and_then(|pending| pending.metadata.quiet);
                }
                self.pending_transfer = None;
                self.queue_command_response(&response_params, Some(&error));
                return Err(error);
            }
        };

        let mut response_params = params.clone();
        if params.action.as_deref() == Some("t") {
            if let Some(pending) = &self.pending_transfer {
                response_params = pending.metadata.clone();
                if params.quiet.is_some() {
                    response_params.quiet = params.quiet;
                }
            }
        }

        if self.pending_transfer.is_some() && params.action.as_deref() != Some("t") {
            self.pending_transfer = None;
            // Delete commands abort an incomplete upload, then still execute
            // the requested deletion. Other graphics actions are protocol
            // errors until the chunk chain is complete.
            if params.action.as_deref() != Some("d") {
                let error = "Kitty chunked transfer interrupted by another action".to_string();
                self.queue_command_response(&response_params, Some(&error));
                return Err(error);
            }
        }

        let action = params.action.clone();
        let defer_success_response = matches!(action.as_deref(), Some("t" | "T")) && params.more;
        let is_query = action.as_deref() == Some("q");
        let result = match action.as_deref() {
            Some("t" | "T") => self.handle_transfer(params, cursor_col, cursor_row),
            Some("p") => self.handle_placement(params, cursor_col, cursor_row),
            Some("d") => self.handle_delete(params, cursor_col, cursor_row),
            Some("q") => self.handle_query(params),
            _ => Err("Unknown action".to_string()),
        };
        if !is_query && (result.is_err() || !defer_success_response) {
            if response_params.image_id.is_none() {
                response_params.image_id = self.response_image_id;
            }
            self.queue_command_response(
                &response_params,
                result.as_ref().err().map(String::as_str),
            );
        }
        result
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
            if let Some(pending) = &self.pending_transfer {
                response_params = pending.metadata.clone();
            }
        } else if response_params.quiet.is_none() {
            response_params.quiet = self
                .pending_transfer
                .as_ref()
                .and_then(|pending| pending.metadata.quiet);
        }
        self.pending_transfer = None;
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
        let mut end = control.len().min(MAX_KITTY_CONTROL_BYTES);
        while !control.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        Self::recover_response_controls_from_str(&control[..end])
    }

    fn recover_response_controls_bytes(payload: &[u8]) -> KittyGraphicsParams {
        let payload = payload.strip_prefix(b"G").unwrap_or(payload);
        let end = payload
            .iter()
            .take(MAX_KITTY_CONTROL_BYTES)
            .position(|byte| *byte == b';')
            .unwrap_or_else(|| payload.len().min(MAX_KITTY_CONTROL_BYTES));
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

    fn parse_params(payload: &str) -> Result<KittyGraphicsParams, String> {
        let mut params = KittyGraphicsParams::default();
        let payload = payload.strip_prefix('G').unwrap_or(payload);
        let (control, data) = match payload.split_once(';') {
            Some((control, data)) => (control, Some(data)),
            None => (payload, None),
        };
        if control.len() > MAX_KITTY_CONTROL_BYTES {
            return Err(format!(
                "Kitty control data exceeded the {} byte limit",
                MAX_KITTY_CONTROL_BYTES
            ));
        }
        params.data = data.map(str::to_owned);

        for pair in control.split(',') {
            if pair.is_empty() {
                continue;
            }
            let (key, value) = pair
                .split_once('=')
                .ok_or_else(|| format!("Malformed Kitty control pair: {pair}"))?;

            match key {
                "a" => {
                    if !matches!(value, "t" | "T" | "p" | "q" | "d") {
                        return Err(format!("Unsupported Kitty action: {value}"));
                    }
                    params.action = Some(value.to_string());
                    params.has_first_chunk_metadata = true;
                }
                "i" => {
                    params.image_id = Some(Self::parse_u32_control("i", value)?);
                    params.has_first_chunk_metadata = true;
                }
                "I" => {
                    params.image_number = Some(Self::parse_u32_control("I", value)?);
                    params.has_first_chunk_metadata = true;
                }
                "p" => {
                    params.placement_id = Some(Self::parse_u32_control("p", value)?);
                    params.has_first_chunk_metadata = true;
                }
                "d" => {
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
                    params.has_first_chunk_metadata = true;
                }
                "f" => {
                    params.format = Some(
                        ImageFormat::from_control(value)
                            .ok_or_else(|| format!("Unsupported Kitty image format: {value}"))?,
                    );
                    params.has_first_chunk_metadata = true;
                }
                "t" => {
                    let mut chars = value.chars();
                    let medium = chars
                        .next()
                        .filter(|_| chars.next().is_none())
                        .ok_or_else(|| format!("Invalid Kitty transmission medium: {value}"))?;
                    params.transmission_medium = Some(medium);
                    params.has_first_chunk_metadata = true;
                }
                "s" => {
                    params.width = Some(Self::parse_u32_control("s", value)?);
                    params.has_first_chunk_metadata = true;
                }
                "v" => {
                    params.height = Some(Self::parse_u32_control("v", value)?);
                    params.has_first_chunk_metadata = true;
                }
                "x" => {
                    params.x = Some(Self::parse_u32_control("x", value)?);
                    params.has_first_chunk_metadata = true;
                }
                "y" => {
                    params.y = Some(Self::parse_u32_control("y", value)?);
                    params.has_first_chunk_metadata = true;
                }
                "w" => {
                    params.crop_width = Some(Self::parse_u32_control("w", value)?);
                    params.has_first_chunk_metadata = true;
                }
                "h" => {
                    params.crop_height = Some(Self::parse_u32_control("h", value)?);
                    params.has_first_chunk_metadata = true;
                }
                "X" => {
                    params.cell_x_offset = Some(Self::parse_u32_control("X", value)?);
                    params.has_first_chunk_metadata = true;
                }
                "Y" => {
                    params.cell_y_offset = Some(Self::parse_u32_control("Y", value)?);
                    params.has_first_chunk_metadata = true;
                }
                "c" => {
                    params.display_columns = Some(Self::parse_u32_control("c", value)?);
                    params.has_first_chunk_metadata = true;
                }
                "r" => {
                    params.display_rows = Some(Self::parse_u32_control("r", value)?);
                    params.has_first_chunk_metadata = true;
                }
                "C" => {
                    let policy = Self::parse_u32_control("C", value)?;
                    if policy > 1 {
                        return Err(format!("Invalid Kitty cursor policy: {value}"));
                    }
                    params.cursor_policy = Some(policy);
                    params.has_first_chunk_metadata = true;
                }
                "z" => {
                    params.z = Some(Self::parse_i32_control("z", value)?);
                    params.has_first_chunk_metadata = true;
                }
                "m" => {
                    params.more = match value {
                        "0" => false,
                        "1" => true,
                        _ => return Err(format!("Invalid Kitty chunk flag: {value}")),
                    };
                    params.more_specified = true;
                }
                "q" => {
                    let quiet = Self::parse_u32_control("q", value)?;
                    if quiet > 2 {
                        return Err(format!("Invalid Kitty quiet flag: {value}"));
                    }
                    params.quiet = Some(quiet as u8);
                }
                "U" | "P" | "Q" | "H" | "V" => {
                    return Err(format!(
                        "Unsupported Kitty placement control: {key}={value}"
                    ));
                }
                "o" => {
                    return Err(format!(
                        "Kitty data compression is not supported: o={value}"
                    ));
                }
                "S" | "O" => {
                    return Err(format!(
                        "Unsupported Kitty external-data control: {key}={value}"
                    ));
                }
                _ => {
                    // Ignore forward-compatible control keys on the first
                    // chunk, but remember that this is not a legal m/q-only
                    // continuation command.
                    params.has_first_chunk_metadata = true;
                }
            }
        }

        Ok(params)
    }

    fn parse_u32_control(key: &str, value: &str) -> Result<u32, String> {
        value
            .parse::<u32>()
            .map_err(|_| format!("Invalid Kitty {key} value: {value}"))
    }

    fn parse_i32_control(key: &str, value: &str) -> Result<i32, String> {
        value
            .parse::<i32>()
            .map_err(|_| format!("Invalid Kitty {key} value: {value}"))
    }

    /// 处理传输操作 (a=t)
    fn handle_transfer(
        &mut self,
        mut params: KittyGraphicsParams,
        cursor_col: u32,
        cursor_row: u32,
    ) -> Result<(), String> {
        if self.pending_transfer.is_some() {
            if !params.more_specified || params.has_first_chunk_metadata {
                self.pending_transfer = None;
                return Err(
                    "Kitty continuation chunks may contain only m and optional q controls"
                        .to_string(),
                );
            }

            let remaining = MAX_PENDING_TRANSFER_BYTES
                .saturating_sub(self.pending_transfer.as_ref().map_or(0, |p| p.data.len()));
            let chunk = match Self::decode_base64_payload(
                params.data.as_deref().unwrap_or_default(),
                remaining,
            ) {
                Ok(chunk) => chunk,
                Err(error) => {
                    self.pending_transfer = None;
                    return Err(error);
                }
            };

            let pending = self
                .pending_transfer
                .as_mut()
                .ok_or("Missing pending Kitty transfer")?;
            pending.data.extend_from_slice(&chunk);
            if params.more {
                return Ok(());
            }

            let pending = self
                .pending_transfer
                .take()
                .ok_or("Missing pending Kitty transfer")?;
            return self.finish_transfer(pending.metadata, pending.data, cursor_col, cursor_row);
        }

        Self::validate_transfer_metadata(&params)?;
        let encoded = params.data.take().ok_or("No image data provided")?;
        let data = Self::decode_base64_payload(&encoded, MAX_PENDING_TRANSFER_BYTES)?;

        if params.more {
            params.more = false;
            params.more_specified = false;
            params.data = None;
            self.pending_transfer = Some(PendingTransfer {
                metadata: params,
                data,
            });
            return Ok(());
        }

        self.finish_transfer(params, data, cursor_col, cursor_row)
    }

    fn validate_transfer_metadata(params: &KittyGraphicsParams) -> Result<(), String> {
        if params.transmission_medium.unwrap_or('d') != 'd' {
            return Err("Only direct Kitty image transmission (t=d) is supported".to_string());
        }
        if params.image_id.is_some() && params.image_number.is_some() {
            return Err("Kitty commands cannot specify both i and I".to_string());
        }

        match params.format.unwrap_or(ImageFormat::Rgba) {
            ImageFormat::Rgb | ImageFormat::Rgba => {
                let width = params.width.ok_or("Missing width for raw image format")?;
                let height = params.height.ok_or("Missing height for raw image format")?;
                Self::validate_decoded_size(width, height)?;
            }
            ImageFormat::Png => {}
        }
        Ok(())
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

    fn resolve_image_reference(&self, params: &KittyGraphicsParams) -> Result<u32, String> {
        if params.image_id.is_some() && params.image_number.is_some() {
            return Err("Kitty commands cannot specify both i and I".to_string());
        }
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

    fn decode_base64_payload(encoded: &str, max_decoded: usize) -> Result<Vec<u8>, String> {
        let max_encoded = max_decoded.saturating_add(2) / 3 * 4;
        if encoded.len() > max_encoded {
            return Err(format!(
                "Pending Kitty transfer exceeded {} MiB; dropping",
                MAX_PENDING_TRANSFER_BYTES / 1024 / 1024
            ));
        }

        let standard = base64::engine::general_purpose::STANDARD;
        let data = standard.decode(encoded).or_else(|standard_error| {
            base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(encoded)
                .map_err(|_| standard_error)
        });
        let data = data.map_err(|error| format!("Base64 decode error: {error}"))?;
        if data.len() > max_decoded {
            return Err(format!(
                "Pending Kitty transfer exceeded {} MiB; dropping",
                MAX_PENDING_TRANSFER_BYTES / 1024 / 1024
            ));
        }
        Ok(data)
    }

    fn finish_transfer(
        &mut self,
        params: KittyGraphicsParams,
        mut final_data: Vec<u8>,
        cursor_col: u32,
        cursor_row: u32,
    ) -> Result<(), String> {
        Self::validate_transfer_metadata(&params)?;
        // Image numbers get a terminal-assigned protocol id. Anonymous images
        // use a private storage id while remaining i=0 on the wire, allowing
        // multiple anonymous a=T commands to coexist safely.
        let (protocol_id, image_number, image_id) = if let Some(number) = params.image_number {
            let generated = self.allocate_generated_image_id()?;
            self.response_image_id = Some(generated);
            (generated, Some(number), generated)
        } else if let Some(protocol_id) = params.image_id.filter(|id| *id != 0) {
            self.relocate_anonymous_storage_collision(protocol_id)?;
            (protocol_id, None, protocol_id)
        } else {
            (0, None, self.allocate_generated_image_id()?)
        };
        let format = params.format.unwrap_or(ImageFormat::Rgba);

        // Normalize all supported wire formats to RGBA for the renderer.
        let (width, height) = match format {
            ImageFormat::Png => {
                let (decoded_data, width, height) = self.decode_png(final_data)?;
                final_data = decoded_data;
                (width, height)
            }
            ImageFormat::Rgb => {
                let width = params.width.ok_or("Missing width for raw image format")?;
                let height = params.height.ok_or("Missing height for raw image format")?;
                let pixels = Self::validate_decoded_size(width, height)?;
                let expected = pixels.checked_mul(3).ok_or("Image dimensions overflow")?;
                if final_data.len() != expected {
                    return Err(format!(
                        "RGB data has {} bytes, expected {} for {}x{}",
                        final_data.len(),
                        expected,
                        width,
                        height
                    ));
                }
                let mut rgba = Vec::with_capacity(pixels * 4);
                for chunk in final_data.chunks_exact(3) {
                    rgba.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
                }
                final_data = rgba;
                (width, height)
            }
            ImageFormat::Rgba => {
                let width = params.width.ok_or("Missing width for raw image format")?;
                let height = params.height.ok_or("Missing height for raw image format")?;
                let pixels = Self::validate_decoded_size(width, height)?;
                let expected = pixels.checked_mul(4).ok_or("Image dimensions overflow")?;
                if final_data.len() != expected {
                    return Err(format!(
                        "RGBA data has {} bytes, expected {} for {}x{}",
                        final_data.len(),
                        expected,
                        width,
                        height
                    ));
                }
                (width, height)
            }
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

        if params.action.as_deref() == Some("T") {
            let mut placement_params = params;
            placement_params.image_id = Some(protocol_id);
            placement_params.image_number = None;
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
    fn decode_png(&self, data: Vec<u8>) -> Result<(Vec<u8>, u32, u32), String> {
        if data.len() > MAX_KITTY_IMAGE_BYTES {
            return Err(format!(
                "Compressed image exceeds {} MiB",
                MAX_KITTY_IMAGE_BYTES / 1024 / 1024
            ));
        }

        let mut reader =
            image::ImageReader::with_format(std::io::Cursor::new(data), image::ImageFormat::Png);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_KITTY_IMAGE_DIMENSION);
        limits.max_image_height = Some(MAX_KITTY_IMAGE_DIMENSION);
        limits.max_alloc = Some(MAX_KITTY_IMAGE_BYTES as u64);
        reader.limits(limits);
        let img = reader
            .decode()
            .map_err(|e| format!("Failed to load image: {}", e))?;

        let width = img.width();
        let height = img.height();
        Self::validate_decoded_size(width, height)?;
        let rgba_image = img.to_rgba8();

        log::debug!(
            "[KITTY_GRAPHICS] Decoded PNG image {}x{} -> RGBA {}B",
            width,
            height,
            rgba_image.len()
        );

        Ok((rgba_image.into_raw(), width, height))
    }

    fn validate_decoded_size(width: u32, height: u32) -> Result<usize, String> {
        if width == 0
            || height == 0
            || width > MAX_KITTY_IMAGE_DIMENSION
            || height > MAX_KITTY_IMAGE_DIMENSION
        {
            return Err(format!(
                "Image dimensions {}x{} exceed the supported limit",
                width, height
            ));
        }
        let pixels = (width as usize)
            .checked_mul(height as usize)
            .ok_or("Image dimensions overflow")?;
        let decoded_bytes = pixels.checked_mul(4).ok_or("Image dimensions overflow")?;
        if decoded_bytes > MAX_KITTY_IMAGE_BYTES {
            return Err(format!(
                "Decoded image would use {} bytes (limit {})",
                decoded_bytes, MAX_KITTY_IMAGE_BYTES
            ));
        }
        Ok(pixels)
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
    fn handle_query(&mut self, mut params: KittyGraphicsParams) -> Result<(), String> {
        let result = (|| {
            if params.image_id.is_some() && params.image_number.is_some() {
                return Err("Kitty commands cannot specify both i and I".to_string());
            }
            if params.image_id.filter(|id| *id != 0).is_none()
                && params.image_number.filter(|number| *number != 0).is_none()
            {
                return Err("Kitty query requires a non-zero i or I identifier".to_string());
            }
            if params.transmission_medium.unwrap_or('d') != 'd' {
                return Err("Only direct Kitty image transmission (t=d) is supported".to_string());
            }

            let encoded = params
                .data
                .take()
                .filter(|encoded| !encoded.is_empty())
                .ok_or("No image data provided for Kitty query")?;
            let data = Self::decode_base64_payload(&encoded, MAX_PENDING_TRANSFER_BYTES)?;
            match params.format.unwrap_or(ImageFormat::Rgba) {
                ImageFormat::Png => {
                    self.decode_png(data)?;
                }
                ImageFormat::Rgb | ImageFormat::Rgba => {
                    let width = params
                        .width
                        .ok_or_else(|| "Missing width for raw image format".to_string())?;
                    let height = params
                        .height
                        .ok_or_else(|| "Missing height for raw image format".to_string())?;
                    let channels = if params.format == Some(ImageFormat::Rgb) {
                        3
                    } else {
                        4
                    };
                    let expected = Self::validate_decoded_size(width, height)?
                        .checked_mul(channels)
                        .ok_or_else(|| "Image dimensions overflow".to_string())?;
                    if data.len() != expected {
                        return Err(format!(
                            "Image data has {} bytes, expected {expected}",
                            data.len()
                        ));
                    }
                }
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

    #[test]
    fn parses_standard_apc_and_preserves_base64_padding() {
        let params = KittyGraphicsState::parse_params("Gf=24,i=1,s=1,v=1;AQI=").unwrap();
        assert_eq!(params.action.as_deref(), Some("t"));
        assert_eq!(params.image_id, Some(1));
        assert_eq!(params.width, Some(1));
        assert_eq!(params.height, Some(1));
        assert_eq!(params.format, Some(ImageFormat::Rgb));
        assert_eq!(params.data.as_deref(), Some("AQI="));
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

        let error = state
            .parse_graphics_payload(&format!("Gf=100,i=8;{}", encode(b"not a PNG")))
            .unwrap_err();
        assert!(error.contains("Failed to load image"));
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
        assert!(state.pending_transfer.is_none());
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
        assert!(error.contains("only m and optional q"));
        assert!(state.pending_transfer.is_none());
        assert_eq!(state.image_count(), 0);
    }

    #[test]
    fn invalid_final_base64_aborts_transfer() {
        let mut state = KittyGraphicsState::new();
        state
            .parse_graphics_payload("Gf=32,i=12,s=1,v=1,m=1;AQID")
            .unwrap();
        assert!(state.parse_graphics_payload("Gm=0;%%%=").is_err());
        assert!(state.pending_transfer.is_none());
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
        assert!(state.pending_transfer.is_none());
        assert!(state.get_image(22).is_none());
        assert!(state.get_image(23).is_none());
    }

    #[test]
    fn final_chunk_cannot_bypass_total_transfer_limit() {
        let error = KittyGraphicsState::decode_base64_payload("AAAA", 0).unwrap_err();
        assert!(error.contains("exceeded"));
    }

    #[test]
    fn raw_formats_require_an_exact_decoded_length() {
        let mut state = KittyGraphicsState::new();
        for rgba in [&[1, 2, 3][..], &[1, 2, 3, 4, 5][..]] {
            let error = state
                .parse_graphics_payload(&format!("Gf=32,i=13,s=1,v=1;{}", encode(rgba)))
                .unwrap_err();
            assert!(error.contains("expected 4"));
        }
        assert!(state.get_image(13).is_none());
    }

    #[test]
    fn oversized_raw_image_is_rejected_before_allocation() {
        let mut state = KittyGraphicsState::new();
        let payload = format!(
            "Gf=32,i=1,s={},v={};AAAA",
            MAX_KITTY_IMAGE_DIMENSION, MAX_KITTY_IMAGE_DIMENSION
        );
        let error = state.parse_graphics_payload(&payload).unwrap_err();
        assert!(error.contains("Decoded image would use"));
        assert_eq!(state.image_count(), 0);
    }

    #[test]
    fn non_direct_transmission_is_rejected() {
        let mut state = KittyGraphicsState::new();
        let error = state
            .parse_graphics_payload("Gf=32,t=f,i=1,s=1,v=1;L3RtcC9pbWFnZQ==")
            .unwrap_err();
        assert!(error.contains("Only direct"));
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
        assert!(error.contains("Only direct"));
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
            assert!(error.contains("Unsupported Kitty placement control"));
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

        let oversized = format!("Gi=49,{}", "x".repeat(MAX_KITTY_CONTROL_BYTES));
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
