//! Render and drive the production agenda without a display or provider account.
use slint::platform::{
    Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use slint::{ComponentHandle, Model, ModelRc, Rgb8Pixel, VecModel};
use std::{cell::Cell, rc::Rc};

slint::slint! {
    #[edition = "1.19"]
    import { Palette } from "std-widgets.slint";
    import { CalendarAgendaRow, CalendarAgendaFilterRow, CalendarEventRow } from "../ui/models.slint";
    import { CalendarAgenda, CalendarAgendaFilter } from "../ui/components/calendar-agenda.slint";
    export { CalendarAgendaNavigation } from "../ui/components/calendar-agenda.slint";
    export { AppTheme, MotionSettings } from "../ui/design-system.slint";
    import { AppTheme } from "../ui/design-system.slint";
    export component AgendaHarness inherits Window {
        preferred-width: 800px;
        preferred-height: 600px;
        in property <[CalendarAgendaRow]> rows;
        in property <[CalendarAgendaFilterRow]> filters;
        in property <int> count;
        in property <int> filter-count;
        in property <float> anchor;
        in property <int> scroll-request;
        in property <bool> compact;
        in-out property <bool> dark: false;
        out property <string> cursor: agenda.keyboard-key;
        out property <length> scroll-y: agenda.scroll-offset;
        callback opened(int);
        callback filter-toggled(int, bool);
        callback reset-filters;
        callback new-event;
        public function focus-list() { agenda.focus-list(); }
        init => { Palette.color-scheme = ColorScheme.light; }
        changed dark => { Palette.color-scheme = root.dark ? ColorScheme.dark : ColorScheme.light; }
        Rectangle {
            width: parent.width;
            height: parent.height;
            background: AppTheme.surface;
            Text { x: 16px; height: 64px; text: "October 2026"; color: AppTheme.heading; vertical-alignment: center; }
            CalendarAgendaFilter {
                x: parent.width - self.width - 16px;
                y: 8px;
                compact: root.compact;
                filters: root.filters;
                filter-count: root.filter-count;
                toggled(id, checked) => { root.filter-toggled(id, checked); }
                cleared => { root.reset-filters(); }
            }
            agenda := CalendarAgenda {
                y: 64px;
                width: parent.width;
                height: parent.height - 64px;
                rows: root.rows;
                compact: root.compact;
                event-count: root.count;
                filter-count: root.filter-count;
                anchor: root.anchor;
                scroll-request: root.scroll-request;
                event-clicked(event) => { root.opened(event.id); }
                new-event => { root.new-event(); }
                clear-filters => { root.reset-filters(); }
            }
        }
    }
}

struct Headless(Rc<MinimalSoftwareWindow>);
impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn rows(count: i32) -> Vec<CalendarAgendaRow> {
    let mut rows = vec![CalendarAgendaRow {
        key: "day".into(),
        kind: "day".into(),
        date_label: "Thursday · 8 October".into(),
        is_today: true,
        count,
        ..Default::default()
    }];
    rows.extend((1..=count).map(|id| CalendarAgendaRow {
        key: format!("event-{id}").into(),
        kind: "event".into(),
        time_label: "09:30".into(),
        end_label: "10:30".into(),
        event: CalendarEventRow {
            id,
            title: format!("Planning session {id}").into(),
            calendar: "Work".into(),
            account: "work@calendar.example".into(),
            color_index: id.rem_euclid(4),
            ..Default::default()
        },
        ..Default::default()
    }));
    rows.push(CalendarAgendaRow {
        key: "end".into(),
        kind: "end".into(),
        ..Default::default()
    });
    annotate(&mut rows);
    rows
}

fn annotate(rows: &mut [CalendarAgendaRow]) {
    let mut offset = 0.;
    let mut previous = -1;
    for (index, row) in rows.iter_mut().enumerate() {
        row.offset = offset;
        row.previous_event = previous;
        offset += 68.;
        if row.kind == "event" {
            previous = index as i32;
        }
    }
    let mut next = -1;
    for (index, row) in rows.iter_mut().enumerate().rev() {
        row.next_event = next;
        if row.kind == "event" {
            next = index as i32;
        }
    }
}

fn render(window: &Rc<MinimalSoftwareWindow>, name: &str) {
    slint::platform::update_timers_and_animations();
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        let size = window.size();
        let mut pixels = vec![Rgb8Pixel::default(); (size.width * size.height) as usize];
        renderer.render(&mut pixels, size.width as usize);
        if let Some(directory) = std::env::var_os("FLECTAR_AGENDA_SCREENSHOT_DIR") {
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            let bytes: Vec<_> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
            image::save_buffer(
                directory.join(format!("{name}.png")),
                &bytes,
                size.width,
                size.height,
                image::ColorType::Rgb8,
            )
            .unwrap();
        }
    });
}

fn click(ui: &AgendaHarness, x: f32, y: f32) {
    let position = slint::LogicalPosition::new(x, y);
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}
fn key(ui: &AgendaHarness, value: slint::platform::Key) {
    let text: slint::SharedString = value.into();
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
}

#[test]
fn busy_agenda_scroll_keyboard_identity_filters_and_responsive_actions() {
    use slint::platform::Key;
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    let ui = AgendaHarness::new().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(800, 600));
    ui.global::<MotionSettings>().set_enabled(false);
    ui.global::<CalendarAgendaNavigation>()
        .on_index(|rows, key| {
            rows.iter()
                .position(|row| row.kind == "event" && row.key == key)
                .map(|i| i as i32)
                .unwrap_or(-1)
        });
    let model = Rc::new(VecModel::from(rows(1500)));
    ui.set_rows(model.clone().into());
    ui.set_count(1500);
    ui.set_filters(ModelRc::new(VecModel::from(vec![
        CalendarAgendaFilterRow {
            id: -1,
            name: "Local calendar".into(),
            checked: true,
            ..Default::default()
        },
    ])));
    let opened = Rc::new(Cell::new(-1));
    let result = opened.clone();
    ui.on_opened(move |id| result.set(id));
    let toggled = Rc::new(Cell::new((0, true)));
    let result = toggled.clone();
    ui.on_filter_toggled(move |id, checked| result.set((id, checked)));
    let created = Rc::new(Cell::new(false));
    let result = created.clone();
    ui.on_new_event(move || result.set(true));
    let resets = Rc::new(Cell::new(0));
    let result = resets.clone();
    ui.on_reset_filters(move || result.set(result.get() + 1));
    ui.show().unwrap();
    render(&window, "busy-initial");
    click(&ui, 220., 160.);
    assert_eq!(opened.get(), 1);
    ui.invoke_focus_list();
    key(&ui, Key::DownArrow);
    assert_eq!(ui.get_cursor(), "event-2");
    for _ in 0..12 {
        key(&ui, Key::DownArrow);
    }
    render(&window, "keyboard-scroll");
    assert!(ui.get_scroll_y() < -400.);
    key(&ui, Key::Return);
    assert_eq!(opened.get(), 14);
    // A live Now marker inserted before the cursor must preserve event identity.
    let mut changed = rows(1500);
    changed.insert(
        2,
        CalendarAgendaRow {
            key: "now".into(),
            kind: "now".into(),
            time_label: "09:45".into(),
            ..Default::default()
        },
    );
    annotate(&mut changed);
    model.set_vec(changed);
    render(&window, "live-now");
    key(&ui, Key::Return);
    assert_eq!(opened.get(), 14);
    key(&ui, Key::DownArrow);
    key(&ui, Key::Return);
    assert_eq!(opened.get(), 15);
    // Return to Today even when the selected date itself has not changed.
    ui.set_anchor(0.);
    ui.set_scroll_request(1);
    render(&window, "today-reset");
    assert_eq!(ui.get_scroll_y(), 0.);
    // Jump near the tail: virtualization must keep the final event actionable.
    ui.set_anchor(1501. * 68.);
    ui.set_scroll_request(2);
    render(&window, "busy-tail");
    ui.invoke_focus_list();
    key(&ui, Key::UpArrow);
    key(&ui, Key::Return);
    assert_eq!(opened.get(), 1500);
    // The filter opens by keyboard and its checkboxes accept Space.
    ui.set_anchor(0.);
    ui.set_scroll_request(3);
    render(&window, "filter-before");
    click(&ui, 768., 24.);
    render(&window, "filter-open");
    key(&ui, Key::Tab);
    key(&ui, Key::Space);
    assert_eq!(toggled.get(), (-1, false));
    key(&ui, Key::Escape);
    // Phone/tablet resizing retains pointer actions in both themes.
    model.set_vec(rows(4));
    ui.set_count(4);
    ui.set_compact(true);
    for (width, height, dark, name) in [
        (390, 844, false, "phone-light"),
        (390, 844, true, "phone-dark"),
        (768, 1024, false, "tablet-light"),
        (768, 1024, true, "tablet-dark"),
    ] {
        ui.window()
            .set_size(slint::PhysicalSize::new(width, height));
        ui.set_dark(dark);
        ui.set_scroll_request(ui.get_scroll_request() + 1);
        render(&window, name);
        click(&ui, 150., 160.);
        assert_eq!(opened.get(), 1);
    }
    model.set_vec(vec![]);
    ui.set_count(0);
    ui.set_filter_count(1);
    render(&window, "filtered-empty");
    ui.invoke_focus_list();
    key(&ui, Key::Tab);
    key(&ui, Key::Return);
    assert_eq!(resets.get(), 1);
    model.set_vec(rows(4));
    ui.set_count(4);
    ui.set_filter_count(0);
    render(&window, "filters-reset");
    click(&ui, 150., 160.);
    assert_eq!(opened.get(), 1);
    model.set_vec(vec![]);
    ui.set_count(0);
    render(&window, "empty");
    // Tab from the focused list reaches the empty-state action.
    ui.invoke_focus_list();
    key(&ui, Key::Tab);
    key(&ui, Key::Return);
    assert!(created.get());
}
