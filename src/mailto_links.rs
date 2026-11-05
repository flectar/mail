//! Opening mailto links, whether clicked in a message or handed over by the
//! system when Flectar Mail is the default email app.
//!
//! Links wait until the mail store is ready and an account can send, then
//! each opens the composer pre-filled for the user to review; nothing is sent
//! on a link's behalf. The composer holds one message, so a link never
//! replaces what the user is writing: a blank composer is reused, anything
//! else is saved to Drafts first, where other clients open another window.

use super::*;
use flectar_mail_core::mailto::{self, Mailbox, Mailto};
use std::collections::VecDeque;

#[cfg(target_os = "macos")]
mod macos;

/// More mail links than this in one launch are ignored.
#[cfg(desktop)]
pub(crate) const MAX_LAUNCH_LINKS: usize = 16;

/// The option the Windows registration puts before the link it passes:
/// `flectar-mail.exe --mailto "%1"`.
#[cfg(desktop)]
pub(crate) const MAILTO_OPTION: &str = "--mailto";

/// The mail links a launch was opened with, or `None` for a launch that is
/// not about mail links, whose options then apply as usual.
///
/// - `--mailto <link>` must be the whole command line, as Thunderbird
///   requires of its `-osint` launches: a link that broke out of its quotes
///   could otherwise add options such as `--background`. Anything else after
///   `--mailto` opens nothing.
/// - Desktop entries pass each link as its own argument (`%U`), so every
///   `mailto:` argument is a link and other arguments are ignored.
#[cfg(desktop)]
pub(crate) fn launch_links(
    arguments: impl IntoIterator<Item = std::ffi::OsString>,
) -> Option<Vec<String>> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    if arguments.iter().any(|argument| argument == MAILTO_OPTION) {
        return Some(match arguments.as_slice() {
            [option, link] if option == MAILTO_OPTION => link
                .to_str()
                .filter(|link| mailto::is_mailto(link))
                .map(|link| vec![link.to_owned()])
                .unwrap_or_default(),
            _ => Vec::new(),
        });
    }
    let links = arguments
        .iter()
        .filter_map(|argument| argument.to_str())
        .filter(|argument| mailto::is_mailto(argument))
        .take(MAX_LAUNCH_LINKS)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    (!links.is_empty()).then_some(links)
}

/// What opening a link needs from the rest of the application.
#[derive(Clone)]
pub(super) struct Composer {
    pub(super) state: Rc<RefCell<InboxState>>,
    pub(super) runtime: Rc<tokio::runtime::Runtime>,
    pub(super) intent: Rc<RefCell<ComposeIntent>>,
    pub(super) document: Rc<RefCell<RichComposeDocument>>,
    pub(super) editor: Rc<RefCell<LazyComposeEditor>>,
}

/// How long a link waits for the open message's sender identity, which
/// loads just after the composer opens, before saving that message anyway.
const IDENTITY_RETRY: Duration = Duration::from_millis(100);
const IDENTITY_RETRIES: u32 = 50;

#[derive(Default)]
struct Links {
    pending: VecDeque<Mailto>,
    composer: Option<Composer>,
    scheduled: bool,
    /// Retries spent waiting for the open message's sender identity.
    identity_waits: u32,
}

thread_local! {
    static LINKS: RefCell<Links> = RefCell::default();
}

/// Connect the composer. Links received before this wait for it.
pub(super) fn install(app: &AppWindow, composer: Composer) {
    LINKS.with(|links| links.borrow_mut().composer = Some(composer));
    open_pending_soon(app);
}

/// Open the mail links macOS hands to the application. They arrive as Apple
/// Events rather than on the command line, also for the launch itself, and
/// bring the window back like any other launch.
#[cfg(target_os = "macos")]
pub(super) fn receive_from_macos(app: &AppWindow) {
    let app = app.as_weak();
    macos::on_open_url(move |url| {
        if !mailto::is_mailto(&url) {
            return;
        }
        let Some(app) = app.upgrade() else {
            return;
        };
        window_controller::activate(&app, None);
        open(&app, "apple-event", [url]);
    });
}

/// Open mailto links in the composer as soon as it can take them. `source`
/// says how they arrived ("message", "launch", "forwarded" or
/// "apple-event") for the diagnostic line.
pub(super) fn open<S: AsRef<str>>(
    app: &AppWindow,
    source: &'static str,
    uris: impl IntoIterator<Item = S>,
) {
    let (mut received, mut unreadable) = (0_usize, 0_usize);
    LINKS.with(|links| {
        let mut links = links.borrow_mut();
        for uri in uris {
            match mailto::parse(uri.as_ref()) {
                Some(message) => {
                    links.pending.push_back(message);
                    received += 1;
                }
                None => unreadable += 1,
            }
        }
    });
    if received + unreadable > 0 {
        // Whether a link reached the application, without any of its
        // content, like the FLECTAR_RENDERER lines.
        eprintln!(
            "FLECTAR_MAIL_LINKS {}",
            serde_json::json!({
                "event": "received", "source": source,
                "links": received, "unreadable": unreadable,
            })
        );
    }
    if unreadable > 0 {
        app.set_render_status(UiMessage::plain("This email link is too long to open."));
    }
    open_pending_soon(app);
}

/// Try the waiting links again on the next turn of the event loop: after
/// startup, when accounts change and when the composer closes. Deferring keeps
/// this out of whatever borrow of the application state the caller holds.
pub(super) fn open_pending_soon(app: &AppWindow) {
    open_pending_after(app, Duration::ZERO);
}

fn open_pending_after(app: &AppWindow, delay: Duration) {
    let idle = LINKS.with(|links| {
        let mut links = links.borrow_mut();
        let idle = !links.pending.is_empty() && !links.scheduled;
        links.scheduled |= idle;
        idle
    });
    if !idle {
        return;
    }
    let app = app.as_weak();
    Timer::single_shot(delay, move || {
        LINKS.with(|links| links.borrow_mut().scheduled = false);
        if let Some(app) = app.upgrade() {
            open_pending(&app);
        }
    });
}

fn open_pending(app: &AppWindow) {
    while let Some((message, composer)) = LINKS.with(|links| {
        let links = links.borrow();
        Some((links.pending.front()?.clone(), links.composer.clone()?))
    }) {
        if !app.get_startup_ready() {
            return;
        }
        if composer.state.borrow().connected_accounts.is_empty() {
            // Without an account to send from, a link is dropped, as
            // Thunderbird does, rather than opening much later.
            LINKS.with(|links| links.borrow_mut().pending.clear());
            app.set_sync_status(UiMessage::plain("Add an account to send mail."));
            return;
        }
        let saved_previous = match make_room(app) {
            Room::Free => false,
            Room::Saved => true,
            Room::Busy => return,
            Room::IdentityLoading => {
                LINKS.with(|links| links.borrow_mut().identity_waits += 1);
                open_pending_after(app, IDENTITY_RETRY);
                return;
            }
        };
        LINKS.with(|links| links.borrow_mut().identity_waits = 0);
        if !compose(app, &composer, &message, saved_previous) {
            return;
        }
        LINKS.with(|links| links.borrow_mut().pending.pop_front());
    }
}

enum Room {
    /// No message was open, or only a blank one.
    Free,
    /// What the user was writing went to Drafts.
    Saved,
    /// The open message is sending or could not be saved (its notice says
    /// why); the link opens when it closes.
    Busy,
    /// The open message's sender identity is still loading, as right after
    /// another link opened it; saving it now would fail.
    IdentityLoading,
}

/// Close a blank composer, or save what the user is writing to Drafts.
fn make_room(app: &AppWindow) -> Room {
    if !app.get_compose_open() {
        return Room::Free;
    }
    if app.get_compose_sending() {
        return Room::Busy;
    }
    if composer_is_blank(app) {
        app.invoke_close_compose();
        return if app.get_compose_open() {
            Room::Busy
        } else {
            Room::Free
        };
    }
    let identity_loading = app
        .global::<AccountMailPreferences>()
        .get_composer_sender_email()
        .is_empty();
    if identity_loading && LINKS.with(|links| links.borrow().identity_waits) < IDENTITY_RETRIES {
        return Room::IdentityLoading;
    }
    app.invoke_save_compose(
        app.get_compose_account_id(),
        app.get_compose_to(),
        app.get_compose_cc(),
        app.get_compose_bcc(),
        app.get_compose_subject(),
        app.get_compose_body(),
        false,
    );
    if app.get_compose_open() {
        app.set_sync_status(UiMessage::plain(
            "The email link will open when you close this message.",
        ));
        return Room::Busy;
    }
    Room::Saved
}

/// Nothing the user wrote: empty fields, no attachments, and at most the
/// signature the composer inserted.
fn composer_is_blank(app: &AppWindow) -> bool {
    [
        app.get_compose_to(),
        app.get_compose_cc(),
        app.get_compose_bcc(),
        app.get_compose_subject(),
    ]
    .iter()
    .all(|text| text.trim().is_empty())
        && app.get_compose_attachments().row_count() == 0
        && account_mail_preferences::only_inserted_signature(&app.get_compose_body())
}

fn compose(app: &AppWindow, composer: &Composer, message: &Mailto, saved_previous: bool) -> bool {
    app.invoke_open_compose();
    if !app.get_compose_open() {
        return false;
    }
    app.set_compose_to(recipient_field(&message.to).into());
    app.set_compose_cc(recipient_field(&message.cc).into());
    app.set_compose_bcc(recipient_field(&message.bcc).into());
    if let Some(subject) = &message.subject {
        app.set_compose_subject(subject.into());
    }
    if let Some(body) = &message.body {
        let mut document = composer.document.borrow_mut();
        let selection = document.load(body, 0, 0);
        apply_rich_compose(app, &document, selection, &mut composer.editor.borrow_mut());
    }
    if let Some(header_id) = &message.in_reply_to {
        thread_reply(app, composer, header_id);
    }
    if !message.attachment_requests.is_empty() {
        app.set_compose_notice(UiMessage::detail(
            "Files from links are never attached for you. Attach them yourself if you trust them: {}",
            message.attachment_requests.join(", "),
        ));
        app.set_compose_notice_is_error(false);
    } else if saved_previous {
        app.set_compose_notice(UiMessage::plain(
            "The message you were writing was saved to Drafts.",
        ));
        app.set_compose_notice_is_error(false);
    }
    true
}

/// Make the message a reply to the one the link names. When the account it
/// is written from has that message, the reply threads with it like any
/// other; otherwise its Message-ID still goes into In-Reply-To and
/// References, as a mailing list archive's reply link expects. The sending
/// account is never changed for a link.
fn thread_reply(app: &AppWindow, composer: &Composer, header_id: &str) {
    let account_id = i64::from(app.get_compose_account_id());
    let core = composer.state.borrow().core.clone();
    let parent = core.and_then(|core| {
        composer
            .runtime
            .block_on(core.message_by_header_id(account_id, header_id))
            .ok()
            .flatten()
    });
    let mut intent = composer.intent.borrow_mut();
    intent.mode = "reply".to_owned();
    match parent {
        Some(parent) => intent.in_reply_to_message_id = Some(parent),
        None => intent.in_reply_to_header = Some(header_id.to_owned()),
    }
    app.set_compose_mode("reply".into());
}

/// The composer's recipient syntax: entries separated by commas, each
/// `Name <address>` or a bare address. Names lose the characters that syntax
/// gives meaning to.
fn recipient_field(mailboxes: &[Mailbox]) -> String {
    mailboxes
        .iter()
        .map(|mailbox| {
            let name = mailbox.name.as_deref().map(|name| {
                name.chars()
                    .filter(|c| !matches!(c, ',' | ';' | '<' | '>' | '"'))
                    .collect::<String>()
            });
            match name.as_deref().map(str::trim) {
                Some(name) if !name.is_empty() => format!("{name} <{}>", mailbox.email),
                _ => mailbox.email.clone(),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(desktop)]
    #[test]
    fn launch_links_come_only_from_mail_link_command_lines() {
        let links = |arguments: &[&str]| {
            launch_links(
                arguments
                    .iter()
                    .map(|argument| std::ffi::OsString::from(*argument)),
            )
        };
        assert_eq!(links(&[]), None);
        assert_eq!(links(&["--background"]), None);
        assert_eq!(
            links(&["--mailto", "mailto:a@example.com?subject=x"]),
            Some(vec!["mailto:a@example.com?subject=x".into()])
        );
        assert_eq!(
            links(&["MAILTO:a@example.com", "--verbose", "mailto:"]),
            Some(vec!["MAILTO:a@example.com".into(), "mailto:".into()])
        );
        // A link that escaped its quotes cannot add options or open anything.
        for arguments in [
            &["--mailto", "mailto:a@example.com", "--background", ""][..],
            &["--mailto", "mailto:a@example.com\\", "--background"],
            &["--background", "--mailto", "mailto:a@example.com"],
            &["--mailto", "https://example.com"],
            &["--mailto"],
        ] {
            assert_eq!(links(arguments), Some(Vec::new()), "{arguments:?}");
        }
        let many = vec!["mailto:a@example.com"; MAX_LAUNCH_LINKS + 1];
        assert_eq!(links(&many).unwrap().len(), MAX_LAUNCH_LINKS);
    }

    fn mailbox(name: Option<&str>, email: &str) -> Mailbox {
        Mailbox {
            name: name.map(str::to_owned),
            email: email.to_owned(),
        }
    }

    #[test]
    fn recipient_field_round_trips_through_the_composer_syntax() {
        let field = recipient_field(&[
            mailbox(Some("Doe, Jane"), "jane@example.com"),
            mailbox(None, "bob+list@example.com"),
            mailbox(Some(" <\"> "), "x@example.com"),
        ]);
        assert_eq!(
            field,
            "Doe Jane <jane@example.com>, bob+list@example.com, x@example.com"
        );
    }
}
