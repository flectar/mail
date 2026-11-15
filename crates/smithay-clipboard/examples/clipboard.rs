// The example just demonstrates how to integrate the smithay-clipboard into the
// application. For more details on what is going on, consult the
// `smithay-client-toolkit` examples.

use sctk::compositor::{CompositorHandler, CompositorState};
use sctk::output::{OutputHandler, OutputState};
use sctk::reexports::calloop::{EventLoop, LoopHandle};
use sctk::reexports::calloop_wayland_source::WaylandSource;
use sctk::reexports::client::globals::registry_queue_init;
use sctk::reexports::client::protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface};
use sctk::reexports::client::{Connection, Proxy, QueueHandle};
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::seat::keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers};
use sctk::seat::{Capability, SeatHandler, SeatState};
use sctk::shell::xdg::window::{Window, WindowConfigure, WindowDecorations, WindowHandler};
use sctk::shell::xdg::XdgShell;
use sctk::shell::WaylandSurface;
use sctk::shm::slot::{Buffer, SlotPool};
use sctk::shm::{Shm, ShmHandler};
use sctk::{
    delegate_compositor, delegate_keyboard, delegate_output, delegate_registry, delegate_seat,
    delegate_shm, delegate_xdg_shell, delegate_xdg_window, registry_handlers,
};
use smithay_clipboard::Clipboard;

const MIN_DIM_SIZE: usize = 256;

fn main() {
    let connection = Connection::connect_to_env().unwrap();
    let (globals, event_queue) = registry_queue_init(&connection).unwrap();
    let queue_handle = event_queue.handle();
    let mut event_loop: EventLoop<SimpleWindow> =
        EventLoop::try_new().expect("Failed to initialize the event loop!");
    let loop_handle = event_loop.handle();
    WaylandSource::new(connection.clone(), event_queue).insert(loop_handle).unwrap();

    let compositor =
        CompositorState::bind(&globals, &queue_handle).expect("wl_compositor not available");
    let xdg_shell = XdgShell::bind(&globals, &queue_handle).expect("xdg shell is not available");

    let shm = Shm::bind(&globals, &queue_handle).expect("wl shm is not available.");
    let surface = compositor.create_surface(&queue_handle);
    let window = xdg_shell.create_window(surface, WindowDecorations::RequestServer, &queue_handle);

    window.set_title(String::from("smithay-clipboard example. Press C/c/P/p to copy/paste"));
    window.set_min_size(Some((MIN_DIM_SIZE as u32, MIN_DIM_SIZE as u32)));
    window.commit();

    let clipboard = unsafe { Clipboard::new(connection.display().id().as_ptr() as *mut _) };

    let pool = SlotPool::new(MIN_DIM_SIZE * MIN_DIM_SIZE * 4, &shm).expect("Failed to create pool");

    let mut simple_window = SimpleWindow {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &queue_handle),
        output_state: OutputState::new(&globals, &queue_handle),
        shm,
        clipboard,

        exit: false,
        first_configure: true,
        pool,
        width: 256,
        height: 256,
        buffer: None,
        window,
        keyboard: None,
        keyboard_focus: false,
        loop_handle: event_loop.handle(),
    };

    // We don't draw immediately, the configure will notify us when to first draw.
    loop {
        event_loop.dispatch(None, &mut simple_window).unwrap();

        if simple_window.exit {
            break;
        }
    }
}

struct SimpleWindow {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    clipboard: Clipboard,

    exit: bool,
    first_configure: bool,
    pool: SlotPool,
    width: u32,
    height: u32,
    buffer: Option<Buffer>,
    window: Window,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    keyboard_focus: bool,
    loop_handle: LoopHandle<'static, SimpleWindow>,
}

impl CompositorHandler for SimpleWindow {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
        // Not needed for this example.
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
        // Not needed for this example.
    }

    fn frame(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        self.draw(conn, qh);
    }
}

impl OutputHandler for SimpleWindow {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl WindowHandler for SimpleWindow {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        _window: &Window,
        configure: WindowConfigure,
        _serial: u32,
    ) {
        println!("Window configured to: {:?}", configure);

        self.buffer = None;
        self.width = configure.new_size.0.map(|v| v.get()).unwrap_or(256);
        self.height = configure.new_size.1.map(|v| v.get()).unwrap_or(256);

        // Initiate the first draw.
        if self.first_configure {
            self.first_configure = false;
            self.draw(conn, qh);
        }
    }
}

impl SeatHandler for SimpleWindow {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            println!("Set keyboard capability");
            let keyboard = self
                .seat_state
                .get_keyboard_with_repeat(
                    qh,
                    &seat,
                    None,
                    self.loop_handle.clone(),
                    Box::new(|_state, _wl_kbd, event| {
                        println!("Repeat: {:?} ", event);
                    }),
                )
                .expect("Failed to create keyboard");

            self.keyboard = Some(keyboard);
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_some() {
            println!("Unset keyboard capability");
            self.keyboard.take().unwrap().release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for SimpleWindow {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        keysyms: &[Keysym],
    ) {
        if self.window.wl_surface() == surface {
            println!("Keyboard focus on window with pressed syms: {keysyms:?}");
            self.keyboard_focus = true;
        }
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if self.window.wl_surface() == surface {
            println!("Release keyboard focus on window");
            self.keyboard_focus = false;
        }
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        match event.utf8.as_deref() {
            // Paste primary.
            Some("P") => match self.clipboard.load_primary() {
                Ok(contents) => println!("Paste from primary clipboard: {contents}"),
                Err(err) => eprintln!("Error loading from primary clipboard: {err}"),
            },
            // Paste clipboard.
            Some("p") => match self.clipboard.load() {
                Ok(contents) => println!("Paste from clipboard: {contents}"),
                Err(err) => eprintln!("Error loading from clipboard: {err}"),
            },
            // Copy primary.
            Some("C") => {
                let to_store = "Copy primary";
                self.clipboard.store_primary(to_store);
                println!("Copied string into primary clipboard: {}", to_store);
            },
            // Copy clipboard.
            Some("c") => {
                let to_store = "Copy";
                self.clipboard.store(to_store);
                println!("Copied string into clipboard: {}", to_store);
            },
            _ => (),
        }
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _event: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: u32,
    ) {
    }
}

impl ShmHandler for SimpleWindow {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl SimpleWindow {
    pub fn draw(&mut self, _conn: &Connection, qh: &QueueHandle<Self>) {
        let width = self.width;
        let height = self.height;
        let stride = self.width as i32 * 4;

        let buffer = self.buffer.get_or_insert_with(|| {
            self.pool
                .create_buffer(width as i32, height as i32, stride, wl_shm::Format::Argb8888)
                .expect("create buffer")
                .0
        });

        let canvas = match self.pool.canvas(buffer) {
            Some(canvas) => canvas,
            None => {
                // This should be rare, but if the compositor has not released the previous
                // buffer, we need double-buffering.
                let (second_buffer, canvas) = self
                    .pool
                    .create_buffer(
                        self.width as i32,
                        self.height as i32,
                        stride,
                        wl_shm::Format::Argb8888,
                    )
                    .expect("create buffer");
                *buffer = second_buffer;
                canvas
            },
        };

        // Draw to the window:
        canvas.chunks_exact_mut(4).enumerate().for_each(|(_, chunk)| {
            // ARGB color.
            let color = 0xFF181818u32;

            let array: &mut [u8; 4] = chunk.try_into().unwrap();
            *array = color.to_le_bytes();
        });

        // Damage the entire window
        self.window.wl_surface().damage_buffer(0, 0, self.width as i32, self.height as i32);

        // Request our next frame
        self.window.wl_surface().frame(qh, self.window.wl_surface().clone());

        // Attach and commit to present.
        buffer.attach_to(self.window.wl_surface()).expect("buffer attach");
        self.window.commit();
    }
}

delegate_compositor!(SimpleWindow);
delegate_output!(SimpleWindow);
delegate_shm!(SimpleWindow);

delegate_seat!(SimpleWindow);
delegate_keyboard!(SimpleWindow);

delegate_xdg_shell!(SimpleWindow);
delegate_xdg_window!(SimpleWindow);

delegate_registry!(SimpleWindow);

impl ProvidesRegistryState for SimpleWindow {
    registry_handlers![OutputState, SeatState,];

    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
}
