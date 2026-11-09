//! Opening mailto links, whether clicked in a message or handed over by the
//! system when Flectar Mail is the default email app.
//!
//! Links wait until the mail store is ready and an account can send, then
//! each opens the main window's composer pre-filled for the user to review;
//! nothing is sent on a link's behalf. That composer holds one message, so a
//! link never replaces what the user is writing: a blank composer is reused,
//! anything else is saved to Drafts first. Messages moved into windows of
//! their own are left alone.

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

/// How long a link waits for the open message's sender identity, which
/// loads just after the composer opens, before saving that message anyway.
const IDENTITY_RETRY: Duration = Duration::from_millis(100);
const IDENTITY_RETRIES: u32 = 50;

#[derive(Default)]
struct Links {
    pending: VecDeque<Mailto>,
    composers: Option<Rc<composer::Composers>>,
    scheduled: bool,
    /// Retries spent waiting for the open message's sender identity.
    identity_waits: u32,
}

thread_local! {
    static LINKS: RefCell<Links> = RefCell::default();
}

/// Connect the composers. Links received before this wait for them.
pub(super) fn install(app: &AppWindow, composers: Rc<composer::Composers>) {
    LINKS.with(|links| links.borrow_mut().composers = Some(composers));
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
    while let Some((message, composers)) = LINKS.with(|links| {
        let links = links.borrow();
        Some((links.pending.front()?.clone(), links.composers.clone()?))
    }) {
        if !app.get_startup_ready() {
            return;
        }
        if !composers.can_send() {
            // Without an account to send from, a link is dropped, as
            // Thunderbird does, rather than opening much later.
            LINKS.with(|links| links.borrow_mut().pending.clear());
            app.set_sync_status(UiMessage::plain("Add an account to send mail."));
            return;
        }
        let saved_previous = match make_room(app, &composers) {
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
        if !compose(app, &composers, &message, saved_previous) {
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
fn make_room(app: &AppWindow, composers: &Rc<composer::Composers>) -> Room {
    let Some(session) = composers.in_app() else {
        return Room::Free;
    };
    let Some(host) = session.host() else {
        return Room::Free;
    };
    let ui = host.ui();
    if ui.get_sending() {
        return Room::Busy;
    }
    if session.is_blank() {
        ui.invoke_close();
        return if ui.get_open() {
            Room::Busy
        } else {
            Room::Free
        };
    }
    let identity_loading = host.preferences().get_composer_sender_email().is_empty();
    if identity_loading && LINKS.with(|links| links.borrow().identity_waits) < IDENTITY_RETRIES {
        return Room::IdentityLoading;
    }
    ui.invoke_save(
        ui.get_account_id(),
        ui.get_to(),
        ui.get_cc(),
        ui.get_bcc(),
        ui.get_subject(),
        ui.get_body(),
        false,
    );
    if ui.get_open() {
        app.set_sync_status(UiMessage::plain(
            "The email link will open when you close this message.",
        ));
        return Room::Busy;
    }
    Room::Saved
}

fn compose(
    app: &AppWindow,
    composers: &Rc<composer::Composers>,
    message: &Mailto,
    saved_previous: bool,
) -> bool {
    let Some(session) = composers.open_new(app, "") else {
        return false;
    };
    let Some(host) = session.host() else {
        return false;
    };
    let ui = host.ui();
    ui.set_to(recipient_field(&message.to).into());
    ui.set_cc(recipient_field(&message.cc).into());
    ui.set_bcc(recipient_field(&message.bcc).into());
    if let Some(subject) = &message.subject {
        ui.set_subject(subject.into());
    }
    if let Some(body) = &message.body {
        session.load_text(body);
    }
    if let Some(header_id) = &message.in_reply_to {
        // Make the message a reply to the one the link names. When the
        // account it is written from has that message, the reply threads
        // with it like any other; otherwise its Message-ID still goes into
        // In-Reply-To and References, as a mailing list archive's reply link
        // expects. The sending account is never changed for a link.
        let parent = composers.message_by_header_id(i64::from(ui.get_account_id()), header_id);
        let mut intent = session.intent.borrow_mut();
        intent.mode = "reply".to_owned();
        match parent {
            Some(parent) => intent.in_reply_to_message_id = Some(parent),
            None => intent.in_reply_to_header = Some(header_id.clone()),
        }
        ui.set_mode("reply".into());
    }
    if !message.attachment_requests.is_empty() {
        ui.set_notice(
            UiMessage::detail(
                "Files from links are never attached for you. Attach them yourself if you trust them: {}",
                message.attachment_requests.join(", "),
            ),
            false,
        );
    } else if saved_previous {
        ui.set_notice(
            UiMessage::plain("The message you were writing was saved to Drafts."),
            false,
        );
    }
    true
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
