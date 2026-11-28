//! The messages shown in the reader of the screenshots.
//!
//! `resources/screenshots/demo-email.html`, the Cloudflare message of every
//! desktop screenshot, and `resources/store/store-email.html`, the one in the
//! Google Play listing, are ordinary light-only HTML emails. They are rendered
//! by the same reader, at the width the reading pane lays them out at and with
//! the message colors of the current theme preset, so dark screenshots show
//! the shipping adaptation rather than a painted mock-up.
//! `scripts/generate-screenshots.sh` runs `email_screenshots` to give the
//! slint-viewer screenshots one body image per preset and theme, and
//! `scripts/generate-store-assets.sh` runs `store_email_screenshot`.
use super::*;
use message_appearance::EmailAppearance;
use screenshot_fixture::ScreenshotWindow;
use std::path::{Path, PathBuf};

const HTML_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/resources/screenshots/demo-email.html"
);
const STORE_HTML_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/resources/store/store-email.html"
);
const PRESETS: [&str; 4] = ["default", "teal", "green", "purple"];
const THEMES: [&str; 2] = ["light", "dark"];
const SCALE: f32 = 2.0;
const WIDTH: u32 = 1320;
const HEIGHT: u32 = 800;
/// The phone in the Google Play screenshots, in logical pixels.
const PHONE_WIDTH: u32 = 400;
const PHONE_HEIGHT: u32 = 820;

fn render(app: &AppWindow, html_path: &str, scale: f32) -> renderer::RenderedEmail {
    let width = app.global::<ReaderUi>().get_email_viewport_width();
    assert!(width > 1.0, "the reading pane must be laid out first");
    renderer::render_screenshot_email(
        &std::fs::read_to_string(html_path).unwrap(),
        width.ceil() as u32,
        scale,
        EmailAppearance::from_app(app),
    )
    .unwrap()
}

/// Show the message in the reader through the application's own projection.
pub(super) fn show(app: &AppWindow, scale: f32) {
    mail_render_projection::apply_cpu_frame(&ReaderRef::main(app), render(app, HTML_PATH, scale));
}

fn output_dir() -> PathBuf {
    let Some(out) = std::env::var_os("FLECTAR_EMAIL_SCREENSHOTS").map(PathBuf::from) else {
        panic!("set FLECTAR_EMAIL_SCREENSHOTS to the output directory");
    };
    std::fs::create_dir_all(&out).unwrap();
    out
}

/// Save the message as `name.png`, and the reader projection that shows it as
/// `name.json`. The reader paints a message as a column of tiles; the
/// slint-viewer screenshots use the same pixels as one image.
fn save(rendered: &renderer::RenderedEmail, out: &Path, name: &str) {
    let tiles: Vec<_> = rendered
        .tiles
        .iter()
        .map(|tile| {
            let pixels = tile.image.to_rgba8().unwrap();
            image::RgbaImage::from_raw(pixels.width(), pixels.height(), pixels.as_bytes().to_vec())
                .unwrap()
        })
        .collect();
    let mut image = image::RgbaImage::new(rendered.width, tiles.iter().map(|t| t.height()).sum());
    let mut y = 0;
    for tile in &tiles {
        image::imageops::replace(&mut image, tile, 0, y);
        y += i64::from(tile.height());
    }

    let path = out.join(format!("{name}.png"));
    image.save(&path).unwrap();
    let aspect = rendered.height as f32 / rendered.width as f32;
    let projection = serde_json::json!({
        "email_tiles": [{ "image": path, "y": 0, "height": aspect }],
        "email-content-aspect": aspect,
    });
    std::fs::write(
        out.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&projection).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "writes screenshots; run by scripts/generate-screenshots.sh"]
fn email_screenshots() {
    let out = output_dir();
    let screen = ScreenshotWindow::new(SCALE);
    let app = &screen.app;
    for preset in PRESETS {
        app.global::<AppTheme>().set_preset(preset.into());
        for theme in THEMES {
            app.set_theme_mode(theme.into());
            screen.capture(WIDTH, HEIGHT);
            let name = format!("demo-email-{preset}-{theme}");
            save(&render(app, HTML_PATH, SCALE), &out, &name);
        }
    }
}

#[test]
#[ignore = "writes screenshots; run by scripts/generate-store-assets.sh"]
fn store_email_screenshot() {
    let out = output_dir();
    let screen = ScreenshotWindow::new(SCALE);
    let app = &screen.app;
    app.set_theme_mode("light".into());
    // A phone shows the reader once a tab with a conversation is shown.
    app.global::<TabsUi>().set_shown_conversation(true);
    app.global::<TabsUi>().set_shown(1);
    screen.capture(PHONE_WIDTH, PHONE_HEIGHT);
    save(&render(app, STORE_HTML_PATH, SCALE), &out, "store-email");
}
