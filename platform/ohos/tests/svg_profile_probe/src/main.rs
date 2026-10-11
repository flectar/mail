use resvg::{tiny_skia, usvg};
use std::{fs, hint::black_box, path::PathBuf, time::Instant};

fn render(tree: &usvg::Tree, width: u32, height: u32) -> tiny_skia::Pixmap {
    // Exact target sizing/transform used by Slint 1.18.1 ParsedSVG::render.
    let requested = tiny_skia::IntSize::from_wh(width, height).unwrap();
    let target = tree.size().to_int_size().scale_to(requested);
    let size = target.to_size();
    let transform = tiny_skia::Transform::from_scale(
        size.width() / tree.size().width(),
        size.height() / tree.size().height(),
    );
    let mut pixmap = tiny_skia::Pixmap::new(target.width(), target.height()).unwrap();
    resvg::render(tree, transform, &mut pixmap.as_mut());
    pixmap
}

fn main() {
    assert!(
        cfg!(debug_assertions),
        "both fixtures must preserve debug assertions"
    );
    let output = PathBuf::from(std::env::args().nth(1).expect("output directory"));
    fs::create_dir_all(&output).unwrap();
    let root = PathBuf::from(std::env::args().nth(2).expect("production source root"));
    let inputs = [
        ("search", "ui/icons/lucide/outline/search.svg"),
        ("sync", "ui/icons/lucide/outline/sync.svg"),
        ("inbox", "ui/icons/lucide/filled/inbox-flectar.svg"),
    ];
    let options = usvg::Options::default();
    let mut cases = Vec::new();
    for (name, path) in inputs {
        let svg = fs::read(root.join(path)).unwrap();
        let tree = usvg::Tree::from_data(&svg, &options).unwrap();
        for physical in [24, 45, 54, 144] {
            let id = format!("{name}-{physical}");
            let reference = render(&tree, physical, physical);
            assert_eq!(
                (reference.width(), reference.height()),
                (physical, physical)
            );
            assert!(
                reference.data().chunks_exact(4).any(|p| p[3] > 0),
                "empty icon: {id}"
            );
            assert!(
                reference
                    .data()
                    .chunks_exact(4)
                    .any(|p| p[3] > 0 && p[3] < 255),
                "anti alias coverage: {id}"
            );
            fs::write(output.join(format!("{id}.rgba")), reference.data()).unwrap();
            for _ in 0..8 {
                black_box(render(&tree, physical, physical));
            }
            let mut render_rounds = Vec::new();
            for _ in 0..7 {
                let started = Instant::now();
                for _ in 0..32 {
                    black_box(render(&tree, physical, physical));
                }
                render_rounds.push(started.elapsed().as_secs_f64() * 1_000_000. / 32.);
            }
            let mut parse_rounds = Vec::new();
            for _ in 0..7 {
                let started = Instant::now();
                for _ in 0..32 {
                    black_box(usvg::Tree::from_data(black_box(&svg), &options).unwrap());
                }
                parse_rounds.push(started.elapsed().as_secs_f64() * 1_000_000. / 32.);
            }
            cases.push(format!(
                "{{\"id\":\"{id}\",\"svg\":\"{path}\",\"physical_size\":{physical},\"raw_color\":\"default-currentColor-black\",\"render_us_per_iteration\":{:?},\"parse_us_per_iteration\":{:?}}}",
                render_rounds, parse_rounds));
        }
    }
    fs::write(output.join("measurements.json"), format!(
        "{{\"host_only\":true,\"harness_debug_assertions\":{},\"warmup\":8,\"rounds\":7,\"iterations_per_round\":32,\"cases\":[{}]}}\n",
        cfg!(debug_assertions), cases.join(","))).unwrap();
    println!("12 real SVG/size cases; 7 rounds x 32 renders; debug assertions on");
}
