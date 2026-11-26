//! Real compositor presentation feedback, scoped to one surface and render serial.
//! A frame callback or successful queue submission is not presentation proof.
use crate::event::WindowEvent;
use crate::platform_impl::wayland::event_loop::sink::EventSink;
use crate::platform_impl::wayland::{state::WinitState, window::WindowRequests, WindowId};
use sctk::globals::GlobalData;
use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{delegate_dispatch, Connection, Dispatch, Proxy, QueueHandle};
use sctk::reexports::protocols::wp::presentation_time::client::wp_presentation::WpPresentation;
use sctk::reexports::protocols::wp::presentation_time::client::wp_presentation_feedback::{
    self, WpPresentationFeedback,
};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, Weak};

// BEGIN native receipt state
#[derive(Debug)]
struct Receipt {
    serial: u64,
    submitted: Option<bool>,
    shown: Option<bool>,
}
impl Receipt {
    fn new(serial: u64) -> Self {
        Self {
            serial,
            submitted: None,
            shown: None,
        }
    }
    fn finish(&mut self, serial: u64, submitted: bool) -> Option<bool> {
        if serial != self.serial || self.submitted.is_some() {
            return None;
        }
        self.submitted = Some(submitted);
        self.ready()
    }
    fn outcome(&mut self, shown: bool) -> Option<bool> {
        if self.shown.is_some() {
            return None;
        }
        self.shown = Some(shown);
        self.ready()
    }
    fn invalidate(&mut self) -> Option<bool> {
        self.submitted = Some(false);
        self.ready()
    }
    fn ready(&self) -> Option<bool> {
        match (self.submitted, self.shown) {
            (_, Some(false)) | (Some(false), Some(true)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        }
    }
}
// END native receipt state

pub struct PresentationState {
    presentation: WpPresentation,
}
impl PresentationState {
    pub fn new(globals: &GlobalList, qh: &QueueHandle<WinitState>) -> Result<Self, BindError> {
        let presentation = globals.bind(qh, 1..=1, GlobalData)?;
        Ok(Self { presentation })
    }
}
struct Flight {
    receipt: Receipt,
    proxy: WpPresentationFeedback,
}
pub struct FeedbackData {
    owner: Weak<WindowFeedback>,
    serial: u64,
}
pub struct WindowFeedback {
    manager: Arc<PresentationState>,
    pending: Mutex<Option<Flight>>,
    window: WindowId,
    requests: Arc<WindowRequests>,
    events: Arc<Mutex<EventSink>>,
}
impl WindowFeedback {
    pub fn new(
        manager: Arc<PresentationState>,
        window: WindowId,
        requests: Arc<WindowRequests>,
        events: Arc<Mutex<EventSink>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            manager,
            pending: Mutex::new(None),
            window,
            requests,
            events,
        })
    }
    pub fn begin(
        self: &Arc<Self>,
        serial: u64,
        surface: &WlSurface,
        qh: &QueueHandle<WinitState>,
    ) -> bool {
        if serial == 0 || self.requests.closed.load(Ordering::Acquire) {
            return false;
        }
        let mut pending = self.pending.lock().unwrap();
        // One outstanding protocol object, including canceled-but-not-yet-
        // destroyed feedback. It cannot attach to a later frame as fresh proof.
        if pending.is_some() {
            return false;
        }
        let proxy = self.manager.presentation.feedback(
            surface,
            qh,
            FeedbackData {
                owner: Arc::downgrade(self),
                serial,
            },
        );
        *pending = Some(Flight {
            receipt: Receipt::new(serial),
            proxy,
        });
        true
    }
    pub fn finish(&self, serial: u64, submitted: bool) {
        let mut pending = self.pending.lock().unwrap();
        let Some(flight) = pending.as_mut() else {
            return;
        };
        if flight.receipt.serial != serial {
            return;
        }
        let ready = flight.receipt.finish(serial, submitted);
        if ready.is_some() {
            pending.take();
        }
        drop(pending);
        if ready == Some(true) {
            self.emit(serial);
        }
        if !submitted || ready == Some(false) {
            self.requests
                .redraw_requested
                .store(true, Ordering::Relaxed);
        }
    }
    pub fn invalidate(&self) {
        let mut pending = self.pending.lock().unwrap();
        if pending
            .as_mut()
            .is_some_and(|flight| flight.receipt.invalidate().is_some())
        {
            pending.take();
        }
        self.requests
            .redraw_requested
            .store(true, Ordering::Relaxed);
    }
    fn receive(&self, proxy: &WpPresentationFeedback, serial: u64, shown: bool) {
        let mut pending = self.pending.lock().unwrap();
        let Some(flight) = pending.as_mut() else {
            return;
        };
        if flight.receipt.serial != serial || flight.proxy.id() != proxy.id() {
            return;
        }
        let ready = flight.receipt.outcome(shown);
        if ready.is_some() {
            pending.take();
        }
        drop(pending);
        if ready == Some(true) {
            self.emit(serial);
        }
        if ready == Some(false) {
            self.requests
                .redraw_requested
                .store(true, Ordering::Relaxed);
        }
    }
    fn emit(&self, serial: u64) {
        if !self.requests.closed.load(Ordering::Acquire) {
            self.events
                .lock()
                .unwrap()
                .push_window_event(WindowEvent::NativeFramePresented { serial }, self.window);
        }
    }
}
impl Dispatch<WpPresentation, GlobalData, WinitState> for PresentationState {
    fn event(
        _: &mut WinitState,
        _: &WpPresentation,
        _: <WpPresentation as Proxy>::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        // clock_id describes a timestamp domain. We do not turn this receipt
        // into a physical scanout or input-latency measurement.
    }
}
impl Dispatch<WpPresentationFeedback, FeedbackData, WinitState> for PresentationState {
    fn event(
        _: &mut WinitState,
        feedback: &WpPresentationFeedback,
        event: <WpPresentationFeedback as Proxy>::Event,
        data: &FeedbackData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        let shown = match event {
            wp_presentation_feedback::Event::Presented { .. } => true,
            wp_presentation_feedback::Event::Discarded => false,
            _ => return,
        };
        if let Some(owner) = data.owner.upgrade() {
            owner.receive(feedback, data.serial, shown);
        }
    }
}
delegate_dispatch!(WinitState: [WpPresentation: GlobalData] => PresentationState);
delegate_dispatch!(WinitState: [WpPresentationFeedback: FeedbackData] => PresentationState);

#[cfg(test)]
mod tests {
    use super::Receipt;
    #[test]
    fn only_successful_submission_and_real_presented_qualify() {
        let mut r = Receipt::new(7);
        assert_eq!(r.finish(7, true), None);
        assert_eq!(r.outcome(true), Some(true));
    }
    #[test]
    fn early_presented_waits_for_exact_submission() {
        let mut r = Receipt::new(7);
        assert_eq!(r.outcome(true), None);
        assert_eq!(r.finish(8, true), None);
        assert_eq!(r.finish(7, true), Some(true));
    }
    #[test]
    fn canceled_frame_cannot_borrow_later_commit_feedback() {
        let mut r = Receipt::new(7);
        assert_eq!(r.finish(7, false), None);
        assert_eq!(r.outcome(true), Some(false));
    }
    #[test]
    fn discarded_and_replayed_events_never_qualify() {
        let mut r = Receipt::new(7);
        assert_eq!(r.outcome(false), Some(false));
        assert_eq!(r.outcome(true), None);
        assert_eq!(r.finish(7, true), Some(false));
    }
    #[test]
    fn invalidated_submission_never_acks_a_later_commit() {
        let mut r = Receipt::new(7);
        assert_eq!(r.finish(7, true), None);
        assert_eq!(r.invalidate(), None);
        assert_eq!(r.outcome(true), Some(false));
    }
    #[test]
    fn duplicate_finish_cannot_upgrade_failed_submission() {
        let mut r = Receipt::new(7);
        assert_eq!(r.finish(7, false), None);
        assert_eq!(r.finish(7, true), None);
        assert_eq!(r.outcome(true), Some(false));
    }
}
