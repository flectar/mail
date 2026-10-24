// Every visible text must fit the box it is laid out in. Type sizes follow
// the font size the desktop reports, so a height tuned for Slint's 12px
// fallback cuts the bottom off glyphs where KDE reports 10pt (13.3px) or
// GNOME 11pt (14.7px). This lays out the real window in each state below at
// those base sizes, in English and the translations the bundled font covers.
// Each text element must get the size its own content needs, no clipping
// container may cut it, and a button's label must stay inside the button.
// Compared with the English layout at the fallback size, a text must not
// spill out of a surface that held it or overlap its neighbors more, and an
// input that keeps its width must still show its whole value.
//
// Build with SLINT_EMIT_DEBUG_INFO=1 to name the offending elements. Set
// FLECTAR_TEXT_FIT_SNAPSHOTS to a directory to save a render of each state.

use flectar_mail::{
    AccountImport, AccountRow, AppIcon, AppTheme, AppWindow, CalendarAgendaRow, CalendarDay,
    CalendarEventRow, CalendarSourceRow, CalendarWeekDay, ContactAccountRow, ContactRow,
    ContactSidebarRow, ContactSidebarRowKind, ContactSyncState, ContactValueRow, EmailRow,
    ImportAccountRow, ImportFailureRow, ImportFindingRow, ImportProfileRow, LocalizedMessage,
    MailAttachment, MailAttachments, MailLabelRow, MailListEntry, MailProfileRow, MailboxRow,
    MotionSettings, ProfileMemberRow, SidebarRow, SidebarRowKind, StorageUsageRow,
    ThreadMessageRow,
};
use i_slint_core::accessibility::{
    AccessibilityAction, AccessibleStringProperty, SupportedAccessibilityAction,
};
use i_slint_core::item_rendering::{PlainOrStyledText, RenderString, RenderText};
use i_slint_core::item_tree::{ItemRc, ParentItemTraversalMode};
use i_slint_core::items::{
    AccessibleRole, BasicBorderRectangle, BorderRectangle, Clip, ComplexText, Flickable, Opacity,
    Rectangle, SimpleText, StyledTextItem, TextHorizontalAlignment, TextInput, TextOverflow,
    TextVerticalAlignment, TextWrap,
};
use i_slint_core::layout::Orientation;
use i_slint_core::lengths::{LogicalLength, LogicalPoint, LogicalRect, LogicalSize};
use i_slint_core::window::WindowInner;
use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use slint::{ComponentHandle, Model, ModelRc, Rgb8Pixel, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::rc::Rc;
use std::time::Duration;

// Slint's fallback, then KDE 10pt, GNOME 11pt and a 12pt desktop font.
const BASE_FONT_SIZES: [f32; 4] = [12.0, 40.0 / 3.0, 44.0 / 3.0, 16.0];
const WINDOWS: [(&str, u32, u32); 2] = [("desktop", 1320, 800), ("phone", 390, 844)];
// Catalogs the bundled font covers, at the fallback and the largest size;
// their longer strings mostly test widths. Russian and Chinese text falls
// back to system fonts, which differ between machines.
const TRANSLATIONS: [&str; 4] = ["de", "es", "fr", "tr"];
// Measured sizes are rounded up to whole logical pixels.
const TOLERANCE: f32 = 0.5;
// Line boxes leave room above capitals and below descenders, so stacked
// lines may share this much of them without their glyphs meeting.
const LEADING: f32 = 2.0;

thread_local! {
    // The platform clock; `settle` advances it so every animation and timer
    // completes before a layout is measured.
    static CLOCK: Cell<Duration> = const { Cell::new(Duration::ZERO) };
}

struct Headless(Rc<MinimalSoftwareWindow>);
impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
    fn duration_since_start(&self) -> Duration {
        CLOCK.get()
    }
}

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(rows))
}

fn message(key: &str) -> LocalizedMessage {
    LocalizedMessage {
        key: key.into(),
        ..Default::default()
    }
}

// Between them, the accounts show every calendar and contacts control an
// account card can have.
fn accounts() -> Vec<AccountRow> {
    vec![
        AccountRow {
            id: 1,
            drag_key: "1".into(),
            name: "Alex Morgan".into(),
            email: "alex.morgan@example.com".into(),
            provider: "generic".into(),
            mail_protocol: "imap".into(),
            status: "Synced".into(),
            initials: "AM".into(),
            profile_id: "work".into(),
            profile_name: "Work".into(),
            profile_color: slint::Color::from_rgb_u8(0x8b, 0x5c, 0xf6),
            has_profile: true,
            username: "alex.morgan@example.com".into(),
            incoming_security: "tls".into(),
            smtp_security: "tls".into(),
            incoming_host: "imap.example.com".into(),
            incoming_port: "993".into(),
            smtp_host: "smtp.example.com".into(),
            smtp_port: "465".into(),
            mail_history: "sixMonths".into(),
            calendar_connected: true,
            calendar_enabled: true,
            calendar_status: "Connected".into(),
            caldav_url: "https://dav.example.com/calendars/alex/".into(),
            caldav_username: "alex.morgan@example.com".into(),
            carddav_connected: true,
            carddav_enabled: true,
            carddav_url: "https://dav.example.com/addressbooks/alex/".into(),
            carddav_username: "alex.morgan@example.com".into(),
            ..Default::default()
        },
        AccountRow {
            id: 2,
            drag_key: "2".into(),
            name: "Support Team".into(),
            email: "support@example.org".into(),
            provider: "gmail".into(),
            mail_protocol: "gmail".into(),
            status: "Sync failed".into(),
            sync_error: "The server closed the connection before the mailbox was selected.".into(),
            initials: "ST".into(),
            mail_history: "sixMonths".into(),
            ..Default::default()
        },
        AccountRow {
            id: 3,
            drag_key: "3".into(),
            name: "Sam Rivera".into(),
            email: "sam@example.net".into(),
            provider: "generic".into(),
            mail_protocol: "imap".into(),
            status: "idle".into(),
            initials: "SR".into(),
            username: "sam@example.net".into(),
            incoming_security: "tls".into(),
            smtp_security: "tls".into(),
            mail_history: "sixMonths".into(),
            carddav_connected: true,
            carddav_status: "Authentication failed".into(),
            ..Default::default()
        },
    ]
}

fn emails() -> Vec<EmailRow> {
    let row = |id: i32, sender: &str, subject: &str, preview: &str, time: &str| EmailRow {
        id,
        account_id: 1,
        account: "Alex Morgan".into(),
        folder: "Inbox".into(),
        sender: sender.into(),
        address: "sender@example.com".into(),
        initials: sender[..1].into(),
        subject: subject.into(),
        preview: preview.into(),
        time: time.into(),
        unread: id % 2 == 0,
        starred: id == 1,
        has_attachments: id == 2,
        message_count: if id == 1 { 3 } else { 1 },
        label_summary: if id == 3 { "Travel".into() } else { "".into() },
        selected: id == 1,
        ..Default::default()
    };
    vec![
        row(
            1,
            "Jamie Rivera",
            "Quarterly planning notes and the revised agenda for Thursday",
            "Here are the notes from today, including the open questions about the launch.",
            "9:41 AM",
        ),
        row(
            2,
            "Priya Shah",
            "Invoice 2026-118",
            "Attached is the invoice for September. Payment is due within thirty days.",
            "Yesterday",
        ),
        row(
            3,
            "Northwind Travel",
            "Your itinerary has changed",
            "Your departure time moved by forty minutes. No action is needed.",
            "Mon",
        ),
        row(
            4,
            "Sam Okafor",
            "Re: Design review",
            "Sounds good, let us meet after lunch.",
            "Sep 12",
        ),
    ]
}

fn mail_entries(grouped: bool) -> Vec<MailListEntry> {
    let header = |kind: &str| MailListEntry {
        is_header: true,
        group_key: kind.into(),
        group_kind: kind.into(),
        group_count: 2,
        expanded: true,
        show_row: true,
        ..Default::default()
    };
    let mut entries = Vec::new();
    for (index, email) in emails().into_iter().enumerate() {
        if grouped && index % 2 == 0 {
            entries.push(header(if index == 0 { "today" } else { "last-week" }));
        }
        entries.push(MailListEntry {
            show_row: true,
            email_index: index as i32,
            email,
            ..Default::default()
        });
    }
    entries
}

fn sidebar_rows() -> Vec<SidebarRow> {
    let mailbox = |label: &str, scope: &str, count: &str| MailboxRow {
        account_id: 1,
        label: label.into(),
        scope: scope.into(),
        count: count.into(),
        is_standard: true,
        is_selectable: true,
        ..Default::default()
    };
    let row = |key: &str, kind, mailbox| SidebarRow {
        key: key.into(),
        kind,
        mailbox,
        open: true,
        show_row: true,
        ..Default::default()
    };
    vec![
        row(
            "unified-heading",
            SidebarRowKind::UnifiedHeading,
            MailboxRow::default(),
        ),
        row(
            "unified-inbox",
            SidebarRowKind::UnifiedInbox,
            mailbox("Unified Inbox", "Unified Inbox", "14"),
        ),
        row(
            "accounts-heading",
            SidebarRowKind::AccountsHeading,
            MailboxRow::default(),
        ),
        row(
            "account-1",
            SidebarRowKind::Account,
            MailboxRow {
                is_account: true,
                avatar: "AM".into(),
                detail: "alex.morgan@example.com".into(),
                ..mailbox("Alex Morgan", "account:1", "9")
            },
        ),
        row(
            "folder-1",
            SidebarRowKind::Folder,
            mailbox("Inbox", "folder:1", "9"),
        ),
        row(
            "folder-2",
            SidebarRowKind::Folder,
            mailbox("Sent", "folder:2", ""),
        ),
        row(
            "folder-3",
            SidebarRowKind::Folder,
            mailbox("Archive", "folder:3", ""),
        ),
        row(
            "new-folder-1",
            SidebarRowKind::NewFolder,
            mailbox("", "", ""),
        ),
    ]
}

fn calendar_event(id: i32, title: &str, day_index: i32, start_minutes: i32) -> CalendarEventRow {
    CalendarEventRow {
        id,
        editable: true,
        title: title.into(),
        detail: "Conference room 2 · Alex Morgan".into(),
        iso_date: format!("2026-10-{:02}", 19 + day_index).into(),
        day_index,
        month_index: 22 + day_index,
        start_minutes,
        duration_minutes: 60,
        color_index: id % 4,
        provider: "CalDAV".into(),
        account: "alex.morgan@example.com".into(),
        calendar: "Work".into(),
        location: "Conference room 2".into(),
        organizer: "Alex Morgan".into(),
        description: "Review the open launch questions and agree on owners.".into(),
        attendees: "Jamie Rivera, Priya Shah".into(),
        status: "confirmed".into(),
        ..Default::default()
    }
}

fn work_profile() -> MailProfileRow {
    MailProfileRow {
        id: "work".into(),
        name: "Work".into(),
        color: slint::Color::from_rgb_u8(0x8b, 0x5c, 0xf6),
        account_count: 1,
    }
}

fn apply_fixture(app: &AppWindow) {
    app.global::<MotionSettings>().set_enabled(false);
    // Linux shows the app icon card with its longest description and note.
    app.global::<AppIcon>().set_available(true);
    app.global::<AppIcon>().set_platform("linux".into());
    app.set_startup_ready(true);
    app.set_startup_hydrated(true);
    app.set_connected_accounts(model(accounts()));
    app.set_mail_profiles(model(vec![work_profile()]));
    // The profile editor's checklist; the app fills it as the editor opens.
    app.set_profile_members(model(
        accounts()
            .into_iter()
            .map(|account| ProfileMemberRow {
                id: account.id,
                checked: account.has_profile,
                other_profile: if account.id == 3 {
                    "Personal".into()
                } else {
                    Default::default()
                },
                name: account.name,
                email: account.email,
                initials: account.initials,
                ..Default::default()
            })
            .collect(),
    ));
    app.set_mail_list_entries(model(mail_entries(false)));
    app.set_emails(model(emails()));
    app.set_sidebar_rows(model(sidebar_rows()));
    app.set_unified_count("14".into());
    app.set_has_selected(true);
    app.set_text_mode(true);
    app.set_selected_sender("Jamie Rivera".into());
    app.set_selected_address("jamie.rivera@example.com".into());
    app.set_selected_subject("Quarterly planning notes and the revised agenda for Thursday".into());
    app.set_selected_time("Oct 19, 2026, 9:41 AM".into());
    app.set_selected_to("Alex Morgan".into());
    app.set_selected_initials("JR".into());
    app.set_selected_plain_text(
        "Hi Alex,\n\nHere are the notes from today, including the open questions about the launch.\n\nJamie"
            .into(),
    );
    app.set_thread_messages(model(vec![
        ThreadMessageRow {
            index: 0,
            sender: "Jamie Rivera".into(),
            address: "jamie.rivera@example.com".into(),
            initials: "JR".into(),
            preview: "Here are the notes from today.".into(),
            time: "Oct 18".into(),
            to: "Alex Morgan".into(),
            ..Default::default()
        },
        ThreadMessageRow {
            index: 1,
            sender: "Alex Morgan".into(),
            address: "alex.morgan@example.com".into(),
            initials: "AM".into(),
            preview: "Thanks, I added two questions.".into(),
            time: "9:41 AM".into(),
            to: "Jamie Rivera".into(),
            outgoing: true,
            selected: true,
            ..Default::default()
        },
    ]));
    let labels: Vec<MailLabelRow> = [("Travel", "#22a06b"), ("Receipts", "#8b5cf6")]
        .into_iter()
        .enumerate()
        .map(|(index, (name, color))| MailLabelRow {
            id: index as i32 + 1,
            account_id: 1,
            account_name: "Alex Morgan".into(),
            name: name.into(),
            display_name: name.into(),
            expanded: true,
            color: slint::Color::from_argb_encoded(
                0xff000000 | u32::from_str_radix(&color[1..], 16).unwrap(),
            ),
            applied: index == 0,
            ..Default::default()
        })
        .collect();
    app.set_mail_label_results(model(labels.clone()));
    app.set_mail_labels(model(labels));
    app.set_selected_mail_label_count(1);
    app.set_sync_message(message("Local data ready."));
    app.set_app_version("0.1.0-alpha.7".into());
    app.set_storage_loaded(true);
    app.set_storage_total("18.42 GB".into());
    app.set_storage_usage(model(
        [
            ("mail", "9.84 GB", 0.53, 0.0),
            ("attachments", "5.21 GB", 0.28, 0.53),
            ("files", "1.76 GB", 0.1, 0.81),
            ("databases", "1.34 GB", 0.07, 0.91),
            ("other", "279.6 MB", 0.02, 0.98),
        ]
        .into_iter()
        .map(|(kind, size, fraction, offset)| StorageUsageRow {
            kind: kind.into(),
            size: size.into(),
            fraction,
            offset,
            color: slint::Color::from_rgb_u8(0x3b, 0x82, 0xf6),
        })
        .collect(),
    ));

    let week = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    app.set_calendar_week_days(model(
        week.iter()
            .enumerate()
            .map(|(index, weekday)| CalendarWeekDay {
                weekday: (*weekday).into(),
                day: (19 + index).to_string().into(),
                iso_date: format!("2026-10-{}", 19 + index).into(),
                is_today: index == 0,
                ..Default::default()
            })
            .collect(),
    ));
    app.set_calendar_month_days(model(
        (0..42)
            .map(|index| {
                let day = (index + 28) % 31 + 1;
                CalendarDay {
                    day: day.to_string().into(),
                    iso_date: format!("2026-10-{day:02}").into(),
                    outside_month: !(3..34).contains(&index),
                    is_today: index == 21,
                    is_selected: index == 21,
                    has_events: index % 5 == 0,
                    ..Default::default()
                }
            })
            .collect(),
    ));
    let events = vec![
        calendar_event(1, "Quarterly planning review", 0, 9 * 60),
        calendar_event(2, "Design critique", 1, 11 * 60),
        calendar_event(3, "Customer call with Northwind", 3, 15 * 60),
        CalendarEventRow {
            all_day: true,
            ..calendar_event(4, "Company offsite", 2, 0)
        },
    ];
    app.set_calendar_agenda_rows(model(
        events
            .iter()
            .enumerate()
            .flat_map(|(index, event)| {
                [
                    CalendarAgendaRow {
                        key: format!("day-{index}").into(),
                        kind: "day".into(),
                        iso_date: event.iso_date.clone(),
                        date_label: "Monday, October 19".into(),
                        is_today: index == 0,
                        count: 1,
                        ..Default::default()
                    },
                    CalendarAgendaRow {
                        key: format!("event-{index}").into(),
                        kind: "event".into(),
                        iso_date: event.iso_date.clone(),
                        time_label: "9:00 AM".into(),
                        end_label: "10:00 AM".into(),
                        previous_event: index as i32 - 1,
                        next_event: index as i32 + 1,
                        event: event.clone(),
                        ..Default::default()
                    },
                ]
            })
            .collect(),
    ));
    app.set_calendar_agenda_event_count(events.len() as i32);
    app.set_calendar_events(model(events));
    app.set_calendar_sources(model(vec![CalendarSourceRow {
        id: 1,
        account_id: 1,
        group_start: true,
        name: "Work".into(),
        account_name: "Alex Morgan".into(),
        email: "alex.morgan@example.com".into(),
        provider: "CalDAV".into(),
        color: "#3b82f6".into(),
        enabled: true,
        is_default: true,
        last_synced: "Synced 2 minutes ago".into(),
        ..Default::default()
    }]));
    app.set_calendar_month_title("October 2026".into());
    app.set_calendar_period_title("Oct 19 – 25, 2026".into());
    app.set_calendar_week_label("Week 43".into());
    app.set_calendar_selected_date("2026-10-19".into());
    app.set_calendar_selected_date_has_events(true);
    app.set_calendar_timezone("Europe/Madrid".into());

    app.set_contacts(model(
        [
            ("Jamie Rivera", "jamie.rivera@example.com", "Northwind"),
            ("Priya Shah", "priya.shah@example.org", "Contoso Finance"),
            ("Sam Okafor", "sam.okafor@example.net", ""),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (name, email, company))| ContactRow {
            id: index as i32 + 1,
            name: name.into(),
            email: email.into(),
            initials: name[..1].into(),
            company: company.into(),
            detail: email.into(),
            last_contacted: "2 days ago".into(),
            favorite: index == 0,
            selected: index == 0,
            ..Default::default()
        })
        .collect(),
    ));
    let account = ContactAccountRow {
        id: 1,
        name: "Alex Morgan".into(),
        email: "alex.morgan@example.com".into(),
        initials: "AM".into(),
        count: "3".into(),
        ..Default::default()
    };
    app.set_contact_sidebar_rows(model(
        [
            ContactSidebarRowKind::UnifiedHeading,
            ContactSidebarRowKind::UnifiedSection,
            ContactSidebarRowKind::All,
            ContactSidebarRowKind::Favorites,
            ContactSidebarRowKind::Suggestions,
            ContactSidebarRowKind::AccountsHeading,
            ContactSidebarRowKind::Account,
            ContactSidebarRowKind::Contacts,
            ContactSidebarRowKind::Hint,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, kind)| ContactSidebarRow {
            key: index.to_string().into(),
            kind,
            // As in the app, only an account's own rows carry it.
            account: match kind {
                ContactSidebarRowKind::Account | ContactSidebarRowKind::Contacts => account.clone(),
                _ => ContactAccountRow::default(),
            },
            open: true,
        })
        .collect(),
    ));
    app.set_contact_total_count(3);
    app.set_contact_favorite_count(1);
    app.set_contact_has_selection(true);
    app.set_contact_id(1);
    app.set_contact_name("Jamie Rivera".into());
    app.set_contact_emails(model(vec![
        ContactValueRow {
            value: "jamie.rivera@example.com".into(),
            label: "WORK".into(),
            preference: 1,
            ..Default::default()
        },
        ContactValueRow {
            value: "jamie@example.net".into(),
            label: "HOME".into(),
            preference: 100,
            ..Default::default()
        },
    ]));
    app.set_contact_phones(model(vec![ContactValueRow {
        value: "+1 555 0100".into(),
        label: "CELL".into(),
        preference: 100,
        ..Default::default()
    }]));
    app.set_contact_source("Personal · jamie.rivera@example.com".into());
    app.set_contact_sync(ContactSyncState::Pending);
    app.on_usable_contact_email(|email| flectar_mail_core::carddav::vcard::usable_email(&email));
    app.set_contact_company("Northwind".into());
    app.set_contact_job_title("Product lead".into());
    app.set_contact_notes("Prefers morning meetings.".into());
    app.set_contact_initials("JR".into());
    app.set_contact_interactions("12 messages".into());
    app.set_contact_last_interacted("2 days ago".into());
}

// Each state starts from the populated mail window.
fn reset(app: &AppWindow) {
    // The phone editor stays open when switching workspaces. Return to the
    // contact list so each scenario starts from the same navigation state.
    if app.get_active_view() == "contacts" {
        app.invoke_system_back_requested();
    }
    if app.get_connected_accounts().row_count() == 0 {
        app.set_connected_accounts(model(accounts()));
    }
    app.set_startup_ready(true);
    app.set_workspace_layout("default".into());
    app.set_mail_list_entries(model(mail_entries(false)));
    app.set_active_view("mail".into());
    app.set_mail_sidebar_collapsed(false);
    app.set_active_mail_profile(MailProfileRow::default());
    app.set_settings_open(false);
    app.set_contact_source("Personal · jamie.rivera@example.com".into());
    app.set_contact_sync(ContactSyncState::Pending);
    app.set_settings_tab("General".into());
    app.set_compose_open(false);
    app.set_calendar_editor_open(false);
    app.set_rendering_info_open(false);
    app.set_oauth_setup_open(false);
    app.set_custom_oauth_open(false);
    app.set_account_form_open(false);
    app.global::<AppTheme>().set_preset("default".into());
    app.global::<MailAttachments>().set_rows(model(Vec::new()));
    app.global::<AccountImport>().set_open(false);
    WindowInner::from_pub(app.window()).close_all_popups();
}

fn import_finding(code: &str, detail: &str, blocking: bool) -> ImportFindingRow {
    ImportFindingRow {
        code: code.into(),
        detail: detail.into(),
        count: detail.parse().unwrap_or_default(),
        blocking,
    }
}

/// Opens the account import dialog at `stage` with a representative plan.
fn open_import(app: &AppWindow, stage: &str) {
    let import = app.global::<AccountImport>();
    import.set_folder_supported(true);
    import.set_profiles(model(vec![ImportProfileRow {
        name: "default-release".into(),
        path: "/home/jamie/.thunderbird/x7k2.default-release".into(),
        is_default: true,
    }]));
    import.set_source("thunderbird".into());
    import.set_accounts(model(vec![
        ImportAccountRow {
            email: "jamie.rivera@example.com".into(),
            label: "Work".into(),
            server: "imap.example.com".into(),
            status: "ready".into(),
            sign_in: "password".into(),
            selected: true,
            findings: model(vec![
                import_finding("password-mechanism-changed", "", false),
                import_finding("aliases-skipped", "2", false),
            ]),
        },
        ImportAccountRow {
            email: "jamie@outlook.com".into(),
            status: "ready".into(),
            sign_in: "microsoft".into(),
            selected: true,
            findings: model(vec![import_finding(
                "microsoft-sign-in-replaces-password",
                "",
                false,
            )]),
            ..Default::default()
        },
        ImportAccountRow {
            email: "archive@example.net".into(),
            label: "Old POP".into(),
            status: "unsupported".into(),
            sign_in: "password".into(),
            findings: model(vec![
                import_finding("pop3-unsupported", "pop.example.net", true),
                import_finding("unencrypted-outgoing", "smtp.example.net", true),
            ]),
            ..Default::default()
        },
    ]));
    import.set_selected_count(2);
    import.set_feeds(1);
    import.set_imported(2);
    import.set_already_present(1);
    import.set_failures(model(vec![ImportFailureRow {
        email: "broken@example.org".into(),
        error: "incomplete IMAP settings for broken@example.org".into(),
    }]));
    import.set_error_code("unknown-format".into());
    import.set_stage(stage.into());
    import.set_open(true);
}

thread_local! {
    // Translations of the current language, for finding controls by label.
    static CATALOG: RefCell<BTreeMap<String, String>> = RefCell::default();
}

/// Reads the source text to translation pairs of a bundled catalog.
fn load_catalog(language: &str) -> BTreeMap<String, String> {
    if language.is_empty() {
        return BTreeMap::new();
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("lang")
        .join(language)
        .join("LC_MESSAGES/flectar-mail.po");
    let unquote = |line: &str| {
        line.trim()
            .trim_matches('"')
            .replace("\\\"", "\"")
            .replace("\\n", "\n")
            .replace("\\\\", "\\")
    };
    let mut catalog = BTreeMap::new();
    let (mut id, mut text, mut field) = (String::new(), String::new(), "");
    // Slint skips fuzzy entries, so the app shows their source text.
    let (mut fuzzy, mut next_fuzzy) = (false, false);
    for line in std::fs::read_to_string(path).unwrap().lines() {
        if let Some(rest) = line.strip_prefix("msgid ") {
            if !id.is_empty() && !text.is_empty() && !fuzzy {
                catalog.insert(std::mem::take(&mut id), std::mem::take(&mut text));
            }
            (id, text, field) = (unquote(rest), String::new(), "id");
            fuzzy = std::mem::take(&mut next_fuzzy);
        } else if let Some(flags) = line.strip_prefix("#,") {
            next_fuzzy = flags.split(',').any(|flag| flag.trim() == "fuzzy");
            field = "";
        } else if let Some(rest) = line.strip_prefix("msgstr ") {
            (text, field) = (unquote(rest), "text");
        } else if line.starts_with('"') {
            match field {
                "id" => id += &unquote(line),
                "text" => text += &unquote(line),
                _ => {}
            }
        } else {
            field = "";
        }
    }
    if !id.is_empty() && !text.is_empty() && !fuzzy {
        catalog.insert(id, text);
    }
    catalog
}

/// The current language's text for a source string.
fn translate(text: &str) -> String {
    CATALOG
        .with_borrow(|catalog| catalog.get(text).cloned())
        .unwrap_or_else(|| text.into())
}

/// Activates the first shown control with this source-language label, the
/// way assistive technology does, to reach dialogs without a public property.
fn press(app: &AppWindow, label: &str) {
    press_labelled(app, &translate(label));
}

fn press_labelled(app: &AppWindow, label: &str) {
    let window = window_area(app);
    let mut shown = Vec::new();
    let found = roots(app).into_iter().find_map(|root| {
        root.visit_descendants(|item| {
            if paint_area(item, window).is_none()
                || !item
                    .supported_accessibility_actions()
                    .contains(SupportedAccessibilityAction::Default)
            {
                return ControlFlow::Continue(());
            }
            let text = item.accessible_string_property(AccessibleStringProperty::Label);
            if text.as_deref() == Some(label) {
                return ControlFlow::Break(item.clone());
            }
            shown.extend(text.map(|text| text.to_string()));
            ControlFlow::Continue(())
        })
    });
    found
        .unwrap_or_else(|| panic!("no shown control labelled {label:?}; found {shown:?}"))
        .accessible_action(&AccessibilityAction::Default);
}

/// The window's item tree and those of its open popups.
fn roots(app: &AppWindow) -> Vec<ItemRc> {
    let inner = WindowInner::from_pub(app.window());
    let mut roots = vec![ItemRc::new_root(inner.component())];
    roots.extend(
        inner
            .active_popups()
            .iter()
            .map(|popup| ItemRc::new_root(popup.component.clone())),
    );
    roots
}

struct State {
    name: String,
    setup: Box<dyn Fn(&AppWindow)>,
    /// The control that closes what `setup` opened when no property does.
    dismiss: Option<&'static str>,
    /// Reaches controls the phone layout keeps on another page.
    desktop_only: bool,
    /// Reaches controls only the phone layout has.
    phone_only: bool,
}

fn state(name: &str, setup: impl Fn(&AppWindow) + 'static) -> State {
    State {
        name: name.into(),
        setup: Box::new(setup),
        dismiss: None,
        desktop_only: false,
        phone_only: false,
    }
}

fn states() -> Vec<State> {
    let mut states = vec![
        state("mail", |_| {}),
        state("mail grouped", |app| {
            app.set_mail_list_entries(model(mail_entries(true)))
        }),
        state("mail minimal", |app| {
            app.set_workspace_layout("minimal".into())
        }),
        state("mail attachments", |app| {
            app.global::<MailAttachments>().set_rows(model(
                [
                    ("Quarterly plan.pdf", "PDF · 2.4 MB"),
                    ("Launch notes.docx", "Document · 88 KB"),
                ]
                .into_iter()
                .enumerate()
                .map(|(index, (name, detail))| MailAttachment {
                    id: index.to_string().into(),
                    name: name.into(),
                    detail: detail.into(),
                    previewable: index == 0,
                    ..Default::default()
                })
                .collect(),
            ))
        }),
        State {
            desktop_only: true,
            ..state("mail labels", |app| press(app, "Manage labels"))
        },
        // The tab strip with the sidebar closed, its pill before the tabs.
        State {
            desktop_only: true,
            ..state("mail sidebar collapsed", |app| {
                app.set_active_mail_profile(work_profile());
                app.set_mail_sidebar_collapsed(true);
            })
        },
        State {
            desktop_only: true,
            ..state("profile menu", |app| press(app, "Profile"))
        },
        State {
            desktop_only: true,
            ..state("profile menu with a profile", |app| {
                app.set_active_mail_profile(work_profile());
                press(app, "Profile");
            })
        },
        State {
            dismiss: Some("Close folders"),
            phone_only: true,
            ..state("mail folders", |app| {
                app.set_active_mail_profile(work_profile());
                press(app, "Hide folders");
            })
        },
        State {
            dismiss: Some("Cancel"),
            desktop_only: true,
            ..state("mail folder dialog", |app| press(app, "New folder"))
        },
        state("startup", |app| {
            app.set_connected_accounts(model(Vec::new()));
            app.set_startup_ready(false);
        }),
        state("onboarding", |app| {
            app.set_connected_accounts(model(Vec::new()))
        }),
        state("onboarding mail account", |app| {
            app.set_connected_accounts(model(Vec::new()));
            press(app, "Connect another provider");
        }),
        state("calendar week", |app| {
            app.set_active_view("calendar".into());
            app.set_calendar_view_mode("week".into());
        }),
        state("calendar month", |app| {
            app.set_active_view("calendar".into());
            app.set_calendar_view_mode("month".into());
        }),
        state("calendar agenda", |app| {
            app.set_active_view("calendar".into());
            app.set_calendar_view_mode("agenda".into());
        }),
        state("calendar event editor", |app| {
            app.set_active_view("calendar".into());
            app.set_calendar_event_title("Quarterly planning review".into());
            app.set_calendar_event_date("2026-10-19".into());
            app.set_calendar_editor_open(true);
        }),
        State {
            dismiss: Some("Close"),
            ..state("calendar event details", |app| {
                app.set_active_view("calendar".into());
                app.set_calendar_view_mode("week".into());
                let label =
                    translate("View details for {}").replace("{}", "Quarterly planning review");
                press_labelled(app, &label);
            })
        },
        state("contacts", |app| app.set_active_view("contacts".into())),
        state("contact editor", |app| {
            app.set_active_view("contacts".into());
            // The host callback is unset, so this opens the phone editor
            // while retaining the populated fixture for layout checks.
            press(app, "New contact");
        }),
        state("contact conflict", |app| {
            app.set_active_view("contacts".into());
            app.set_contact_sync(ContactSyncState::Conflict);
            press(app, "New contact");
        }),
        state("contact kept after removal", |app| {
            app.set_active_view("contacts".into());
            app.set_contact_source("".into());
            app.set_contact_sync(ContactSyncState::Detached);
            press(app, "New contact");
        }),
        state("contact sync failure", |app| {
            app.set_active_view("contacts".into());
            app.set_contact_sync(ContactSyncState::Failed);
            app.set_contact_sync_detail(
                "Server returned no ETag after saving; waiting to reconcile".into(),
            );
            press(app, "New contact");
        }),
        state("files", |app| app.set_active_view("files".into())),
        state("compose", |app| {
            app.set_compose_to("jamie.rivera@example.com".into());
            app.set_compose_subject("Re: Quarterly planning notes".into());
            app.set_compose_open(true);
        }),
        state("rendering info", |app| app.set_rendering_info_open(true)),
        state("oauth setup", |app| app.set_oauth_setup_open(true)),
        state("settings custom theme", |app| {
            app.global::<AppTheme>().set_preset("custom".into());
            app.set_settings_tab("General".into());
            app.set_settings_open(true);
        }),
        state("settings profile editor", |app| {
            app.set_settings_tab("Accounts".into());
            app.set_settings_open(true);
            press(app, "Add profile");
        }),
        State {
            dismiss: Some("Keep my data"),
            ..state("settings delete confirmation", |app| {
                app.set_settings_tab("Data".into());
                app.set_settings_open(true);
                press(app, "Delete everything");
            })
        },
        state("import choose", |app| open_import(app, "choose")),
        state("import review", |app| open_import(app, "review")),
        state("import done", |app| open_import(app, "done")),
        state("import error", |app| open_import(app, "error")),
        state("settings profile picker", |app| {
            app.set_settings_tab("Accounts".into());
            app.set_settings_open(true);
            press(app, "No profile");
        }),
    ];
    for tab in [
        "General",
        "Accounts",
        "Contacts",
        "Notifications",
        "Shortcuts",
        "Data",
        "Services",
        "About",
    ] {
        states.push(state(&format!("settings {tab}"), move |app| {
            app.set_settings_tab(tab.into());
            app.set_settings_open(true);
        }));
    }
    states
}

/// Runs change handlers and timers, and paints the frame when it is wanted.
fn settle(app: &AppWindow, window: &MinimalSoftwareWindow, paint: bool) -> Option<image::RgbImage> {
    let size = app.window().size();
    let mut pixels = vec![Rgb8Pixel::default(); (size.width * size.height) as usize];
    for _ in 0..2 {
        CLOCK.set(CLOCK.get() + Duration::from_secs(1));
        slint::platform::update_timers_and_animations();
        if paint {
            app.window().request_redraw();
            window.draw_if_needed(|renderer| {
                renderer.render(&mut pixels, size.width as usize);
            });
        }
    }
    let bytes = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    paint.then(|| image::RgbImage::from_raw(size.width, size.height, bytes).unwrap())
}

struct TextItem {
    text: String,
    wrap: TextWrap,
    overflow: TextOverflow,
    alignment: (TextHorizontalAlignment, TextVerticalAlignment),
    input: bool,
}

fn plain(text: PlainOrStyledText) -> String {
    match text {
        PlainOrStyledText::Plain(text) => text.to_string(),
        PlainOrStyledText::Styled(text) => {
            i_slint_core::styled_text::get_raw_text(&text).into_owned()
        }
    }
}

fn text_item(item: &ItemRc) -> Option<TextItem> {
    fn read<T: RenderText>(text: std::pin::Pin<&T>) -> TextItem {
        TextItem {
            text: plain(RenderString::text(text)),
            wrap: RenderText::wrap(text),
            overflow: RenderText::overflow(text),
            alignment: RenderText::alignment(text),
            input: false,
        }
    }
    if let Some(text) = item.downcast::<SimpleText>() {
        return Some(read(text.as_pin_ref()));
    }
    if let Some(text) = item.downcast::<ComplexText>() {
        return Some(read(text.as_pin_ref()));
    }
    if let Some(text) = item.downcast::<StyledTextItem>() {
        return Some(read(text.as_pin_ref()));
    }
    item.downcast::<TextInput>().map(|input| {
        let input = input.as_pin_ref();
        TextItem {
            text: input.text().to_string(),
            wrap: input.wrap(),
            overflow: TextOverflow::Clip,
            alignment: (input.horizontal_alignment(), input.vertical_alignment()),
            input: true,
        }
    })
}

fn window_rect(item: &ItemRc) -> LogicalRect {
    let geometry = item.geometry();
    LogicalRect::new(item.map_to_window(geometry.origin), geometry.size)
}

/// Returns the area this item may paint in, or `None` when it is hidden.
/// Clips outside the nearest scroll view only decide what is scrolled into
/// view, so a whole page is checked at once.
fn paint_area(item: &ItemRc, window: LogicalRect) -> Option<LogicalRect> {
    let mut area: Option<LogicalRect> = None;
    let mut scrolled = false;
    let mut current = item.parent_item(ParentItemTraversalMode::StopAtPopups);
    while let Some(ancestor) = current {
        if let Some(clip) = ancestor.downcast::<Clip>() {
            let clip = clip.as_pin_ref();
            if clip.is_visibility_clip() && clip.clip() {
                return None;
            }
        }
        if let Some(opacity) = ancestor.downcast::<Opacity>()
            && opacity.as_pin_ref().opacity() < 0.01
        {
            return None;
        }
        if ancestor.downcast::<Flickable>().is_some() {
            scrolled = true;
        } else if !scrolled && ancestor.borrow().as_ref().clips_children() {
            let rect = window_rect(&ancestor);
            area = Some(area.map_or(rect, |area| area.intersection(&rect).unwrap_or_default()));
        }
        current = ancestor.parent_item(ParentItemTraversalMode::StopAtPopups);
    }
    let rect = window_rect(item);
    let area = match area {
        Some(area) if scrolled => area,
        Some(area) => area.intersection(&window).unwrap_or_default(),
        None if scrolled => rect,
        None => window,
    };
    area.intersects(&rect).then_some(area)
}

// Standard widget internals name nothing a reader can find in ui/.
fn is_widget_internal(id: &str) -> bool {
    id.ends_with("::root")
        || [
            "ScrollView::",
            "ListView::",
            "VerticalBox::",
            "HorizontalBox::",
            "GridBox::",
        ]
        .iter()
        .any(|prefix| id.starts_with(prefix))
}

/// Names the element by its type and the nearest ids of the components
/// around it, e.g. `TextInput in CopyableDiagnostic::diagnostic-text`.
/// Components that are themselves a text element are reported once, by type.
fn element_key(item: &ItemRc) -> (String, u32) {
    let mut type_name = String::new();
    let mut ids: Vec<String> = Vec::new();
    let mut current = Some(item.clone());
    while let Some(element) = current {
        for index in 0..element.element_count().unwrap_or(0) {
            for (element_type, id) in element
                .element_type_names_and_ids(index)
                .unwrap_or_default()
            {
                if type_name.is_empty() {
                    type_name = element_type.to_string();
                }
                let id = if is_widget_internal(&id) {
                    // `Component::root` still names the component itself.
                    id.strip_suffix("::root")
                        .filter(|name| !is_widget_internal(&format!("{name}::")))
                        .map(str::to_owned)
                } else {
                    Some(id.to_string())
                };
                if let Some(id) = id.filter(|id| !id.is_empty())
                    && !ids.contains(&id)
                {
                    ids.push(id);
                }
            }
        }
        if ids.len() >= 3 {
            break;
        }
        current = element.parent_item(ParentItemTraversalMode::StopAtPopups);
    }
    match type_name.as_str() {
        "" => (
            "text (build with SLINT_EMIT_DEBUG_INFO=1 to name it)".into(),
            item.index(),
        ),
        "Text" | "TextInput" => (format!("{type_name} in {}", ids.join(" < ")), item.index()),
        component => (format!("component {component}"), 0),
    }
}

/// Finds the `.slint` line that spells out a source-language text.
struct Sources(Vec<(String, Vec<String>)>);

impl Sources {
    fn load() -> Self {
        fn visit(dir: &std::path::Path, files: &mut Vec<(String, Vec<String>)>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
            entries.sort_by_key(|entry| entry.path());
            for entry in entries {
                let path = entry.path();
                if path.is_dir() {
                    visit(&path, files);
                } else if path
                    .extension()
                    .is_some_and(|extension| extension == "slint")
                {
                    let relative = path
                        .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                        .unwrap()
                        .display()
                        .to_string();
                    let source = std::fs::read_to_string(&path).unwrap();
                    files.push((relative, source.lines().map(str::to_owned).collect()));
                }
            }
        }
        let mut files = Vec::new();
        visit(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ui"),
            &mut files,
        );
        Self(files)
    }

    fn find(&self, needle: &str) -> Option<String> {
        self.0.iter().find_map(|(path, lines)| {
            lines
                .iter()
                .position(|line| line.contains(needle))
                .map(|index| format!("{path}:{}", index + 1))
        })
    }

    /// Where a text-based component is declared, or where a text is spelled.
    fn locate(&self, key: &str, text: &str) -> Option<String> {
        if let Some(component) = key.strip_prefix("component ") {
            return self.find(&format!("component {component} inherits"));
        }
        let literal: String = text
            .chars()
            .take(48)
            .collect::<String>()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        if text.chars().count() > 48 {
            self.find(&format!("\"{literal}"))
        } else {
            self.find(&format!("\"{literal}\""))
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Problem {
    Height,
    Width,
    Container,
    Spill,
    Overlap,
    Hidden,
    Outside,
}

struct Finding {
    // Repeated rows share their item index within the same component.
    key: (String, u32, Problem),
    text: String,
    actual: f32,
    needed: f32,
    // The text drawn over this one.
    partner: String,
}

/// How far the text spills out of each surface around it, nearest first, up
/// to its scroll view. Layout boxes are not surfaces: content may overhang
/// their padding without drawing outside anything.
fn spills(item: &ItemRc) -> Vec<f32> {
    let rect = window_rect(item);
    let mut spills = Vec::new();
    let mut current = item.parent_item(ParentItemTraversalMode::StopAtPopups);
    while let Some(ancestor) = current {
        if ancestor.downcast::<Flickable>().is_some() {
            break;
        }
        let surface = ancestor.downcast::<Rectangle>().is_some()
            || ancestor.downcast::<BasicBorderRectangle>().is_some()
            || ancestor.downcast::<BorderRectangle>().is_some()
            || ancestor
                .downcast::<Clip>()
                .is_some_and(|clip| !clip.as_pin_ref().is_visibility_clip());
        if surface {
            let bounds = window_rect(&ancestor);
            spills.push(
                (bounds.min_y() - rect.min_y())
                    .max(rect.max_y() - bounds.max_y())
                    .max(bounds.min_x() - rect.min_x())
                    .max(rect.max_x() - bounds.max_x())
                    .max(0.0),
            );
        }
        current = ancestor.parent_item(ParentItemTraversalMode::StopAtPopups);
    }
    spills
}

// A text element and its place among the visited texts of that element, so
// each text is compared with itself at the fallback size.
type Instance = ((String, u32), usize);

/// How a text sits in its surroundings, compared with the fallback size.
struct Placement {
    text: String,
    /// How far it spills out of each surface around it, nearest first.
    spills: Vec<f32>,
    /// A single-line input's width, and how much of its value is scrolled
    /// out of view.
    input: Option<(f32, f32)>,
}
type Placements = BTreeMap<Instance, Placement>;
// Texts drawn over one another, by how deep they overlap.
type Overlaps = BTreeMap<(Instance, Instance), (f32, String, String)>;

/// Where a text is drawn, and the backdrop it shares with its neighbors.
struct Ink {
    instance: Instance,
    backdrop: (usize, Option<ItemRc>),
    rect: LogicalRect,
    text: String,
}

/// The part of its box a text with this content size is drawn in.
fn ink_rect(text: &TextItem, rect: LogicalRect, content: LogicalSize) -> LogicalRect {
    let size = LogicalSize::new(
        content.width.min(rect.width()),
        content.height.min(rect.height()),
    );
    let x = match text.alignment.0 {
        TextHorizontalAlignment::Center => (rect.width() - size.width) / 2.0,
        TextHorizontalAlignment::Right | TextHorizontalAlignment::End => rect.width() - size.width,
        _ => 0.0,
    };
    let y = match text.alignment.1 {
        TextVerticalAlignment::Center => (rect.height() - size.height) / 2.0,
        TextVerticalAlignment::Bottom => rect.height() - size.height,
        _ => 0.0,
    };
    LogicalRect::new(LogicalPoint::new(rect.min_x() + x, rect.min_y() + y), size)
}

/// What every clip around the item, scroll views included, leaves visible.
fn clip_area(item: &ItemRc, window: LogicalRect) -> LogicalRect {
    let mut area = window;
    let mut current = item.parent_item(ParentItemTraversalMode::StopAtPopups);
    while let Some(ancestor) = current {
        if ancestor.borrow().as_ref().clips_children() {
            area = area
                .intersection(&window_rect(&ancestor))
                .unwrap_or_default();
        }
        current = ancestor.parent_item(ParentItemTraversalMode::StopAtPopups);
    }
    area
}

/// The nearest ancestor that paints behind the text. Texts on different
/// backdrops may be stacked on purpose, like a dialog over the window.
fn backdrop(item: &ItemRc) -> Option<ItemRc> {
    let mut current = item.parent_item(ParentItemTraversalMode::StopAtPopups);
    while let Some(ancestor) = current {
        let background = if let Some(rect) = ancestor.downcast::<Rectangle>() {
            Some(rect.as_pin_ref().background())
        } else if let Some(rect) = ancestor.downcast::<BasicBorderRectangle>() {
            Some(rect.as_pin_ref().background())
        } else {
            ancestor
                .downcast::<BorderRectangle>()
                .map(|rect| rect.as_pin_ref().background())
        };
        if background.is_some_and(|background| !background.is_transparent()) {
            return Some(ancestor);
        }
        current = ancestor.parent_item(ParentItemTraversalMode::StopAtPopups);
    }
    None
}

/// The button a text is the label of, if any.
fn button(item: &ItemRc) -> Option<ItemRc> {
    let mut current = item.parent_item(ParentItemTraversalMode::StopAtPopups);
    while let Some(ancestor) = current {
        if ancestor.accessible_role() == AccessibleRole::Button {
            return Some(ancestor);
        }
        current = ancestor.parent_item(ParentItemTraversalMode::StopAtPopups);
    }
    None
}

fn overlaps(inks: &[Ink]) -> Overlaps {
    let mut overlaps = Overlaps::new();
    for (index, ink) in inks.iter().enumerate() {
        for other in inks[index + 1..]
            .iter()
            .filter(|other| other.backdrop == ink.backdrop)
        {
            let Some(shared) = ink.rect.intersection(&other.rect) else {
                continue;
            };
            // A shared strip wider than it is tall is between stacked lines;
            // otherwise the texts are side by side.
            let (depth, allowed) = if shared.height() <= shared.width() {
                (shared.height(), LEADING)
            } else {
                (shared.width(), 0.0)
            };
            if depth > allowed + TOLERANCE {
                overlaps.insert(
                    (ink.instance.clone(), other.instance.clone()),
                    (depth, ink.text.clone(), other.text.clone()),
                );
            }
        }
    }
    overlaps
}

fn window_area(app: &AppWindow) -> LogicalRect {
    let size = app.window().size().to_logical(app.window().scale_factor());
    LogicalRect::new(
        LogicalPoint::default(),
        LogicalSize::new(size.width, size.height),
    )
}

fn check(app: &AppWindow) -> (Vec<Finding>, Placements, Overlaps) {
    let adapter = WindowInner::from_pub(app.window()).window_adapter();
    let window = window_area(app);
    let mut findings = Vec::new();
    let mut placements = Placements::new();
    let mut inks = Vec::new();
    let mut instances: BTreeMap<(String, u32), usize> = BTreeMap::new();
    for (root_index, root) in roots(app).into_iter().enumerate() {
        root.visit_descendants(|item| {
            let Some(text) = text_item(item) else {
                return ControlFlow::<()>::Continue(());
            };
            if text.text.trim().is_empty() {
                return ControlFlow::Continue(());
            }
            let Some(area) = paint_area(item, window) else {
                return ControlFlow::Continue(());
            };
            let key = element_key(item);
            let geometry = item.geometry();
            let layout = |orientation, cross_axis: f32| {
                item.borrow()
                    .as_ref()
                    .layout_info(orientation, cross_axis, &adapter, item)
            };
            let mut report = |problem, actual: f32, needed: f32| {
                findings.push(Finding {
                    key: (key.0.clone(), key.1, problem),
                    text: text.text.clone(),
                    actual,
                    needed,
                    partner: String::new(),
                })
            };

            let vertical = layout(Orientation::Vertical, geometry.width());
            // Elided text needs room for one line; anything else for all of it.
            let mut height = vertical.min;
            if text.overflow == TextOverflow::Elide {
                height = height.min(layout(Orientation::Vertical, f32::MAX).min);
            }
            if geometry.height() + TOLERANCE < height {
                report(Problem::Height, geometry.height(), height);
            }
            let width = layout(Orientation::Horizontal, -1.0);
            // Single-line inputs scroll, and elision is a deliberate cut.
            if !text.input && text.overflow == TextOverflow::Clip {
                let needed = match text.wrap {
                    TextWrap::NoWrap => width.preferred,
                    _ => width.min,
                };
                if geometry.width() + TOLERANCE < needed {
                    report(Problem::Width, geometry.width(), needed);
                }
            }
            let instance = instances.entry(key.clone()).or_default();
            *instance += 1;
            let instance = (key.clone(), *instance);
            let input = (text.input && text.wrap == TextWrap::NoWrap).then(|| {
                (
                    geometry.width(),
                    (width.preferred - geometry.width()).max(0.0),
                )
            });
            placements.insert(
                instance.clone(),
                Placement {
                    text: text.text.clone(),
                    spills: spills(item),
                    input,
                },
            );
            let rect = window_rect(item);
            let content = LogicalSize::new(width.preferred, vertical.preferred);
            let ink = ink_rect(&text, rect, content);
            // A label belongs inside its button at every size, the fallback included.
            if let Some(button) = button(item) {
                let bounds = window_rect(&button);
                let outside = (bounds.min_x() - ink.min_x())
                    .max(ink.max_x() - bounds.max_x())
                    .max(bounds.min_y() - ink.min_y())
                    .max(ink.max_y() - bounds.max_y());
                if outside > TOLERANCE {
                    report(Problem::Outside, outside, 0.0);
                }
            }
            if let Some(ink) = ink.intersection(&clip_area(item, window)) {
                inks.push(Ink {
                    instance,
                    backdrop: (root_index, backdrop(item)),
                    rect: ink,
                    text: text.text.clone(),
                });
            }
            // A clipping container that is not a scroll view cuts the text.
            let shown = rect.intersection(&area).unwrap_or_default();
            if shown.height() + TOLERANCE < rect.height().min(height) {
                report(
                    Problem::Container,
                    shown.height(),
                    rect.height().min(height),
                );
            } else if shown.width() + TOLERANCE < rect.width() {
                report(Problem::Container, shown.width(), rect.width());
            }
            ControlFlow::Continue(())
        });
    }
    (findings, placements, overlaps(&inks))
}

struct Occurrence {
    text: String,
    actual: f32,
    needed: f32,
    partner: String,
    base: f32,
    context: String,
    count: usize,
}

#[test]
fn text_fits_at_desktop_font_sizes() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    let app = AppWindow::new().unwrap();
    apply_fixture(&app);
    app.show().unwrap();
    // Opt-in renders of every checked state, for comparing layouts by eye.
    let snapshots = std::env::var_os("FLECTAR_TEXT_FIT_SNAPSHOTS").map(std::path::PathBuf::from);

    let sources = Sources::load();
    let mut hints: BTreeMap<(String, u32), String> = BTreeMap::new();
    let mut report: BTreeMap<(String, u32, Problem), Occurrence> = BTreeMap::new();
    // Some rows let text overhang their own background, or line boxes touch,
    // by design, so each state is compared with its English layout at the
    // fallback size. Translations are held to that design too.
    let mut fallback: BTreeMap<(&str, String), (Placements, Overlaps)> = BTreeMap::new();
    let context = WindowInner::from_pub(app.window()).context().clone();
    let runs = BASE_FONT_SIZES.map(|base| ("", base)).into_iter().chain(
        TRANSLATIONS.into_iter().flat_map(|language| {
            [BASE_FONT_SIZES[0], BASE_FONT_SIZES[3]].map(|base| (language, base))
        }),
    );
    for (language, base) in runs {
        slint::select_bundled_translation(language).unwrap();
        CATALOG.set(load_catalog(language));
        context.set_platform_default_font_size(Some(LogicalLength::new(base)));
        for (window_name, width, height) in WINDOWS {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            for state in states() {
                if state.desktop_only && window_name != "desktop"
                    || state.phone_only && window_name != "phone"
                {
                    continue;
                }
                reset(&app);
                // Change handlers close what the previous state opened.
                slint::platform::update_timers_and_animations();
                (state.setup)(&app);
                if let Some(frame) = settle(&app, &window, snapshots.is_some()) {
                    let name = format!(
                        "{}-{base:.1}-{window_name}-{}.png",
                        if language.is_empty() { "en" } else { language },
                        state.name.replace(' ', "-")
                    );
                    frame.save(snapshots.as_ref().unwrap().join(name)).unwrap();
                }
                let (mut findings, placements, overlapping) = check(&app);
                // English at the fallback size runs first.
                let scenario = (window_name, state.name.clone());
                if let Some((designed, designed_overlaps)) = fallback.get(&scenario) {
                    for (instance, now) in &placements {
                        let Some(before) = designed.get(instance) else {
                            continue;
                        };
                        let ((element, index), _) = instance;
                        // A surface that held the text at the fallback size must still hold it.
                        let spill = before
                            .spills
                            .iter()
                            .zip(&now.spills)
                            .filter(|(before, _)| **before <= TOLERANCE)
                            .fold(0.0_f32, |spill, (_, now)| spill.max(*now));
                        if spill > TOLERANCE {
                            findings.push(Finding {
                                key: (element.clone(), *index, Problem::Spill),
                                text: now.text.clone(),
                                actual: spill,
                                needed: 0.0,
                                partner: String::new(),
                            });
                        }
                        // A field sized for its value must keep showing all of it.
                        // Fields that give way to longer labels scroll instead.
                        if let (Some((width_before, hidden_before)), Some((width, hidden))) =
                            (before.input, now.input)
                            && (width - width_before).abs() <= TOLERANCE
                            && hidden_before <= TOLERANCE
                            && hidden > TOLERANCE
                        {
                            findings.push(Finding {
                                key: (element.clone(), *index, Problem::Hidden),
                                text: now.text.clone(),
                                actual: hidden,
                                needed: 0.0,
                                partner: String::new(),
                            });
                        }
                    }
                    for (pair, (depth, text, other_text)) in &overlapping {
                        let (instance, other) = pair;
                        if !designed.contains_key(instance) || !designed.contains_key(other) {
                            continue;
                        }
                        // Texts must overlap no more than they do at the fallback size.
                        let before = designed_overlaps
                            .get(pair)
                            .map_or(0.0, |designed| designed.0);
                        if *depth > before + TOLERANCE {
                            let ((element, index), _) = instance;
                            let other_text: String = other_text.chars().take(40).collect();
                            findings.push(Finding {
                                key: (element.clone(), *index, Problem::Overlap),
                                text: text.clone(),
                                actual: *depth,
                                needed: 0.0,
                                partner: format!("{} {other_text:?}", other.0.0),
                            });
                        }
                    }
                } else {
                    fallback.insert(scenario, (placements, overlapping));
                }
                if let Some(label) = state.dismiss {
                    press(&app, label);
                }
                for finding in findings {
                    let (element, index, _) = &finding.key;
                    if language.is_empty()
                        && let Some(hint) = sources.locate(element, &finding.text)
                    {
                        hints.entry((element.clone(), *index)).or_insert(hint);
                    }
                    let occurrence = report.entry(finding.key).or_insert(Occurrence {
                        text: finding.text,
                        actual: finding.actual,
                        needed: finding.needed,
                        partner: finding.partner,
                        base,
                        context: format!(
                            "{}{window_name}, {}",
                            if language.is_empty() {
                                String::new()
                            } else {
                                format!("{language}, ")
                            },
                            state.name
                        ),
                        count: 0,
                    });
                    occurrence.count += 1;
                }
            }
        }
    }
    slint::select_bundled_translation("").unwrap();

    let lines: Vec<String> = report
        .iter()
        .map(|((element, index, problem), occurrence)| {
            let (actual, needed) = (occurrence.actual, occurrence.needed);
            let problem = match problem {
                Problem::Height => format!("height {actual:.1}px, needs {needed:.1}px"),
                Problem::Width => format!("width {actual:.1}px, needs {needed:.1}px"),
                Problem::Container => {
                    format!("its clipping container shows {actual:.1}px of {needed:.1}px")
                }
                Problem::Spill => format!(
                    "spills {actual:.1}px out of a surface that holds it at the fallback size"
                ),
                Problem::Overlap => format!("overlaps {} by {actual:.1}px", occurrence.partner),
                Problem::Hidden => {
                    format!("hides {actual:.1}px of a value that shows whole at the fallback size")
                }
                Problem::Outside => format!("reaches {actual:.1}px outside its button"),
            };
            let location = hints
                .get(&(element.clone(), *index))
                .map_or(String::new(), |hint| format!("{hint}: "));
            let text: String = occurrence.text.chars().take(40).collect();
            format!(
                "{location}{element} {text:?}: {problem} from {:.1}px ({}; {} cases)",
                occurrence.base, occurrence.context, occurrence.count
            )
        })
        .collect();
    assert!(
        lines.is_empty(),
        "{} text elements do not fit their box:\n{}",
        lines.len(),
        lines.join("\n")
    );
}
