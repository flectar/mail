use parley::{
    FontContext,
    fontique::{FallbackKey, GenericFamily, QueryFamily, QueryStatus, Script, SourceKind},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};
mod compose_editor {
    pub const UI_FONT_DATA: &[u8] = include_bytes!(concat!(
        env!("FEIHONG_CONTRACT_REPOSITORY"),
        "/resources/fonts/google-sans-flex/GoogleSansFlex-VariableFont_GRAD,ROND,opsz,slnt,wdth,wght.ttf"
    ));
}
mod blitz_dom {
    pub const BULLET_FONT: &[u8] = include_bytes!(concat!(
        env!("FEIHONG_CONTRACT_REPOSITORY"),
        "/crates/blitz-dom/assets/moz-bullet-font.otf"
    ));
}
mod platform_services {
    use std::sync::OnceLock;
    static PATHS: OnceLock<Vec<String>> = OnceLock::new();
    pub fn configure(paths: Vec<String>) {
        assert!(PATHS.set(paths).is_ok());
    }
    pub fn font_paths() -> &'static [String] {
        PATHS.get().unwrap()
    }
}
include!("production_builder_read.rs");
include!("actual_prewarm_functions.rs");
include!("actual_main_font_diagnostics.rs");
static DIAGNOSTIC_LOGS: Mutex<Vec<String>> = Mutex::new(Vec::new());
fn diagnostic_logger(message: &str) {
    DIAGNOSTIC_LOGS.lock().unwrap().push(message.to_owned());
    eprintln!("{message}");
}
static CONSTRUCTORS: AtomicUsize = AtomicUsize::new(0);
static PANIC_BACKGROUND: AtomicBool = AtomicBool::new(false);
static BACKGROUND_DONE: AtomicBool = AtomicBool::new(false);
static BACKGROUND_GATE: Mutex<Option<(mpsc::Receiver<()>, mpsc::Sender<()>)>> = Mutex::new(None);
fn create_email_font_ctx() -> FontContext {
    CONSTRUCTORS.fetch_add(1, Ordering::SeqCst);
    // Private fixture hooks only; the body delegates to the actual production font builder.
    let background = BACKGROUND_GATE.lock().unwrap().take();
    if let Some((gate, started)) = background {
        started.send(()).unwrap();
        gate.recv().unwrap();
        if PANIC_BACKGROUND.load(Ordering::SeqCst) {
            BACKGROUND_DONE.store(true, Ordering::SeqCst);
            panic!("intentional background-constructor failure for contract");
        }
        let result = create_email_font_ctx_with_system_fonts(false);
        BACKGROUND_DONE.store(true, Ordering::SeqCst);
        result
    } else {
        create_email_font_ctx_with_system_fonts(false)
    }
}
fn install_gate() -> (mpsc::Sender<()>, mpsc::Receiver<()>) {
    let (release, gate) = mpsc::channel();
    let (started, started_rx) = mpsc::channel();
    *BACKGROUND_GATE.lock().unwrap() = Some((gate, started));
    (release, started_rx)
}
fn until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < deadline, "bounded host observer timeout");
        std::thread::yield_now();
    }
}
fn fallback_names(ctx: &mut FontContext, script: [u8; 4]) -> Vec<String> {
    let ids: Vec<_> = ctx
        .collection
        .fallback_families(FallbackKey::new(Script::from_bytes(script), None))
        .collect();
    ids.into_iter()
        .map(|id| ctx.collection.family_name(id).unwrap().to_string())
        .collect()
}
fn blob_ids(ctx: &mut FontContext) -> Vec<u64> {
    let names: Vec<_> = ctx.collection.family_names().map(str::to_string).collect();
    let mut ids = Vec::new();
    for name in names {
        if let Some(f) = ctx.collection.family_by_name(&name) {
            for font in f.fonts() {
                if let SourceKind::Memory(b) = font.source().kind() {
                    ids.push(b.id());
                }
            }
        }
    }
    ids.sort_unstable();
    ids.dedup();
    ids
}
fn assert_template_clone_same(template: &mut FontContext, clone: &mut FontContext) {
    assert_eq!(blob_ids(template), blob_ids(clone));
    for script in [*b"Hani", *b"Hans", *b"Hant", *b"Zyyy", *b"Latn"] {
        assert_eq!(
            fallback_names(template, script),
            fallback_names(clone, script)
        );
    }
    let tid: Vec<_> = template
        .collection
        .generic_families(GenericFamily::SansSerif)
        .collect();
    let cid: Vec<_> = clone
        .collection
        .generic_families(GenericFamily::SansSerif)
        .collect();
    assert_eq!(tid, cid);
    // Real Fontique candidate selection and character maps must still choose the
    // same family, collection face and glyph after the template is cloned.
    for (script, named, text) in [
        (*b"Hani", true, "飞鸿邮箱"),
        (*b"Hans", true, "中文编码"),
        (*b"Hant", true, "飛鴻郵箱"),
        (*b"Zyyy", true, "123😀☕"),
        (*b"Latn", true, "Latin123"),
        (*b"Latn", false, "Latin123"),
    ] {
        let a = selected_glyphs(template, script, named, text);
        let b = selected_glyphs(clone, script, named, text);
        assert_eq!(a, b, "template clone glyph selection changed");
        assert!(
            a.iter().all(Option::is_some),
            "actual fixture glyph coverage missing"
        );
    }
}
fn selected_glyphs(
    ctx: &mut FontContext,
    script: [u8; 4],
    named: bool,
    text: &str,
) -> Vec<Option<(parley::fontique::FamilyId, u32, u32)>> {
    let mut query = ctx.collection.query(&mut ctx.source_cache);
    if named {
        query.set_families([QueryFamily::Named("Missing family in actual HTML")]);
    } else {
        query.set_families([QueryFamily::Generic(GenericFamily::SansSerif)]);
    }
    query.set_fallbacks(FallbackKey::new(Script::from_bytes(script), None));
    let mut candidates = Vec::new();
    query.matches_with(|font| {
        candidates.push(font.clone());
        QueryStatus::Continue
    });
    text.chars()
        .map(|ch| {
            candidates.iter().find_map(|font| {
                font.charmap()?
                    .map(ch)
                    .filter(|glyph| *glyph != 0)
                    .map(|glyph| (font.family.0, font.index, glyph))
            })
        })
        .collect()
}
fn main() {
    fn require_send_sync_clone<T: Send + Sync + Clone>() {}
    require_send_sync_clone::<FontContext>();
    require_send_sync_clone::<parley::fontique::Collection>();
    require_send_sync_clone::<parley::fontique::SourceCache>();
    let requested = std::env::args().nth(1).unwrap();
    let diagnostics = requested == "ready-diagnostics";
    let mode = if diagnostics {
        "ready".to_owned()
    } else {
        requested
    };
    if diagnostics {
        configure_platform_font_diagnostics(diagnostic_logger);
    }
    // The runner supplies these compile-time inputs; no host asset location is
    // assumed, and paths never need to be interpolated into Rust source text.
    let mut paths = vec![
        env!("FEIHONG_CONTRACT_CJK_FONT").to_string(),
        concat!(
            env!("FEIHONG_CONTRACT_REPOSITORY"),
            "/resources/fonts/noto-emoji/NotoEmoji[wght].ttf"
        )
        .to_string(),
    ];
    paths.sort();
    platform_services::configure(paths);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let (release, started) = install_gate();
    if mode == "panic" {
        PANIC_BACKGROUND.store(true, Ordering::SeqCst);
    }
    prewarm_email_font_ctx(runtime.handle());
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(PREWARMED_EMAIL_FONT_CTX.get().is_none());
    match mode.as_str() {
        "pending" => {
            // This must complete while the actual background builder is still held at its private fixture gate.
            let mut first = build_email_font_ctx();
            assert_eq!(CONSTRUCTORS.load(Ordering::SeqCst), 2);
            assert!(PREWARMED_EMAIL_FONT_CTX.get().is_none());
            let first_ids = blob_ids(&mut first);
            let first_hani = fallback_names(&mut first, *b"Hani");
            release.send(()).unwrap();
            until(|| PREWARMED_EMAIL_FONT_CTX.get().is_some());
            let mut second = build_email_font_ctx();
            assert_eq!(CONSTRUCTORS.load(Ordering::SeqCst), 2);
            assert_eq!(first_ids, blob_ids(&mut second));
            assert_eq!(first_hani, fallback_names(&mut second, *b"Hani"));
            let mut template = PREWARMED_EMAIL_FONT_CTX.get().unwrap().clone();
            let mut new_owner = std::thread::spawn(build_email_font_ctx).join().unwrap();
            assert_template_clone_same(&mut template, &mut new_owner);
            println!(
                "PASS pending: UI fallback completed while background gate remained blocked; no ready wait; existing owner cache unchanged after publication; fresh owner clones published template"
            );
        }
        "ready" => {
            release.send(()).unwrap();
            until(|| PREWARMED_EMAIL_FONT_CTX.get().is_some());
            let mut template = PREWARMED_EMAIL_FONT_CTX.get().unwrap().clone();
            let mut first = build_email_font_ctx();
            assert_eq!(CONSTRUCTORS.load(Ordering::SeqCst), 1);
            assert_template_clone_same(&mut template, &mut first);
            first.collection.set_fallbacks(
                FallbackKey::new(Script::from_bytes(*b"Hani"), None),
                std::iter::empty(),
            );
            first
                .collection
                .set_generic_families(GenericFamily::SansSerif, std::iter::empty());
            let mut second = build_email_font_ctx();
            assert_template_clone_same(&mut template, &mut second);
            assert!(!fallback_names(&mut second, *b"Hani").is_empty());
            prewarm_email_font_ctx(runtime.handle());
            assert_eq!(CONSTRUCTORS.load(Ordering::SeqCst), 1);
            println!(
                "PASS ready: background-only construction; UI get/clone, multi-family CJK fallback/IDs/Blob preserved; independent clone mutations; repeat prewarm skips completed template"
            );
        }
        "panic" => {
            release.send(()).unwrap();
            until(|| BACKGROUND_DONE.load(Ordering::SeqCst));
            // Schedule a second blocking observer after the failed initialization job, proving it has returned.
            let (done_tx, done_rx) = mpsc::channel();
            runtime
                .handle()
                .spawn_blocking(move || done_tx.send(()).unwrap());
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(PREWARMED_EMAIL_FONT_CTX.get().is_none());
            let mut fallback = build_email_font_ctx();
            assert_eq!(CONSTRUCTORS.load(Ordering::SeqCst), 2);
            assert!(!fallback_names(&mut fallback, *b"Hani").is_empty());
            println!(
                "PASS panic: background failure returned without publication; original UI constructor remained available with Chinese fallback"
            );
        }
        _ => panic!("unknown mode"),
    }
    // A second queued job on the one-thread blocking pool completes only after the
    // actual initialization job (including its diagnostic callback) has returned.
    let (logged_tx, logged_rx) = mpsc::channel();
    runtime
        .handle()
        .spawn_blocking(move || logged_tx.send(()).unwrap());
    logged_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let logs = DIAGNOSTIC_LOGS.lock().unwrap();
    if diagnostics && cfg!(debug_assertions) {
        assert_eq!(logs.len(), 2);
        assert!(logs.iter().any(|l| {
            l.starts_with("FeihongFontInit background=1 success=1 published=1 bootstrap_ms=")
        }));
        assert!(
            logs.iter()
                .any(|l| l.starts_with("FeihongFontInit background=0 ready=1 bootstrap_ms="))
        );
    } else {
        assert!(
            logs.is_empty(),
            "diagnostics must remain silent without explicit debug opt-in"
        );
    }
}
