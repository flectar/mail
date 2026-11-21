//! Icon button help bubbles, driven by pointer and keyboard without a display
//! server: they follow the pointer after keyboard use, sit centred under
//! their button with a caret, and stay inside the window.
use slint::platform::{
    Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use slint::{ComponentHandle, LogicalPosition};
use std::rc::Rc;

slint::slint! {
    #[edition = "1.19"]
    import { ToolbarIconButton } from "../ui/components/controls.slint";
    import { MotionSettings } from "../ui/design-system.slint";
    import { WindowChrome } from "../ui/window-chrome.slint";

    export component TooltipHarness inherits Window {
        preferred-width: 400px;
        preferred-height: 120px;
        background: white;
        init => {
            MotionSettings.enabled = false;
            WindowChrome.window-width = 400px;
        }

        ToolbarIconButton { x: 60px; y: 10px; tooltip-text: "Archive message"; }
        ToolbarIconButton { x: 200px; y: 10px; tooltip-text: "Star message"; }
        // Against the window's trailing edge, like a toolbar's last action.
        ToolbarIconButton { x: 352px; y: 10px; tooltip-text: "More message actions"; }
    }
}

const WIDTH: usize = 400;
const HEIGHT: usize = 120;
const ARCHIVE: f32 = 80.0;
const STAR: f32 = 220.0;
const MORE: f32 = 372.0;
const ROW: f32 = 30.0;
// Inside a bubble below the buttons, above its text, and inside its caret.
const BUBBLE_Y: usize = 60;
const CARET_Y: usize = 55;

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
        // Two moves, as a pointer passing over the button reports.
        for x in [x - 4.0, x] {
            self.send(WindowEvent::PointerMoved {
                position: LogicalPosition::new(x, ROW),
            });
        }
    }

    fn click(&self, x: f32) {
        let position = LogicalPosition::new(x, ROW);
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

    fn key(&self, text: &str) {
        self.send(WindowEvent::KeyPressed { text: text.into() });
        self.send(WindowEvent::KeyReleased { text: text.into() });
    }

    fn frame(&self) -> Vec<slint::Rgb8Pixel> {
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

#[test]
fn bubble_follows_the_pointer_and_stays_in_the_window() {
    let harness = Harness::new();

    harness.hover(ARCHIVE);
    let frame = harness.frame();
    assert!(
        dark(&frame, ARCHIVE, BUBBLE_Y),
        "hovered button shows its help"
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

    // A key pressed while a clicked button keeps the focus switches help to
    // the keyboard. Moving the pointer over another button brings it back.
    harness.click(STAR);
    harness.key("j");
    assert!(
        dark(&harness.frame(), STAR, BUBBLE_Y),
        "focused button's help"
    );
    harness.hover(ARCHIVE);
    let frame = harness.frame();
    assert!(
        dark(&frame, ARCHIVE, BUBBLE_Y),
        "help follows the pointer again"
    );
    assert!(!dark(&frame, STAR, BUBBLE_Y));
}
