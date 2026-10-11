//! Exact production SettingsDialog; observer exports are inserted in /tmp.
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, ModelRc, VecModel, Rgb8Pixel};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

include!("GENERATED_UI");

struct ProbePlatform(Rc<MinimalSoftwareWindow>);
impl Platform for ProbePlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> { Ok(self.0.clone()) }
}
struct Harness {
    ui: SettingsProbe,
    window: Rc<MinimalSoftwareWindow>,
    created: Rc<RefCell<BTreeMap<String, usize>>>,
    offsets: Rc<RefCell<BTreeMap<String, f32>>>,
    pixels: Vec<Rgb8Pixel>,
}
impl Harness {
    fn new(software: bool, tab: &str) -> Self {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        slint::platform::set_platform(Box::new(ProbePlatform(window.clone()))).unwrap();
        let ui = SettingsProbe::new().unwrap();
        ui.global::<MotionSettings>().set_software_rendering(software);
        ui.global::<MotionSettings>().set_enabled(false);
        ui.set_tab(tab.into());
        ui.set_accounts(ModelRc::new(VecModel::from(vec![AccountRow {
            id: 7, name: "Contract account".into(), email: "fixture@example.invalid".into(),
            provider: "imap".into(), mail_protocol: "imap".into(),
            caldav_url: "https://fixture.invalid/calendar".into(), caldav_username: "Fixture".into(),
            ..Default::default()
        }])));
        ui.on_matches(|terms, query| terms.to_lowercase().contains(&query.to_lowercase()));
        let created = Rc::new(RefCell::new(BTreeMap::new()));
        let observer = created.clone();
        ui.on_created(move |name| { *observer.borrow_mut().entry(name.to_string()).or_default() += 1; });
        let offsets = Rc::new(RefCell::new(BTreeMap::new()));
        let observer = offsets.clone();
        ui.on_scrolled(move |name, value| { observer.borrow_mut().insert(name.to_string(), value); });
        window.set_size(slint::PhysicalSize::new(1280, 560));
        ui.show().unwrap();
        ui.set_construct(true);
        let mut this = Self { ui, window, created, offsets, pixels: vec![Rgb8Pixel::default(); 1280 * 560] };
        this.settle();
        this
    }
    fn settle(&mut self) {
        for _ in 0..3 {
            slint::platform::update_timers_and_animations();
            self.window.request_redraw();
            self.window.draw_if_needed(|renderer| { renderer.render(&mut self.pixels, 1280); });
        }
        self.ui.invoke_report();
        slint::platform::update_timers_and_animations();
        self.window.request_redraw();
        self.window.draw_if_needed(|renderer| { renderer.render(&mut self.pixels, 1280); });
    }
    fn count(&self, name: &str) -> usize { self.created.borrow().get(name).copied().unwrap_or(0) }
    fn tab(&mut self, name: &str) { self.ui.set_tab(name.into()); self.settle(); }
    fn inspect_scroll(&mut self, page: &str) -> f32 {
        self.ui.invoke_scroll(page.into(), 0., false);
        self.settle();
        *self.offsets.borrow().get(page).expect("actual ScrollView must report its offset")
    }
    fn screenshot(&self, name: &str) {
        let output = std::path::PathBuf::from(std::env::var_os("SETTINGS_CONTRACT_OUTPUT").unwrap());
        let mut data = b"P6\n1280 560\n255\n".to_vec();
        for pixel in &self.pixels { data.extend_from_slice(&[pixel.r, pixel.g, pixel.b]); }
        std::fs::write(output.join(format!("{name}.ppm")), data).unwrap();
    }
}

#[test]
fn cpu_initial_general_loads_only_general_and_search_creates_matching_pages() {
    let mut h = Harness::new(true, "General");
    assert_eq!(h.count("General"), 1);
    assert_eq!(h.created.borrow().len(), 1, "CPU initial settings must leave unvisited subtrees unconstructed");
    h.screenshot("general");
    h.ui.invoke_search("address book".into()); h.settle();
    assert_eq!(h.ui.get_tab(), "Contacts");
    assert_eq!(h.count("Contacts"), 1);
    h.ui.invoke_search("snapshot".into()); h.settle();
    assert_eq!(h.ui.get_tab(), "Data");
    assert_eq!(h.count("Data"), 1);
    h.ui.invoke_search("".into()); h.settle();
    for name in ["Accounts", "Contacts", "Data", "Notifications", "Shortcuts", "About", "General"] {
        h.tab(name); h.tab("General"); h.tab(name);
        assert_eq!(h.count(name), 1, "each visited page must retain its original instance");
    }
}

#[test]
fn visited_page_retains_its_actual_scrollview() {
    let mut h = Harness::new(true, "Accounts");
    assert_eq!(h.count("Accounts"), 1, "direct Accounts opening constructs the selected page");
    assert_eq!(h.count("General"), 1, "General is always eager for preview rollback");
    assert_eq!(h.created.borrow().len(), 2);
    h.window.dispatch_event(WindowEvent::PointerScrolled {
        position: slint::LogicalPosition::new(760., 320.), delta_x: 0., delta_y: -240.,
    });
    h.settle();
    let offset = h.inspect_scroll("Accounts");
    assert!(offset < -1., "real wheel input must scroll the production Accounts page: {offset}");
    h.tab("General");
    h.tab("Accounts");
    let restored = h.inspect_scroll("Accounts");
    assert!(h.count("Accounts") == 1 && restored == offset,
            "visited page must keep its actual ScrollView instance and viewport-y: before={offset}, after={restored}, constructions={}", h.count("Accounts"));
    h.screenshot("accounts-retained");
}

#[test]
fn whole_dialog_destruction_releases_visited_instances() {
    let mut h = Harness::new(true, "Accounts");
    h.window.dispatch_event(WindowEvent::PointerScrolled {
        position: slint::LogicalPosition::new(760., 320.), delta_x: 0., delta_y: -240.,
    });
    h.settle();
    assert!(h.inspect_scroll("Accounts") < -1.);
    h.ui.set_open(false); h.settle();
    h.ui.set_construct(false); h.settle();
    h.ui.set_tab("General".into()); h.ui.set_open(true);
    h.ui.set_construct(true); h.settle();
    assert_eq!(h.count("General"), 2, "whole dialog reopens with a new General instance");
    assert_eq!(h.count("Accounts"), 1, "a new dialog must not retain the previous visited latch");
    h.tab("Accounts");
    assert_eq!(h.count("Accounts"), 2);
    assert_eq!(h.inspect_scroll("Accounts"), 0., "new dialog gets a fresh actual ScrollView");
}

#[test]
fn non_cpu_retains_eager_construction_and_visible_pages() {
    let mut h = Harness::new(false, "General");
    for name in ["General", "Accounts", "Contacts", "Data", "Notifications", "Shortcuts", "About",
                 "caldav", "imap", "profile-editor", "account-profile-picker"] {
        assert_eq!(h.count(name), 1, "non CPU construction remains eager for {name}");
    }
    h.tab("Accounts");
    h.tab("General");
    assert_eq!(h.count("Accounts"), 1);
}

#[test]
fn original_modal_functions_and_settings_close_still_clear_state() {
    let mut h = Harness::new(true, "Accounts");
    for kind in ["caldav", "imap", "profile-editor", "account-profile-picker"] {
        assert_eq!(h.count(kind), 0);
        h.ui.invoke_modal(kind.into(), true); h.settle();
        assert_eq!(h.count(kind), 1);
        h.ui.invoke_modal(kind.into(), false); h.settle();
        h.ui.invoke_modal(kind.into(), true); h.settle();
        assert_eq!(h.count(kind), 1, "closed modal is retained for subsequent openings");
        match kind {
            "caldav" => assert_eq!(h.ui.get_caldav_id(), 7),
            "imap" => assert!(h.ui.get_imap_open()),
            "profile-editor" => assert_eq!(h.ui.get_profile_name(), "Contract profile"),
            _ => assert_eq!(h.ui.get_picker_id(), 7),
        }
        h.ui.invoke_modal(kind.into(), false); h.settle();
    }
    h.ui.invoke_modal("imap".into(), true); h.settle();
    h.ui.set_email("edited@example.invalid".into());
    h.ui.set_password("synthetic fixture only".into());
    h.tab("General"); h.tab("Accounts");
    assert_eq!(h.ui.get_email(), "edited@example.invalid");
    assert_eq!(h.ui.get_password(), "synthetic fixture only");
    h.ui.invoke_modal("caldav".into(), true);
    h.ui.set_caldav_password("synthetic fixture only".into());
    h.ui.invoke_modal("profile-editor".into(), true);
    h.ui.invoke_modal("account-profile-picker".into(), true); h.settle();
    h.ui.set_open(false); h.settle();
    assert_eq!(h.ui.get_caldav_id(), -1);
    assert_eq!(h.ui.get_caldav_password(), "");
    assert_eq!(h.ui.get_picker_id(), -1);
    assert!(!h.ui.get_imap_open());
    assert!(!h.ui.get_profile_open());
    assert!(!h.ui.get_picker_open());
}

#[test]
fn real_theme_preview_survives_tabs_and_original_close_watch_reverts_snapshot() {
    let mut h = Harness::new(true, "General");
    let original = h.ui.global::<AppTheme>().get_custom_light_primary();
    let preview = slint::Color::from_rgb_u8(18, 171, 96);
    h.ui.invoke_preview(preview); h.settle();
    assert_eq!(h.ui.global::<AppTheme>().get_custom_light_primary(), preview);
    h.tab("Accounts"); h.tab("General");
    assert_eq!(h.ui.global::<AppTheme>().get_custom_light_primary(), preview);
    h.ui.set_open(false); h.settle();
    assert_eq!(h.ui.global::<AppTheme>().get_custom_light_primary(), original,
               "original SettingsDialog close watcher must cancel actual ThemeEditor preview");
    h.ui.set_open(true); h.settle();
    let second_preview = slint::Color::from_rgb_u8(162, 52, 228);
    h.ui.invoke_preview(second_preview); h.settle();
    h.ui.set_open(false); h.settle();
    assert_eq!(h.ui.global::<AppTheme>().get_custom_light_primary(), original,
               "second preview must take a fresh snapshot after cancellation");
}
