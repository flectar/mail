//! Contact directory view-model and Slint model projection.

use crate::{
    AppWindow, ContactAccountRow, ContactRow, ContactSidebarRow, ContactSidebarRowKind, I18n,
    date_format::{self, DateStyle},
    paged_visible_count,
};
use chrono::{Local, TimeZone};
use flectar_mail_core::models::{
    ContactRecord, ContactRecordCursor, ContactRecordPage, ContactSync,
};
use slint::{ComponentHandle, Model, VecModel};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

pub(crate) struct ContactDirectoryState {
    pub(crate) contacts: Vec<ContactRecord>,
    pub(crate) photos: HashMap<String, (slint::Image, slint::Image)>,
    pub(crate) rows: Rc<VecModel<ContactRow>>,
    pub(crate) sidebar_rows: Rc<crate::retained_model::RetainedModel<ContactSidebarRow>>,
    pub(crate) collapsed_sections: HashSet<String>,
    pub(crate) selected_id: Option<i64>,
    pub(crate) query: String,
    pub(crate) scope: String,
    pub(crate) page: usize,
    pub(crate) next_cursor: Option<ContactRecordCursor>,
    pub(crate) total_count: usize,
    pub(crate) favorite_count: usize,
    pub(crate) suggestion_count: usize,
    pub(crate) account_counts: HashMap<i64, usize>,
    pub(crate) editing_new: bool,
    pub(crate) using_core: bool,
}

impl ContactDirectoryState {
    pub(crate) fn new(contacts: Vec<ContactRecord>, using_core: bool) -> Self {
        Self {
            selected_id: contacts.first().map(|contact| contact.id),
            contacts,
            photos: HashMap::new(),
            rows: Rc::new(VecModel::default()),
            sidebar_rows: Rc::default(),
            collapsed_sections: HashSet::new(),
            query: String::new(),
            scope: "All contacts".to_owned(),
            page: 1,
            next_cursor: None,
            total_count: 0,
            favorite_count: 0,
            suggestion_count: 0,
            account_counts: HashMap::new(),
            editing_new: false,
            using_core,
        }
    }

    pub(crate) fn begin_core_query(&mut self) {
        self.contacts.clear();
        self.selected_id = None;
        self.page = 1;
        self.next_cursor = None;
        self.editing_new = false;
        self.using_core = true;
    }

    pub(crate) fn apply_core_page(
        &mut self,
        cursor: Option<&ContactRecordCursor>,
        page: ContactRecordPage,
    ) -> bool {
        if cursor.is_some() && self.next_cursor.as_ref() != cursor {
            return false;
        }
        if cursor.is_none() {
            self.contacts = page.records;
        } else {
            let mut known = self
                .contacts
                .iter()
                .map(|contact| contact.id)
                .collect::<std::collections::HashSet<_>>();
            self.contacts.extend(
                page.records
                    .into_iter()
                    .filter(|contact| known.insert(contact.id)),
            );
        }
        self.next_cursor = page.next_cursor;
        self.total_count = page.total_count;
        self.favorite_count = page.favorite_count;
        self.suggestion_count = page.suggestion_count;
        self.account_counts = page.account_counts.into_iter().collect();
        if self.selected_id.is_none() {
            self.selected_id = self.contacts.first().map(|contact| contact.id);
        }
        true
    }
}

fn contact_avatar_tone(contact: &ContactRecord) -> i32 {
    // FNV-1a gives each contact a stable, inexpensive palette slot. Using the
    // normalized email keeps the fallback consistent when a display name is
    // edited and avoids a visibly changing "random" color between launches.
    let mut hash = 0x811c_9dc5_u32;
    let identity = if contact.email.is_empty() {
        contact.id.to_string()
    } else {
        contact.email.clone()
    };
    let email = identity.trim();
    // Unicode lowercasing can depend on surrounding characters (Greek sigma).
    // Keep that normalization for non-ASCII addresses, but hash ordinary email
    // bytes directly without an intermediate string.
    let normalized = if email.is_ascii() {
        std::borrow::Cow::Borrowed(email)
    } else {
        std::borrow::Cow::Owned(email.to_lowercase())
    };
    for byte in normalized.bytes().map(|byte| byte.to_ascii_lowercase()) {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    (hash % 6) as i32
}

fn record_initials(contact: &ContactRecord) -> String {
    let source = if contact.name.trim().is_empty() {
        contact.email.as_str()
    } else {
        contact.name.as_str()
    };
    let mut initials = source
        .split_whitespace()
        .filter_map(|part| part.chars().next())
        .take(2)
        .collect::<String>();
    if initials.is_empty() {
        initials.push('@');
    }
    initials.to_uppercase()
}

fn contact_last_interacted(app: &AppWindow, value: Option<i64>) -> String {
    let Some(value) = value else {
        return app.global::<I18n>().invoke_no_contact_history().into();
    };
    Local
        .timestamp_millis_opt(value)
        .single()
        .map(|time| {
            let date = date_format::current().date(DateStyle::Medium, time.date_naive());
            app.global::<I18n>()
                .invoke_last_contacted(date.into())
                .into()
        })
        .unwrap_or_else(|| {
            app.global::<I18n>()
                .invoke_contact_history_available()
                .into()
        })
}

fn contact_matches(contact: &ContactRecord, query: &str) -> bool {
    if query.split_whitespace().next().is_none() {
        return true;
    }
    let fields = [
        contact.name.as_str(),
        contact.email.as_str(),
        contact.phone.as_str(),
        contact.company.as_str(),
        contact.job_title.as_str(),
        contact.website.as_str(),
        contact.tags.as_str(),
        contact.postal_address.as_str(),
    ];
    let mut haystack = String::with_capacity(
        fields.iter().map(|field| field.len()).sum::<usize>() + fields.len() - 1,
    );
    for (index, field) in fields.into_iter().enumerate() {
        if index > 0 {
            haystack.push(' ');
        }
        haystack.push_str(field);
    }
    if haystack.is_ascii() {
        haystack.make_ascii_lowercase();
    } else {
        haystack = haystack.to_lowercase();
    }
    query
        .to_lowercase()
        .split_whitespace()
        .all(|token| haystack.contains(token))
}

fn scope_account_id(scope: &str) -> Option<i64> {
    scope.strip_prefix("Account:")?.parse().ok()
}

fn contact_belongs_to_account(contact: &ContactRecord, account_id: i64) -> bool {
    contact.is_managed || contact.account_ids.contains(&account_id)
}

fn visible_contact_ids(directory: &ContactDirectoryState) -> Vec<i64> {
    if directory.using_core {
        return directory
            .contacts
            .iter()
            .map(|contact| contact.id)
            .collect();
    }
    directory
        .contacts
        .iter()
        .filter(|contact| {
            if directory.scope == "Favorites" {
                contact.is_managed && contact.is_favorite
            } else if directory.scope == "Suggestions" {
                !contact.is_managed
            } else if let Some(account_id) = scope_account_id(&directory.scope) {
                contact.is_managed && contact_belongs_to_account(contact, account_id)
            } else {
                contact.is_managed
            }
        })
        .filter(|contact| contact_matches(contact, &directory.query))
        .map(|contact| contact.id)
        .collect()
}

pub(crate) fn apply_contact_rows(app: &AppWindow, state: &Rc<RefCell<ContactDirectoryState>>) {
    let directory = state.borrow();
    let visible = visible_contact_ids(&directory);
    let shown_count = if directory.using_core {
        visible.len()
    } else {
        paged_visible_count(directory.page, visible.len())
    };
    let rows = visible
        .iter()
        .take(shown_count)
        .filter_map(|id| directory.contacts.iter().find(|contact| contact.id == *id))
        .filter_map(|contact| {
            Some(ContactRow {
                id: i32::try_from(contact.id).ok()?,
                name: contact.name.clone().into(),
                email: contact.email.clone().into(),
                initials: record_initials(contact).into(),
                avatar_tone: contact_avatar_tone(contact),
                avatar: contact
                    .details
                    .photo_hash
                    .as_ref()
                    .and_then(|h| directory.photos.get(h))
                    .map(|p| p.0.clone())
                    .unwrap_or_default(),
                has_avatar: app.get_show_avatars()
                    && contact
                        .details
                        .photo_hash
                        .as_ref()
                        .is_some_and(|h| directory.photos.contains_key(h)),
                source: contact.details.source.clone().unwrap_or_default().into(),
                company: contact.company.clone().into(),
                detail: match (contact.company.is_empty(), contact.job_title.is_empty()) {
                    (false, false) => format!("{} · {}", contact.company, contact.job_title),
                    (false, true) => contact.company.clone(),
                    (true, false) => contact.job_title.clone(),
                    (true, true) => contact.details.source.clone().unwrap_or_default(),
                }
                .into(),
                last_contacted: contact_last_interacted(app, contact.last_interacted).into(),
                favorite: contact.is_favorite,
                suggested: !contact.is_managed,
                selected: directory.selected_id == Some(contact.id) && !directory.editing_new,
            })
        })
        .collect::<Vec<_>>();
    crate::reconcile_model_rows(&directory.rows, rows, |row| row.id);
    let account_rows = contact_account_rows(app, &directory);
    app.set_contact_scope(directory.scope.clone().into());
    app.set_contact_search_query(directory.query.clone().into());
    let total_count = if directory.using_core {
        directory.total_count
    } else {
        directory.contacts.len()
    };
    let favorite_count = if directory.using_core {
        directory.favorite_count
    } else {
        directory
            .contacts
            .iter()
            .filter(|contact| contact.is_favorite)
            .count()
    };
    app.set_contact_total_count(total_count.min(i32::MAX as usize) as i32);
    app.set_contact_favorite_count(favorite_count.min(i32::MAX as usize) as i32);
    let suggestion_count = if directory.using_core {
        directory.suggestion_count
    } else {
        directory
            .contacts
            .iter()
            .filter(|contact| !contact.is_managed)
            .count()
    };
    app.set_contact_suggestion_count(suggestion_count.min(i32::MAX as usize) as i32);
    app.set_contact_can_load_more(if directory.using_core {
        directory.next_cursor.is_some()
    } else {
        shown_count < visible.len()
    });
    let sidebar_rows = make_contact_sidebar_rows(account_rows, &directory.collapsed_sections);
    let sidebar_model = Rc::clone(&directory.sidebar_rows);
    drop(directory);
    sidebar_model.reconcile_by(sidebar_rows, |row| row.key.clone(), same_sidebar_row);
}

pub(crate) fn clear_contact_form(app: &AppWindow) {
    app.set_contact_emails(slint::ModelRc::new(slint::VecModel::from(vec![
        crate::ContactValueRow::default(),
    ])));
    app.set_contact_phones(slint::ModelRc::new(slint::VecModel::from(vec![
        crate::ContactValueRow::default(),
    ])));
    app.set_contact_id(0);
    app.set_contact_read_only(false);
    app.set_contact_source("".into());
    app.set_contact_sync(crate::ContactSyncState::Local);
    app.set_contact_sync_detail("".into());
    app.set_contact_has_photo(false);
    app.set_contact_photo(Default::default());
    app.set_contact_name("".into());
    app.set_contact_company("".into());
    app.set_contact_job_title("".into());
    app.set_contact_website("".into());
    app.set_contact_birthday("".into());
    app.set_contact_address("".into());
    app.set_contact_notes("".into());
    app.set_contact_tags("".into());
    app.set_contact_favorite(false);
    app.set_contact_is_suggested(false);
    app.set_contact_initials("?".into());
    app.set_contact_interactions(app.global::<I18n>().invoke_new_contact_entry());
    app.set_contact_last_interacted("".into());
}

pub(crate) fn contact_values(
    model: slint::ModelRc<crate::ContactValueRow>,
) -> Vec<flectar_mail_core::models::ContactValue> {
    model
        .iter()
        .filter(|r| !r.value.trim().is_empty())
        .map(|r| flectar_mail_core::models::ContactValue {
            key: r.key.to_string(),
            value: r.value.trim().to_owned(),
            label: r.label.trim().to_owned(),
            preference: if r.preference <= 0 {
                100
            } else {
                r.preference.min(100) as u8
            },
        })
        .collect()
}

fn editable_values(
    values: &[flectar_mail_core::models::ContactValue],
    primary: &str,
) -> Vec<flectar_mail_core::models::ContactValue> {
    if values.is_empty() && !primary.is_empty() {
        vec![flectar_mail_core::models::ContactValue {
            value: primary.into(),
            preference: 100,
            ..Default::default()
        }]
    } else {
        values.to_vec()
    }
}

fn contact_sync_state(sync: &ContactSync) -> (crate::ContactSyncState, &str) {
    use crate::ContactSyncState as State;
    match sync {
        ContactSync::Local => (State::Local, ""),
        ContactSync::Detached => (State::Detached, ""),
        ContactSync::Synced => (State::Synced, ""),
        ContactSync::Pending => (State::Pending, ""),
        ContactSync::Conflict => (State::Conflict, ""),
        ContactSync::Unavailable => (State::Unavailable, ""),
        ContactSync::Failed(detail) => (State::Failed, detail),
    }
}

fn apply_contact_form(app: &AppWindow, contact: &ContactRecord) {
    let rows = |values: &[flectar_mail_core::models::ContactValue], primary: &str| {
        let mut rows = values
            .iter()
            .map(|v| crate::ContactValueRow {
                key: v.key.clone().into(),
                value: v.value.clone().into(),
                label: v.label.clone().into(),
                preference: i32::from(v.preference),
            })
            .collect::<Vec<_>>();
        if rows.is_empty() {
            rows.push(crate::ContactValueRow {
                value: primary.into(),
                ..Default::default()
            });
        }
        slint::ModelRc::new(slint::VecModel::from(rows))
    };
    app.set_contact_emails(rows(&contact.details.emails, &contact.email));
    app.set_contact_phones(rows(&contact.details.phones, &contact.phone));
    app.set_contact_id(i32::try_from(contact.id).unwrap_or(i32::MAX));
    app.set_contact_read_only(contact.details.read_only);
    app.set_contact_source(contact.details.source.clone().unwrap_or_default().into());
    let (sync, detail) = contact_sync_state(&contact.details.sync);
    app.set_contact_sync(sync);
    app.set_contact_sync_detail(detail.into());
    app.set_contact_name(contact.name.clone().into());
    app.set_contact_company(contact.company.clone().into());
    app.set_contact_job_title(contact.job_title.clone().into());
    app.set_contact_website(contact.website.clone().into());
    app.set_contact_birthday(contact.birthday.clone().into());
    app.set_contact_address(contact.postal_address.clone().into());
    app.set_contact_notes(contact.notes.clone().into());
    app.set_contact_tags(contact.tags.clone().into());
    app.set_contact_favorite(contact.is_favorite);
    app.set_contact_is_suggested(!contact.is_managed);
    app.set_contact_initials(record_initials(contact).into());
    app.set_contact_interactions(
        app.global::<I18n>()
            .invoke_interaction_count(contact.interactions as i32),
    );
    app.set_contact_last_interacted(contact_last_interacted(app, contact.last_interacted).into());
    app.set_contact_has_selection(true);
}

/// Product teardown must not discard an editor with unsaved changes.
pub(crate) fn has_unsaved_edits(app: &AppWindow, state: &ContactDirectoryState) -> bool {
    if state.editing_new {
        return true;
    }
    let Some(contact) = state
        .selected_id
        .and_then(|id| state.contacts.iter().find(|c| c.id == id))
    else {
        return false;
    };
    contact_values(app.get_contact_emails())
        != editable_values(&contact.details.emails, &contact.email)
        || contact_values(app.get_contact_phones())
            != editable_values(&contact.details.phones, &contact.phone)
        || app.get_contact_name().as_str() != contact.name
        || app.get_contact_company().as_str() != contact.company
        || app.get_contact_job_title().as_str() != contact.job_title
        || app.get_contact_website().as_str() != contact.website
        || app.get_contact_birthday().as_str() != contact.birthday
        || app.get_contact_address().as_str() != contact.postal_address
        || app.get_contact_notes().as_str() != contact.notes
        || app.get_contact_tags().as_str() != contact.tags
        || app.get_contact_favorite() != contact.is_favorite
}

/// Keep the editor's source record while it has unsaved input.
pub(crate) fn release_directory(app: &AppWindow, state: &mut ContactDirectoryState) -> bool {
    if has_unsaved_edits(app, state) {
        return false;
    }
    state.contacts = Vec::new();
    state.photos.clear();
    state.rows.set_vec(Vec::new());
    state.next_cursor = None;
    true
}

pub(crate) fn apply_contact_directory(app: &AppWindow, state: &Rc<RefCell<ContactDirectoryState>>) {
    {
        let mut directory = state.borrow_mut();
        if !directory.editing_new {
            let visible = visible_contact_ids(&directory);
            if !directory
                .selected_id
                .is_some_and(|selected| visible.contains(&selected))
            {
                directory.selected_id = visible.first().copied();
            }
        }
    }
    apply_contact_rows(app, state);
    let directory = state.borrow();
    if directory.editing_new {
        app.set_contact_has_selection(true);
        return;
    }
    if let Some(contact) = directory
        .selected_id
        .and_then(|id| directory.contacts.iter().find(|contact| contact.id == id))
    {
        apply_contact_form(app, contact);
        let photo = contact
            .details
            .photo_hash
            .as_ref()
            .and_then(|h| directory.photos.get(h));
        app.set_contact_has_photo(app.get_show_avatars() && photo.is_some());
        app.set_contact_photo(photo.map(|p| p.1.clone()).unwrap_or_default());
    } else {
        clear_contact_form(app);
        app.set_contact_has_selection(false);
    }
}

fn contact_account_rows(
    app: &AppWindow,
    directory: &ContactDirectoryState,
) -> Vec<ContactAccountRow> {
    // Index local counts once, rather than scanning all contacts per account.
    let mut local_counts = HashMap::<i64, usize>::new();
    let mut managed_count = 0;
    if !directory.using_core {
        for contact in &directory.contacts {
            if contact.is_managed {
                managed_count += 1;
            } else {
                for id in contact.account_ids.iter().copied().collect::<HashSet<_>>() {
                    *local_counts.entry(id).or_default() += 1;
                }
            }
        }
    }
    let connected_accounts = app.get_connected_accounts();
    (0..connected_accounts.row_count())
        .filter_map(|index| connected_accounts.row_data(index))
        .map(|account| {
            let selected = directory.scope == format!("Account:{}", account.id);
            let count = if directory.using_core {
                directory
                    .account_counts
                    .get(&i64::from(account.id))
                    .copied()
                    .unwrap_or_default()
            } else {
                local_counts
                    .get(&i64::from(account.id))
                    .copied()
                    .unwrap_or_default()
                    + managed_count
            };
            ContactAccountRow {
                id: account.id,
                name: account.name,
                email: account.email,
                initials: account.initials,
                avatar: account.avatar_small,
                has_avatar: account.has_avatar,
                count: if count == 0 {
                    "".into()
                } else {
                    count.to_string().into()
                },
                selected,
            }
        })
        .collect::<Vec<_>>()
}

pub(crate) fn refresh_contact_sidebar(app: &AppWindow, state: &Rc<RefCell<ContactDirectoryState>>) {
    let directory = state.borrow();
    let accounts = contact_account_rows(app, &directory);
    let rows = make_contact_sidebar_rows(accounts, &directory.collapsed_sections);
    let model = Rc::clone(&directory.sidebar_rows);
    drop(directory);
    model.reconcile_by(rows, |row| row.key.clone(), same_sidebar_row);
}

fn make_contact_sidebar_rows(
    accounts: Vec<ContactAccountRow>,
    collapsed: &HashSet<String>,
) -> Vec<ContactSidebarRow> {
    use ContactSidebarRowKind as Kind;
    let row = |kind, key: &str| ContactSidebarRow {
        kind,
        key: key.into(),
        open: !collapsed.contains(key),
        ..Default::default()
    };
    let mut rows = vec![
        row(Kind::UnifiedHeading, "unified-heading"),
        row(Kind::UnifiedSection, "unified"),
    ];
    if !collapsed.contains("unified") {
        rows.extend([
            row(Kind::All, "all"),
            row(Kind::Favorites, "favorites"),
            row(Kind::Suggestions, "suggestions"),
        ]);
    }
    if !accounts.is_empty() {
        rows.push(row(Kind::AccountsHeading, "accounts-heading"));
    }
    for account in accounts {
        let key = format!("account:{}", account.id);
        let open = !collapsed.contains(&key);
        rows.push(ContactSidebarRow {
            account: account.clone(),
            ..row(Kind::Account, &key)
        });
        if open {
            rows.push(ContactSidebarRow {
                account,
                ..row(Kind::Contacts, &format!("contacts:{}", key))
            });
        }
    }
    rows.push(row(Kind::Hint, "hint"));
    rows
}

fn same_sidebar_row(a: &ContactSidebarRow, b: &ContactSidebarRow) -> bool {
    let ContactAccountRow {
        id,
        name,
        email,
        initials,
        avatar,
        has_avatar,
        count,
        selected,
    } = &a.account;
    let other = &b.account;
    a.key == b.key
        && a.kind == b.kind
        && a.open == b.open
        && *id == other.id
        && *name == other.name
        && *email == other.email
        && *initials == other.initials
        && *has_avatar == other.has_avatar
        && *count == other.count
        && *selected == other.selected
        && (!has_avatar || *avatar == other.avatar)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(email: &str) -> ContactRecord {
        ContactRecord {
            details: Default::default(),
            id: 1,
            name: "Example Person".to_owned(),
            email: email.to_owned(),
            phone: String::new(),
            company: String::new(),
            job_title: String::new(),
            website: String::new(),
            birthday: String::new(),
            postal_address: String::new(),
            notes: String::new(),
            tags: String::new(),
            is_favorite: false,
            interactions: 0,
            last_interacted: None,
            account_ids: Vec::new(),
            is_managed: false,
        }
    }

    fn cursor(id: i64) -> ContactRecordCursor {
        ContactRecordCursor {
            is_favorite: false,
            sort_name: format!("Person {id}"),
            email: format!("person-{id}@example.com"),
            id,
        }
    }

    fn page(start: i64, count: i64, next_cursor: Option<ContactRecordCursor>) -> ContactRecordPage {
        ContactRecordPage {
            records: (start..start + count)
                .map(|id| {
                    let mut record = contact(&format!("person-{id}@example.com"));
                    record.id = id;
                    record
                })
                .collect(),
            next_cursor,
            total_count: 61,
            favorite_count: 0,
            suggestion_count: 0,
            account_counts: vec![(1, 61)],
        }
    }

    #[test]
    fn sidebar_expansion_uses_account_ids_and_ignores_missing_avatar_images() {
        use ContactSidebarRowKind as Kind;
        let accounts = vec![
            ContactAccountRow {
                id: 1,
                name: "Same name".into(),
                ..Default::default()
            },
            ContactAccountRow {
                id: 2,
                name: "Same name".into(),
                ..Default::default()
            },
        ];
        let collapsed = HashSet::from(["account:1".to_owned(), "unified".to_owned()]);
        let rows = make_contact_sidebar_rows(accounts.clone(), &collapsed);
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row.kind, Kind::All | Kind::Favorites))
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.kind == Kind::Contacts && row.account.id == 1)
        );
        assert!(
            rows.iter()
                .any(|row| row.kind == Kind::Contacts && row.account.id == 2)
        );
        let rebuilt = make_contact_sidebar_rows(accounts, &collapsed);
        assert!(
            rows.iter()
                .zip(&rebuilt)
                .all(|(a, b)| same_sidebar_row(a, b))
        );
        let mut changed = rebuilt[2].clone();
        changed.account.count = "5".into();
        assert!(!same_sidebar_row(&rows[2], &changed));
    }

    #[test]
    fn avatar_tone_is_stable_for_normalized_email() {
        let lower = contact("person@example.com");
        let mixed = contact("  Person@Example.COM  ");
        assert_eq!(contact_avatar_tone(&lower), contact_avatar_tone(&mixed));
        assert!((0..6).contains(&contact_avatar_tone(&lower)));
    }

    #[test]
    fn core_pages_append_only_at_the_expected_cursor() {
        let mut directory = ContactDirectoryState::new(Vec::new(), true);
        directory.begin_core_query();
        assert!(directory.apply_core_page(None, page(1, 25, Some(cursor(25)))));
        let first_cursor = directory.next_cursor.clone().unwrap();
        assert!(directory.apply_core_page(Some(&first_cursor), page(26, 25, Some(cursor(50)))));
        assert_eq!(directory.contacts.len(), 50);
        assert_eq!(directory.next_cursor, Some(cursor(50)));
        assert!(!directory.apply_core_page(Some(&first_cursor), page(26, 25, Some(cursor(50)))));
        assert_eq!(directory.contacts.len(), 50);
    }
}
