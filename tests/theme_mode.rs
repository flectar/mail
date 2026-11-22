//! The System theme follows the OS. Choosing it again after Dark or Light
//! must pick up the OS scheme, keep following OS changes, and windows opened
//! while it is chosen must match.

use flectar_mail::{AppTheme, AppWindow, ComposerWindow, MessageColors};
use i_slint_core::items::ColorScheme;
use i_slint_core::window::WindowInner;
use slint::ComponentHandle;
use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::rc::Rc;

struct Headless;

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
    }
}

/// What the platform backend reports when the OS appearance changes.
fn set_os_scheme(app: &AppWindow, scheme: ColorScheme) {
    WindowInner::from_pub(app.window())
        .context()
        .set_color_scheme(scheme);
}

/// Whether the main window is dark once its `changed` handlers ran, as they
/// do before every frame.
fn dark(app: &AppWindow) -> bool {
    slint::platform::update_timers_and_animations();
    let dark = app.global::<AppTheme>().get_dark();
    assert_eq!(
        app.global::<MessageColors>().get_dark(),
        dark,
        "message colors follow the theme"
    );
    dark
}

fn choose(app: &AppWindow, mode: &str) {
    app.set_theme_mode(mode.into());
}

#[test]
fn system_theme_follows_the_os_after_a_forced_one() {
    slint::platform::set_platform(Box::new(Headless)).unwrap();
    let app = AppWindow::new().unwrap();
    set_os_scheme(&app, ColorScheme::Dark);
    assert!(dark(&app), "System on a dark OS");

    for forced in ["dark", "light"] {
        choose(&app, forced);
        assert_eq!(dark(&app), forced == "dark", "{forced} is forced");
        choose(&app, "system");
        assert!(dark(&app), "System after {forced} on a dark OS");
    }

    set_os_scheme(&app, ColorScheme::Light);
    assert!(!dark(&app), "System follows the OS turning light");
    choose(&app, "dark");
    assert!(dark(&app), "Dark on a light OS");
    choose(&app, "system");
    assert!(!dark(&app), "System after Dark on a light OS");
    set_os_scheme(&app, ColorScheme::Dark);
    assert!(dark(&app), "System follows the OS turning dark");

    let composer = ComposerWindow::new().unwrap();
    slint::platform::update_timers_and_animations();
    assert!(
        composer.global::<AppTheme>().get_dark(),
        "a composer window opened on System matches a dark OS"
    );
    set_os_scheme(&app, ColorScheme::Light);
    slint::platform::update_timers_and_animations();
    assert!(
        !composer.global::<AppTheme>().get_dark(),
        "the composer window follows the OS too"
    );
}
