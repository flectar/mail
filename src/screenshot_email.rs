//! The Cloudflare message shown in the reader of every desktop screenshot.
//!
//! `resources/screenshots/demo-email.html` is an ordinary light-only HTML
//! email. It is rendered by the same reader, at the width the reading pane
//! lays it out at and with the message colors of the current theme preset, so
//! dark screenshots show the shipping adaptation rather than a painted mock-up.
//! `scripts/generate-screenshots.sh` runs `email_screenshots` to give the
//! slint-viewer screenshots one body image per preset and theme.
use super::*;
use message_appearance::EmailAppearance;
use screenshot_fixture::ScreenshotWindow;

const HTML_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/resources/screenshots/demo-email.html"
);
const PRESETS: [&str; 4] = ["default", "teal", "green", "purple"];
const THEMES: [&str; 2] = ["light", "dark"];
const SCALE: f32 = 2.0;
const WIDTH: u32 = 1320;
const HEIGHT: u32 = 800;

fn render(app: &AppWindow, scale: f32) -> renderer::RenderedEmail {
    let width = app.global::<ReaderUi>().get_email_viewport_width();
    assert!(width > 1.0, "the reading pane must be laid out first");
    renderer::render_screenshot_email(
        &std::fs::read_to_string(HTML_PATH).unwrap(),
        width.ceil() as u32,
        scale,
        EmailAppearance::from_app(app),
    )
    .unwrap()
}

/// Show the message in the reader through the application's own projection.
pub(super) fn show(app: &AppWindow, scale: f32) {
    mail_render_projection::apply_cpu_frame(&ReaderRef::main(app), render(app, scale));
}

#[test]
#[ignore = "writes screenshots; run by scripts/generate-screenshots.sh"]
fn email_screenshots() {
    let Some(out) = std::env::var_os("FLECTAR_EMAIL_SCREENSHOTS").map(std::path::PathBuf::from)
    else {
        panic!("set FLECTAR_EMAIL_SCREENSHOTS to the output directory");
    };
    std::fs::create_dir_all(&out).unwrap();

    let screen = ScreenshotWindow::new(SCALE);
    let app = &screen.app;
    for preset in PRESETS {
        app.global::<AppTheme>().set_preset(preset.into());
        for theme in THEMES {
            app.set_theme_mode(theme.into());
            screen.capture(WIDTH, HEIGHT);
            let rendered = render(app, SCALE);

            // The reader paints the message as a column of tiles; the
            // slint-viewer screenshots use the same pixels as one image.
            let tiles: Vec<_> = rendered
                .tiles
                .iter()
                .map(|tile| {
                    let pixels = tile.image.to_rgba8().unwrap();
                    image::RgbaImage::from_raw(
                        pixels.width(),
                        pixels.height(),
                        pixels.as_bytes().to_vec(),
                    )
                    .unwrap()
                })
                .collect();
            let mut image =
                image::RgbaImage::new(rendered.width, tiles.iter().map(|t| t.height()).sum());
            let mut y = 0;
            for tile in &tiles {
                image::imageops::replace(&mut image, tile, 0, y);
                y += i64::from(tile.height());
            }

            let name = format!("demo-email-{preset}-{theme}");
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
    }
}
