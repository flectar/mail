//! Where the app's windows were (#254). The main window comes back at the
//! size and place it had, and maximized again if it was, and a composer
//! window opens at the size the last one had. Each device keeps its own, in
//! `windows.json` beside the saved tabs.
//!
//! Windows and macOS give the main window its place back once it is still on
//! a screen, as Microsoft's and Apple's guidance for restoring windows has it.
//! Linux leaves the place to the window manager: Wayland doesn't let an
//! application place its windows, and GNOME's guidance is to leave it to the
//! window manager on X11 as well. The system places composer windows
//! everywhere. A window closed in full screen comes back maximized, so the app
//! never starts covering the whole screen.

use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, Timer, TimerMode};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const FILE_NAME: &str = "windows.json";
/// A save waits until the window has stopped moving and resizing.
const SAVE_DELAY: Duration = Duration::from_secs(1);
/// Sizes outside this range come from a damaged file. The windows' own
/// minimum sizes still apply to what is inside it.
const MIN_SIZE: f32 = 200.0;
const MAX_SIZE: f32 = 100_000.0;
/// How often, and for how long, a window waits for its native window to be
/// maximized on Windows.
#[cfg(target_os = "windows")]
const MAXIMIZE_POLL: Duration = Duration::from_millis(50);
#[cfg(target_os = "windows")]
const MAXIMIZE_DEADLINE: Duration = Duration::from_secs(10);

/// Which window a place is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Main,
    Composer,
}

/// A window's place when it isn't maximized, and whether it was.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Place {
    /// Its size, in logical pixels.
    width: f32,
    height: f32,
    /// Where the top left corner of its frame is, as the platform's window
    /// positions go: physical pixels on Windows, points on macOS. None where
    /// the platform places the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    position: Option<[i32; 2]>,
    #[serde(default)]
    maximized: bool,
}

impl Place {
    fn has_size(&self) -> bool {
        (MIN_SIZE..=MAX_SIZE).contains(&self.width) && (MIN_SIZE..=MAX_SIZE).contains(&self.height)
    }

    /// Take in what the window is like now. A maximized or full screen
    /// window keeps the place it goes back to when restored, and a minimized
    /// one isn't anywhere the user put it.
    fn follow(&mut self, now: &WindowState) {
        if now.minimized {
            return;
        }
        self.maximized = now.maximized || now.full_screen;
        if self.maximized {
            return;
        }
        self.width = now.width;
        self.height = now.height;
        self.position = now.position;
    }
}

/// What a window is like, read from it.
#[derive(Clone, Copy, Debug, Default)]
struct WindowState {
    minimized: bool,
    maximized: bool,
    full_screen: bool,
    width: f32,
    height: f32,
    position: Option<[i32; 2]>,
}

impl WindowState {
    fn read(kind: Kind, window: &slint::Window) -> Self {
        let size = window.size().to_logical(window.scale_factor());
        Self {
            minimized: window.is_minimized(),
            maximized: window.is_maximized(),
            full_screen: window.is_fullscreen(),
            width: size.width,
            height: size.height,
            position: (kind == Kind::Main).then(|| position(window)).flatten(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Saved {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    main: Option<Place>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    composer: Option<Place>,
}

impl Saved {
    fn place(&self, kind: Kind) -> Option<Place> {
        match kind {
            Kind::Main => self.main,
            Kind::Composer => self.composer,
        }
    }

    fn place_mut(&mut self, kind: Kind) -> &mut Option<Place> {
        match kind {
            Kind::Main => &mut self.main,
            Kind::Composer => &mut self.composer,
        }
    }
}

/// What each kind of window needs while the app runs.
#[derive(Default)]
struct Windows {
    /// Reads the window once the event that moved it is handled, as Slint
    /// updates its maximized state only then.
    read: Timer,
    /// On Windows, the window is maximized once it is shown.
    #[cfg(target_os = "windows")]
    maximize_when_shown: Cell<bool>,
}

struct Places {
    path: PathBuf,
    runtime: tokio::runtime::Handle,
    saved: RefCell<Saved>,
    written: RefCell<Saved>,
    main: Windows,
    composer: Windows,
    save_timer: Timer,
    writes: Cell<u64>,
    newest_write: Arc<Mutex<u64>>,
}

thread_local! {
    static PLACES: RefCell<Option<Rc<Places>>> = const { RefCell::new(None) };
}

fn places() -> Option<Rc<Places>> {
    PLACES.with(|places| places.borrow().clone())
}

/// Read where the windows were. Call once, before the main window is shown.
pub(crate) fn install(data_dir: &Path, runtime: &tokio::runtime::Handle) {
    let path = data_dir.join(FILE_NAME);
    let saved = read_file(&path).unwrap_or_default();
    let places = Places {
        path,
        runtime: runtime.clone(),
        written: RefCell::new(saved.clone()),
        saved: RefCell::new(saved),
        main: Windows::default(),
        composer: Windows::default(),
        save_timer: Timer::default(),
        writes: Cell::new(0),
        newest_write: Arc::default(),
    };
    PLACES.with(|slot| *slot.borrow_mut() = Some(Rc::new(places)));
}

/// Give a window that isn't shown yet the place its kind had: the size, the
/// main window's position where the platform takes it, and maximized.
pub(crate) fn restore<C: ComponentHandle + 'static>(kind: Kind, component: &C) {
    let Some(places) = places() else {
        return;
    };
    let Some(place) = places.saved.borrow().place(kind) else {
        return;
    };
    let window = component.window();
    if place.has_size() {
        window.set_size(slint::LogicalSize::new(place.width, place.height));
    }
    if let Some([x, y]) = place.position
        && kind == Kind::Main
        && on_a_screen(x, y)
    {
        set_position(window, x, y);
    }
    if place.maximized {
        // winit shows a window it creates maximized at once, before its first
        // frame, and keeps the frame it made for the size as the one the window
        // is restored to, which is too tall for a window that draws its own
        // title bar. On Windows the window is maximized once it is shown.
        #[cfg(target_os = "windows")]
        places.windows(kind).maximize_when_shown.set(true);
        #[cfg(not(target_os = "windows"))]
        window.set_maximized(true);
    }
}

/// A window of `kind` was just shown. One that comes back maximized on
/// Windows is maximized once its native window is there, which Slint creates
/// on the event loop.
pub(crate) fn shown<C: ComponentHandle + 'static>(kind: Kind, component: &C) {
    #[cfg(target_os = "windows")]
    if places().is_some_and(|places| places.windows(kind).maximize_when_shown.replace(false)) {
        maximize_once_there(
            component.as_weak(),
            std::time::Instant::now() + MAXIMIZE_DEADLINE,
        );
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (kind, component);
}

/// Maximize the window once it has a native window, holding no strong
/// reference to it meanwhile, so one that never opens is freed.
#[cfg(target_os = "windows")]
fn maximize_once_there<C: ComponentHandle + 'static>(
    weak: slint::Weak<C>,
    deadline: std::time::Instant,
) {
    use slint::winit_030::WinitWindowAccessor;
    Timer::single_shot(MAXIMIZE_POLL, move || {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let window = component.window();
        if window.has_winit_window() && window.is_visible() {
            window.set_maximized(true);
        } else if std::time::Instant::now() < deadline {
            maximize_once_there(weak.clone(), deadline);
        }
    });
}

/// A window of `kind` moved or changed its size or state. Call from its winit
/// event filter, which runs before Slint handles the event.
pub(crate) fn changed<C: ComponentHandle + 'static>(kind: Kind, component: &C) {
    let Some(places) = places() else {
        return;
    };
    let weak = component.as_weak();
    places
        .windows(kind)
        .read
        .start(TimerMode::SingleShot, Duration::ZERO, move || {
            if let Some(component) = weak.upgrade() {
                follow(kind, component.window());
            }
        });
}

fn follow(kind: Kind, window: &slint::Window) {
    let Some(places) = places() else {
        return;
    };
    // A window being hidden or destroyed reports nothing the user chose.
    if !window.is_visible() {
        return;
    }
    let now = WindowState::read(kind, window);
    places
        .saved
        .borrow_mut()
        .place_mut(kind)
        .get_or_insert_default()
        .follow(&now);
    let weak = Rc::downgrade(&places);
    places
        .save_timer
        .start(TimerMode::SingleShot, SAVE_DELAY, move || {
            if let Some(places) = weak.upgrade() {
                places.save(false);
            }
        });
}

/// Write the places now, for an app that is about to exit: timers stop with
/// the event loop.
pub(crate) fn flush() {
    if let Some(places) = places() {
        places.save_timer.stop();
        places.save(true);
    }
}

impl Places {
    fn windows(&self, kind: Kind) -> &Windows {
        match kind {
            Kind::Main => &self.main,
            Kind::Composer => &self.composer,
        }
    }

    /// Write what changed since the last write. Writes run one at a time, and
    /// one that finds a newer write done skips, so older places can never
    /// replace newer ones.
    fn save(&self, blocking: bool) {
        let saved = self.saved.borrow().clone();
        if *self.written.borrow() == saved {
            return;
        }
        *self.written.borrow_mut() = saved.clone();
        let number = self.writes.get() + 1;
        self.writes.set(number);
        let newest = Arc::clone(&self.newest_write);
        let path = self.path.clone();
        let write = move || {
            let mut newest = newest
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *newest > number {
                return;
            }
            *newest = number;
            if let Err(error) = write_file(&path, &saved) {
                eprintln!("could not save window places: {error}");
            }
        };
        if blocking {
            write();
        } else {
            self.runtime.spawn_blocking(write);
        }
    }
}

fn read_file(path: &Path) -> Option<Saved> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn write_file(path: &Path, saved: &Saved) -> Result<(), Box<dyn std::error::Error>> {
    let parent = path
        .parent()
        .ok_or("window places have no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, saved)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

/// The main window's position as the platform gives it back.
#[cfg(target_os = "windows")]
fn position(window: &slint::Window) -> Option<[i32; 2]> {
    let position = window.position();
    Some([position.x, position.y])
}

/// Points, which winit places windows in on macOS whatever the scale of the
/// screen they are on.
#[cfg(target_os = "macos")]
fn position(window: &slint::Window) -> Option<[i32; 2]> {
    let position = window.position().to_logical(window.scale_factor());
    Some([position.x.round() as i32, position.y.round() as i32])
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn position(_window: &slint::Window) -> Option<[i32; 2]> {
    None
}

#[cfg(target_os = "windows")]
fn set_position(window: &slint::Window, x: i32, y: i32) {
    window.set_position(slint::PhysicalPosition::new(x, y));
}

#[cfg(target_os = "macos")]
fn set_position(window: &slint::Window, x: i32, y: i32) {
    window.set_position(slint::LogicalPosition::new(x as f32, y as f32));
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn set_position(_window: &slint::Window, _x: i32, _y: i32) {}

/// How much of the top of a window must be on a screen to grab it there.
#[cfg(any(target_os = "windows", target_os = "macos"))]
const GRAB_WIDTH: i32 = 160;
#[cfg(any(target_os = "windows", target_os = "macos"))]
const GRAB_HEIGHT: i32 = 32;

/// Whether the top of a window at `x`, `y` is on one of the screens, which
/// one that was on may no longer be after a monitor was unplugged.
#[cfg(target_os = "windows")]
fn on_a_screen(x: i32, y: i32) -> bool {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromRect};
    let top = RECT {
        left: x,
        top: y,
        right: x.saturating_add(GRAB_WIDTH),
        bottom: y.saturating_add(GRAB_HEIGHT),
    };
    // SAFETY: `top` is a valid rectangle, and MonitorFromRect only reads it.
    !unsafe { MonitorFromRect(&top, MONITOR_DEFAULTTONULL) }.is_null()
}

/// winit counts macOS window positions from the top left of the main
/// display, while AppKit counts screen frames from its bottom left.
#[cfg(target_os = "macos")]
fn on_a_screen(x: i32, y: i32) -> bool {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let screens = NSScreen::screens(mtm);
    // The first screen is the main display, the one with the menu bar.
    if screens.count() == 0 {
        return false;
    }
    let main_height = screens.objectAtIndex(0).frame().size.height;
    let (left, top) = (f64::from(x), f64::from(y));
    let (right, bottom) = (left + f64::from(GRAB_WIDTH), top + f64::from(GRAB_HEIGHT));
    (0..screens.count()).any(|index| {
        let frame = screens.objectAtIndex(index).visibleFrame();
        let screen_top = main_height - (frame.origin.y + frame.size.height);
        let screen_bottom = screen_top + frame.size.height;
        let screen_right = frame.origin.x + frame.size.width;
        left < screen_right && right > frame.origin.x && top < screen_bottom && bottom > screen_top
    })
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn on_a_screen(_x: i32, _y: i32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normal(width: f32, height: f32, position: Option<[i32; 2]>) -> WindowState {
        WindowState {
            width,
            height,
            position,
            ..WindowState::default()
        }
    }

    /// A maximized or full screen window keeps the size and position it is
    /// restored to, and a minimized one changes nothing.
    #[test]
    fn places_keep_the_restored_size_and_position() {
        let mut place = Place::default();
        place.follow(&normal(900.0, 700.0, Some([40, 60])));
        assert_eq!(
            place,
            Place {
                width: 900.0,
                height: 700.0,
                position: Some([40, 60]),
                maximized: false,
            }
        );

        place.follow(&WindowState {
            maximized: true,
            ..normal(1920.0, 1040.0, Some([0, 0]))
        });
        assert!(place.maximized);
        assert_eq!(
            (place.width, place.height, place.position),
            (900.0, 700.0, Some([40, 60]))
        );

        place.follow(&WindowState {
            minimized: true,
            ..normal(160.0, 28.0, Some([-32000, -32000]))
        });
        assert!(place.maximized);
        assert_eq!(place.position, Some([40, 60]));

        place.follow(&WindowState {
            full_screen: true,
            ..normal(2560.0, 1440.0, Some([0, 0]))
        });
        assert!(place.maximized);
        assert_eq!((place.width, place.height), (900.0, 700.0));

        place.follow(&normal(1000.0, 650.0, None));
        assert_eq!(
            place,
            Place {
                width: 1000.0,
                height: 650.0,
                position: None,
                maximized: false,
            }
        );
    }

    /// A size out of range, as a damaged file has, isn't given back.
    #[test]
    fn sizes_out_of_range_are_left_alone() {
        let place = |width, height| Place {
            width,
            height,
            ..Place::default()
        };
        assert!(place(900.0, 700.0).has_size());
        assert!(!place(0.0, 0.0).has_size());
        assert!(!place(900.0, 20.0).has_size());
        assert!(!place(f32::NAN, 700.0).has_size());
        assert!(!place(f32::INFINITY, 700.0).has_size());
        assert!(!place(900.0, 1.0e9).has_size());
    }

    /// Places round-trip through the file, and a damaged or missing file
    /// reads as no places at all.
    #[test]
    fn places_round_trip_and_damaged_files_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        assert_eq!(read_file(&path), None);

        let saved = Saved {
            main: Some(Place {
                width: 1200.0,
                height: 760.0,
                position: Some([-1400, 120]),
                maximized: true,
            }),
            composer: Some(Place {
                width: 800.0,
                height: 740.0,
                position: None,
                maximized: false,
            }),
        };
        write_file(&path, &saved).unwrap();
        assert_eq!(read_file(&path), Some(saved));

        std::fs::write(&path, b"{\"main\": {\"width\": \"wide\"}}").unwrap();
        assert_eq!(read_file(&path), None);
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(read_file(&path), None);
        // Fields a later version adds don't keep the rest from loading.
        std::fs::write(
            &path,
            br#"{"main":{"width":1000,"height":700,"maximized":false,"monitor":"DP-1"},"reader":{}}"#,
        )
        .unwrap();
        assert_eq!(
            read_file(&path).and_then(|saved| saved.main),
            Some(Place {
                width: 1000.0,
                height: 700.0,
                position: None,
                maximized: false,
            })
        );
    }
}
