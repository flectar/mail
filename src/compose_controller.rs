//! Compose view-model projection and recipient/file helpers.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ComposeIntent {
    pub mode: String,
    pub in_reply_to_message_id: Option<i64>,
    pub draft_id: Option<i64>,
}

impl Default for ComposeIntent {
    fn default() -> Self {
        Self {
            mode: "new".to_owned(),
            in_reply_to_message_id: None,
            draft_id: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PreparedMessageCompose {
    pub account_id: i64,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub body: String,
    pub intent: ComposeIntent,
}

pub(super) fn prepare_message_compose(
    source: &ComposeSource,
    action: &str,
    rendered_plain_text: &str,
) -> Result<PreparedMessageCompose, String> {
    if !matches!(action, "reply" | "reply_all" | "forward") {
        return Err(format!("unsupported compose action: {action}"));
    }

    let original_body = if rendered_plain_text.trim().is_empty() {
        source.text_body.as_str()
    } else {
        rendered_plain_text
    };
    let sender = format_address(&source.from);
    let sent_at = chrono::Local
        .timestamp_millis_opt(source.date)
        .single()
        .map(|date| date.format("%b %-d, %Y at %-I:%M %p").to_string())
        .unwrap_or_else(|| "an earlier time".to_owned());

    if action == "forward" {
        return Ok(PreparedMessageCompose {
            account_id: source.account_id,
            to: String::new(),
            cc: String::new(),
            subject: prefixed_subject(&source.subject, "Fwd"),
            body: format!(
                "\n\n---------- Forwarded message ----------\nFrom: {sender}\nDate: {sent_at}\nSubject: {}\nTo: {}\n\n{}",
                source.subject,
                join_addresses(&source.to),
                original_body.trim_end(),
            ),
            intent: ComposeIntent {
                mode: "forward".to_owned(),
                in_reply_to_message_id: None,
                draft_id: None,
            },
        });
    }

    let own_email = source.account_email.trim().to_ascii_lowercase();
    let mut candidates = Vec::new();
    if !source.from.email.eq_ignore_ascii_case(&own_email) {
        candidates.push(source.from.clone());
    }
    candidates.extend(source.to.iter().cloned());
    if action == "reply_all" {
        candidates.extend(source.cc.iter().cloned());
    }
    let recipients = deduplicated_addresses(candidates, &own_email);
    let primary = recipients
        .first()
        .ok_or_else(|| "the selected message has no reply recipient".to_owned())?;
    let cc = if action == "reply_all" {
        join_addresses(&recipients[1..])
    } else {
        String::new()
    };
    let quoted = original_body
        .trim_end()
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");

    Ok(PreparedMessageCompose {
        account_id: source.account_id,
        to: format_address(primary),
        cc,
        subject: prefixed_subject(&source.subject, "Re"),
        body: format!("\n\nOn {sent_at}, {sender} wrote:\n{quoted}"),
        intent: ComposeIntent {
            mode: action.to_owned(),
            in_reply_to_message_id: Some(source.message_id),
            draft_id: None,
        },
    })
}

pub(super) fn compose_addresses(
    addresses: &[flectar_mail_core::models::Address],
) -> String {
    join_addresses(addresses)
}

fn deduplicated_addresses(
    addresses: impl IntoIterator<Item = flectar_mail_core::models::Address>,
    own_email: &str,
) -> Vec<flectar_mail_core::models::Address> {
    let mut seen = HashSet::new();
    addresses
        .into_iter()
        .filter(|address| {
            let email = address.email.trim().to_ascii_lowercase();
            !email.is_empty() && email != own_email && seen.insert(email)
        })
        .collect()
}

fn join_addresses(addresses: &[flectar_mail_core::models::Address]) -> String {
    addresses
        .iter()
        .map(format_address)
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_address(address: &flectar_mail_core::models::Address) -> String {
    match address
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        Some(name) => {
            let name = name
                .chars()
                .filter(|character| !matches!(character, ',' | ';' | '<' | '>' | '\r' | '\n'))
                .collect::<String>();
            format!("{} <{}>", name.trim(), address.email)
        }
        None => address.email.clone(),
    }
}

fn prefixed_subject(subject: &str, prefix: &str) -> String {
    let subject = subject.trim();
    let already_prefixed = match prefix {
        "Re" => subject
            .get(..3)
            .is_some_and(|value| value.eq_ignore_ascii_case("re:")),
        "Fwd" => {
            subject
                .get(..4)
                .is_some_and(|value| value.eq_ignore_ascii_case("fwd:"))
                || subject
                    .get(..3)
                    .is_some_and(|value| value.eq_ignore_ascii_case("fw:"))
        }
        _ => false,
    };
    if already_prefixed {
        subject.to_owned()
    } else if subject.is_empty() {
        format!("{prefix}:")
    } else {
        format!("{prefix}: {subject}")
    }
}

pub(super) fn perform_selected_action(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    action: &str,
) -> Result<(), String> {
    let (selected_id, thread_id, core, using_core) = {
        let state = state.borrow();
        let selected = state
            .selected_id
            .and_then(|id| state.messages.iter().find(|message| message.id == id));
        (
            state.selected_id,
            selected.and_then(|message| message.thread_id),
            state.core.clone(),
            state.using_core,
        )
    };
    if !using_core {
        return Err("mail account is not ready".to_owned());
    }
    let core = core.ok_or_else(|| "mail core is unavailable".to_owned())?;
    let thread_id = thread_id.ok_or_else(|| "no message is selected".to_owned())?;
    runtime.block_on(core.perform_message_action(thread_id, action))?;
    // Don't clear `selected_id` here: if the message is still visible after
    // the action (e.g. archiving a starred message while viewing "Starred"),
    // leaving it selected keeps the reading pane and the highlighted row in
    // sync. `refresh_from_source` below repaints the pane on its own once it
    // sees the message actually left the reloaded page — including dropping
    // it from a retained pagination tail (see `merge_refreshed_mail_head`)
    // when the message was beyond the first page.
    //
    // Only archive/spam/trash actually move a message to a different folder,
    // so only they can make it leave the current (folder-scoped) view. Star,
    // unstar, mark-read, and mark-unread never do, so passing `selected_id`
    // for those would risk wrongly dropping an unrelated message that simply
    // sits deep in the tail and was never re-fetched by the head refresh.
    let acted_on_ids = matches!(action, "archive" | "spam" | "trash")
        .then_some(selected_id)
        .flatten()
        .into_iter()
        .collect::<Vec<_>>();
    refresh_from_source(app, state, runtime, true, &acted_on_ids)
}

pub(super) fn format_file_size(size: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if size < 1024 {
        format!("{size} B")
    } else if (size as f64) < MB {
        format!("{:.1} KB", size as f64 / KB)
    } else {
        format!("{:.1} MB", size as f64 / MB)
    }
}

pub(super) fn apply_compose_files(app: &AppWindow, files: &[ComposeFile]) {
    let rows = files
        .iter()
        .map(|file| {
            let kind = file
                .path
                .extension()
                .and_then(|extension| extension.to_str())
                .filter(|extension| !extension.is_empty())
                .map(str::to_uppercase)
                .unwrap_or_else(|| "FILE".to_owned());
            ComposeAttachment {
                name: file.filename.clone().into(),
                detail: format!("{kind} · {}", format_file_size(file.size)).into(),
            }
        })
        .collect::<Vec<_>>();
    let total = files.iter().map(|file| file.size).sum::<u64>();
    app.set_compose_attachments(ModelRc::new(VecModel::from(rows)));
    app.set_compose_attachment_summary(
        if files.is_empty() {
            "".to_owned()
        } else {
            format!(
                "{} {} · {}",
                files.len(),
                if files.len() == 1 { "file" } else { "files" },
                format_file_size(total),
            )
        }
        .into(),
    );
}

pub(super) fn apply_rich_compose(
    app: &AppWindow,
    document: &RichComposeDocument,
    selection: ComposeSelection,
    editor: &mut CosmicComposeEditor,
) {
    app.set_compose_body(document.text().into());
    apply_rich_compose_state(app, document, selection);
    apply_compose_editor_surface(app, document, selection, editor);
}

pub(super) fn apply_compose_editor_surface(
    app: &AppWindow,
    document: &RichComposeDocument,
    selection: ComposeSelection,
    editor: &mut CosmicComposeEditor,
) {
    let width = app.get_compose_editor_width().max(1.0);
    let viewport_height = app.get_compose_editor_viewport_height().max(1.0);
    let (width, viewport_height) = if width <= 1.0 || viewport_height <= 1.0 {
        (640.0, 360.0)
    } else {
        (width, viewport_height)
    };
    let rendered = editor.render(
        document,
        selection,
        width,
        viewport_height,
        app.get_compose_editor_scroll_y().max(0.0),
        app.window().scale_factor(),
        compose_editor_style(app),
        app.get_compose_editor_preedit_text().as_str(),
    );
    apply_rendered_compose_editor(app, rendered);
}

pub(super) fn apply_rendered_compose_editor(app: &AppWindow, rendered: RenderedComposeEditor) {
    app.set_compose_editor_tiles(ModelRc::new(VecModel::from(
        rendered
            .tiles
            .into_iter()
            .map(|tile| ComposeEditorTile {
                image: tile.image,
                y: tile.y,
                height: tile.height,
            })
            .collect::<Vec<_>>(),
    )));
    app.set_compose_editor_content_height(rendered.content_height);
    app.set_compose_editor_caret_x(rendered.caret_x);
    app.set_compose_editor_caret_y(rendered.caret_y);
    app.set_compose_editor_caret_height(rendered.caret_height);
}

pub(super) fn compose_editor_style(app: &AppWindow) -> ComposeEditorStyle {
    let selection = app.get_compose_editor_selection_color();
    ComposeEditorStyle {
        text: app.get_compose_editor_text_color(),
        link: app.get_compose_editor_link_color(),
        selection: slint::Color::from_argb_u8(
            140,
            selection.red(),
            selection.green(),
            selection.blue(),
        ),
        selected_text: app.get_compose_editor_selected_text_color(),
    }
}

pub(super) fn apply_rich_compose_state(
    app: &AppWindow,
    document: &RichComposeDocument,
    selection: ComposeSelection,
) {
    let active = document.active_marks();
    app.set_compose_selection_start(selection.start);
    app.set_compose_selection_end(selection.end);
    app.set_compose_bold_active(active.bold);
    app.set_compose_italic_active(active.italic);
    app.set_compose_underline_active(active.underline);
    app.set_compose_strike_active(active.strike);
    app.set_compose_code_active(active.code);
    app.set_compose_active_link_url(document.active_link().unwrap_or_default().into());
}

pub(super) fn apply_compose_contacts(
    app: &AppWindow,
    contacts: &[flectar_mail_core::models::Address],
) {
    let rows = contacts
        .iter()
        .map(|contact| ComposeContact {
            name: contact.name.clone().unwrap_or_default().into(),
            email: contact.email.clone().into(),
            initials: contact_initials(contact).into(),
        })
        .collect::<Vec<_>>();
    app.set_compose_contact_suggestions(ModelRc::new(VecModel::from(rows)));
}

pub(super) fn contact_initials(contact: &flectar_mail_core::models::Address) -> String {
    let source = contact
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&contact.email);
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

pub(super) fn compose_recipient_query(recipients: &str) -> String {
    let token_start = recipients.rfind([',', ';']).map_or(0, |index| index + 1);
    recipients[token_start..].trim().to_owned()
}

pub(super) fn complete_compose_recipient(
    recipients: &str,
    contact: &flectar_mail_core::models::Address,
) -> String {
    if recipients
        .to_lowercase()
        .contains(&contact.email.to_lowercase())
    {
        return recipients.to_owned();
    }
    let token_start = recipients.rfind([',', ';']).map_or(0, |index| index + 1);
    let prefix = recipients[..token_start].trim_end();
    let clean_name = contact
        .name
        .as_deref()
        .unwrap_or_default()
        .chars()
        .filter(|character| !matches!(character, ',' | ';' | '<' | '>'))
        .collect::<String>();
    let recipient =
        if clean_name.trim().is_empty() || clean_name.trim().eq_ignore_ascii_case(&contact.email) {
            contact.email.clone()
        } else {
            format!("{} <{}>", clean_name.trim(), contact.email)
        };
    if prefix.is_empty() {
        format!("{recipient}, ")
    } else {
        format!("{prefix} {recipient}, ")
    }
}

pub(super) fn clear_compose(
    app: &AppWindow,
    files: &Rc<RefCell<Vec<ComposeFile>>>,
    document: &Rc<RefCell<RichComposeDocument>>,
    editor: &Rc<RefCell<CosmicComposeEditor>>,
    contacts: &Rc<RefCell<Vec<flectar_mail_core::models::Address>>>,
) {
    files.borrow_mut().clear();
    document.borrow_mut().reset();
    editor.borrow_mut().reset();
    contacts.borrow_mut().clear();
    apply_compose_files(app, &[]);
    apply_compose_contacts(app, &[]);
    app.set_compose_open(false);
    app.set_compose_mode("new".into());
    app.set_compose_to("".into());
    app.set_compose_cc("".into());
    app.set_compose_bcc("".into());
    app.set_compose_subject("".into());
    app.set_compose_body("".into());
    app.set_compose_notice(UiMessage::EMPTY);
    app.set_compose_notice_is_error(false);
    apply_rich_compose(
        app,
        &document.borrow(),
        ComposeSelection::default(),
        &mut editor.borrow_mut(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use flectar_mail_core::models::Address;

    fn address(name: &str, email: &str) -> Address {
        Address {
            name: (!name.is_empty()).then(|| name.to_owned()),
            email: email.to_owned(),
        }
    }

    fn source() -> ComposeSource {
        ComposeSource {
            message_id: 91,
            account_id: 7,
            account_email: "alex@example.com".to_owned(),
            from: address("Maya", "maya@example.com"),
            to: vec![
                address("Alex", "alex@example.com"),
                address("Jon", "jon@example.com"),
            ],
            cc: vec![
                address("Jon Duplicate", "JON@example.com"),
                address("Lena", "lena@example.com"),
            ],
            subject: "Launch plan".to_owned(),
            date: 1_777_000_000_000,
            text_body: "First line\nSecond line".to_owned(),
        }
    }

    #[test]
    fn reply_all_excludes_the_sender_account_and_deduplicates_recipients() {
        let prepared = prepare_message_compose(&source(), "reply_all", "").unwrap();

        assert_eq!(prepared.account_id, 7);
        assert_eq!(prepared.to, "Maya <maya@example.com>");
        assert_eq!(
            prepared.cc,
            "Jon <jon@example.com>, Lena <lena@example.com>"
        );
        assert_eq!(prepared.subject, "Re: Launch plan");
        assert!(prepared.body.contains("> First line\n> Second line"));
        assert_eq!(prepared.intent.in_reply_to_message_id, Some(91));
    }

    #[test]
    fn forward_starts_unaddressed_and_does_not_join_the_original_thread() {
        let prepared = prepare_message_compose(&source(), "forward", "Rendered body").unwrap();

        assert!(prepared.to.is_empty());
        assert!(prepared.cc.is_empty());
        assert_eq!(prepared.subject, "Fwd: Launch plan");
        assert!(
            prepared
                .body
                .contains("---------- Forwarded message ----------")
        );
        assert!(prepared.body.ends_with("Rendered body"));
        assert_eq!(prepared.intent.mode, "forward");
        assert_eq!(prepared.intent.in_reply_to_message_id, None);
    }

    #[test]
    fn reply_subject_does_not_stack_prefixes() {
        let mut reply = source();
        reply.subject = "RE: Launch plan".to_owned();
        assert_eq!(
            prepare_message_compose(&reply, "reply", "")
                .unwrap()
                .subject,
            "RE: Launch plan"
        );

        reply.subject = "FW: Launch plan".to_owned();
        assert_eq!(
            prepare_message_compose(&reply, "forward", "")
                .unwrap()
                .subject,
            "FW: Launch plan"
        );
    }
}
