use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{Error, ErrorKind, Read, Result, Write};
use std::mem;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use sctk::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use sctk::data_device_manager::data_offer::{DataOfferError, DataOfferHandler, DragOffer};
use sctk::data_device_manager::data_source::{CopyPasteSource, DataSourceHandler};
use sctk::data_device_manager::{DataDeviceManagerState, WritePipe};
use sctk::primary_selection::device::{PrimarySelectionDevice, PrimarySelectionDeviceHandler};
use sctk::primary_selection::selection::{PrimarySelectionSource, PrimarySelectionSourceHandler};
use sctk::primary_selection::PrimarySelectionManagerState;
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::seat::pointer::{PointerData, PointerEvent, PointerEventKind, PointerHandler};
use sctk::seat::{Capability, SeatHandler, SeatState};
use sctk::{
    delegate_data_device, delegate_pointer, delegate_primary_selection, delegate_registry,
    delegate_seat, registry_handlers,
};

use sctk::reexports::calloop::timer::{TimeoutAction, Timer};
use sctk::reexports::calloop::{LoopHandle, PostAction, RegistrationToken};
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

use crate::drop::{file_paths, portal_key, wanted_types, DropEvent, DropHandler, URI_LIST};
use crate::mime::{normalize_to_lf, MimeType, ALLOWED_MIME_TYPES};

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

    /// Where files dragged onto the client's surfaces go, if anywhere.
    drops: Option<Box<dyn DropHandler>>,
    /// The drag over one of the client's surfaces that may hold files.
    drag: Option<Drag>,
    /// A drop whose data is being read.
    reading: Option<DropReading>,
    /// Counts drags, so a read or timer of an older one does nothing.
    drags_seen: u64,
}

/// A drag that may hold files, over one of the client's surfaces.
struct Drag {
    serial: u64,
    surface: usize,
    offer: DragOffer,
    /// The portal key type to read, if the drag offers one.
    portal: Option<String>,
    uri_list: bool,
    /// The URI list's read, while it goes on.
    list_read: Option<RegistrationToken>,
    /// The local files the URI list names, once read.
    listed: Option<Vec<PathBuf>>,
    /// Whether the surface heard that files came over it.
    announced: bool,
}

/// A drop whose data is being read.
struct DropReading {
    serial: u64,
    surface: usize,
    offer: DragOffer,
    announced: bool,
    portal_read: Option<RegistrationToken>,
    list_read: Option<RegistrationToken>,
    portal_data: Option<Vec<u8>>,
    listed: Option<Vec<PathBuf>>,
}

#[derive(Clone, Copy)]
enum DropPart {
    PortalKey,
    UriList,
}

/// How long a source gets to hand over a drop's data.
const DROP_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

impl State {
    #[must_use]
    pub fn new(
        globals: &GlobalList,
        queue_handle: &QueueHandle<Self>,
        loop_handle: LoopHandle<'static, Self>,
        reply_tx: Sender<Result<String>>,
        drops: Option<Box<dyn DropHandler>>,
    ) -> Option<Self> {
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
            drops,
            drag: None,
            reading: None,
            drags_seen: 0,
        })
    }

    fn drop_event(&mut self, event: DropEvent) {
        if let Some(drops) = self.drops.as_mut() {
            drops.event(event);
        }
    }

    /// Tells the surface under the drag that files came over it, once.
    fn announce_drag(&mut self) {
        let Some(drag) = self.drag.as_mut().filter(|drag| !drag.announced) else {
            return;
        };
        drag.announced = true;
        let surface = drag.surface;
        self.drop_event(DropEvent::Enter { surface });
    }

    /// Ends the drag over a surface without a drop.
    fn end_drag(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        if let Some(token) = drag.list_read {
            self.loop_handle.remove(token);
        }
        if drag.announced {
            self.drop_event(DropEvent::Leave { surface: drag.surface });
        }
    }

    /// Reads one type of a drag's data, without blocking the thread.
    fn read_drag(
        &mut self,
        offer: &DragOffer,
        mime: String,
        serial: u64,
        part: DropPart,
    ) -> Result<RegistrationToken> {
        let read_pipe = offer.receive(mime).map_err(|err| Error::new(ErrorKind::Other, err))?;
        unsafe {
            set_non_blocking(read_pipe.as_raw_fd())?;
        }
        let mut reader_buffer = [0; 4096];
        let mut content = Vec::new();
        self.loop_handle
            .insert_source(read_pipe, move |_, file, state| {
                let file = unsafe { file.get_mut() };
                loop {
                    match file.read(&mut reader_buffer) {
                        Ok(0) => {
                            state.drag_part_read(serial, part, Some(mem::take(&mut content)));
                            break PostAction::Remove;
                        },
                        Ok(n) => content.extend_from_slice(&reader_buffer[..n]),
                        Err(err) if err.kind() == ErrorKind::WouldBlock => {
                            break PostAction::Continue
                        },
                        Err(err) if err.kind() == ErrorKind::Interrupted => {},
                        Err(_) => {
                            state.drag_part_read(serial, part, None);
                            break PostAction::Remove;
                        },
                    }
                }
            })
            .map_err(|err| Error::new(ErrorKind::Other, err.to_string()))
    }

    /// A read of a drag's data ended, with the data unless it failed.
    fn drag_part_read(&mut self, serial: u64, part: DropPart, data: Option<Vec<u8>>) {
        if let Some(reading) = self.reading.as_mut().filter(|reading| reading.serial == serial) {
            match part {
                DropPart::PortalKey => {
                    reading.portal_read = None;
                    reading.portal_data = data;
                },
                DropPart::UriList => {
                    reading.list_read = None;
                    reading.listed = data.as_deref().map(file_paths);
                },
            }
            if reading.portal_read.is_none() && reading.list_read.is_none() {
                self.finish_drop();
            }
            return;
        }
        // Over the surface still, where the URI list says whether files came.
        let Some(drag) = self.drag.as_mut().filter(|drag| drag.serial == serial) else {
            return;
        };
        drag.list_read = None;
        drag.listed = data.as_deref().map(file_paths);
        match drag.listed.as_ref().map(Vec::is_empty) {
            // A link, say, which isn't taken.
            Some(true) => drag.offer.accept_mime_type(drag.offer.serial, None),
            Some(false) => self.announce_drag(),
            None => {},
        }
    }

    /// Turns the drop's data into files and tells the source it went
    /// through. A portal transfer can end once its source hears that, so its
    /// key is resolved first.
    fn finish_drop(&mut self) {
        let Some(reading) = self.reading.take() else {
            return;
        };
        // Reads the timeout cut short.
        for token in [reading.portal_read, reading.list_read].into_iter().flatten() {
            self.loop_handle.remove(token);
        }
        let key = reading.portal_data.as_deref().and_then(portal_key);
        let mut paths = match (key, self.drops.as_mut()) {
            (Some(key), Some(drops)) => drops.retrieve_files(&key).unwrap_or_default(),
            _ => Vec::new(),
        };
        // The source's own names, which a sandboxed client may not be able to
        // open. The handler can tell its user so.
        if paths.is_empty() {
            paths = reading.listed.unwrap_or_default();
        }
        let surface = reading.surface;
        if paths.is_empty() {
            // Nothing was taken, so the source hears the drop was cancelled.
            reading.offer.destroy();
            if reading.announced {
                self.drop_event(DropEvent::Leave { surface });
            }
            return;
        }
        if !reading.offer.selected_action.is_empty() {
            reading.offer.finish();
        }
        self.drop_event(DropEvent::Drop { surface, paths });
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
            .ok_or_else(|| Error::new(ErrorKind::Other, "no events received on any seat"))?;
        let seat = self
            .seats
            .get_mut(latest)
            .ok_or_else(|| Error::new(ErrorKind::Other, "active seat lost"))?;

        if !seat.has_focus {
            return Err(Error::new(ErrorKind::Other, "client doesn't have focus"));
        }

        let (read_pipe, mime_type) = match ty {
            SelectionTarget::Clipboard => {
                let selection = seat
                    .data_device
                    .as_ref()
                    .and_then(|data| data.data().selection_offer())
                    .ok_or_else(|| Error::new(ErrorKind::Other, "selection is empty"))?;

                let mime_type =
                    selection.with_mime_types(MimeType::find_allowed).ok_or_else(|| {
                        Error::new(ErrorKind::NotFound, "supported mime-type is not found")
                    })?;

                (
                    selection.receive(mime_type.to_string()).map_err(|err| match err {
                        DataOfferError::InvalidReceive => {
                            Error::new(ErrorKind::Other, "offer is not ready yet")
                        },
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
                    .ok_or_else(|| Error::new(ErrorKind::Other, "selection is empty"))?;

                let mime_type =
                    selection.with_mime_types(MimeType::find_allowed).ok_or_else(|| {
                        Error::new(ErrorKind::NotFound, "supported mime-type is not found")
                    })?;

                (selection.receive(mime_type.to_string())?, mime_type)
            },
        };

        // Mark FD as non-blocking so we won't block ourselves.
        unsafe {
            set_non_blocking(read_pipe.as_raw_fd())?;
        }

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
        unsafe {
            if set_non_blocking(write_pipe.as_raw_fd()).is_err() {
                return;
            }
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

                // Selection sources are tied to the keyboard, so add/remove decives
                // when we gain/loss capability.

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
                seat_state.data_device = None;
                seat_state.primary_device = None;

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
        data_device: &WlDataDevice,
        _: f64,
        _: f64,
        surface: &WlSurface,
    ) {
        if self.drops.is_none() {
            return;
        }
        // A drag that moved on without leaving first.
        self.end_drag();
        let Some(offer) = drag_offer(data_device) else {
            return;
        };
        let Some((portal, uri_list)) = offer.with_mime_types(wanted_types) else {
            offer.accept_mime_type(offer.serial, None);
            return;
        };
        offer.accept_mime_type(offer.serial, Some(portal.clone().unwrap_or(URI_LIST.into())));
        offer.set_actions(DndAction::Copy, DndAction::Copy);
        self.drags_seen += 1;
        let serial = self.drags_seen;
        // Links come as a URI list too, so without a portal key the list says
        // whether files came.
        let list_read = match portal {
            Some(_) => None,
            None => self.read_drag(&offer, URI_LIST.into(), serial, DropPart::UriList).ok(),
        };
        let announce = portal.is_some();
        self.drag = Some(Drag {
            serial,
            surface: surface.id().as_ptr() as usize,
            offer,
            portal,
            uri_list,
            list_read,
            listed: None,
            announced: false,
        });
        if announce {
            self.announce_drag();
        }
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        self.end_drag();
    }

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}

    fn drop_performed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        data_device: &WlDataDevice,
    ) {
        let Some(offer) = drag_offer(data_device) else {
            self.end_drag();
            return;
        };
        let Some(drag) = self.drag.take() else {
            return;
        };
        // A drop still being read when another comes gives way.
        self.finish_drop();
        let serial = drag.serial;
        let mut reading = DropReading {
            serial,
            surface: drag.surface,
            offer: offer.clone(),
            announced: drag.announced,
            portal_read: None,
            list_read: drag.list_read,
            portal_data: None,
            listed: drag.listed,
        };
        if let Some(mime) = drag.portal {
            reading.portal_read = self.read_drag(&offer, mime, serial, DropPart::PortalKey).ok();
        }
        if drag.uri_list && reading.list_read.is_none() && reading.listed.is_none() {
            reading.list_read =
                self.read_drag(&offer, URI_LIST.into(), serial, DropPart::UriList).ok();
        }
        let waiting = reading.portal_read.is_some() || reading.list_read.is_some();
        self.reading = Some(reading);
        if !waiting {
            self.finish_drop();
            return;
        }
        // A source that never closes its end doesn't keep the drop waiting.
        let _ = self.loop_handle.insert_source(
            Timer::from_duration(DROP_READ_TIMEOUT),
            move |_, _, state| {
                if state.reading.as_ref().map_or(false, |reading| reading.serial == serial) {
                    state.finish_drop();
                }
                TimeoutAction::Drop
            },
        );
    }

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

/// The drag the data device has now.
fn drag_offer(data_device: &WlDataDevice) -> Option<DragOffer> {
    data_device.data::<sctk::data_device_manager::data_device::DataDeviceData>()?.drag_offer()
}

unsafe fn set_non_blocking(raw_fd: RawFd) -> std::io::Result<()> {
    let flags = libc::fcntl(raw_fd, libc::F_GETFL);

    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let result = libc::fcntl(raw_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    if result < 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(())
}
