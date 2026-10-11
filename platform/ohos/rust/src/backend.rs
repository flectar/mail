use crate::{
    framebuffer::OpaqueRgba,
    native,
    performance::{PerformanceStats, Phase},
};
use i_slint_core::{
    input::{InternalKeyEvent, KeyEventType, TouchPhase},
    lengths::PhysicalEdges,
    platform::InternalEvent,
    window::{InputMethodProperties, InputMethodRequest, WindowAdapterInternal},
};
use slint::platform::{
    self, Clipboard, EventLoopProxy, Platform, PlatformError, PointerEventButton, WindowAdapter,
    WindowEvent, WindowEventDispatchResult,
    software_renderer::{RepaintBufferType, SoftwareRenderer},
};
use slint::{LogicalPosition, PhysicalSize, Rgb8Pixel, Window};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender, SyncSender},
    time::{Duration, Instant},
};

pub enum Event {
    Task(Box<dyn FnOnce() + Send>),
    Quit,
    Frame,
    Viewport {
        width: u32,
        height: u32,
        density: f32,
        insets: PhysicalEdges,
    },
    Surface {
        generation: u64,
        available: bool,
    },
    Foreground(bool),
    Focus(bool),
    Pointer {
        kind: i32,
        x: f32,
        y: f32,
        button: i32,
    },
    Touch {
        kind: i32,
        id: i32,
        x: f32,
        y: f32,
    },
    Scroll {
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
    },
    Key {
        pressed: bool,
        text: String,
    },
    Insert(String),
    Preview {
        text: String,
        start: i32,
        end: i32,
    },
    FinishPreview,
    Selection {
        start: i32,
        end: i32,
    },
    Delete {
        forward: bool,
        units: usize,
    },
    Back(SyncSender<bool>),
    Suspend,
}

/// Never divide coordinates by an invalid platform scale factor.
pub fn density(value: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        1.0
    }
}
pub fn logical_position(x: f32, y: f32, scale: f32) -> LogicalPosition {
    LogicalPosition::new(x / density(scale), y / density(scale))
}

/// UTF-16 positions from OHOS are mapped to UTF-8 boundaries, never inside a surrogate pair.
pub fn utf16_to_byte(text: &str, units: usize) -> usize {
    let mut consumed = 0;
    for (byte, character) in text.char_indices() {
        if consumed + character.len_utf16() > units {
            return byte;
        }
        consumed += character.len_utf16();
    }
    text.len()
}
fn utf16_to_byte_ceil(text: &str, units: usize) -> usize {
    let floor = utf16_to_byte(text, units);
    if byte_to_utf16(text, floor) < units {
        text[floor..]
            .chars()
            .next()
            .map_or(floor, |c| floor + c.len_utf8())
    } else {
        floor
    }
}
pub fn byte_to_utf16(text: &str, byte: usize) -> usize {
    text.char_indices()
        .take_while(|(offset, c)| *offset + c.len_utf8() <= byte.min(text.len()))
        .map(|(_, c)| c.len_utf16())
        .sum()
}

#[derive(Default, Copy, Clone, Debug)]
pub struct SurfaceState {
    pub generation: u64,
    pub available: bool,
}
impl SurfaceState {
    pub fn update(&mut self, generation: u64, available: bool) {
        if generation >= self.generation {
            self.generation = generation;
            self.available = available;
        }
    }
    pub fn can_present(&self, generation: u64) -> bool {
        self.available && self.generation == generation
    }
}

struct Proxy(Sender<Event>);
impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        self.0
            .send(Event::Quit)
            .map_err(|_| slint::EventLoopError::EventLoopTerminated)
    }
    fn invoke_from_event_loop(
        &self,
        event: Box<dyn FnOnce() + Send>,
    ) -> Result<(), slint::EventLoopError> {
        self.0
            .send(Event::Task(event))
            .map_err(|_| slint::EventLoopError::EventLoopTerminated)
    }
}

pub struct OhosWindow {
    window: Window,
    renderer: SoftwareRenderer,
    size: Cell<PhysicalSize>,
    redraw: Cell<bool>,
    shown: Cell<bool>,
    insets: Cell<PhysicalEdges>,
    ime: RefCell<InputMethodProperties>,
    ime_enabled: Cell<bool>,
    focused: Cell<bool>,
    foreground: Cell<bool>,
    ime_suspensions: Cell<u32>,
    // Slint 1.18 updates IME properties on cursor/text changes, but a pure
    // preedit update need not change either. Retain the native composition.
    preedit: RefCell<String>,
    preedit_cursor: Cell<usize>,
    preedit_anchor: Cell<usize>,
}
impl OhosWindow {
    fn new() -> Rc<Self> {
        Rc::new_cyclic(|weak: &std::rc::Weak<Self>| Self {
            window: Window::new(weak.clone()),
            renderer: SoftwareRenderer::new_with_repaint_buffer_type(
                RepaintBufferType::ReusedBuffer,
            ),
            size: Cell::new(PhysicalSize::new(1, 1)),
            redraw: Cell::new(true),
            shown: Cell::new(false),
            insets: Cell::new(PhysicalEdges::default()),
            ime: RefCell::new(InputMethodProperties::default()),
            ime_enabled: Cell::new(false),
            focused: Cell::new(true),
            foreground: Cell::new(true),
            ime_suspensions: Cell::new(0),
            preedit: RefCell::new(String::new()),
            preedit_cursor: Cell::new(0),
            preedit_anchor: Cell::new(0),
        })
    }
    fn key(
        &self,
        text: impl Into<slint::SharedString>,
        pressed: bool,
    ) -> WindowEventDispatchResult {
        self.window
            .dispatch_event_with_result(if pressed {
                WindowEvent::KeyPressed { text: text.into() }
            } else {
                WindowEvent::KeyReleased { text: text.into() }
            })
            .unwrap_or(WindowEventDispatchResult::Rejected)
    }
    fn composition(&self, event: InternalKeyEvent) {
        self.window.dispatch_event(WindowEvent::internal(event));
    }
    fn publish_ime(&self) {
        let props = self.ime.borrow();
        let preedit = self.preedit.borrow();
        let mut surrounding = props.text.to_string();
        let offset = props.preedit_offset.min(surrounding.len());
        if surrounding.is_char_boundary(offset) {
            surrounding.insert_str(offset, &preedit);
        }
        let cursor = byte_to_utf16(&props.text, props.cursor_position)
            + byte_to_utf16(&preedit, self.preedit_cursor.get());
        let anchor = if !preedit.is_empty() {
            byte_to_utf16(&props.text, props.preedit_offset)
                + byte_to_utf16(&preedit, self.preedit_anchor.get())
        } else {
            byte_to_utf16(
                &props.text,
                props.anchor_position.unwrap_or(props.cursor_position),
            )
        };
        let scale = self.window.scale_factor();
        let input_type = match props.input_type {
            i_slint_core::items::InputType::Password => 7,
            i_slint_core::items::InputType::Number => 2,
            i_slint_core::items::InputType::Decimal => 12,
            _ => 0,
        };
        native::ime_update(
            self.ime_enabled.get()
                && self.focused.get()
                && self.foreground.get()
                && self.ime_suspensions.get() == 0,
            &surrounding,
            cursor as i32,
            anchor as i32,
            [
                props.cursor_rect_origin.x * scale,
                props.cursor_rect_origin.y * scale,
                props.cursor_rect_size.width * scale,
                props.cursor_rect_size.height * scale,
            ],
            input_type,
        );
    }
    fn commit(&self, text: String, replacement_range: Option<std::ops::Range<i32>>) {
        self.preedit.borrow_mut().clear();
        self.preedit_cursor.set(0);
        self.preedit_anchor.set(0);
        let mut event = InternalKeyEvent::default();
        event.event_type = KeyEventType::CommitComposition;
        event.key_event.text = text.into();
        event.replacement_range = replacement_range;
        self.composition(event);
    }
    fn process(&self, event: Event, surface: &mut SurfaceState, foreground: &mut bool) -> bool {
        match event {
            Event::Task(task) => task(),
            Event::Quit => return false,
            Event::Frame => {}
            Event::Viewport {
                width,
                height,
                density: scale,
                insets,
            } => {
                let scale = density(scale);
                self.insets.set(insets);
                self.size
                    .set(PhysicalSize::new(width.max(1), height.max(1)));
                self.window.dispatch_event(WindowEvent::ScaleFactorChanged {
                    scale_factor: scale,
                });
                self.window.dispatch_event(WindowEvent::Resized {
                    size: self.size.get().to_logical(scale),
                });
                self.redraw.set(true);
            }
            Event::Surface {
                generation,
                available,
            } => {
                surface.update(generation, available);
                self.redraw.set(true);
            }
            Event::Foreground(value) => {
                if *foreground != value {
                    native::set_ui_interactive(value);
                }
                *foreground = value;
                self.foreground.set(value);
                self.redraw.set(value);
                #[cfg(all(feature = "application", target_env = "ohos"))]
                flectar_mail::set_application_foreground(value);
                self.window.dispatch_event(WindowEvent::WindowActiveChanged(
                    value && self.focused.get(),
                ));
                if !value {
                    native::ime_update(false, "", 0, 0, [0.; 4], 0);
                } else if self.ime_enabled.get() {
                    self.publish_ime();
                }
            }
            Event::Suspend => {
                if *foreground {
                    native::set_ui_interactive(false);
                }
                *foreground = false;
                self.foreground.set(false);
                self.window
                    .dispatch_event(WindowEvent::WindowActiveChanged(false));
                #[cfg(all(feature = "application", target_env = "ohos"))]
                flectar_mail::set_application_foreground(false);
                native::ime_update(false, "", 0, 0, [0.; 4], 0);
            }
            Event::Focus(value) => {
                self.focused.set(value);
                self.window.dispatch_event(WindowEvent::WindowActiveChanged(
                    value && self.foreground.get(),
                ));
                self.publish_ime();
            }
            Event::Pointer { kind, x, y, button } => {
                let position = logical_position(x, y, self.window.scale_factor());
                let button = match button {
                    1 => PointerEventButton::Right,
                    2 => PointerEventButton::Middle,
                    _ => PointerEventButton::Left,
                };
                let event = match kind {
                    0 => WindowEvent::PointerPressed { position, button },
                    1 => WindowEvent::PointerReleased { position, button },
                    2 => WindowEvent::PointerMoved { position },
                    _ => WindowEvent::PointerExited,
                };
                self.window.dispatch_event(event);
            }
            Event::Touch { kind, id, x, y } => {
                let position = logical_position(x, y, self.window.scale_factor());
                let phase = match kind {
                    0 => TouchPhase::Started,
                    1 => TouchPhase::Ended,
                    2 => TouchPhase::Moved,
                    _ => TouchPhase::Cancelled,
                };
                self.window
                    .dispatch_event(WindowEvent::internal(InternalEvent::Touch {
                        id: id.max(0),
                        position: i_slint_core::lengths::LogicalPoint::new(position.x, position.y),
                        phase,
                    }));
            }
            Event::Scroll { x, y, dx, dy } => {
                self.window.dispatch_event(WindowEvent::PointerScrolled {
                    position: logical_position(x, y, self.window.scale_factor()),
                    delta_x: dx / density(self.window.scale_factor()),
                    delta_y: dy / density(self.window.scale_factor()),
                })
            }
            Event::Key { pressed, text } => {
                self.key(text, pressed);
            }
            Event::Insert(text) => self.commit(text, None),
            Event::Preview { text, start, end } => {
                let props = self.ime.borrow().clone();
                let old_preedit = self.preedit.borrow().clone();
                let offset = byte_to_utf16(&props.text, props.preedit_offset);
                let within_preedit = !old_preedit.is_empty()
                    && start >= offset as i32
                    && end >= start
                    && end <= (offset + old_preedit.encode_utf16().count()) as i32;
                let text = if within_preedit {
                    let mut value = old_preedit;
                    let range = utf16_to_byte(&value, start as usize - offset)
                        ..utf16_to_byte_ceil(&value, end as usize - offset);
                    value.replace_range(range, &text);
                    value
                } else {
                    text
                };
                *self.preedit.borrow_mut() = text.clone();
                self.preedit_cursor.set(text.len());
                self.preedit_anchor.set(text.len());
                let mut event = InternalKeyEvent::default();
                event.event_type = KeyEventType::UpdateComposition;
                event.preedit_selection = Some(text.len() as i32..text.len() as i32);
                event.preedit_text = text.into();
                if start >= 0 && end >= start && !within_preedit {
                    event.replacement_range = Some(
                        (utf16_to_byte(&props.text, start as usize) as i32
                            - props.preedit_offset as i32)
                            ..(utf16_to_byte(&props.text, end as usize) as i32
                                - props.preedit_offset as i32),
                    );
                }
                self.composition(event);
                self.publish_ime();
            }
            Event::FinishPreview => {
                let text = self.preedit.borrow().clone();
                if !text.is_empty() {
                    self.commit(text, None);
                }
            }
            Event::Selection { start, end } => {
                let props = self.ime.borrow().clone();
                let preedit = self.preedit.borrow().clone();
                let offset = byte_to_utf16(&props.text, props.preedit_offset);
                let within_preedit = !preedit.is_empty()
                    && start >= offset as i32
                    && end >= 0
                    && start <= (offset + preedit.encode_utf16().count()) as i32
                    && end >= offset as i32
                    && end <= (offset + preedit.encode_utf16().count()) as i32;
                let mut event = InternalKeyEvent::default();
                event.event_type = KeyEventType::UpdateComposition;
                if within_preedit {
                    let anchor = utf16_to_byte(&preedit, start as usize - offset);
                    let cursor = utf16_to_byte(&preedit, end as usize - offset);
                    self.preedit_anchor.set(anchor);
                    self.preedit_cursor.set(cursor);
                    event.preedit_selection = Some(anchor as i32..cursor as i32);
                    event.preedit_text = preedit.into();
                    self.composition(event);
                    self.publish_ime();
                    return true;
                }
                event.cursor_position =
                    Some(utf16_to_byte(&props.text, end.max(0) as usize) as i32);
                event.anchor_position =
                    Some(utf16_to_byte(&props.text, start.max(0) as usize) as i32);
                event.preedit_text = preedit.into();
                self.composition(event);
            }
            Event::Delete { forward, units } => {
                let props = self.ime.borrow().clone();
                let mut preedit = self.preedit.borrow().clone();
                if !preedit.is_empty() {
                    let cursor = self.preedit_cursor.get();
                    let anchor = self.preedit_anchor.get();
                    let cursor_units = byte_to_utf16(&preedit, cursor);
                    let other = if anchor != cursor {
                        anchor
                    } else if forward {
                        utf16_to_byte_ceil(&preedit, cursor_units.saturating_add(units))
                    } else {
                        utf16_to_byte(&preedit, cursor_units.saturating_sub(units))
                    };
                    let start = cursor.min(other);
                    preedit.replace_range(start..cursor.max(other), "");
                    *self.preedit.borrow_mut() = preedit.clone();
                    self.preedit_cursor.set(start);
                    self.preedit_anchor.set(start);
                    let mut event = InternalKeyEvent::default();
                    event.event_type = KeyEventType::UpdateComposition;
                    event.preedit_text = preedit.into();
                    event.preedit_selection = Some(start as i32..start as i32);
                    self.composition(event);
                    self.publish_ime();
                    return true;
                }
                let cursor_units = byte_to_utf16(&props.text, props.cursor_position);
                let other = props
                    .anchor_position
                    .filter(|anchor| *anchor != props.cursor_position)
                    .unwrap_or_else(|| {
                        if forward {
                            utf16_to_byte_ceil(&props.text, cursor_units.saturating_add(units))
                        } else {
                            utf16_to_byte(&props.text, cursor_units.saturating_sub(units))
                        }
                    });
                let start = other.min(props.cursor_position);
                let end = other.max(props.cursor_position);
                self.commit(
                    String::new(),
                    Some(
                        (start as i32 - props.preedit_offset as i32)
                            ..(end as i32 - props.preedit_offset as i32),
                    ),
                );
            }
            Event::Back(reply) => {
                let accepted = self.key(slint::platform::Key::Escape, true)
                    == WindowEventDispatchResult::Accepted;
                self.key(slint::platform::Key::Escape, false);
                let _ = reply.send(accepted);
            }
        }
        true
    }
}
impl WindowAdapter for OhosWindow {
    fn window(&self) -> &Window {
        &self.window
    }
    fn renderer(&self) -> &dyn platform::Renderer {
        &self.renderer
    }
    fn size(&self) -> PhysicalSize {
        self.size.get()
    }
    fn set_size(&self, _size: slint::WindowSize) { /* Stage owns physical geometry. */
    }
    fn set_visible(&self, visible: bool) -> Result<(), PlatformError> {
        self.shown.set(visible);
        self.redraw.set(visible);
        Ok(())
    }
    fn request_redraw(&self) {
        self.redraw.set(true);
    }
    fn internal(&self, _: i_slint_core::InternalToken) -> Option<&dyn WindowAdapterInternal> {
        Some(self)
    }
}
impl WindowAdapterInternal for OhosWindow {
    fn safe_area_inset(&self) -> PhysicalEdges {
        self.insets.get()
    }
    fn input_method_request(&self, request: InputMethodRequest) {
        let show_keyboard = matches!(&request, InputMethodRequest::Enable(_));
        match request {
            InputMethodRequest::Enable(props) => {
                self.ime_enabled.set(true);
                *self.preedit.borrow_mut() = props.preedit_text.to_string();
                self.preedit_cursor.set(props.preedit_text.len());
                self.preedit_anchor.set(props.preedit_text.len());
                *self.ime.borrow_mut() = props;
            }
            InputMethodRequest::Update(props) => {
                *self.ime.borrow_mut() = props;
            }
            InputMethodRequest::Disable => {
                self.ime_enabled.set(false);
                self.preedit.borrow_mut().clear();
                self.preedit_cursor.set(0);
                self.preedit_anchor.set(0);
            }
            _ => return,
        }
        self.publish_ime();
        if show_keyboard
            && self.focused.get()
            && self.foreground.get()
            && self.ime_suspensions.get() == 0
        {
            native::ime_show();
        }
    }
}

pub struct OhosPlatform {
    sender: Sender<Event>,
    receiver: RefCell<Receiver<Event>>,
    window: Rc<OhosWindow>,
}
thread_local! {
    static OWNER_WINDOW: RefCell<std::rc::Weak<OhosWindow>> = const { RefCell::new(std::rc::Weak::new()) };
}
/// Detach before a synchronous host modal blocks the Slint owner thread. A
/// native blur event cannot be consumed until that modal returns its reply.
pub(crate) struct InputMethodSuspension(Option<Rc<OhosWindow>>);
#[allow(dead_code)]
pub(crate) fn suspend_input_method() -> InputMethodSuspension {
    let window = OWNER_WINDOW.with(|owner| owner.borrow().upgrade());
    if let Some(window) = &window {
        window.ime_suspensions.set(window.ime_suspensions.get() + 1);
        window.publish_ime();
    }
    InputMethodSuspension(window)
}
impl Drop for InputMethodSuspension {
    fn drop(&mut self) {
        if let Some(window) = &self.0 {
            window
                .ime_suspensions
                .set(window.ime_suspensions.get().saturating_sub(1));
            window.publish_ime();
        }
    }
}
impl OhosPlatform {
    pub fn new(sender: Sender<Event>, receiver: Receiver<Event>) -> Self {
        let window = OhosWindow::new();
        OWNER_WINDOW.with(|owner| *owner.borrow_mut() = Rc::downgrade(&window));
        Self {
            sender,
            receiver: RefCell::new(receiver),
            window,
        }
    }
}

/// Slint input handlers use the animation clock for gesture velocity and timers.
/// Advance it before dispatching queued input, rather than after a whole batch.
fn update_clock(performance: &PerformanceStats) -> Duration {
    let started = performance.enabled().then(Instant::now);
    platform::update_timers_and_animations();
    started.map_or(Duration::ZERO, |started| started.elapsed())
}

impl Platform for OhosPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }
    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(self.sender.clone())))
    }
    fn set_clipboard_text(&self, text: &str, _: Clipboard) {
        native::clipboard_set(text);
    }
    fn clipboard_text(&self, _: Clipboard) -> Option<String> {
        let value = native::clipboard_get();
        #[cfg(all(feature = "application", target_env = "ohos"))]
        let value = value.or_else(|| {
            crate::services::clipboard_read()
                .map_err(|error| native::log(&error))
                .ok()
        });
        value
    }
    fn run_event_loop(&self) -> Result<(), PlatformError> {
        // This is the dedicated Rust UI thread. Restore normal scheduling on
        // every exit path, including queued Quit and a framebuffer limit error.
        struct UiScheduling;
        impl Drop for UiScheduling {
            fn drop(&mut self) {
                native::set_ui_interactive(false);
            }
        }
        native::set_ui_interactive(true);
        let _scheduling = UiScheduling;
        let mut surface = SurfaceState::default();
        let mut foreground = true;
        let mut pixels = Vec::<OpaqueRgba>::new();
        let mut framebuffer_identity = None;
        let mut frame_ready = true;
        let mut performance = PerformanceStats::new(crate::diagnostics_enabled());
        loop {
            let timeout = platform::duration_until_next_timer_update()
                .unwrap_or(Duration::from_millis(250))
                .min(Duration::from_millis(250));
            let event = self.receiver.borrow().recv_timeout(timeout);
            let events_started = Instant::now();
            let mut event_count = 0;
            let mut timers_elapsed = Duration::ZERO;
            if let Ok(event) = event {
                timers_elapsed += update_clock(&performance);
                event_count += 1;
                if matches!(event, Event::Frame) {
                    frame_ready = true;
                }
                if !self.window.process(event, &mut surface, &mut foreground) {
                    break;
                }
            } else if matches!(event, Err(mpsc::RecvTimeoutError::Disconnected)) {
                break;
            }
            // Give drawing a turn even when pointer/Tokio producers keep the
            // queue busy. Events retain FIFO order and are never discarded.
            while event_count < 128 && events_started.elapsed() < Duration::from_millis(4) {
                let Ok(event) = self.receiver.borrow().try_recv() else {
                    break;
                };
                // A task or previous input can take measurable time even in a
                // bounded batch. Each next event must observe a fresh clock.
                timers_elapsed += update_clock(&performance);
                event_count += 1;
                if matches!(event, Event::Frame) {
                    frame_ready = true;
                }
                if !self.window.process(event, &mut surface, &mut foreground) {
                    return Ok(());
                }
            }
            performance.record_event_batch(
                event_count,
                events_started.elapsed().saturating_sub(timers_elapsed),
            );
            // Timers may also become due while input is being dispatched.
            timers_elapsed += update_clock(&performance);
            if performance.enabled() {
                performance.record_phase(Phase::Timers, timers_elapsed);
            }
            if self.window.window.has_active_animations() {
                self.window.redraw.set(true);
            }
            // Mark actual pending redraw/animation work. A static screen does
            // not generate a synthetic FPS failure. Surface FPS during a
            // continuous gesture remains the external acceptance measurement.
            performance.set_active(foreground && self.window.redraw.get());
            if let Some(report) = performance.maybe_flush() {
                native::log(&format!("FeihongPerf {report}"));
            }
            if foreground
                && surface.can_present(surface.generation)
                && self.window.shown.get()
                && self.window.redraw.get()
            {
                if !frame_ready {
                    continue;
                }
                let size = self.window.size.get();
                let count = size.width as usize * size.height as usize;
                if count > 32 * 1024 * 1024 {
                    return Err("OHOS surface exceeds framebuffer limit".into());
                }
                pixels.resize(count, OpaqueRgba::default());
                let identity = (
                    surface.generation,
                    size,
                    self.window.window.scale_factor().to_bits(),
                );
                if framebuffer_identity != Some(identity) {
                    // Even equal-area resizes change row layout. A new Surface
                    // or scale must first populate the entire retained source.
                    self.window
                        .renderer
                        .set_repaint_buffer_type(RepaintBufferType::NewBuffer);
                }
                // Request the next tick before CPU drawing. If drawing spans
                // that tick, its queued Frame can immediately unblock the next
                // frame instead of waiting for another refresh after present.
                // A fast frame still waits for its one outstanding VSync.
                frame_ready = !native::request_frame();
                let frame_started = performance.enabled().then(Instant::now);
                let damage = self
                    .window
                    .renderer
                    .render(&mut pixels, size.width as usize);
                framebuffer_identity = Some(identity);
                self.window
                    .renderer
                    .set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
                if let Some(started) = frame_started {
                    performance.record_phase(Phase::SlintRender, started.elapsed());
                }
                let present_started = performance.enabled().then(Instant::now);
                let presented = native::present(
                    surface.generation,
                    &pixels,
                    size.width,
                    size.height,
                    &damage,
                );
                if let Some(started) = present_started {
                    performance.record_phase(Phase::NativePresent, started.elapsed());
                }
                if let Some(started) = frame_started {
                    performance.record_frame(presented, started.elapsed());
                }
                if presented {
                    self.window.redraw.set(false);
                }
            }
        }
        performance.set_active(false);
        if let Some(report) = performance.flush() {
            native::log(&format!("FeihongPerf {report}"));
        }
        native::ime_update(false, "", 0, 0, [0.; 4], 0);
        Ok(())
    }
}

pub fn register_fonts(paths: &[String]) -> Result<(), String> {
    let mut collection = slint::fontique_011::shared_collection();
    register_fonts_in_collection(&mut collection, paths)
}

fn register_fonts_in_collection(
    collection: &mut slint::fontique_011::fontique::Collection,
    paths: &[String],
) -> Result<(), String> {
    use slint::fontique_011::fontique;
    let mut usable = 0;
    let mut chinese = false;
    let mut latin_families = Vec::new();
    #[cfg(debug_assertions)]
    let mut emoji_fonts = EmojiFontDiagnostics::default();
    for path in paths {
        let data = match std::fs::read(path) {
            Ok(data) => fontique::Blob::new(std::sync::Arc::new(data)),
            Err(error) => {
                native::log(&format!("skipping optional font {path}: {error}"));
                continue;
            }
        };
        let families = collection.register_fonts(data.clone(), None);
        if families.is_empty() {
            native::log(&format!("skipping optional font {path}: no usable family"));
            continue;
        }
        usable += families.len();
        #[cfg(debug_assertions)]
        emoji_fonts.record_loaded(path, data.as_ref());
        chinese |= families.iter().flat_map(|family| &family.1).any(|font| {
            font.charmap_index()
                .charmap(data.as_ref())
                .is_some_and(|map| {
                    ['飞', '鸿', '邮', '箱']
                        .iter()
                        .all(|ch| map.map(*ch).is_some_and(|glyph| glyph != 0))
                })
        });
        for (id, fonts) in &families {
            let has_ascii = fonts.iter().any(|font| {
                font.charmap_index()
                    .charmap(data.as_ref())
                    .is_some_and(|map| {
                        (' '..='~').all(|ch| map.map(ch).is_some_and(|glyph| glyph != 0))
                    })
            });
            if has_ascii && !latin_families.iter().any(|(family, _)| family == id) {
                let name = collection.family_name(*id).unwrap_or_default().to_owned();
                latin_families.push((*id, name));
            }
        }
        for script in [*b"Hani", *b"Hira", *b"Kana", *b"Hang", *b"Zyyy"] {
            collection.append_fallbacks(
                fontique::FallbackKey::new(fontique::Script::from_bytes(script), None),
                families.iter().map(|family| family.0),
            );
        }
    }
    if usable == 0 {
        return Err("no usable OHOS platform font was found".into());
    }
    if latin_families.is_empty() {
        return Err("no OHOS platform font covers printable ASCII".into());
    }
    // Slint's software font metrics query named families, SansSerif and
    // SystemUi without a script fallback. Registering files alone leaves
    // those generic lists empty on OHOS, where Fontconfig is unavailable.
    for generic in [
        fontique::GenericFamily::SansSerif,
        fontique::GenericFamily::SystemUi,
        fontique::GenericFamily::Serif,
        fontique::GenericFamily::Monospace,
    ] {
        latin_families.sort_by_key(|(_, name)| {
            (
                generic_font_priority(name, generic),
                name.to_ascii_lowercase(),
            )
        });
        collection.set_generic_families(generic, latin_families.iter().map(|(id, _)| *id));
    }
    latin_families.sort_by_key(|(_, name)| {
        (
            generic_font_priority(name, fontique::GenericFamily::SansSerif),
            name.to_ascii_lowercase(),
        )
    });
    collection.set_fallbacks(
        fontique::FallbackKey::new(fontique::Script::from_bytes(*b"Latn"), None),
        latin_families.iter().map(|(id, _)| *id),
    );
    native::log(&format!(
        "platform fonts registered: families={usable} ASCII families={} Chinese fallback={chinese}",
        latin_families.len()
    ));
    #[cfg(debug_assertions)]
    native::log(&format!(
        "platform emoji fonts loadedFiles={} sbixFiles={} CBDTFiles={} COLRFiles={}",
        emoji_fonts.loaded_files,
        emoji_fonts.sbix_files,
        emoji_fonts.cbdt_files,
        emoji_fonts.colr_files,
    ));
    Ok(())
}

#[cfg(debug_assertions)]
#[derive(Default)]
struct EmojiFontDiagnostics {
    loaded_files: usize,
    sbix_files: usize,
    cbdt_files: usize,
    colr_files: usize,
}

#[cfg(debug_assertions)]
impl EmojiFontDiagnostics {
    fn record_loaded(&mut self, path: &str, data: &[u8]) {
        let is_emoji = std::path::Path::new(path).file_name().is_some_and(|name| {
            name.to_string_lossy()
                .to_ascii_lowercase()
                .contains("emoji")
        });
        if !is_emoji {
            return;
        }
        let [sbix, cbdt, colr] = emoji_color_table_flags(data);
        self.loaded_files += 1;
        self.sbix_files += usize::from(sbix);
        self.cbdt_files += usize::from(cbdt);
        self.colr_files += usize::from(colr);
    }
}

/// Inspect only directory tags in bytes already read by normal font loading.
/// TTC faces share table payloads; count each loaded file once per table type.
#[cfg(debug_assertions)]
fn emoji_color_table_flags(data: &[u8]) -> [bool; 3] {
    fn record_face(data: &[u8], offset: usize, flags: &mut [bool; 3]) {
        let Some(end) = offset.checked_add(12) else {
            return;
        };
        let Some(header) = data.get(offset..end) else {
            return;
        };
        match &header[..4] {
            b"\0\x01\0\0" | b"OTTO" | b"true" | b"typ1" => {}
            _ => return,
        }
        let count = usize::from(u16::from_be_bytes([header[4], header[5]]));
        let Some(end) = end.checked_add(count * 16) else {
            return;
        };
        let Some(directory) = data.get(offset + 12..end) else {
            return;
        };
        for record in directory.chunks_exact(16) {
            match &record[..4] {
                b"sbix" => flags[0] = true,
                b"CBDT" => flags[1] = true,
                b"COLR" => flags[2] = true,
                _ => {}
            }
        }
    }

    let mut flags = [false; 3];
    if data.get(..4) == Some(b"ttcf") {
        let Some(header) = data.get(..12) else {
            return flags;
        };
        let count = u32::from_be_bytes(header[8..12].try_into().unwrap()) as usize;
        let Some(end) = count.checked_mul(4).and_then(|n| n.checked_add(12)) else {
            return flags;
        };
        let Some(offsets) = data.get(12..end) else {
            return flags;
        };
        for entry in offsets.chunks_exact(4) {
            record_face(
                data,
                u32::from_be_bytes(entry.try_into().unwrap()) as usize,
                &mut flags,
            );
        }
    } else {
        record_face(data, 0, &mut flags);
    }
    flags
}

fn generic_font_priority(name: &str, generic: slint::fontique_011::fontique::GenericFamily) -> u8 {
    use slint::fontique_011::fontique::GenericFamily;
    let name: String = name
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    let serif = !name.contains("sans")
        && (name.contains("serif") || name.contains("times") || name.contains("georgia"));
    let mono = name.contains("mono") || name.contains("courier") || name.contains("console");
    if (generic == GenericFamily::Serif && serif) || (generic == GenericFamily::Monospace && mono) {
        return if name.starts_with("harmonyos") || name.starts_with("noto") {
            0
        } else {
            1
        };
    }
    if name.starts_with("harmonyossans") && !mono {
        2
    } else if name.starts_with("notosans") && !mono {
        3
    } else if name.starts_with("roboto") && !mono {
        4
    } else if !serif && !mono {
        5
    } else {
        6
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_font_query_works_without_a_system_font_backend() {
        use slint::fontique_011::fontique;
        let mut fonts = fontique::Collection::new(fontique::CollectionOptions {
            shared: false,
            system_fonts: false,
        });
        let mut cache = fontique::SourceCache::default();
        let request = i_slint_core::graphics::FontRequest::default();
        assert!(request.query_fontique(&mut fonts, &mut cache).is_none());
        let bundled_font = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../../resources/fonts/google-sans-flex/GoogleSansFlex-VariableFont_GRAD,ROND,opsz,slnt,wdth,wght.ttf",
        );
        register_fonts_in_collection(&mut fonts, &[bundled_font.to_string_lossy().into_owned()])
            .unwrap();
        let matched = request.query_fontique(&mut fonts, &mut cache).unwrap();
        let charmap = matched.charmap().unwrap();
        assert!((' '..='~').all(|ch| charmap.map(ch).is_some_and(|glyph| glyph != 0)));
        for generic in [
            fontique::GenericFamily::SansSerif,
            fontique::GenericFamily::SystemUi,
            fontique::GenericFamily::Serif,
            fontique::GenericFamily::Monospace,
        ] {
            assert!(fonts.generic_families(generic).next().is_some());
        }
        assert!(
            fonts
                .fallback_families(fontique::FallbackKey::new(
                    fontique::Script::from_bytes(*b"Latn"),
                    None,
                ))
                .next()
                .is_some()
        );
    }

    #[test]
    fn utf16_positions_do_not_split_emoji_or_cjk() {
        let text = "A飞😀鸿";
        assert_eq!(
            [0, 1, 4, 4, 8, 11],
            [0, 1, 2, 3, 4, 5].map(|n| utf16_to_byte(text, n))
        );
        assert_eq!(byte_to_utf16(text, 8), 4);
        assert_eq!(byte_to_utf16(text, 2), 1);
        for (byte, _) in text.char_indices() {
            assert_eq!(utf16_to_byte(text, byte_to_utf16(text, byte)), byte);
        }
    }
    #[test]
    fn viewport_converts_physical_coordinates_and_bad_density() {
        assert_eq!(
            logical_position(240., 120., 2.),
            LogicalPosition::new(120., 60.)
        );
        for scale in [0., -1., f32::NAN, f32::INFINITY] {
            assert_eq!(
                logical_position(24., 12., scale),
                LogicalPosition::new(24., 12.)
            );
        }
    }
    #[test]
    fn stale_surface_events_cannot_resurrect_destroyed_surface() {
        let mut surface = SurfaceState::default();
        surface.update(1, true);
        assert!(surface.can_present(1));
        surface.update(2, false);
        surface.update(1, true);
        assert!(!surface.can_present(1));
        assert!(!surface.can_present(2));
        surface.update(3, true);
        assert!(surface.can_present(3));
        assert!(!surface.can_present(2));
    }
    slint::slint! {
        export component EditorProbe inherits Window {
            width: 200px; height: 80px;
            in-out property <string> value: "飞😀鸿";
            background: #225588;
            input := TextInput { text <=> root.value; width: 180px; height: 40px; }
            public function focus-editor() { input.focus(); }
        }
    }
    #[test]
    fn actual_editor_preserves_preedit_and_utf16_selection() {
        use slint::ComponentHandle;
        let (sender, receiver) = mpsc::channel();
        let platform = OhosPlatform::new(sender, receiver);
        let window = platform.window.clone();
        platform::set_platform(Box::new(platform)).unwrap();
        let probe = EditorProbe::new().unwrap();
        probe.show().unwrap();
        probe.invoke_focus_editor();
        let mut surface = SurfaceState::default();
        let mut foreground = true;
        window.process(
            Event::Viewport {
                width: 200,
                height: 80,
                density: 1.,
                insets: PhysicalEdges::default(),
            },
            &mut surface,
            &mut foreground,
        );
        window.process(
            Event::Selection { start: 1, end: 3 },
            &mut surface,
            &mut foreground,
        );
        assert_eq!(probe.get_value(), "飞😀鸿");
        window.process(
            Event::Delete {
                forward: false,
                units: 1,
            },
            &mut surface,
            &mut foreground,
        );
        assert_eq!(probe.get_value(), "飞鸿");
        window.process(
            Event::Preview {
                text: "邮箱".into(),
                start: -1,
                end: -1,
            },
            &mut surface,
            &mut foreground,
        );
        assert_eq!(
            probe.get_value(),
            "飞鸿",
            "preedit must not be committed early"
        );
        window.process(Event::Insert("邮箱".into()), &mut surface, &mut foreground);
        assert_eq!(probe.get_value(), "飞邮箱鸿");
        window.process(
            Event::Preview {
                text: "😀".into(),
                start: -1,
                end: -1,
            },
            &mut surface,
            &mut foreground,
        );
        window.process(Event::FinishPreview, &mut surface, &mut foreground);
        assert_eq!(probe.get_value(), "飞邮箱😀鸿");
        window.process(Event::FinishPreview, &mut surface, &mut foreground);
        assert_eq!(probe.get_value(), "飞邮箱😀鸿", "finish is idempotent");
        window.process(
            Event::Selection { start: 3, end: 3 },
            &mut surface,
            &mut foreground,
        );
        window.process(
            Event::Delete {
                forward: true,
                units: 1,
            },
            &mut surface,
            &mut foreground,
        );
        assert_eq!(
            probe.get_value(),
            "飞邮箱鸿",
            "one UTF16 unit cannot leave a half-surrogate"
        );
        window.process(
            Event::Preview {
                text: "中😀文".into(),
                start: -1,
                end: -1,
            },
            &mut surface,
            &mut foreground,
        );
        window.process(
            Event::Selection { start: 4, end: 6 },
            &mut surface,
            &mut foreground,
        );
        window.process(
            Event::Delete {
                forward: false,
                units: 1,
            },
            &mut surface,
            &mut foreground,
        );
        assert_eq!(
            probe.get_value(),
            "飞邮箱鸿",
            "editing preedit cannot mutate committed text"
        );
        window.process(
            Event::Preview {
                text: "邮".into(),
                start: 3,
                end: 4,
            },
            &mut surface,
            &mut foreground,
        );
        window.process(Event::FinishPreview, &mut surface, &mut foreground);
        assert_eq!(
            probe.get_value(),
            "飞邮箱邮文鸿",
            "IME can replace a subrange within preedit"
        );
        use std::sync::atomic::Ordering;
        assert!(native::TEST_IME_ENABLED.load(Ordering::SeqCst));
        let shows = native::TEST_IME_SHOW_COUNT.load(Ordering::SeqCst);
        let props = window.ime.borrow().clone();
        window.input_method_request(InputMethodRequest::Update(props));
        assert_eq!(
            native::TEST_IME_SHOW_COUNT.load(Ordering::SeqCst),
            shows,
            "caret/text updates must not reopen a keyboard hidden by the user"
        );
        let props = window.ime.borrow().clone();
        window.input_method_request(InputMethodRequest::Enable(props));
        assert_eq!(
            native::TEST_IME_SHOW_COUNT.load(Ordering::SeqCst),
            shows + 1,
            "clicking an already-focused editor must reopen the keyboard"
        );
        {
            let _modal = suspend_input_method();
            assert!(!native::TEST_IME_ENABLED.load(Ordering::SeqCst));
            window.publish_ime();
            assert!(
                !native::TEST_IME_ENABLED.load(Ordering::SeqCst),
                "property updates cannot steal modal input"
            );
        }
        assert!(native::TEST_IME_ENABLED.load(Ordering::SeqCst));
        window.process(Event::Focus(false), &mut surface, &mut foreground);
        assert!(!native::TEST_IME_ENABLED.load(Ordering::SeqCst));
        window.process(Event::Focus(true), &mut surface, &mut foreground);
        assert!(native::TEST_IME_ENABLED.load(Ordering::SeqCst));
        let mut pixels = vec![Rgb8Pixel::default(); 200 * 80];
        window.renderer.render(&mut pixels, 200);
        assert!(
            pixels
                .iter()
                .any(|p| p.r == 0x22 && p.g == 0x55 && p.b == 0x88)
        );
        probe.hide().unwrap();
    }
}
