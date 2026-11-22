//! Icon button help, driven by pointer and keyboard without a display server.
//! The window draws one bubble for all its buttons: it follows the pointer
//! after keyboard use, sits centred under its button with a caret, stays
//! inside the window, is not covered by a pane drawn after the button, and
//! goes away with its button. A click or an activation keeps it away until
//! the pointer leaves, a button that moves does not leave it behind, a key
//! press hides the help under a resting pointer, and keyboard help moves
//! with a button scrolled into view.
use slint::platform::{
    Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use slint::{ComponentHandle, LogicalPosition};
use std::rc::Rc;
use std::time::{Duration, Instant};

slint::slint! {
    #[edition = "1.19"]
    import { Palette } from "std-widgets.slint";
    import { ToolbarIconButton } from "../ui/components/controls.slint";
    import { ProviderButton } from "../ui/components/provider-button.slint";
    import { ActionTooltip, ActionTooltipLayer } from "../ui/components/tooltip-bubble.slint";
    import { MotionSettings } from "../ui/design-system.slint";
    export { ActionTooltip }

    export component TooltipHarness inherits Window {
        in-out property <bool> dark;
        in-out property <bool> extra-shown: true;
        in-out property <length> star-x: 200px;
        preferred-width: 400px;
        preferred-height: 200px;
        background: root.dark ? #111111 : white;
        init => { MotionSettings.enabled = false; }
        changed dark => {
            Palette.color-scheme = root.dark ? ColorScheme.dark : ColorScheme.light;
        }

        Rectangle {
            x: 0px;
            y: 0px;
            width: 160px;
            height: 50px;
            ToolbarIconButton { x: 60px; y: 10px; tooltip-text: "Archive message"; }
        }
        ToolbarIconButton { x: root.star-x; y: 10px; tooltip-text: "Star message"; }
        // Against the window's trailing edge, like a toolbar's last action.
        ToolbarIconButton { x: 352px; y: 10px; tooltip-text: "More message actions"; }
        // A button that goes away while it is pointed at.
        if root.extra-shown : ToolbarIconButton { x: 280px; y: 10px; tooltip-text: "Snooze"; }
        // A button in a popup draws its own help, lined up with its trailing
        // edge so the popup's edge does not cut it off.
        ToolbarIconButton {
            x: 352px;
            y: 70px;
            tooltip-text: "Edit label";
            tooltip-above: true;
            tooltip-in-popup: true;
        }
        // A provider that can't be chosen yet, whose help above it says why.
        ProviderButton {
            x: 200px;
            y: 140px;
            width: 120px;
            enabled: false;
            text: "Gmail";
            tooltip-text: "Add app keys";
            foreground: black;
            muted: gray;
            surface: white;
            hover-surface: #eeeeee;
            button-border: #cccccc;
        }
        // The pane below the first button, drawn after it, like the message
        // below the reader's toolbar.
        Rectangle {
            x: 0px;
            y: 50px;
            width: 160px;
            height: 70px;
            background: root.dark ? #202020 : #f4f4f4;
        }

        ActionTooltipLayer {
            width: parent.width;
            height: parent.height;
        }
    }
}

const WIDTH: usize = 400;
const HEIGHT: usize = 200;
const ARCHIVE: f32 = 80.0;
const STAR: f32 = 220.0;
const SNOOZE: f32 = 300.0;
const MORE: f32 = 372.0;
const ROW: f32 = 30.0;
// Where the star button is, and where the test moves it to.
const STAR_X: f32 = 200.0;
const MOVED_STAR_X: f32 = 120.0;
const MOVED_STAR: f32 = 140.0;
// The popup's button, and a row through the middle of its bubble.
const POPUP_ROW: f32 = 90.0;
const POPUP_BUBBLE_ROW: usize = 48;
// The disabled provider, and a row through its bubble above it.
const PROVIDER: f32 = 260.0;
const PROVIDER_ROW: f32 = 164.0;
const PROVIDER_BUBBLE_Y: usize = 108;
// A spot with no button.
const EMPTY: (f32, f32) = (10.0, 100.0);
// Inside a bubble below the buttons, above its text, and inside its caret.
const BUBBLE_Y: usize = 60;
const CARET_Y: usize = 55;
// Several refresh intervals of the buttons and a check of the window, to
// show that help neither comes back nor goes missing in that time.
const SETTLE: Duration = Duration::from_millis(2500);
// How long to wait for something that has to happen, however slow the
// machine is.
const LIMIT: Duration = Duration::from_secs(5);

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

struct Harness {
    ui: TooltipHarness,
    window: Rc<MinimalSoftwareWindow>,
}

impl Harness {
    fn new() -> Self {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
        let ui = TooltipHarness::new().unwrap();
        ui.window()
            .set_size(slint::PhysicalSize::new(WIDTH as u32, HEIGHT as u32));
        ui.show().unwrap();
        Self { ui, window }
    }

    fn send(&self, event: WindowEvent) {
        self.ui.window().dispatch_event(event);
        slint::platform::update_timers_and_animations();
    }

    fn hover(&self, x: f32) {
        self.hover_at(x, ROW);
    }

    fn hover_at(&self, x: f32, y: f32) {
        // Two moves, as a pointer passing over the button reports.
        for x in [x - 4.0, x] {
            self.send(WindowEvent::PointerMoved {
                position: LogicalPosition::new(x, y),
            });
        }
    }

    // The pointer goes to a spot with no button.
    fn leave(&self) {
        self.hover_at(EMPTY.0, EMPTY.1);
    }

    // The pointer leaves the window, as a finger lifted from a touch screen
    // does: no move event follows it.
    fn exit(&self) {
        self.send(WindowEvent::PointerExited);
    }

    fn click(&self, x: f32) {
        self.click_at(x, ROW);
    }

    fn click_at(&self, x: f32, y: f32) {
        let position = LogicalPosition::new(x, y);
        self.send(WindowEvent::PointerMoved { position });
        self.send(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        self.send(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
    }

    fn key(&self, text: impl Into<slint::SharedString>) {
        let text = text.into();
        self.send(WindowEvent::KeyPressed { text: text.clone() });
        self.send(WindowEvent::KeyReleased { text });
    }

    // Lets the window's timers run for `time`.
    fn wait(&self, time: Duration) {
        let end = Instant::now() + time;
        while Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
            slint::platform::update_timers_and_animations();
        }
    }

    // Draws frames until `done` holds or `limit` has passed, so a slow
    // machine is waited for and a fast one is not.
    fn wait_until(&self, limit: Duration, done: impl Fn(&[slint::Rgb8Pixel]) -> bool) -> bool {
        let end = Instant::now() + limit;
        loop {
            if done(&self.frame()) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn frame(&self) -> Vec<slint::Rgb8Pixel> {
        slint::platform::update_timers_and_animations();
        let mut pixels = vec![slint::Rgb8Pixel::default(); WIDTH * HEIGHT];
        self.ui.window().request_redraw();
        self.window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, WIDTH);
        });
        pixels
    }
}

fn dark(pixels: &[slint::Rgb8Pixel], x: f32, y: usize) -> bool {
    let pixel = pixels[y * WIDTH + x as usize];
    pixel.r < 80 && pixel.g < 80 && pixel.b < 80
}

fn light(pixels: &[slint::Rgb8Pixel], x: f32, y: usize) -> bool {
    let pixel = pixels[y * WIDTH + x as usize];
    pixel.r > 200 && pixel.g > 200 && pixel.b > 200
}

// The last dark pixel of a row, which for a bubble is its right edge.
fn last_dark(pixels: &[slint::Rgb8Pixel], y: usize) -> Option<usize> {
    (0..WIDTH).rev().find(|x| dark(pixels, *x as f32, y))
}

#[test]
fn window_bubble_follows_the_pointer_and_its_button() {
    let harness = Harness::new();

    // Over the pane drawn after the button, which would cover a bubble the
    // button drew itself.
    harness.hover(ARCHIVE);
    let frame = harness.frame();
    assert!(
        dark(&frame, ARCHIVE, BUBBLE_Y),
        "hovered button shows its help above the next pane"
    );
    assert!(
        dark(&frame, ARCHIVE, CARET_Y),
        "the caret points at the button"
    );
    assert!(!dark(&frame, STAR, BUBBLE_Y));

    // The last action's bubble moves inward, its caret still on the button.
    harness.hover(MORE);
    let frame = harness.frame();
    assert!(dark(&frame, MORE, BUBBLE_Y));
    assert!(dark(&frame, MORE, CARET_Y));
    assert!(
        !dark(&frame, WIDTH as f32 - 4.0, BUBBLE_Y),
        "bubble stays inside"
    );
    assert!(!dark(&frame, ARCHIVE, BUBBLE_Y), "one bubble at a time");

    // A key pressed while a clicked button keeps the focus switches help to
    // the keyboard, though the click itself hid it. Activating the button
    // from the keyboard hides it again, and moving the pointer over another
    // button brings the pointer's help back.
    harness.click(STAR);
    harness.key("j");
    assert!(
        dark(&harness.frame(), STAR, BUBBLE_Y),
        "focused button's help"
    );
    harness.key(slint::platform::Key::Return);
    harness.wait(SETTLE);
    assert!(
        !dark(&harness.frame(), STAR, BUBBLE_Y),
        "help stays away after the keyboard activates the button"
    );
    harness.hover(ARCHIVE);
    let frame = harness.frame();
    assert!(
        dark(&frame, ARCHIVE, BUBBLE_Y),
        "help follows the pointer again"
    );
    assert!(!dark(&frame, STAR, BUBBLE_Y));

    // A click hides the help, and it stays hidden while the pointer rests on
    // the button, over whatever the click opened, through many refreshes.
    harness.click(ARCHIVE);
    assert!(
        !dark(&harness.frame(), ARCHIVE, BUBBLE_Y),
        "a click hides the help"
    );
    harness.wait(SETTLE);
    assert!(
        !dark(&harness.frame(), ARCHIVE, BUBBLE_Y),
        "and it does not come back under a resting pointer"
    );
    // The button keeps the focus the click gave it, but that is not keyboard
    // focus: with the pointer away there is no help to show.
    harness.leave();
    harness.wait(SETTLE);
    assert!(
        !dark(&harness.frame(), ARCHIVE, BUBBLE_Y),
        "a clicked button shows no help once the pointer has left"
    );
    // Without a move event, as after a tap, the pointer leaving is all there
    // is to release the help; the same again from a new click.
    harness.click(ARCHIVE);
    harness.exit();
    harness.wait(SETTLE);
    assert!(
        !dark(&harness.frame(), ARCHIVE, BUBBLE_Y),
        "a tapped button shows no help once the pointer has gone"
    );
    // Pointing at it again is a new hover: the help was released, not left
    // to the focus the click gave the button.
    harness.hover(ARCHIVE);
    assert!(
        dark(&harness.frame(), ARCHIVE, BUBBLE_Y),
        "the help returns when the pointer comes back"
    );

    // Slint takes one platform per process, so the rest runs here too.
    harness.hover(SNOOZE);
    harness.wait(SETTLE);
    assert!(
        dark(&harness.frame(), SNOOZE, BUBBLE_Y),
        "help stays while its button is pointed at"
    );
    // The window removes the button when it next draws.
    harness.ui.set_extra_shown(false);
    assert!(
        harness.wait_until(LIMIT, |frame| !dark(frame, SNOOZE, BUBBLE_Y)),
        "a removed button leaves no help behind"
    );

    // A button that moves under a resting pointer takes its help back, and
    // the help appears neither at the old place nor at the new one.
    harness.hover(STAR);
    assert!(dark(&harness.frame(), STAR, BUBBLE_Y));
    harness.ui.set_star_x(MOVED_STAR_X);
    assert!(
        harness.wait_until(LIMIT, |frame| !dark(frame, STAR, BUBBLE_Y)),
        "a button that moved takes its help back"
    );
    harness.wait(SETTLE);
    let frame = harness.frame();
    assert!(
        !dark(&frame, STAR, BUBBLE_Y),
        "it does not return at the old place"
    );
    assert!(
        !dark(&frame, MOVED_STAR, BUBBLE_Y),
        "nor appear at the new one"
    );
    harness.leave();
    harness.ui.set_star_x(STAR_X);
    harness.frame();

    // A key pressed elsewhere in the window, which the window's event filter
    // reports, hands help to the keyboard: the help of the button under the
    // resting pointer goes and stays away, and pointing at a button brings
    // the pointer's help back.
    harness.hover(MORE);
    assert!(dark(&harness.frame(), MORE, BUBBLE_Y));
    harness.ui.global::<ActionTooltip>().set_keyboard(true);
    harness.wait(SETTLE);
    assert!(
        !dark(&harness.frame(), MORE, BUBBLE_Y),
        "a key press hides the help under a resting pointer"
    );
    harness.hover(MORE);
    assert!(
        dark(&harness.frame(), MORE, BUBBLE_Y),
        "pointing at the button brings it back"
    );
    harness.leave();

    // Keyboard help moves with a button that moves right after it took the
    // focus, as when Slint scrolls it into view, instead of staying behind or
    // going away.
    harness.click(STAR);
    harness.key("j");
    assert!(
        dark(&harness.frame(), STAR, BUBBLE_Y),
        "focused button's help"
    );
    harness.ui.set_star_x(MOVED_STAR_X);
    assert!(
        harness.wait_until(LIMIT, |frame| dark(frame, MOVED_STAR, BUBBLE_Y)
            && !dark(frame, STAR, BUBBLE_Y)),
        "keyboard help follows its button"
    );
    harness.ui.set_star_x(STAR_X);
    harness.hover(MORE);
    harness.leave();

    // In a popup the bubble lines up with the button's trailing edge, so the
    // popup's edge does not cut it off.
    harness.hover_at(MORE, POPUP_ROW);
    let frame = harness.frame();
    let right = last_dark(&frame, POPUP_BUBBLE_ROW).expect("the popup button shows its help");
    assert!(
        (MORE as usize + 14..=MORE as usize + 20).contains(&right),
        "the popup help ends at the button's edge, not centred past it: {right}"
    );
    harness.leave();

    // A disabled provider's help says why it is disabled, and a click on it
    // keeps the help up, since the click does nothing else.
    harness.hover_at(PROVIDER, PROVIDER_ROW);
    assert!(
        dark(&harness.frame(), PROVIDER, PROVIDER_BUBBLE_Y),
        "a disabled provider shows its help"
    );
    harness.click_at(PROVIDER, PROVIDER_ROW);
    harness.wait(SETTLE);
    assert!(
        dark(&harness.frame(), PROVIDER, PROVIDER_BUBBLE_Y),
        "a click on a disabled provider keeps its help"
    );
    harness.leave();

    // A click gives a button the focus, but not keyboard focus. When the
    // window goes inactive and comes back, as a phone app does after another
    // app was used, the button gets its focus back without showing its help.
    harness.click(ARCHIVE);
    harness.exit();
    harness.send(WindowEvent::WindowActiveChanged(false));
    harness.send(WindowEvent::WindowActiveChanged(true));
    harness.wait(SETTLE);
    assert!(
        !dark(&harness.frame(), ARCHIVE, BUBBLE_Y),
        "a clicked button shows no help when its window comes back"
    );

    harness.ui.set_dark(true);
    harness.hover(STAR);
    let frame = harness.frame();
    assert!(
        light(&frame, STAR, BUBBLE_Y),
        "a light bubble on a dark window"
    );
    assert!(light(&frame, STAR, CARET_Y), "and a light caret");
}
