use slint::platform::{
    Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use slint::{Color, ComponentHandle, Rgb8Pixel};
use std::{cell::Cell, rc::Rc};

slint::slint! {
    #[edition = "1.19"]
    import { Palette } from "std-widgets.slint";
    import { ThemeEditor } from "../ui/components/theme-editor.slint";
    import { Button } from "../ui/components/button.slint";
    import { IconButton } from "../ui/components/controls.slint";
    import { AppTheme } from "../ui/design-system.slint";

    export { AppTheme }

    export component ThemeHarness inherits Window {
        preferred-width: 920px;
        preferred-height: 1160px;
        out property <length> editor-height: editor.height;
        callback save-palette(color, color) -> bool;
        init => {
            Palette.color-scheme = ColorScheme.light;
            AppTheme.preset = "custom";
            AppTheme.custom-light-text = #777777;
            AppTheme.custom-light-page-bg = #888888;
            AppTheme.custom-light-surface = #999999;
            AppTheme.custom-dark-text = #777777;
            AppTheme.custom-dark-page-bg = #888888;
            AppTheme.custom-dark-surface = #999999;
        }
        VerticalLayout {
            x: 20px; y: 20px; width: 880px;
            height: self.preferred-height;
            editor := ThemeEditor {
                theme-mode: "light";
                save-custom-theme(lp, lbg, ls, lt, lb, dp, dbg, ds, dt, db) => {
                    return root.save-palette(lp, lt);
                }
            }
        }
        // Solid icons give a direct render check of the actual button foregrounds.
        Button {
            x: 20px; y: 1100px; width: 100px;
            primary: true;
            has-icon: true;
            icon: @image-url("../ui/icons/lucide/filled/mail.svg");
        }
        IconButton {
            x: 150px; y: 1100px;
            primary: true;
            primary-hover-background: black;
            primary-pressed-background: white;
            icon: @image-url("../ui/icons/lucide/filled/mail.svg");
        }
    }
}

struct Headless(Rc<MinimalSoftwareWindow>);
impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

#[test]
fn low_contrast_palettes_save_retry_and_revert_and_primary_foregrounds_adapt() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    let ui = ThemeHarness::new().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(920, 1160));
    ui.show().unwrap();

    let calls = Rc::new(Cell::new(0));
    let succeeds = Rc::new(Cell::new(false));
    let saved_primary = Rc::new(Cell::new(Color::default()));
    let count = calls.clone();
    let success = succeeds.clone();
    let primary = saved_primary.clone();
    ui.on_save_palette(move |accent, text| {
        count.set(count.get() + 1);
        primary.set(accent);
        assert_eq!(text, Color::from_rgb_u8(119, 119, 119));
        success.get()
    });

    let click = |x, y| {
        let position = slint::LogicalPosition::new(x, y);
        ui.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        ui.window().dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
    };
    let draw = || {
        slint::platform::update_timers_and_animations();
        ui.window().request_redraw();
        let mut pixels = vec![Rgb8Pixel::default(); 920 * 1160];
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, 920);
        });
        pixels
    };

    draw();
    click(780.0, ui.get_editor_height() - 12.0);
    assert_eq!(calls.get(), 0, "an unedited palette should not save");

    let theme = ui.global::<AppTheme>();
    let original = theme.get_custom_light_primary();
    // Editing the saturation/brightness picker must mark the palette dirty.
    click(500.0, 460.0);
    let edited = theme.get_custom_light_primary();
    assert_ne!(edited, original);
    draw();
    click(780.0, ui.get_editor_height() - 12.0);
    assert_eq!(calls.get(), 1, "low contrast must not block persistence");
    assert_eq!(saved_primary.get(), edited);
    succeeds.set(true);
    click(780.0, ui.get_editor_height() - 12.0);
    assert_eq!(calls.get(), 2, "a failed save must remain retryable");
    click(780.0, ui.get_editor_height() - 12.0);
    assert_eq!(calls.get(), 2, "a successful save must clear dirty state");

    click(650.0, 470.0);
    assert_ne!(theme.get_custom_light_primary(), edited);
    click(530.0, ui.get_editor_height() - 12.0);
    assert_eq!(
        theme.get_custom_light_primary(),
        edited,
        "Revert restores the saved palette"
    );
    click(780.0, ui.get_editor_height() - 12.0);
    assert_eq!(calls.get(), 2);

    // Remove hover from the buttons before sampling their rendered icon centers.
    ui.window().dispatch_event(WindowEvent::PointerExited);
    for (background, expected) in [
        (Color::from_rgb_u8(215, 153, 33), Rgb8Pixel::new(0, 0, 0)),
        (Color::from_rgb_u8(142, 192, 124), Rgb8Pixel::new(0, 0, 0)),
        (
            Color::from_rgb_u8(9, 105, 218),
            Rgb8Pixel::new(255, 255, 255),
        ),
    ] {
        theme.set_custom_light_primary(background);
        let pixels = draw();
        assert_eq!(pixels[1119 * 920 + 70], expected, "labeled primary icon");
        assert_eq!(pixels[1119 * 920 + 166], expected, "icon-only primary icon");
    }

    let position = slint::LogicalPosition::new(166.0, 1116.0);
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position });
    let pixels = draw();
    assert_eq!(
        pixels[1119 * 920 + 166],
        Rgb8Pixel::new(255, 255, 255),
        "dark hover background needs a white icon"
    );
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    let pixels = draw();
    assert_eq!(
        pixels[1120 * 920 + 166],
        Rgb8Pixel::new(0, 0, 0),
        "light pressed background needs a black icon"
    );
}
