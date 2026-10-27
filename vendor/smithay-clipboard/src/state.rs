use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{Error, ErrorKind, Read, Result, Write};
use std::mem;
use std::os::unix::io::{AsRawFd, RawFd};
use std::rc::Rc;
use std::sync::mpsc::Sender;

use sctk::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use sctk::data_device_manager::data_offer::{DataOfferError, DataOfferHandler, DragOffer};
use sctk::data_device_manager::data_source::{CopyPasteSource, DataSourceHandler};
use sctk::data_device_manager::{DataDeviceManagerState, WritePipe};
use sctk::primary_selection::PrimarySelectionManagerState;
use sctk::primary_selection::device::{PrimarySelectionDevice, PrimarySelectionDeviceHandler};
use sctk::primary_selection::selection::{PrimarySelectionSource, PrimarySelectionSourceHandler};
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::seat::pointer::{PointerData, PointerEvent, PointerEventKind, PointerHandler};
use sctk::seat::{Capability, SeatHandler, SeatState};
use sctk::{
    delegate_data_device, delegate_pointer, delegate_primary_selection, delegate_registry,
    delegate_seat, registry_handlers,
};

use sctk::reexports::calloop::{LoopHandle, PostAction};
use sctk::reexports::client::globals::GlobalList;
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
use sctk::reexports::client::protocol::wl_data_source::WlDataSource;
use sctk::reexports::client::protocol::wl_keyboard::WlKeyboard;
use sctk::reexports::client::protocol::wl_pointer::WlPointer;
use sctk::reexports::client::protocol::wl_seat::WlSeat;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, Dispatch, Proxy, QueueHandle};
use sctk::reexports::protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
    zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
};
use wayland_backend::client::ObjectId;

use crate::mime::{ALLOWED_MIME_TYPES, MimeType, normalize_to_lf};

pub struct State {
    pub primary_selection_manager_state: Option<PrimarySelectionManagerState>,
    pub data_device_manager_state: Option<DataDeviceManagerState>,
    pub reply_tx: Sender<Result<String>>,
    pub exit: bool,

    registry_state: RegistryState,
    seat_state: SeatState,

    seats: HashMap<ObjectId, ClipboardSeatState>,
    /// The latest seat which got an event.
    latest_seat: Option<ObjectId>,

    loop_handle: LoopHandle<'static, Self>,
    queue_handle: QueueHandle<Self>,

    primary_sources: Vec<PrimarySelectionSource>,
    primary_selection_content: Rc<[u8]>,

    data_sources: Vec<CopyPasteSource>,
    data_selection_content: Rc<[u8]>,
}

impl State {
    #[must_use]
    pub fn new(
        globals: &GlobalList,
        queue_handle: &QueueHandle<Self>,
        loop_handle: LoopHandle<'static, Self>,
        reply_tx: Sender<Result<String>>,
    ) -> Option<Self> {
        // NOTE: while it's mutable, it's not part of the hash compute.
        #[allow(clippy::mutable_key_type)]
        let mut seats = HashMap::new();

        let data_device_manager_state = DataDeviceManagerState::bind(globals, queue_handle).ok();
        let primary_selection_manager_state =
            PrimarySelectionManagerState::bind(globals, queue_handle).ok();

        // When both globals are not available nothing could be done.
        if data_device_manager_state.is_none() && primary_selection_manager_state.is_none() {
            return None;
        }

        let seat_state = SeatState::new(globals, queue_handle);
        for seat in seat_state.seats() {
            seats.insert(seat.id(), Default::default());
        }

        Some(Self {
            registry_state: RegistryState::new(globals),
            primary_selection_content: Rc::from([]),
            data_selection_content: Rc::from([]),
            queue_handle: queue_handle.clone(),
            primary_selection_manager_state,
            primary_sources: Vec::new(),
            data_device_manager_state,
            data_sources: Vec::new(),
            latest_seat: None,
            loop_handle,
            exit: false,
            seat_state,
            reply_tx,
            seats,
        })
    }

    /// Store selection for the given target.
    ///
    /// Selection source is only created when `Some(())` is returned.
    pub fn store_selection(&mut self, ty: SelectionTarget, contents: String) -> Option<()> {
        let latest = self.latest_seat.as_ref()?;
        let seat = self.seats.get_mut(latest)?;

        if !seat.has_focus {
            return None;
        }

        let contents = Rc::from(contents.into_bytes());

        match ty {
            SelectionTarget::Clipboard => {
                let mgr = self.data_device_manager_state.as_ref()?;
                self.data_selection_content = contents;
                let source =
                    mgr.create_copy_paste_source(&self.queue_handle, ALLOWED_MIME_TYPES.iter());
                source.set_selection(seat.data_device.as_ref().unwrap(), seat.latest_serial);
                self.data_sources.push(source);
            },
            SelectionTarget::Primary => {
                let mgr = self.primary_selection_manager_state.as_ref()?;
                self.primary_selection_content = contents;
                let source =
                    mgr.create_selection_source(&self.queue_handle, ALLOWED_MIME_TYPES.iter());
                source.set_selection(seat.primary_device.as_ref().unwrap(), seat.latest_serial);
                self.primary_sources.push(source);
            },
        }

        Some(())
    }

    /// Load selection for the given target.
    pub fn load_selection(&mut self, ty: SelectionTarget) -> Result<()> {
        let latest = self
            .latest_seat
            .as_ref()
            .ok_or_else(|| Error::other("no events received on any seat"))?;
        let seat = self.seats.get_mut(latest).ok_or_else(|| Error::other("active seat lost"))?;

        if !seat.has_focus {
            return Err(Error::other("client doesn't have focus"));
        }

        let (read_pipe, mime_type) = match ty {
            SelectionTarget::Clipboard => {
                let selection = seat
                    .data_device
                    .as_ref()
                    .and_then(|data| data.data().selection_offer())
                    .ok_or_else(|| Error::other("selection is empty"))?;

                let mime_type =
                    selection.with_mime_types(MimeType::find_allowed).ok_or_else(|| {
                        Error::new(ErrorKind::NotFound, "supported mime-type is not found")
                    })?;

                (
                    selection.receive(mime_type.to_string()).map_err(|err| match err {
                        DataOfferError::InvalidReceive => Error::other("offer is not ready yet"),
                        DataOfferError::Io(err) => err,
                    })?,
                    mime_type,
                )
            },
            SelectionTarget::Primary => {
                let selection = seat
                    .primary_device
                    .as_ref()
                    .and_then(|data| data.data().selection_offer())
                    .ok_or_else(|| Error::other("selection is empty"))?;

                let mime_type =
                    selection.with_mime_types(MimeType::find_allowed).ok_or_else(|| {
                        Error::new(ErrorKind::NotFound, "supported mime-type is not found")
                    })?;

                (selection.receive(mime_type.to_string())?, mime_type)
            },
        };

        // Mark FD as non-blocking so we won't block ourselves.
        set_non_blocking(read_pipe.as_raw_fd())?;

        let mut reader_buffer = [0; 4096];
        let mut content = Vec::new();
        let _ = self.loop_handle.insert_source(read_pipe, move |_, file, state| {
            let file = unsafe { file.get_mut() };
            loop {
                match file.read(&mut reader_buffer) {
                    Ok(0) => {
                        let utf8 = String::from_utf8_lossy(&content);
                        let content = match utf8 {
                            Cow::Borrowed(_) => {
                                // Don't clone the read data.
                                let mut to_send = Vec::new();
                                mem::swap(&mut content, &mut to_send);
                                String::from_utf8(to_send).unwrap()
                            },
                            Cow::Owned(content) => content,
                        };

                        // Post-process the content according to mime type.
                        let content = match mime_type {
                            MimeType::TextPlainUtf8 | MimeType::TextPlain => {
                                normalize_to_lf(content)
                            },
                            MimeType::Utf8String => content,
                        };

                        let _ = state.reply_tx.send(Ok(content));
                        break PostAction::Remove;
                    },
                    Ok(n) => content.extend_from_slice(&reader_buffer[..n]),
                    Err(err) if err.kind() == ErrorKind::WouldBlock => break PostAction::Continue,
                    Err(err) => {
                        let _ = state.reply_tx.send(Err(err));
                        break PostAction::Remove;
                    },
                };
            }
        });

        Ok(())
    }

    fn send_request(&mut self, ty: SelectionTarget, write_pipe: WritePipe, mime: String) {
        // We can only send strings, so don't do anything with the mime-type.
        if MimeType::find_allowed(&[mime]).is_none() {
            return;
        }

        // Mark FD as non-blocking so we won't block ourselves.
        if set_non_blocking(write_pipe.as_raw_fd()).is_err() {
            return;
        }

        // Don't access the content on the state directly, since it could change during
        // the send.
        let contents = match ty {
            SelectionTarget::Clipboard => self.data_selection_content.clone(),
            SelectionTarget::Primary => self.primary_selection_content.clone(),
        };

        let mut written = 0;
        let _ = self.loop_handle.insert_source(write_pipe, move |_, file, _| {
            let file = unsafe { file.get_mut() };
            loop {
                match file.write(&contents[written..]) {
                    Ok(n) if written + n == contents.len() => {
                        written += n;
                        break PostAction::Remove;
                    },
                    Ok(n) => written += n,
                    Err(err) if err.kind() == ErrorKind::WouldBlock => break PostAction::Continue,
                    Err(_) => break PostAction::Remove,
                }
            }
        });
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
        self.seats.insert(seat.id(), Default::default());
    }

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        let seat_state = self.seats.get_mut(&seat.id()).unwrap();

        match capability {
            Capability::Keyboard => {
                seat_state.keyboard = Some(seat.get_keyboard(qh, seat.id()));

                // Initialize selection devices on keyboard availability; once created,
                // retain them until the seat is removed, including keyboard hotplug.

                if seat_state.data_device.is_none() && self.data_device_manager_state.is_some() {
                    seat_state.data_device = self
                        .data_device_manager_state
                        .as_ref()
                        .map(|mgr| mgr.get_data_device(qh, &seat));
                }

                if seat_state.primary_device.is_none()
                    && self.primary_selection_manager_state.is_some()
                {
                    seat_state.primary_device = self
                        .primary_selection_manager_state
                        .as_ref()
                        .map(|mgr| mgr.get_selection_device(qh, &seat));
                }
            },
            Capability::Pointer => {
                seat_state.pointer = self.seat_state.get_pointer(qh, &seat).ok();
            },
            _ => (),
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        let seat_state = self.seats.get_mut(&seat.id()).unwrap();
        match capability {
            Capability::Keyboard => {
                // Selection devices belong to the seat, not its keyboard capability.
                // Keep them alive across virtual-keyboard hotplug: an in-flight
                // data_offer still needs its object id registered on this display.
                // remove_seat drops both devices at the real seat lifetime boundary.

                if let Some(keyboard) = seat_state.keyboard.take() {
                    if keyboard.version() >= 3 {
                        keyboard.release()
                    }
                }
            },
            Capability::Pointer => {
                if let Some(pointer) = seat_state.pointer.take() {
                    if pointer.version() >= 3 {
                        pointer.release()
                    }
                }
            },
            _ => (),
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
        self.seats.remove(&seat.id());
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        let seat = pointer.data::<PointerData>().unwrap().seat();
        let seat_id = seat.id();
        let seat_state = match self.seats.get_mut(&seat_id) {
            Some(seat_state) => seat_state,
            None => return,
        };

        let mut updated_serial = false;
        for event in events {
            match event.kind {
                PointerEventKind::Press { serial, .. }
                | PointerEventKind::Release { serial, .. } => {
                    updated_serial = true;
                    seat_state.latest_serial = serial;
                },
                _ => (),
            }
        }

        // Only update the seat we're using when the serial got updated.
        if updated_serial {
            self.latest_seat = Some(seat_id);
        }
    }
}

impl DataDeviceHandler for State {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataDevice,
        _: f64,
        _: f64,
        _: &WlSurface,
    ) {
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    // The selection is finished and ready to be used.
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataSourceHandler for State {
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        mime: String,
        write_pipe: WritePipe,
    ) {
        self.send_request(SelectionTarget::Clipboard, write_pipe, mime)
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, deleted: &WlDataSource) {
        self.data_sources.retain(|source| source.inner() != deleted)
    }

    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
}

impl DataOfferHandler for State {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }

    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

impl ProvidesRegistryState for State {
    registry_handlers![SeatState];

    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
}

impl PrimarySelectionDeviceHandler for State {
    fn selection(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpPrimarySelectionDeviceV1,
    ) {
    }
}

impl PrimarySelectionSourceHandler for State {
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpPrimarySelectionSourceV1,
        mime: String,
        write_pipe: WritePipe,
    ) {
        self.send_request(SelectionTarget::Primary, write_pipe, mime);
    }

    fn cancelled(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        deleted: &ZwpPrimarySelectionSourceV1,
    ) {
        self.primary_sources.retain(|source| source.inner() != deleted)
    }
}

impl Dispatch<WlKeyboard, ObjectId, State> for State {
    fn event(
        state: &mut State,
        _: &WlKeyboard,
        event: <WlKeyboard as sctk::reexports::client::Proxy>::Event,
        data: &ObjectId,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        use sctk::reexports::client::protocol::wl_keyboard::Event as WlKeyboardEvent;
        let seat_state = match state.seats.get_mut(data) {
            Some(seat_state) => seat_state,
            None => return,
        };
        match event {
            WlKeyboardEvent::Key { serial, .. } | WlKeyboardEvent::Modifiers { serial, .. } => {
                seat_state.latest_serial = serial;
                state.latest_seat = Some(data.clone());
            },
            // NOTE both selections rely on keyboard focus.
            WlKeyboardEvent::Enter { serial, .. } => {
                seat_state.latest_serial = serial;
                seat_state.has_focus = true;
            },
            WlKeyboardEvent::Leave { .. } => {
                seat_state.latest_serial = 0;
                seat_state.has_focus = false;
            },
            _ => (),
        }
    }
}

delegate_seat!(State);
delegate_pointer!(State);
delegate_data_device!(State);
delegate_primary_selection!(State);
delegate_registry!(State);

#[derive(Debug, Clone, Copy)]
pub enum SelectionTarget {
    /// The target is clipboard selection.
    Clipboard,
    /// The target is primary selection.
    Primary,
}

#[derive(Debug, Default)]
struct ClipboardSeatState {
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    data_device: Option<DataDevice>,
    primary_device: Option<PrimarySelectionDevice>,
    has_focus: bool,

    /// The latest serial used to set the selection content.
    latest_serial: u32,
}

impl Drop for ClipboardSeatState {
    fn drop(&mut self) {
        if let Some(keyboard) = self.keyboard.take() {
            if keyboard.version() >= 3 {
                keyboard.release();
            }
        }

        if let Some(pointer) = self.pointer.take() {
            if pointer.version() >= 3 {
                pointer.release();
            }
        }
    }
}

fn set_non_blocking(raw_fd: RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(raw_fd, libc::F_GETFL) };

    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let result = unsafe { libc::fcntl(raw_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(test)]
mod keyboard_capability_tests {
    use super::*;
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use sctk::reexports::calloop::EventLoop;
    use sctk::reexports::client::globals::registry_queue_init;

    const KEYBOARD: u32 = 2;
    const OFFER_MIME: &str = "text/plain;charset=utf-8";

    enum Arg<'a> {
        Uint(u32),
        Str(&'a str),
    }

    /// Minimal Wayland wire codec for a scripted compositor endpoint.
    struct Wire(UnixStream);

    impl Wire {
        fn request(&mut self) -> Result<Option<(u32, u16, Vec<u8>)>> {
            let mut header = [0; 8];
            match self.0.read_exact(&mut header) {
                Ok(()) => {},
                Err(error) if error.kind() == ErrorKind::UnexpectedEof => return Ok(None),
                Err(error) => return Err(error),
            }
            let object = u32::from_ne_bytes(header[..4].try_into().unwrap());
            let word = u32::from_ne_bytes(header[4..].try_into().unwrap());
            let mut body = vec![0; (word >> 16) as usize - header.len()];
            self.0.read_exact(&mut body)?;
            Ok(Some((object, word as u16, body)))
        }

        fn event(&mut self, object: u32, opcode: u16, args: &[Arg<'_>]) -> Result<()> {
            let mut body = Vec::new();
            for arg in args {
                match arg {
                    Arg::Uint(value) => body.extend_from_slice(&value.to_ne_bytes()),
                    Arg::Str(text) => {
                        body.extend_from_slice(&(text.len() as u32 + 1).to_ne_bytes());
                        body.extend_from_slice(text.as_bytes());
                        body.push(0);
                        body.resize(body.len().next_multiple_of(4), 0);
                    },
                }
            }
            let size = (body.len() + 8) as u32;
            let mut message = Vec::with_capacity(size as usize);
            message.extend_from_slice(&object.to_ne_bytes());
            message.extend_from_slice(&((size << 16) | u32::from(opcode)).to_ne_bytes());
            message.extend_from_slice(&body);
            self.0.write_all(&message)
        }
    }

    fn word(body: &[u8], index: usize) -> u32 {
        u32::from_ne_bytes(body[index * 4..index * 4 + 4].try_into().unwrap())
    }

    #[derive(PartialEq)]
    enum Phase {
        WithKeyboard,
        KeyboardRemoved,
        KeyboardRestored,
        Finished,
    }

    /// Compositor script: the seat's keyboard is unplugged and plugged back,
    /// as a virtual keyboard (wtype, remote desktop, KVM) does. A selection
    /// offer for the seat's data device is already in flight when the client
    /// handles the removal: it is written after the client's requests for the
    /// capability change but before the compositor handles them, exactly like
    /// events crossing those requests on a real socket.
    fn compositor(stream: UnixStream, finished: &AtomicBool) -> Result<()> {
        let mut wire = Wire(stream);
        let mut interfaces = HashMap::from([(1, "wl_display")]);
        let mut seat = 0;
        let mut keyboards = 0;
        let mut devices = Vec::new();
        let mut released = Vec::new();
        let mut next_offer = 0xff00_0000;
        let mut phase = Phase::WithKeyboard;
        let mut seat_device_released = false;
        let mut offer_selection = |wire: &mut Wire, interfaces: &mut HashMap<u32, &str>, device| {
            let offer = next_offer;
            next_offer += 1;
            interfaces.insert(offer, "wl_data_offer");
            wire.event(device, 0, &[Arg::Uint(offer)])?;
            wire.event(offer, 0, &[Arg::Str(OFFER_MIME)])?;
            wire.event(device, 5, &[Arg::Uint(offer)])
        };
        while let Some((object, opcode, body)) = wire.request()? {
            match (interfaces.get(&object).copied(), opcode) {
                (Some("wl_display"), 0) => {
                    let callback = word(&body, 0);
                    if phase == Phase::KeyboardRestored && keyboards == 2 {
                        assert_eq!(
                            devices.len(),
                            1,
                            "keyboard restoration allocated a duplicate selection device"
                        );
                        // The selection is offered to the device the client
                        // currently uses for the seat.
                        offer_selection(&mut wire, &mut interfaces, *devices.last().unwrap())?;
                        phase = Phase::Finished;
                        finished.store(true, Ordering::SeqCst);
                    }
                    wire.event(callback, 0, &[Arg::Uint(0)])?;
                    wire.event(1, 1, &[Arg::Uint(callback)])?;
                },
                (Some("wl_display"), 1) => {
                    let registry = word(&body, 0);
                    interfaces.insert(registry, "wl_registry");
                    wire.event(registry, 0, &[Arg::Uint(1), Arg::Str("wl_seat"), Arg::Uint(7)])?;
                    wire.event(
                        registry,
                        0,
                        &[Arg::Uint(2), Arg::Str("wl_data_device_manager"), Arg::Uint(3)],
                    )?;
                },
                (Some("wl_registry"), 0) => {
                    let interface_words = (word(&body, 1) as usize).div_ceil(4);
                    let id = word(&body, 2 + interface_words + 1);
                    match word(&body, 0) {
                        1 => {
                            seat = id;
                            interfaces.insert(id, "wl_seat");
                            wire.event(seat, 0, &[Arg::Uint(KEYBOARD)])?;
                        },
                        2 => {
                            interfaces.insert(id, "wl_data_device_manager");
                        },
                        name => return Err(Error::other(format!("bind of unknown global {name}"))),
                    }
                },
                (Some("wl_seat"), 1) => {
                    interfaces.insert(word(&body, 0), "wl_keyboard");
                    keyboards += 1;
                },
                (Some("wl_data_device_manager"), 1) => {
                    let device = word(&body, 0);
                    interfaces.insert(device, "wl_data_device");
                    devices.push(device);
                    if phase == Phase::WithKeyboard {
                        // Unplug the only keyboard of the seat.
                        wire.event(seat, 0, &[Arg::Uint(0)])?;
                        phase = Phase::KeyboardRemoved;
                    }
                },
                // wl_data_device.release
                (Some("wl_data_device"), 2) => {
                    if phase == Phase::Finished {
                        seat_device_released = true;
                    }
                    released.push(object);
                },
                // wl_keyboard.release: the client finished handling the removal.
                (Some("wl_keyboard"), 0) => {
                    released.push(object);
                    if phase == Phase::KeyboardRemoved {
                        offer_selection(&mut wire, &mut interfaces, devices[0])?;
                        for id in released.drain(..) {
                            wire.event(1, 1, &[Arg::Uint(id)])?;
                        }
                        // Plug a keyboard back in.
                        wire.event(seat, 0, &[Arg::Uint(KEYBOARD)])?;
                        phase = Phase::KeyboardRestored;
                    }
                },
                // wl_data_offer.destroy of a server-allocated id.
                (Some("wl_data_offer"), 2) => {},
                (interface, opcode) => {
                    return Err(Error::other(format!(
                        "unexpected request {interface:?}@{object} opcode {opcode}"
                    )));
                },
            }
        }
        assert!(seat_device_released, "seat teardown did not release its selection device");
        Ok(())
    }

    // Deterministic regression for a fatal client error seen on Sway when a
    // virtual keyboard was hot-unplugged: destroying the data device on
    // keyboard loss made libwayland discard the in-flight offer without
    // reserving its id, so the next offer failed with "not a valid new object
    // id" and the whole shared display connection died.
    #[test]
    #[ignore = "requires native AF_UNIX socket pairs; run on hosted fixture machine"]
    fn native_keyboard_unplug_keeps_selection_devices_and_connection() {
        let (client, server) = UnixStream::pair().unwrap();
        server.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let finished = Arc::new(AtomicBool::new(false));
        let script = {
            let finished = finished.clone();
            std::thread::spawn(move || {
                let control = server.try_clone().unwrap();
                let result = compositor(server, &finished);
                // Never leave the client blocked on a failed script.
                let _ = control.shutdown(Shutdown::Both);
                result
            })
        };

        let connection = Connection::from_socket(client).unwrap();
        let (globals, mut queue) = registry_queue_init::<State>(&connection).unwrap();
        let event_loop = EventLoop::<State>::try_new().unwrap();
        let (reply_tx, _reply_rx) = std::sync::mpsc::channel();
        let mut state =
            State::new(&globals, &queue.handle(), event_loop.handle(), reply_tx).unwrap();
        for _ in 0..8 {
            if let Err(error) = queue.roundtrip(&mut state) {
                panic!("display connection failed after keyboard unplug: {error}");
            }
            if finished.load(Ordering::SeqCst) {
                break;
            }
        }
        assert!(finished.load(Ordering::SeqCst), "compositor script did not complete");

        let seat = state.seats.values().next().unwrap();
        assert!(seat.keyboard.is_some(), "keyboard was not restored");
        let offer = seat
            .data_device
            .as_ref()
            .and_then(|device| device.data().selection_offer())
            .expect("selection offered after the keyboard returned is lost");
        assert!(offer.with_mime_types(|mimes| mimes.iter().any(|mime| mime == OFFER_MIME)));
        queue.roundtrip(&mut state).unwrap();

        let seat = state.seat_state.seats().next().unwrap();
        state.remove_seat(&connection, &queue.handle(), seat);
        assert!(state.seats.is_empty(), "removed seat retained clipboard resources");
        queue.roundtrip(&mut state).unwrap();
        drop((state, queue, globals, event_loop, connection));
        script.join().unwrap().unwrap();
    }
}
