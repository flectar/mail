// The control gallery in docs/design-system/gallery.slint is built from the
// real components. Building it here keeps it from drifting from them, and
// drawing it once shows that every control lays out in both themes.

use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use slint::{ComponentHandle, Rgb8Pixel};
use std::rc::Rc;

slint::slint! {
    #[edition = "1.19"]
    export { Gallery } from "../docs/design-system/gallery.slint";
}

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

#[test]
fn the_control_gallery_draws_in_both_themes() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    for dark in [false, true] {
        let gallery = Gallery::new().unwrap();
        gallery.set_dark(dark);
        gallery.show().unwrap();
        slint::platform::update_timers_and_animations();
        let size = slint::PhysicalSize::new(1120, 1830);
        gallery.window().set_size(size);
        let mut pixels = vec![Rgb8Pixel::default(); (size.width * size.height) as usize];
        let drawn = window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, size.width as usize);
        });
        assert!(drawn, "the gallery should draw");
        assert!(
            pixels.iter().any(|pixel| *pixel != pixels[0]),
            "the gallery should draw its controls, not only a background"
        );
        gallery.hide().unwrap();
    }
}
