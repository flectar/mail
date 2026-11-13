//! Compose view-model projection and recipient/file helpers.

use super::*;
use flectar_mail_core::sender_identities::{is_own_address, reply_sender};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ComposeIntent {
    pub mode: String,
    pub in_reply_to_message_id: Option<i64>,
    /// The Message-ID a reply answers when that message is not stored here.
    pub in_reply_to_header: Option<String>,
    pub draft_id: Option<i64>,
}

impl Default for ComposeIntent {
    fn default() -> Self {
        Self {
            mode: "new".to_owned(),
            in_reply_to_message_id: None,
            in_reply_to_header: None,
            draft_id: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PreparedMessageCompose {
    pub account_id: i64,
    pub sender_email: String,
    pub to: String,
    pub cc: String,
    pub subject: String,
    /// What the editor starts with.
    pub body: String,
    /// A message edited as new keeps its formatting when this is its own.
    pub saved: Option<SavedBody>,
    /// The original a reply or forward carries below the editor.
    pub quote: Option<ComposeQuote>,
    pub intent: ComposeIntent,
}

/// A message as it was sent: its HTML, and the plain text that has to say
/// the same for the HTML's formatting to be kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SavedBody {
    pub html: String,
    pub text: String,
}

/// Prepares a reply or forward, or a message edited as new. `translate`
/// renders the quotation headers in the interface language, as mail clients
/// write them for their user.
pub(super) fn prepare_message_compose(
    source: &ComposeSource,
    action: &str,
    rendered_plain_text: &str,
    translate: impl Fn(UiMessage) -> String,
) -> Result<PreparedMessageCompose, String> {
    if !matches!(action, "reply" | "reply_all" | "forward" | "edit_as_new") {
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
        .map(|date| date_format::current().date_time(DateStyle::WeekdayMedium, date.naive_local()));

    // Thunderbird's Edit As New Message: the message as it was, to send
    // again or anew, from the address that sent it when that is one's own.
    if action == "edit_as_new" {
        let sender_email = if is_own_address(&source.sender_identities, &source.from.email) {
            source.from.email.clone()
        } else {
            reply_sender(
                &source.sender_identities,
                source.to.iter().chain(source.cc.iter()),
            )
            .unwrap_or_else(|| source.default_sender_email.clone())
        };
        return Ok(PreparedMessageCompose {
            account_id: source.account_id,
            sender_email,
            to: join_addresses(&source.to),
            cc: join_addresses(&source.cc),
            subject: source.subject.clone(),
            body: original_body.trim_end().to_owned(),
            saved: source.html_body.clone().map(|html| SavedBody {
                html,
                text: source.text_body.clone(),
            }),
            quote: None,
            intent: ComposeIntent::default(),
        });
    }

    if action == "forward" {
        let mut header = vec![format!(
            "---------- {} ----------",
            translate(UiMessage::plain("Forwarded message"))
        )];
        let lines = [
            Some(UiMessage::detail("From: {}", &sender)),
            sent_at
                .as_ref()
                .map(|sent_at| UiMessage::detail("Date: {}", sent_at)),
            Some(UiMessage::detail("Subject: {}", &source.subject)),
            Some(UiMessage::detail("To: {}", join_addresses(&source.to))),
        ];
        header.extend(lines.into_iter().flatten().map(&translate));
        let quote = ComposeQuote::forward(&header, original_body, source.html_body.as_deref());
        // Like a reply, forward from the address the message reached.
        let sender_email = reply_sender(
            &source.sender_identities,
            source.to.iter().chain(source.cc.iter()),
        )
        .unwrap_or_else(|| source.default_sender_email.clone());
        return Ok(PreparedMessageCompose {
            account_id: source.account_id,
            sender_email,
            to: String::new(),
            cc: String::new(),
            subject: prefixed_subject(&source.subject, "Fwd"),
            body: String::new(),
            saved: None,
            quote: Some(quote),
            intent: ComposeIntent {
                mode: "forward".to_owned(),
                in_reply_to_message_id: None,
                in_reply_to_header: None,
                draft_id: None,
            },
        });
    }

    let is_own = |email: &str| is_own_address(&source.sender_identities, email);
    let sender_email = reply_sender(
        &source.sender_identities,
        source
            .to
            .iter()
            .chain(source.cc.iter())
            .chain(std::iter::once(&source.from)),
    )
    .unwrap_or_else(|| source.default_sender_email.clone());
    let candidates = std::iter::once(&source.from)
        .filter(|address| !is_own(&address.email))
        .chain(source.to.iter())
        .chain(source.cc.iter().filter(|_| action == "reply_all"));
    let recipients = deduplicated_addresses(candidates, is_own);
    let primary = recipients
        .first()
        .ok_or_else(|| "the selected message has no reply recipient".to_owned())?;
    let cc = if action == "reply_all" {
        join_addresses(recipients[1..].iter().copied())
    } else {
        String::new()
    };
    let attribution = translate(match &sent_at {
        Some(sent_at) => UiMessage::arguments("On {}, {} wrote:", sent_at, &sender),
        None => UiMessage::detail("{} wrote:", &sender),
    });
    let quote = ComposeQuote::reply(&attribution, original_body, source.html_body.as_deref());

    Ok(PreparedMessageCompose {
        account_id: source.account_id,
        sender_email,
        to: format_address(primary),
        cc,
        subject: prefixed_subject(&source.subject, "Re"),
        body: String::new(),
        saved: None,
        quote: Some(quote),
        intent: ComposeIntent {
            mode: action.to_owned(),
            in_reply_to_message_id: Some(source.message_id),
            in_reply_to_header: None,
            draft_id: None,
        },
    })
}

pub(super) fn compose_addresses(addresses: &[flectar_mail_core::models::Address]) -> String {
    join_addresses(addresses)
}

fn deduplicated_addresses<'a>(
    addresses: impl IntoIterator<Item = &'a flectar_mail_core::models::Address>,
    is_own: impl Fn(&str) -> bool,
) -> Vec<&'a flectar_mail_core::models::Address> {
    let mut seen = HashSet::new();
    addresses
        .into_iter()
        .filter(|address| {
            let email = address.email.trim();
            let email = if email.bytes().any(|byte| byte.is_ascii_uppercase()) {
                std::borrow::Cow::Owned(email.to_ascii_lowercase())
            } else {
                std::borrow::Cow::Borrowed(email)
            };
            !email.is_empty() && !is_own(&email) && seen.insert(email)
        })
        .collect()
}

fn join_addresses<'a>(
    addresses: impl IntoIterator<Item = &'a flectar_mail_core::models::Address>,
) -> String {
    let mut output = String::new();
    append_addresses(&mut output, addresses);
    output
}

fn append_addresses<'a>(
    output: &mut String,
    addresses: impl IntoIterator<Item = &'a flectar_mail_core::models::Address>,
) {
    for (index, address) in addresses.into_iter().enumerate() {
        if index > 0 {
            output.push_str(", ");
        }
        append_address(output, address);
    }
}

fn format_address(address: &flectar_mail_core::models::Address) -> String {
    let mut output = String::new();
    append_address(&mut output, address);
    output
}

fn append_address(output: &mut String, address: &flectar_mail_core::models::Address) {
    match address
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        Some(name) => {
            let name = if name.contains([',', ';', '<', '>', '\r', '\n']) {
                std::borrow::Cow::Owned(
                    name.chars()
                        .filter(|character| {
                            !matches!(character, ',' | ';' | '<' | '>' | '\r' | '\n')
                        })
                        .collect::<String>(),
                )
            } else {
                std::borrow::Cow::Borrowed(name)
            };
            output.push_str(name.trim());
            output.push_str(" <");
            output.push_str(&address.email);
            output.push('>');
        }
        None => output.push_str(&address.email),
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
    _app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    _runtime: &tokio::runtime::Handle,
    action: &str,
) -> Result<(), String> {
    let (selected_id, target, core, using_core) = {
        let state = state.borrow();
        let selected = state
            .selected_id
            .and_then(|id| state.messages.iter().find(|message| message.id == id));
        (
            state.selected_id,
            selected.and_then(MailTarget::of),
            state.core.clone(),
            state.using_core,
        )
    };
    if !using_core {
        return Err("mail account is not ready".to_owned());
    }
    let core = core.ok_or_else(|| "mail core is unavailable".to_owned())?;
    let target = target.ok_or_else(|| "no message is selected".to_owned())?;
    mail_work::enqueue(
        state,
        core,
        vec![(
            selected_id.unwrap(),
            target,
            mail_work::Operation::Action(action.to_owned()),
        )],
    )
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

pub(super) fn apply_compose_files(ui: &ComposerUi<'_>, files: &[ComposeFile]) {
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
    ui.set_attachments(ModelRc::new(VecModel::from(rows)));
    ui.set_attachment_summary(
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

/// Where a composer is shown: its window's `ComposerUi`, the window itself
/// for the scale factor, and whether that window paints at all.
pub(super) struct ComposeView<'a> {
    pub(super) ui: ComposerUi<'a>,
    pub(super) window: &'a slint::Window,
    pub(super) suspended: bool,
}

pub(super) fn apply_rich_compose(
    view: &ComposeView<'_>,
    document: &RichComposeDocument,
    selection: ComposeSelection,
    editor: &mut LazyComposeEditor,
) {
    view.ui.set_body(document.text().into());
    apply_rich_compose_state(&view.ui, document, selection);
    apply_compose_editor_surface(view, document, selection, editor);
}

pub(super) fn apply_compose_editor_surface(
    view: &ComposeView<'_>,
    document: &RichComposeDocument,
    selection: ComposeSelection,
    editor: &mut LazyComposeEditor,
) {
    let ui = &view.ui;
    if view.suspended || !ui.get_open() {
        return;
    }
    let width = ui.get_editor_width().max(1.0);
    let viewport_height = ui.get_editor_viewport_height().max(1.0);
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
        ui.get_editor_scroll_y().max(0.0),
        view.window.scale_factor(),
        compose_editor_style(ui),
        ui.get_editor_preedit_text().as_str(),
    );
    apply_rendered_compose_editor(ui, rendered);
}

pub(super) fn apply_rendered_compose_editor(ui: &ComposerUi<'_>, rendered: RenderedComposeEditor) {
    let started = crate::renderer::render_timings_enabled().then(std::time::Instant::now);
    let tiles = rendered
        .tiles
        .into_iter()
        .map(|tile| ComposeEditorTile {
            image: tile.image,
            y: tile.y,
            height: tile.height,
        })
        .collect::<Vec<_>>();
    let tile_count = tiles.len();
    let retained = ui.get_editor_tiles();
    let replaced_model = if let Some(model) = retained
        .as_any()
        .downcast_ref::<VecModel<ComposeEditorTile>>()
    {
        crate::reconcile_model_rows_by(model, tiles, |tile| tile.y.to_bits(), PartialEq::eq);
        false
    } else {
        ui.set_editor_tiles(ModelRc::new(VecModel::from(tiles)));
        true
    };
    ui.set_editor_content_height(rendered.content_height);
    ui.set_editor_caret_x(rendered.caret_x);
    ui.set_editor_caret_y(rendered.caret_y);
    ui.set_editor_caret_height(rendered.caret_height);
    let [x, y, width, height] = rendered.image_box.unwrap_or_default();
    ui.set_editor_image_shown(rendered.image_box.is_some());
    ui.set_editor_image_x(x);
    ui.set_editor_image_y(y);
    ui.set_editor_image_width(width);
    ui.set_editor_image_height(height);
    if let Some(started) = started {
        eprintln!(
            "compose Slint model update: {:.2}ms tiles={tile_count} replaced_model={replaced_model}",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
}

pub(super) fn compose_editor_style(ui: &ComposerUi<'_>) -> ComposeEditorStyle {
    let selection = ui.get_editor_selection_color();
    ComposeEditorStyle {
        text: ui.get_editor_text_color(),
        link: ui.get_editor_link_color(),
        selection: slint::Color::from_argb_u8(
            140,
            selection.red(),
            selection.green(),
            selection.blue(),
        ),
        selected_text: ui.get_editor_selected_text_color(),
        misspelling: ui.get_editor_misspelling_color(),
    }
}

pub(super) fn apply_rich_compose_state(
    ui: &ComposerUi<'_>,
    document: &RichComposeDocument,
    selection: ComposeSelection,
) {
    let active = document.active_marks();
    ui.set_selection_start(selection.start);
    ui.set_selection_end(selection.end);
    ui.set_bold_active(active.bold);
    ui.set_italic_active(active.italic);
    ui.set_underline_active(active.underline);
    ui.set_strike_active(active.strike);
    ui.set_code_active(active.code);
    ui.set_active_link_url(document.active_link().unwrap_or_default().into());
    ui.set_active_color(color_choice(active.color));
    ui.set_active_highlight(color_choice(active.highlight));
    ui.set_active_size(active.size.map_or("", |size| size.name()).into());
    ui.set_active_family(active.family.map_or("", |family| family.name()).into());
    ui.set_active_align(active.align.name().into());
    let image = document.selected_image();
    ui.set_active_image_size(
        match image {
            Some(image) => rich_compose::ImageSize::of(image).map_or("custom", |size| size.name()),
            None => "",
        }
        .into(),
    );
    ui.set_active_image_alt(image.map_or("", |image| &*image.alt).into());
    ui.set_active_image_width(image.map_or(0, |image| image.width.min(i32::MAX as u32) as i32));
    ui.set_active_image_shown_width(
        image.map_or(0, |image| image.shown_size().0.min(i32::MAX as u32) as i32),
    );
}

/// A color the selection shares as the toolbar shows it: `#rrggbb`, `none` when it has none,
/// or nothing when it mixes several.
fn color_choice(color: Option<Option<rich_compose::Rgb>>) -> slint::SharedString {
    match color {
        Some(Some(color)) => rich_compose::css_rgb(color).into(),
        Some(None) => "none".into(),
        None => slint::SharedString::new(),
    }
}

pub(super) fn apply_compose_contacts(
    ui: &ComposerUi<'_>,
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
    ui.set_contact_suggestions(ModelRc::new(VecModel::from(rows)));
}

pub(super) fn apply_email_templates(
    ui: &ComposerUi<'_>,
    templates: &[flectar_mail_core::models::Snippet],
) {
    let rows = templates
        .iter()
        .filter_map(|template| {
            Some(EmailTemplateRow {
                id: i32::try_from(template.id).ok()?,
                name: template.name.clone().into(),
                shortcut: template.shortcut.clone().unwrap_or_default().into(),
                subject: template.subject.clone().unwrap_or_default().into(),
                body_text: template.body_text.clone().into(),
            })
        })
        .collect::<Vec<_>>();
    ui.set_templates(ModelRc::new(VecModel::from(rows)));
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
    let token_start = recipients.rfind([',', ';', '\n']).map_or(0, |index| index + 1);
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
    let token_start = recipients.rfind([',', ';', '\n']).map_or(0, |index| index + 1);
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
    } else if prefix.ends_with([',', ';']) {
        format!("{prefix} {recipient}, ")
    } else {
        // A newline-separated prefix normalizes to a comma separator so the
        // completed address never glues onto the previous line.
        format!("{prefix}, {recipient}, ")
    }
}

/// The original's files once fetched: each one's path, name and size, or
/// why it could not be had.
#[cfg(test)]
mod tests {
    use super::*;
    use flectar_mail_core::models::{
        Address, SenderDeliveryStatus, SenderIdentity, SenderIdentitySource,
    };

    fn english(message: UiMessage) -> String {
        message.english()
    }

    fn address(name: &str, email: &str) -> Address {
        Address {
            name: (!name.is_empty()).then(|| name.to_owned()),
            email: email.to_owned(),
        }
    }

    fn identity(email: &str, primary: bool, provider_default: bool) -> SenderIdentity {
        SenderIdentity {
            account_id: 7,
            email: email.to_owned(),
            display_name: None,
            reply_to_email: None,
            is_primary: primary,
            is_provider_default: provider_default,
            verification_status: "accepted".into(),
            last_synced_at: 1,
            source: SenderIdentitySource::Provider,
            delivery_status: SenderDeliveryStatus::Unknown,
            delivery_error: None,
        }
    }

    fn source() -> ComposeSource {
        ComposeSource {
            message_id: 91,
            account_id: 7,
            account_email: "alex@example.com".to_owned(),
            sender_identities: vec![
                identity("alex@example.com", true, true),
                identity("work@example.net", false, false),
            ],
            default_sender_email: "alex@example.com".to_owned(),
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
            html_body: None,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn reply_all_excludes_the_sender_account_and_deduplicates_recipients() {
        let prepared = prepare_message_compose(&source(), "reply_all", "", english).unwrap();

        assert_eq!(prepared.account_id, 7);
        assert_eq!(prepared.sender_email, "alex@example.com");
        assert_eq!(prepared.to, "Maya <maya@example.com>");
        assert_eq!(
            prepared.cc,
            "Jon <jon@example.com>, Lena <lena@example.com>"
        );
        assert_eq!(prepared.subject, "Re: Launch plan");
        assert!(prepared.body.is_empty(), "the reply starts empty");
        assert!(
            prepared
                .quote
                .unwrap()
                .plain
                .ends_with("wrote:\n> First line\n> Second line")
        );
        assert_eq!(prepared.intent.in_reply_to_message_id, Some(91));
    }

    #[test]
    fn reply_uses_the_alias_that_received_the_message_and_excludes_all_self_addresses() {
        let mut source = source();
        source.to = vec![
            address("Alex at work", "work@example.net"),
            address("Alex", "alex@example.com"),
            address("Jon", "jon@example.com"),
        ];
        source.cc = vec![address("Work duplicate", "WORK@example.net")];

        let prepared = prepare_message_compose(&source, "reply_all", "", english).unwrap();

        assert_eq!(prepared.sender_email, "work@example.net");
        assert_eq!(prepared.to, "Maya <maya@example.com>");
        assert_eq!(prepared.cc, "Jon <jon@example.com>");
    }

    #[test]
    fn replies_use_wildcard_and_subaddress_matches_and_exclude_them_from_reply_all() {
        let mut source = source();
        source.sender_identities.push(identity("*@team.example", false, false));
        source.to = vec![
            address("Support", "support@team.example"),
            address("Alex", "alex+launch@example.com"),
            address("Jon", "jon@example.com"),
        ];
        source.cc = Vec::new();

        let prepared = prepare_message_compose(&source, "reply_all", "", english).unwrap();

        assert_eq!(prepared.sender_email, "support@team.example");
        assert_eq!(prepared.to, "Maya <maya@example.com>");
        assert_eq!(prepared.cc, "Jon <jon@example.com>");

        source.to = vec![address("Alex", "alex+launch@example.com")];
        let prepared = prepare_message_compose(&source, "reply", "", english).unwrap();
        assert_eq!(prepared.sender_email, "alex@example.com");
    }

    #[test]
    fn forward_uses_the_alias_that_received_the_message() {
        let mut source = source();
        source.to = vec![address("Alex at work", "work@example.net")];
        let prepared = prepare_message_compose(&source, "forward", "", english).unwrap();
        assert_eq!(prepared.sender_email, "work@example.net");
    }

    #[test]
    fn forward_starts_unaddressed_and_does_not_join_the_original_thread() {
        let prepared =
            prepare_message_compose(&source(), "forward", "Rendered body", english).unwrap();

        assert!(prepared.to.is_empty());
        assert!(prepared.cc.is_empty());
        assert_eq!(prepared.subject, "Fwd: Launch plan");
        let quote = prepared.quote.unwrap();
        assert!(quote.forward);
        assert!(
            quote
                .plain
                .starts_with("---------- Forwarded message ----------")
        );
        assert!(quote.plain.ends_with("Rendered body"));
        assert_eq!(prepared.intent.mode, "forward");
        assert_eq!(prepared.intent.in_reply_to_message_id, None);
    }

    #[test]
    fn a_message_edited_as_new_is_the_message_as_it_was_and_no_reply() {
        let prepared =
            prepare_message_compose(&source(), "edit_as_new", "Rendered body\n\n", english)
                .unwrap();

        assert_eq!(prepared.account_id, 7);
        assert_eq!(
            prepared.to, "Alex <alex@example.com>, Jon <jon@example.com>",
            "the same recipients, the account's own address too"
        );
        assert_eq!(
            prepared.cc,
            "Jon Duplicate <JON@example.com>, Lena <lena@example.com>"
        );
        assert_eq!(prepared.subject, "Launch plan", "no prefix");
        assert_eq!(prepared.body, "Rendered body");
        assert_eq!(prepared.intent, ComposeIntent::default());
        // Received: sent from the address it reached.
        assert_eq!(prepared.sender_email, "alex@example.com");

        // Sent from an alias: sent from it again.
        let mut sent = source();
        sent.from = address("Alex at work", "work@example.net");
        let prepared = prepare_message_compose(&sent, "edit_as_new", "", english).unwrap();
        assert_eq!(prepared.sender_email, "work@example.net");
        assert_eq!(prepared.body, "First line\nSecond line");
        assert!(prepared.saved.is_none(), "no HTML to keep");

        // Its HTML comes with the plain text it was sent with, which the
        // HTML has to match for its formatting to be kept.
        sent.html_body = Some("<div>First line</div><div>Second line</div>".to_owned());
        let prepared =
            prepare_message_compose(&sent, "edit_as_new", "Rendered body", english).unwrap();
        let saved = prepared.saved.unwrap();
        assert_eq!(saved.html, "<div>First line</div><div>Second line</div>");
        assert_eq!(saved.text, sent.text_body);
        assert_eq!(prepared.body, "Rendered body");
    }

    #[test]
    fn quotation_headers_use_the_interface_wording_and_date_format() {
        let sent_at = date_format::current().date_time(
            DateStyle::WeekdayMedium,
            Local
                .timestamp_millis_opt(source().date)
                .unwrap()
                .naive_local(),
        );
        let reply = prepare_message_compose(&source(), "reply", "", english).unwrap();
        assert_eq!(
            reply.quote.unwrap().plain,
            format!("On {sent_at}, Maya <maya@example.com> wrote:\n> First line\n> Second line")
        );

        let forward = prepare_message_compose(&source(), "forward", "", english).unwrap();
        assert_eq!(
            forward.quote.unwrap().plain,
            format!(
                "---------- Forwarded message ----------\nFrom: Maya <maya@example.com>\n\
                 Date: {sent_at}\nSubject: Launch plan\n\
                 To: Alex <alex@example.com>, Jon <jon@example.com>\n\nFirst line\nSecond line"
            )
        );

        let shouted = |message: UiMessage| message.english().to_uppercase();
        let forward = prepare_message_compose(&source(), "forward", "", shouted).unwrap();
        assert!(forward.quote.unwrap().plain.starts_with(
            "---------- FORWARDED MESSAGE ----------\nFROM: MAYA <MAYA@EXAMPLE.COM>\nDATE: "
        ));
        let mut undated = source();
        undated.date = i64::MAX;
        let reply = prepare_message_compose(&undated, "reply", "", english).unwrap();
        assert!(
            reply
                .quote
                .unwrap()
                .plain
                .starts_with("Maya <maya@example.com> wrote:\n> First line")
        );

        // An HTML original is quoted as HTML.
        let mut html = source();
        html.html_body = Some("<body><p>First <b>line</b></p></body>".to_owned());
        let reply = prepare_message_compose(&html, "reply", "", english).unwrap();
        assert!(
            reply
                .quote
                .unwrap()
                .html
                .contains("<p>First <b>line</b></p></blockquote>")
        );
    }

    #[test]
    fn reply_subject_does_not_stack_prefixes() {
        let mut reply = source();
        reply.subject = "RE: Launch plan".to_owned();
        assert_eq!(
            prepare_message_compose(&reply, "reply", "", english)
                .unwrap()
                .subject,
            "RE: Launch plan"
        );

        reply.subject = "FW: Launch plan".to_owned();
        assert_eq!(
            prepare_message_compose(&reply, "forward", "", english)
                .unwrap()
                .subject,
            "FW: Launch plan"
        );
    }

    #[test]
    fn recipient_query_uses_text_after_last_separator_including_newlines() {
        assert_eq!(compose_recipient_query("ada@example.com, gr"), "gr");
        assert_eq!(compose_recipient_query("ada@example.com; gr"), "gr");
        assert_eq!(compose_recipient_query("ada@example.com\ngr"), "gr");
    }

    #[test]
    fn complete_recipient_keeps_newline_separated_prefix_usable() {
        let completed = complete_compose_recipient(
            "ada@example.com\ngr",
            &address("Grace", "grace@example.com"),
        );
        assert_eq!(
            completed,
            "ada@example.com, Grace <grace@example.com>, "
        );
    }
}
