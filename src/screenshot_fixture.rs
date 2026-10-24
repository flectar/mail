//! Load the prepared desktop screenshot fixture into the native Slint window.
//! Both renderers receive the output of scripts/prepare-screenshot-data.jq.
use super::*;
use serde_json::Value;
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

/// The real application window, rendered offscreen with the prepared fixture.
pub(super) struct ScreenshotWindow {
    pub app: AppWindow,
    window: Rc<MinimalSoftwareWindow>,
    scale: f32,
}

impl ScreenshotWindow {
    pub fn new(scale: f32) -> Self {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
        let app = AppWindow::new().unwrap();
        app.window()
            .dispatch_event(WindowEvent::ScaleFactorChanged {
                scale_factor: scale,
            });
        apply(&app);
        app.show().unwrap();
        Self { app, window, scale }
    }

    /// Resize to `width`×`height` logical pixels and let layout, change
    /// handlers and transitions settle before painting the final frame.
    pub fn capture(&self, width: u32, height: u32) -> image::RgbImage {
        let (physical_width, physical_height) = (
            (width as f32 * self.scale) as u32,
            (height as f32 * self.scale) as u32,
        );
        self.app
            .window()
            .set_size(slint::PhysicalSize::new(physical_width, physical_height));
        let mut pixels = vec![Rgb8Pixel::default(); (physical_width * physical_height) as usize];
        for _ in 0..3 {
            slint::platform::update_timers_and_animations();
            std::thread::sleep(std::time::Duration::from_millis(300));
            slint::platform::update_timers_and_animations();
            self.app.window().request_redraw();
            self.window.draw_if_needed(|r| {
                r.render(&mut pixels, physical_width as usize);
            });
        }
        self.app.window().request_redraw();
        self.window.draw_if_needed(|r| {
            r.render(&mut pixels, physical_width as usize);
        });
        let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        image::RgbImage::from_raw(physical_width, physical_height, bytes).unwrap()
    }
}

trait FixtureValue: Sized {
    fn read(value: &Value) -> Self;
}

impl FixtureValue for slint::SharedString {
    fn read(value: &Value) -> Self {
        value.as_str().unwrap().into()
    }
}
impl FixtureValue for bool {
    fn read(value: &Value) -> Self {
        value.as_bool().unwrap()
    }
}
impl FixtureValue for i32 {
    fn read(value: &Value) -> Self {
        value.as_i64().unwrap().try_into().unwrap()
    }
}
impl FixtureValue for f32 {
    fn read(value: &Value) -> Self {
        value.as_f64().unwrap() as f32
    }
}
impl FixtureValue for slint::Color {
    fn read(value: &Value) -> Self {
        let hex = value.as_str().unwrap().strip_prefix('#').unwrap();
        assert_eq!(hex.len(), 6, "screenshot colors must be #RRGGBB");
        Self::from_argb_encoded(0xff000000 | u32::from_str_radix(hex, 16).unwrap())
    }
}
impl FixtureValue for slint::Image {
    fn read(value: &Value) -> Self {
        let path = value.as_str().unwrap();
        if path.is_empty() {
            Self::default()
        } else {
            Self::load_from_path(std::path::Path::new(path)).unwrap()
        }
    }
}
impl<T: FixtureValue + Clone + 'static> FixtureValue for ModelRc<T> {
    fn read(value: &Value) -> Self {
        ModelRc::new(VecModel::from(
            value
                .as_array()
                .unwrap()
                .iter()
                .map(T::read)
                .collect::<Vec<_>>(),
        ))
    }
}

// Unspecified fields retain Slint's defaults, as they do in slint-viewer.
macro_rules! fixture_row {
    ($row:ident { $($field:ident),* $(,)? }) => {
        impl FixtureValue for $row {
            fn read(value: &Value) -> Self {
                let mut row = Self::default();
                $(if let Some(value) = value.get(stringify!($field)) {
                    row.$field = FixtureValue::read(value);
                })*
                row
            }
        }
    };
}

fixture_row!(MailLabelRow {
    id,
    account_id,
    account_name,
    name,
    display_name,
    depth,
    has_children,
    expanded,
    can_create_children,
    name_has_emoji,
    color,
    applied,
    is_auto,
    is_global
});
fixture_row!(MailboxRow {
    account_id,
    folder_id,
    parent_folder_id,
    depth,
    has_children,
    expanded,
    is_standard,
    is_selectable,
    can_create_children,
    can_rename,
    can_delete,
    label_has_emoji,
    label,
    scope,
    context,
    detail,
    avatar,
    avatar_image,
    has_avatar,
    is_account,
    count,
    color,
    has_custom_color
});
fixture_row!(SidebarRow {
    key,
    kind,
    mailbox,
    label,
    open,
    show_row,
    reveal_row
});
fixture_row!(EmailRow {
    id,
    account_id,
    account,
    account_color,
    account_profile,
    show_account_marker,
    folder,
    sender,
    address,
    initials,
    favicon,
    favicon_small,
    has_favicon,
    subject,
    preview,
    time,
    unread,
    starred,
    has_attachments,
    has_replied,
    message_count,
    label_summary,
    labels,
    selected,
    checked
});
fixture_row!(MailListEntry {
    is_header,
    group_key,
    group_kind,
    group_count,
    expanded,
    show_row,
    reveal_row,
    email_index,
    email
});
fixture_row!(AccountRow {
    id,
    drag_key,
    name,
    email,
    provider,
    mail_protocol,
    status,
    sync_error,
    initials,
    avatar,
    avatar_small,
    has_avatar,
    profile_id,
    profile_name,
    profile_color,
    has_profile,
    has_account_color,
    username,
    jmap_url,
    incoming_security,
    smtp_security,
    trusted_certificate_pem,
    incoming_host,
    incoming_port,
    smtp_host,
    smtp_port,
    mail_history,
    calendar_connected,
    calendar_enabled,
    calendar_status,
    caldav_url,
    caldav_username,
    carddav_connected,
    carddav_enabled,
    carddav_status,
    carddav_url,
    carddav_username
});
fixture_row!(MailProfileRow {
    id,
    name,
    color,
    account_count
});
fixture_row!(EmailTile { image, y, height });

impl FixtureValue for SidebarRowKind {
    fn read(value: &Value) -> Self {
        match value.as_str().unwrap() {
            "unified-heading" => Self::UnifiedHeading,
            "unified-section" => Self::UnifiedSection,
            "unified-inbox" => Self::UnifiedInbox,
            "unified-mailbox" => Self::UnifiedMailbox,
            "categories-section" => Self::CategoriesSection,
            "important" => Self::Important,
            "other" => Self::Other,
            "category" => Self::Category,
            "global-labels-section" => Self::GlobalLabelsSection,
            "global-label" => Self::GlobalLabel,
            "accounts-heading" => Self::AccountsHeading,
            "account" => Self::Account,
            "folder" => Self::Folder,
            "account-labels-section" => Self::AccountLabelsSection,
            "account-label" => Self::AccountLabel,
            "new-folder" => Self::NewFolder,
            "new-label" => Self::NewLabel,
            kind => panic!("unknown screenshot sidebar kind: {kind}"),
        }
    }
}

fn apply(app: &AppWindow) {
    let path = std::env::var_os("FLECTAR_SCREENSHOT_DATA")
        .expect("run scripts/generate-screenshots.sh to prepare FLECTAR_SCREENSHOT_DATA");
    let data: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    app.set_startup_ready(FixtureValue::read(&data["startup_ready"]));
    app.set_startup_hydrated(FixtureValue::read(&data["startup_hydrated"]));
    app.set_active_view(FixtureValue::read(&data["active_view"]));
    app.set_workspace_layout(FixtureValue::read(&data["workspace_layout"]));
    app.set_text_mode(FixtureValue::read(&data["text_mode"]));
    app.set_has_selected(FixtureValue::read(&data["has_selected"]));
    app.set_selected_scope(FixtureValue::read(&data["selected_scope"]));
    app.set_unified_count(FixtureValue::read(&data["unified_count"]));
    app.set_selected_sender(FixtureValue::read(&data["selected_sender"]));
    app.set_selected_address(FixtureValue::read(&data["selected_address"]));
    app.set_selected_subject(FixtureValue::read(&data["selected_subject"]));
    app.set_selected_time(FixtureValue::read(&data["selected_time"]));
    app.set_selected_to(FixtureValue::read(&data["selected_to"]));
    app.set_selected_label(FixtureValue::read(&data["selected_label"]));
    app.set_selected_initials(FixtureValue::read(&data["selected_initials"]));
    app.set_selected_favicon(FixtureValue::read(&data["selected_favicon"]));
    app.set_selected_has_favicon(FixtureValue::read(&data["selected_has_favicon"]));
    app.set_selected_plain_text(FixtureValue::read(&data["selected_plain_text"]));
    app.set_selected_starred(FixtureValue::read(&data["selected_starred"]));
    app.set_selected_unread(FixtureValue::read(&data["selected_unread"]));
    app.set_selected_mail_label_count(FixtureValue::read(&data["selected_mail_label_count"]));
    app.set_emails(FixtureValue::read(&data["emails"]));
    app.set_mail_list_entries(FixtureValue::read(&data["mail_list_entries"]));
    app.set_sidebar_rows(FixtureValue::read(&data["sidebar_rows"]));
    app.set_connected_accounts(FixtureValue::read(&data["connected_accounts"]));
    app.set_mail_profiles(FixtureValue::read(&data["mail_profiles"]));
    app.set_mail_labels(FixtureValue::read(&data["mail_labels"]));
    app.set_mail_label_results(FixtureValue::read(&data["mail_label_results"]));
    app.set_email_tiles(FixtureValue::read(&data["email_tiles"]));
    app.set_show_account_markers(FixtureValue::read(&data["show_account_markers"]));
    app.set_email_content_aspect(FixtureValue::read(&data["email-content-aspect"]));
    app.global::<MotionSettings>().set_enabled(false);
}
