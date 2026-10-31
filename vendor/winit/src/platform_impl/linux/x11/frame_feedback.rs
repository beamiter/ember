//! Producer-confirmed frames followed by standard, owner-bound X11 scene feedback.
//! Callers MUST only finish with `published=true` after an exact native present
//! ID has completed. A queue submit, generic damage or elapsed time is insufficient.
use std::collections::BTreeMap;

use x11rb::{
    connection::Connection,
    protocol::{
        sync::{self, ConnectionExt as _},
        xproto::{self, ConnectionExt as _},
    },
    wrapper::ConnectionExt as _,
};

use super::{ffi, XConnection};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const LIMIT: usize = 64;

#[derive(Default)]
struct Receipt {
    pass: u64,
    drawn: Option<u64>,
    offset: Option<i32>,
}

#[derive(Default)]
struct Receipts {
    pending: BTreeMap<u64, Receipt>,
    accepted: u64,
}
impl Receipts {
    fn waiting(&self) -> bool {
        !self.pending.is_empty()
    }
    fn insert(&mut self, counter: u64, pass: u64) {
        if self.pending.len() == LIMIT {
            self.pending.pop_first();
        }
        self.pending.insert(
            counter,
            Receipt {
                pass,
                ..Default::default()
            },
        );
    }
    fn receive(&mut self, counter: u64, drawn: Option<u64>, offset: Option<i32>) -> Option<u64> {
        if counter <= self.accepted || counter & 1 != 0 {
            return None;
        }
        let receipt = self.pending.get_mut(&counter)?;
        // First receipt wins, including zero/unknown. A replay cannot upgrade it.
        if receipt.drawn.is_none() {
            receipt.drawn = drawn;
        }
        if receipt.offset.is_none() {
            receipt.offset = offset;
        }
        let time = receipt.drawn?;
        let offset = receipt.offset?;
        let pass = receipt.pass;
        let known = time != 0
            && offset != 0
            && time
                .checked_add_signed(i64::from(offset))
                .is_some_and(|value| value != 0);
        // A complete unknown pair releases pacing but never produces proof.
        // Replays cannot upgrade it, and a fresh frame may retry.
        self.accepted = counter;
        self.pending.retain(|value, _| *value > counter);
        known.then_some(pass)
    }
}

struct Flight {
    pass: u64,
    counter: u64,
    size: [u32; 2],
    basic: Option<u64>,
}

pub(super) struct Feedback {
    root: u32,
    marker: u32,
    protocols: u32,
    request: u32,
    counter_atom: u32,
    drawn: u32,
    timings: u32,
    selection: u32,
    owner: u32,
    counters: [u32; 2],
    value: u64,
    flight: Option<Flight>,
    receipts: Receipts,
    basic: Option<u64>,
    extended_floor: u64,
    configure_pending: bool,
    deferred: bool,
    redraw: bool,
}

fn int64(value: u64) -> sync::Int64 {
    sync::Int64 {
        hi: (value >> 32) as i32,
        lo: value as u32,
    }
}
fn words(data: &[u32; 5], at: usize) -> u64 {
    u64::from(data[at]) | (u64::from(data[at + 1]) << 32)
}

impl Feedback {
    pub(super) fn new(x: &XConnection, window: u32, root: u32, screen: i32) -> Result<Self> {
        let conn = x.xcb_connection();
        conn.sync_initialize(3, 1)?.reply()?;
        let atom =
            |name: &[u8]| -> Result<u32> { Ok(conn.intern_atom(false, name)?.reply()?.atom) };
        let protocols = atom(b"WM_PROTOCOLS")?;
        let request = atom(b"_NET_WM_SYNC_REQUEST")?;
        let counter_atom = atom(b"_NET_WM_SYNC_REQUEST_COUNTER")?;
        let drawn = atom(b"_NET_WM_FRAME_DRAWN")?;
        let timings = atom(b"_NET_WM_FRAME_TIMINGS")?;
        let supported = atom(b"_NET_SUPPORTED")?;
        let marker = atom(b"_JWM_FRAME_SYNC_OWNER")?;
        let selection = atom(format!("_NET_WM_CM_S{screen}").as_bytes())?;
        // Subscribe before reading ownership: a loss/replacement during setup
        // must be queued for invalidation, not missed between query and select.
        x.watch_frame_authority(window, root, selection)?;
        let owner = conn.get_selection_owner(selection)?.reply()?.owner;
        let authority = conn
            .get_property(false, root, marker, xproto::AtomEnum::WINDOW, 0, 2)?
            .reply()?;
        let authority: Vec<_> = authority.value32().into_iter().flatten().collect();
        let hints = conn
            .get_property(false, root, supported, xproto::AtomEnum::ATOM, 0, 4096)?
            .reply()?;
        let hints: Vec<_> = hints.value32().into_iter().flatten().collect();
        if owner == 0
            || authority != [owner]
            || !hints.contains(&drawn)
            || !hints.contains(&timings)
        {
            return Err("extended compositor frame feedback unavailable".into());
        }
        let counters = [conn.generate_id()?, conn.generate_id()?];
        conn.sync_create_counter(counters[0], int64(0))?.check()?;
        if let Err(error) = conn.sync_create_counter(counters[1], int64(0))?.check() {
            conn.sync_destroy_counter(counters[0])?.ignore_error();
            return Err(error.into());
        }
        let mut feedback = Self {
            root,
            marker,
            protocols,
            request,
            counter_atom,
            drawn,
            timings,
            selection,
            owner,
            counters,
            value: 0,
            flight: None,
            receipts: Receipts::default(),
            basic: None,
            extended_floor: 0,
            configure_pending: false,
            deferred: false,
            redraw: false,
        };
        let advertise = (|| -> Result<()> {
            let old = conn
                .get_property(false, window, protocols, xproto::AtomEnum::ATOM, 0, 128)?
                .reply()?;
            if old.bytes_after != 0 {
                return Err("oversized WM_PROTOCOLS".into());
            }
            let mut values: Vec<_> = old.value32().into_iter().flatten().collect();
            if !values.contains(&request) {
                values.push(request);
            }
            conn.change_property32(
                xproto::PropMode::REPLACE,
                window,
                protocols,
                xproto::AtomEnum::ATOM,
                &values,
            )?
            .check()?;
            conn.change_property32(
                xproto::PropMode::REPLACE,
                window,
                counter_atom,
                xproto::AtomEnum::CARDINAL,
                &counters,
            )?
            .check()?;
            Ok(())
        })();
        if let Err(error) = advertise {
            feedback.destroy(x, window);
            return Err(error);
        }
        Ok(feedback)
    }

    pub(super) fn needs_frame(&self) -> bool {
        self.configure_pending
    }

    pub(super) fn invalidate(&mut self) {
        self.flight = None;
        self.receipts = Receipts::default();
        self.redraw |= self.deferred;
        self.deferred = false;
    }

    fn invalidate_generation(&mut self) {
        self.invalidate();
        self.basic = None;
        self.extended_floor = 0;
        self.configure_pending = false;
        // Ownership/mapping transitions are real activity. A single wake resumes
        // queued UI work even when no later input, resize or frame message comes.
        self.redraw = true;
    }

    pub(super) fn selection_changed(&mut self, selection: u32) -> bool {
        if selection != self.selection {
            return false;
        }
        // Invalidate even for owner-XID ABA; event payloads never establish proof.
        self.invalidate_generation();
        true
    }

    pub(super) fn marker_changed(&mut self, root: u32, property: u32) -> bool {
        if root != self.root || property != self.marker {
            return false;
        }
        self.invalidate_generation();
        true
    }

    pub(super) fn mapping_changed(&mut self, mapped: bool) {
        // A remapped backing is a new generation even at the same dimensions.
        // Discard both partial receipts and a native present still in flight;
        // the next mapped output must issue a fresh monotonic counter.
        self.invalidate_generation();
        self.redraw = mapped;
    }

    pub(super) fn take_redraw(&mut self) -> bool {
        std::mem::take(&mut self.redraw)
    }

    pub(super) fn defer(&mut self, x: &XConnection) -> Result<bool> {
        if !self.receipts.waiting() {
            return Ok(false);
        }
        if self.current_owner(x)? != Some(self.owner) {
            self.invalidate_generation();
            return Ok(false);
        }
        self.deferred = true;
        Ok(true)
    }

    fn current_owner(&self, x: &XConnection) -> Result<Option<u32>> {
        let conn = x.xcb_connection();
        let owner = conn.get_selection_owner(self.selection)?.reply()?.owner;
        let authority = conn
            .get_property(
                false,
                self.root,
                self.marker,
                xproto::AtomEnum::WINDOW,
                0,
                2,
            )?
            .reply()?;
        let authority: Vec<_> = authority.value32().into_iter().flatten().collect();
        Ok((owner != 0 && authority == [owner]).then_some(owner))
    }

    pub(super) fn begin(
        &mut self,
        x: &XConnection,
        window: u32,
        pass: u64,
        size: [u32; 2],
    ) -> Result<bool> {
        if self.flight.is_some() || self.receipts.waiting() || size.contains(&0) {
            return Ok(false);
        }
        let conn = x.xcb_connection();
        let Some(owner) = self.current_owner(x)? else {
            self.invalidate_generation();
            return Ok(false);
        };
        if owner != self.owner {
            // Old-owner messages never survive a compositor transition.
            self.invalidate_generation();
            self.owner = owner;
        }
        let geometry = conn.get_geometry(window)?.reply()?;
        if size != [u32::from(geometry.width), u32::from(geometry.height)]
            || conn.get_window_attributes(window)?.reply()?.map_state != xproto::MapState::VIEWABLE
        {
            return Ok(false);
        }
        let floor = self.value.max(self.extended_floor);
        let odd = floor
            .checked_add(if floor & 1 == 0 { 1 } else { 2 })
            .filter(|v| *v < i64::MAX as u64)
            .ok_or("frame counter exhausted")?;
        // The round trip orders the odd counter before native present starts,
        // including drivers with a separate WSI connection/thread.
        conn.sync_set_counter(self.counters[1], int64(odd))?
            .check()?;
        self.value = odd;
        self.flight = Some(Flight {
            pass,
            counter: odd + 1,
            size,
            basic: self.basic,
        });
        Ok(true)
    }

    pub(super) fn finish(
        &mut self,
        x: &XConnection,
        window: u32,
        pass: u64,
        published: bool,
    ) -> Result<()> {
        let Some(flight) = self.flight.take() else {
            return Ok(());
        };
        if flight.pass != pass || !published {
            return Ok(());
        }
        let conn = x.xcb_connection();
        let owner = self.current_owner(x)?;
        let geometry = conn.get_geometry(window)?.reply()?;
        if owner != Some(self.owner)
            || flight.size != [u32::from(geometry.width), u32::from(geometry.height)]
            || conn.get_window_attributes(window)?.reply()?.map_state != xproto::MapState::VIEWABLE
        {
            return Ok(());
        }
        conn.sync_set_counter(self.counters[1], int64(flight.counter))?
            .check()?;
        if let Some(value) = flight.basic {
            conn.sync_set_counter(self.counters[0], int64(value))?
                .check()?;
        }
        if self.basic == flight.basic {
            self.basic = None;
        }
        self.configure_pending = false;
        self.value = flight.counter;
        self.receipts.insert(flight.counter, pass);
        tracing::debug!(
            target: "eva_native_publication", window,
            counter = flight.counter,
            pass,
            "producer frame published"
        );
        Ok(())
    }

    pub(super) fn message(
        &mut self,
        x: &XConnection,
        event: &ffi::XClientMessageEvent,
    ) -> Result<(bool, Option<u64>)> {
        let kind = event.message_type as u32;
        if kind != self.drawn && kind != self.timings && kind != self.protocols {
            return Ok((false, None));
        }
        let data: [u32; 5] = std::array::from_fn(|i| event.data.get_long(i) as u32);
        if kind == self.protocols {
            if data[0] != self.request {
                return Ok((false, None));
            }
            if self.current_owner(x)? != Some(self.owner) {
                self.invalidate_generation();
                let Some(owner) = self.current_owner(x)? else {
                    return Ok((true, None));
                };
                self.owner = owner;
            }
            if event.format == 32 {
                let value = words(&data, 2);
                if value <= i64::MAX as u64 {
                    self.configure_pending = true;
                    if data[4] != 0 {
                        self.extended_floor = self.extended_floor.max(value);
                    } else {
                        self.basic = Some(value);
                    }
                }
            }
            return Ok((true, None));
        }
        if event.format != 32 || self.current_owner(x)? != Some(self.owner) {
            self.invalidate_generation();
            return Ok((true, None));
        }
        let counter = words(&data, 0);
        let waiting = self.receipts.waiting();
        let serial = self.receipts.receive(
            counter,
            (kind == self.drawn).then(|| words(&data, 2)),
            (kind == self.timings).then_some(data[2] as i32),
        );
        if waiting && !self.receipts.waiting() {
            self.redraw |= self.deferred || serial.is_none();
            self.deferred = false;
        }
        if let Some(pass) = serial {
            tracing::debug!(target: "eva_native_publication", counter, pass, "compositor scene receipt accepted");
        }
        Ok((true, serial))
    }

    pub(super) fn destroy(&mut self, x: &XConnection, window: u32) {
        let conn = x.xcb_connection();
        for counter in self.counters {
            if let Ok(cookie) = conn.sync_destroy_counter(counter) {
                cookie.ignore_error();
            }
        }
        if let Ok(cookie) = conn.delete_property(window, self.counter_atom) {
            cookie.ignore_error();
        }
        let _ = conn.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pending_feedback() -> Feedback {
        let mut receipts = Receipts::default();
        receipts.insert(2, 42);
        Feedback {
            root: 1,
            marker: 2,
            protocols: 3,
            request: 4,
            counter_atom: 5,
            drawn: 6,
            timings: 7,
            selection: 8,
            owner: 9,
            counters: [10, 11],
            value: 2,
            flight: None,
            receipts,
            basic: Some(20),
            extended_floor: 30,
            configure_pending: true,
            deferred: true,
            redraw: false,
        }
    }
    #[test]
    fn authority_loss_cancels_old_receipt_and_requests_exactly_one_draw() {
        let mut feedback = pending_feedback();
        assert!(!feedback.selection_changed(99));
        assert!(feedback.receipts.waiting());
        assert!(!feedback.take_redraw());
        assert!(feedback.selection_changed(8));
        assert!(!feedback.receipts.waiting());
        assert_eq!(feedback.receipts.receive(2, Some(100), Some(1)), None);
        assert_eq!(
            feedback.value, 2,
            "counter monotonicity survives invalidation"
        );
        assert_eq!(feedback.basic, None);
        assert_eq!(feedback.extended_floor, 0);
        assert!(!feedback.configure_pending);
        assert!(feedback.take_redraw());
        assert!(
            !feedback.take_redraw(),
            "authority notification must not create an idle repaint loop"
        );
    }
    #[test]
    fn marker_ready_wakes_without_pending_input_and_owner_aba_never_reuses_old_proof() {
        let mut feedback = pending_feedback();
        assert!(!feedback.marker_changed(99, 2));
        assert!(!feedback.marker_changed(1, 99));
        assert!(feedback.marker_changed(1, 2));
        assert!(feedback.take_redraw());
        // A genuine owner event must invalidate even if the numerical owner XID
        // is unchanged/reused. Only a new issued frame may later produce proof.
        feedback.receipts.insert(4, 43);
        assert!(feedback.selection_changed(8));
        assert_eq!(feedback.owner, 9);
        assert_eq!(feedback.receipts.receive(4, Some(100), Some(1)), None);
        assert!(feedback.take_redraw());
        assert!(!feedback.take_redraw());
    }
    #[test]
    fn same_size_unmap_remap_cancels_old_flight_and_receipts_without_proof() {
        let mut feedback = pending_feedback();
        feedback.flight = Some(Flight {
            pass: 43,
            counter: 4,
            size: [1344, 1100],
            basic: Some(20),
        });
        feedback.mapping_changed(false);
        assert!(feedback.flight.is_none());
        assert!(!feedback.receipts.waiting());
        assert_eq!(feedback.receipts.receive(2, Some(100), Some(1)), None);
        assert_eq!(feedback.value, 2);
        assert_eq!(feedback.basic, None);
        assert_eq!(feedback.extended_floor, 0);
        assert!(!feedback.configure_pending);
        assert!(
            !feedback.take_redraw(),
            "wait for real map before redrawing"
        );
        feedback.mapping_changed(true);
        assert!(feedback.take_redraw());
        assert!(!feedback.take_redraw());
        assert_eq!(feedback.receipts.receive(2, Some(100), Some(1)), None);
        feedback.receipts.insert(4, 44);
        assert_eq!(feedback.receipts.receive(4, Some(200), Some(1)), Some(44));
    }
    #[test]
    fn exact_issued_counter_preserves_original_pass_and_rejects_replay() {
        let mut receipts = Receipts::default();
        receipts.insert(2, 99);
        assert!(receipts.waiting());
        assert_eq!(receipts.receive(4, Some(10), Some(1)), None);
        assert_eq!(receipts.receive(2, None, Some(1)), None);
        assert!(receipts.waiting());
        assert_eq!(receipts.receive(2, Some(10), None), Some(99));
        assert!(!receipts.waiting());
        assert_eq!(receipts.receive(2, Some(10), Some(1)), None);
    }
    #[test]
    fn unknown_cannot_be_upgraded_and_new_known_frame_can_progress() {
        let mut receipts = Receipts::default();
        receipts.insert(2, 99);
        assert_eq!(receipts.receive(2, Some(10), Some(0)), None);
        assert!(
            !receipts.waiting(),
            "unknown completion must release pacing without success"
        );
        assert_eq!(receipts.receive(2, Some(10), Some(1)), None);
        receipts.insert(4, 100);
        assert_eq!(receipts.receive(4, Some(10), Some(-1)), Some(100));
        assert!(receipts.pending.is_empty());
    }
    #[test]
    fn invalid_timestamp_and_overflow_fail_closed_and_storage_is_bounded() {
        let mut receipts = Receipts::default();
        receipts.insert(2, 1);
        assert_eq!(receipts.receive(2, Some(u64::MAX), Some(1)), None);
        receipts.insert(4, 2);
        assert_eq!(receipts.receive(4, Some(0), Some(1)), None);
        for n in 3..1000 {
            receipts.insert(n * 2, n);
        }
        assert_eq!(receipts.pending.len(), LIMIT);
    }
}
