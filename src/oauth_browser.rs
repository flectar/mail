//! Keep the redirect listener alive when a desktop browser cannot be launched.
use super::*;

pub(super) fn begin(app: slint::Weak<AppWindow>, url: String) -> Result<(), String> {
    let link = url.clone();
    app.upgrade_in_event_loop(move |app| {
        if app.get_oauth_in_progress() {
            app.set_oauth_authorization_url(link.into());
            app.set_oauth_browser_error(false);
        }
    })
    .map_err(|e| e.to_string())?;
    launch(app, url);
    Ok(())
}

fn launch(app: slint::Weak<AppWindow>, url: String) {
    std::thread::spawn(move || {
        let failed = browser::open(&url).is_err();
        let _ = app.upgrade_in_event_loop(move |app| {
            if app.get_oauth_in_progress() && app.get_oauth_authorization_url().as_str() == url {
                app.set_oauth_browser_error(failed);
            }
        });
    });
}

pub(super) fn register(app: &AppWindow) {
    let weak = app.as_weak();
    app.on_retry_oauth_browser(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let url = app.get_oauth_authorization_url().to_string();
        if app.get_oauth_in_progress() && !url.is_empty() {
            launch(app.as_weak(), url);
        }
    });
    let weak = app.as_weak();
    app.on_copy_oauth_link(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.get_oauth_in_progress() && !app.get_oauth_authorization_url().is_empty() {
            app.invoke_copy_oauth_authorization_url();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::platform::{
        Platform, WindowAdapter,
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    };
    struct Headless(Rc<MinimalSoftwareWindow>, Rc<RefCell<String>>);
    impl Platform for Headless {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
        fn set_clipboard_text(&self, text: &str, _: slint::platform::Clipboard) {
            *self.1.borrow_mut() = text.to_owned();
        }
    }
    #[test]
    fn onboarding_copies_active_link_without_a_mailbox_and_ignores_finished_sign_in() {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        let clipboard = Rc::new(RefCell::new(String::new()));
        slint::platform::set_platform(Box::new(Headless(window.clone(), clipboard.clone())))
            .unwrap();
        let app = AppWindow::new().unwrap();
        register(&app);
        app.set_startup_ready(true);
        app.set_oauth_in_progress(true);
        app.set_oauth_authorization_url(
            "https://accounts.google.com/o/oauth2/v2/auth?state=test".into(),
        );
        app.window().set_size(slint::PhysicalSize::new(390, 640));
        app.show().unwrap();
        slint::platform::update_timers_and_animations();
        app.invoke_copy_oauth_link();
        assert_eq!(
            *clipboard.borrow(),
            app.get_oauth_authorization_url().as_str()
        );
        app.set_oauth_in_progress(false);
        app.set_oauth_authorization_url("".into());
        *clipboard.borrow_mut() = "keep clipboard".into();
        app.invoke_copy_oauth_link();
        assert_eq!(*clipboard.borrow(), "keep clipboard");
    }
}
