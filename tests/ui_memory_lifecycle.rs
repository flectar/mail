use flectar_mail::{AccountRow, AppWindow};
use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use slint::{ComponentHandle, ModelRc, Rgb8Pixel, VecModel};
use std::rc::Rc;

struct Headless(Rc<MinimalSoftwareWindow>);
impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

#[test]
fn deferred_mailbox_and_composer_can_be_created_again() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    let app = AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(1320, 800));
    app.set_startup_ready(true);
    app.set_active_view("mail".into());
    app.show().unwrap();
    let render = || {
        slint::platform::update_timers_and_animations();
        app.window().request_redraw();
        let mut pixels = vec![Rgb8Pixel::default(); 1320 * 800];
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, 1320);
        });
        std::thread::sleep(std::time::Duration::from_millis(5));
        slint::platform::update_timers_and_animations();
        pixels
    };
    let welcome = render();
    let accounts = ModelRc::new(VecModel::from(vec![AccountRow {
        id: 1,
        name: "Memory test".into(),
        email: "memory@example.invalid".into(),
        ..Default::default()
    }]));
    app.set_connected_accounts(accounts.clone());
    let mailbox = render();
    assert!(welcome != mailbox);
    app.set_compose_to("recipient@example.invalid".into());
    app.set_compose_subject("Preserved draft".into());
    app.set_compose_body("Draft text".into());
    for _ in 0..2 {
        app.set_compose_open(true);
        let compose = render();
        render();
        assert!(compose != mailbox);
        assert!(app.get_compose_editor_width() > 1.0);
        assert!(app.get_compose_editor_viewport_height() > 1.0);
        app.set_compose_open(false);
        render();
        assert_eq!(app.get_compose_subject(), "Preserved draft");
        assert_eq!(app.get_compose_body(), "Draft text");
    }
    app.set_connected_accounts(ModelRc::default());
    render();
    app.set_connected_accounts(accounts);
    render();
    assert_eq!(app.get_compose_to(), "recipient@example.invalid");
}
