//! Isolated host code generation probe; no application or native device bridge.
#![allow(dead_code)]
#[path = "../rust/src/framebuffer.rs"]
mod framebuffer;

use framebuffer::OpaqueRgba;
use sha2::{Digest, Sha256};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, WindowAdapter};
use slint::{ComponentHandle, Image, PhysicalSize, Rgb8Pixel, SharedPixelBuffer};
use std::{rc::Rc, time::Instant};

const WIDTH: usize = 1920;
const HEIGHT: usize = 1200;
const STATES: usize = 8;
const CYCLES: usize = 2;

slint::slint! {
    export component OpaqueProfileProbe inherits Window {
        width: 1920px; height: 1200px;
        background: #edf1f5;
        in-out property <image> texture;
        in-out property <int> scene;
        in-out property <length> offset;
        in-out property <string> label;
        Image {
            visible: root.scene != 2;
            x: root.scene == 0 ? root.offset : 48px;
            y: 84px; width: 1800px; height: 1080px;
            source: root.texture;
            image-fit: contain;
        }
        Rectangle {
            visible: root.scene == 1;
            x: 128px + root.offset; y: 164px;
            width: 1500px; height: 800px;
            background: #223c7399; border-radius: 24px;
        }
        Rectangle {
            visible: root.scene == 2;
            x: 80px + root.offset; y: 220px;
            width: 460px; height: 138px;
            background: #617bbccc; border-radius: 16px;
            Text {
                x: 24px; y: 20px; width: 412px; height: 44px;
                text: root.label; font-family: "Google Sans Flex";
                font-size: 28px; color: #ffffff;
            }
            Text {
                x: 24px; y: 72px; width: 400px; height: 36px;
                text: "Reply   Archive   Search";
                font-family: "Google Sans Flex"; font-size: 22px;
                color: #edf2ff;
            }
        }
        Rectangle {
            visible: root.scene == 2;
            x: 600px + root.offset; y: 230px;
            width: 300px; height: 96px;
            background: #ffffff; border-width: 2px;
            border-color: #617bbc; border-radius: 12px;
            Text {
                width: parent.width; height: parent.height;
                text: "Mail controls";
                font-family: "Google Sans Flex"; font-size: 26px;
                color: #21335a;
                horizontal-alignment: center; vertical-alignment: center;
            }
        }
    }
}

struct ProbePlatform(Rc<MinimalSoftwareWindow>);
impl Platform for ProbePlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

fn set_state(ui: &OpaqueProfileProbe, state: usize) {
    ui.set_offset(32. + (state as f32) * 4.);
    ui.set_label(format!("Message {} / Offline ready", state + 1).into());
    slint::platform::update_timers_and_animations();
}

fn main() {
    let output = std::path::PathBuf::from(std::env::args_os().nth(1).expect("snapshot directory"));
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(ProbePlatform(window.clone()))).unwrap();
    let font = include_bytes!("../../../resources/fonts/google-sans-flex/GoogleSansFlex-VariableFont_GRAD,ROND,opsz,slnt,wdth,wght.ttf");
    let blob = slint::fontique_011::fontique::Blob::new(std::sync::Arc::new(font.to_vec()));
    assert!(!slint::fontique_011::shared_collection().register_fonts(blob, None).is_empty());
    let ui = OpaqueProfileProbe::new().unwrap();
    window.set_size(PhysicalSize::new(WIDTH as u32, HEIGHT as u32));
    let mut image = SharedPixelBuffer::<Rgb8Pixel>::new(1800, 1080);
    for (i, p) in image.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = (i % 1800, i / 1800);
        *p = Rgb8Pixel::new((x % 251) as u8, (y % 241) as u8, ((x / 5 + y / 3) % 239) as u8);
    }
    ui.set_texture(Image::from_rgb8(image));
    ui.show().unwrap();
    let mut pixels = vec![OpaqueRgba::default(); WIDTH * HEIGHT];
    for (scene, name) in ["opaque-rgb-texture", "translucent-overlay", "small-text-controls"].into_iter().enumerate() {
        ui.set_scene(scene as i32);
        for state in 0..STATES {
            set_state(&ui, state);
            window.request_redraw();
            window.draw_if_needed(|renderer| { renderer.render(&mut pixels, WIDTH); });
        }
        let mut times = Vec::new();
        let mut hashes = Vec::new();
        let mut damages = Vec::new();
        for frame in 0..STATES * CYCLES {
            let state = frame % STATES;
            set_state(&ui, state);
            let mut sample = None;
            window.request_redraw();
            window.draw_if_needed(|renderer| {
                let started = Instant::now();
                let region = renderer.render(&mut pixels, WIDTH);
                let elapsed = started.elapsed().as_nanos();
                sample = Some((elapsed, region.bounding_box_size()));
            });
            let (elapsed, damage) = sample.expect("requested draw must execute");
            // Hashing, opaque validation and file IO are outside render timing.
            let bytes = OpaqueRgba::as_bytes(&pixels);
            assert!(bytes.chunks_exact(4).all(|p| p[3] == 255));
            let hash = format!("{:x}", Sha256::digest(bytes));
            if frame >= STATES {
                assert_eq!(hash, hashes[state], "a repeated state must render identically");
            }
            assert!(damage.width > 0 && damage.height > 0, "each changed state must paint");
            times.push(elapsed.to_string());
            hashes.push(hash);
            damages.push(format!("[{},{}]", damage.width, damage.height));
            if frame + 1 == STATES * CYCLES {
                let mut ppm = format!("P6\n{} {}\n255\n", WIDTH, HEIGHT).into_bytes();
                for p in bytes.chunks_exact(4) { ppm.extend_from_slice(&p[..3]); }
                std::fs::write(output.join(format!("{name}.ppm")), ppm).unwrap();
            }
        }
        println!("{{\"scene\":\"{name}\",\"width\":{WIDTH},\"height\":{HEIGHT},\"warmup_frames\":{STATES},\"render_ns\":[{}],\"pixel_sha256\":[{}],\"damage_sizes\":[{}]}}",
            times.join(","), hashes.iter().map(|h| format!("\"{h}\"")).collect::<Vec<_>>().join(","), damages.join(","));
    }
}
