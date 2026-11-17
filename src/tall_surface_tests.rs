//! Surfaces that grow with their content, such as a long server diagnostic or
//! hundreds of sender identities. Before Slint 1.19, the software renderer
//! (CPU mode) panicked when it painted a rectangle reaching past ±32767
//! physical pixels (#35).
use super::*;
use slint::Rgb8Pixel;
use slint::platform::{
    Platform, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};

struct Headless(Rc<MinimalSoftwareWindow>);
impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

slint::slint! {
    #[edition = "1.19"]
    import { ScrollView } from "std-widgets.slint";
    import { CopyableDiagnostic } from "../ui/components/copyable-diagnostic.slint";

    component Panel inherits Rectangle {
        in property <int> rows;
        background: #00ff00;
        border-radius: 10px;
        border-width: 1px;
        border-color: #000000;
        VerticalLayout {
            for row in root.rows : Rectangle { height: 50px; }
        }
    }

    export component TallSurfaces inherits Window {
        in property <string> status;
        in property <int> rows;
        in-out property <length> content-y <=> scroll.content-y;
        out property <length> status-height: status-box.height;
        out property <length> panel-y: panel.y;
        out property <length> panel-height: panel.height;
        width: 400px;
        height: 300px;
        background: #ffffff;
        scroll := ScrollView {
            content-width: self.visible-width;
            content-height: column.preferred-height;
            column := VerticalLayout {
                status-box := Rectangle {
                    height: max(36px, status-text.preferred-height + 16px);
                    background: #ff0000;
                    border-radius: 8px;
                    border-width: 1px;
                    border-color: #000000;
                    status-text := CopyableDiagnostic {
                        x: 10px;
                        y: 8px;
                        width: parent.width - 20px;
                        text: root.status;
                        text-color: #000000;
                        font-size: 12px;
                    }
                }
                panel := Panel { rows: root.rows; }
            }
        }
    }

    // Avatars and attachment thumbnails clip to their rounded corners, and a
    // long message or thread scrolls them far out of view.
    export component RoundedClips inherits Window {
        in property <length> scroll-y;
        width: 400px;
        height: 300px;
        background: #ffffff;
        Flickable {
            content-height: 40400px;
            content-y: root.scroll-y;
            far := Rectangle {
                x: 20px;
                y: 20000px;
                width: 100px;
                height: 40px;
                border-radius: 8px;
                clip: true;
                background: #ff0000;
            }
            tall := Rectangle {
                x: 140px;
                y: 50px;
                width: 200px;
                height: 40000px;
                border-radius: 12px;
                border-width: 2px;
                border-color: #000000;
                clip: true;
                background: #0000ff;
                Rectangle { background: #00ff00; }
            }
        }
    }
}

#[test]
fn tall_surfaces_paint_with_the_software_renderer() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    let ui = TallSurfaces::new().unwrap();
    ui.window()
        .dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 2.0 });
    let (width, height) = (800, 600);
    ui.window()
        .set_size(slint::PhysicalSize::new(width, height));
    ui.set_status("A long server diagnostic\n".repeat(3_000).into());
    ui.set_rows(400);
    ui.show().unwrap();
    let mut pixels = vec![Rgb8Pixel::default(); (width * height) as usize];
    // Samples inside each surface's left padding, clear of text and border.
    let mut fill_at = |content_y: f32, y: u32| {
        ui.set_content_y(-content_y);
        ui.window().request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, width as usize);
        });
        let pixel = pixels[(y * 2 * width + 8) as usize];
        (pixel.r, pixel.g, pixel.b)
    };
    let red = (255, 0, 0);
    let green = (0, 255, 0);
    assert_eq!(fill_at(0.0, 20), red);
    assert!(
        ui.get_status_height() * 2.0 > 2.0 * f32::from(i16::MAX),
        "the status box must exceed the renderer's range in both directions"
    );
    assert_eq!(fill_at(ui.get_status_height() / 2.0, 150), red);
    assert!(ui.get_panel_height() * 2.0 > f32::from(i16::MAX));
    let panel_middle = ui.get_panel_y() + ui.get_panel_height() / 2.0;
    assert_eq!(fill_at(panel_middle, 150), green);
    let end = ui.get_panel_y() + ui.get_panel_height() - 300.0;
    assert_eq!(fill_at(end, 280), green);
    ui.hide().unwrap();
}

#[test]
fn account_preferences_paint_hundreds_of_sender_identities() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    let app = AppWindow::new().unwrap();
    app.window()
        .dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 2.0 });
    app.set_startup_ready(true);
    app.set_startup_hydrated(true);
    app.set_connected_accounts(ModelRc::new(VecModel::from(vec![AccountRow {
        id: 1,
        email: "person@example.com".into(),
        ..Default::default()
    }])));
    app.set_active_view("mail".into());
    app.set_settings_tab("Accounts".into());
    app.invoke_open_settings();
    // Servers can report hundreds of aliases for one account.
    let preferences = app.global::<AccountMailPreferences>();
    preferences.set_account_id(1);
    preferences.set_account_label("person@example.com".into());
    preferences.set_sender_identity_rows(ModelRc::new(VecModel::from(
        (0..400)
            .map(|index| SenderIdentityRow {
                email: format!("alias{index}@example.com").into(),
                primary: index == 0,
                delivery: "confirmed".into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    // A tall window shows the top of the panel without scrolling.
    let (width, height) = (2560, 3200);
    app.window()
        .set_size(slint::PhysicalSize::new(width, height));
    app.show().unwrap();
    let mut pixels = vec![Rgb8Pixel::default(); (width * height) as usize];
    let mut paint = || {
        slint::platform::update_timers_and_animations();
        app.window().request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, width as usize);
        });
    };
    paint();
    for _ in 0..12 {
        app.window().dispatch_event(WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(640.0, 800.0),
            delta_x: 0.0,
            delta_y: -2_000.0,
        });
        paint();
    }
    assert_eq!(app.get_active_view(), "settings");
    app.hide().unwrap();
}

#[test]
fn account_cards_paint_long_sync_errors() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    let app = AppWindow::new().unwrap();
    app.window()
        .dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 2.0 });
    app.set_startup_ready(true);
    app.set_startup_hydrated(true);
    // The card shows the server's last error in full until a sync succeeds.
    app.set_connected_accounts(ModelRc::new(VecModel::from(vec![AccountRow {
        id: 1,
        email: "person@example.com".into(),
        sync_error: "A long server diagnostic\n".repeat(3_000).into(),
        ..Default::default()
    }])));
    app.set_active_view("mail".into());
    app.set_settings_tab("Accounts".into());
    app.invoke_open_settings();
    let (width, height) = (2560, 1800);
    app.window()
        .set_size(slint::PhysicalSize::new(width, height));
    app.show().unwrap();
    let mut pixels = vec![Rgb8Pixel::default(); (width * height) as usize];
    let mut paint = || {
        slint::platform::update_timers_and_animations();
        app.window().request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, width as usize);
        });
    };
    paint();
    for _ in 0..30 {
        app.window().dispatch_event(WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(640.0, 600.0),
            delta_x: 0.0,
            delta_y: -2_000.0,
        });
        paint();
    }
    assert_eq!(app.get_active_view(), "settings");
    app.hide().unwrap();
}

#[test]
fn rounded_clips_paint_far_out_of_view() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    let ui = RoundedClips::new().unwrap();
    ui.window()
        .dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 2.0 });
    let (width, height) = (800, 600);
    ui.window()
        .set_size(slint::PhysicalSize::new(width, height));
    ui.show().unwrap();
    let mut pixels = vec![Rgb8Pixel::default(); (width * height) as usize];
    let mut paint_at = |scroll_y: f32| {
        ui.set_scroll_y(scroll_y);
        ui.window().request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, width as usize);
        });
        pixels.clone()
    };
    let at = |frame: &[Rgb8Pixel], x: u32, y: u32| {
        let pixel = frame[(y * width + x) as usize];
        (pixel.r, pixel.g, pixel.b)
    };
    let white = (255, 255, 255);
    let green = (0, 255, 0);
    // The small clip 40000 physical pixels below, and the top of the tall one,
    // whose corner stays rounded.
    let top = paint_at(0.0);
    assert_eq!(at(&top, 500, 400), green);
    assert_eq!(
        at(&top, 280, 100),
        white,
        "the tall clip's corner must stay rounded"
    );
    // Inside the tall clip, both of its ends out of range.
    let middle = paint_at(-20000.0);
    assert_eq!(at(&middle, 500, 300), green);
    assert_eq!(at(&middle, 270, 300), white);
    // Its bottom end, which ends at 500 physical pixels.
    let bottom = paint_at(-39800.0);
    assert_eq!(at(&bottom, 500, 300), green);
    assert_eq!(
        at(&bottom, 280, 499),
        white,
        "the tall clip's corner must stay rounded"
    );
    ui.hide().unwrap();
}
