//! Files sent by link: uploaded to a storage service, such as a Nextcloud server, as they go
//! into the message, which carries links to them in place of the files. The links go in a
//! block after what's written, and a draft keeps each link's details in the block, to reopen
//! as the files it holds.

use super::link_block::{BlockWords, block_html, block_text, take_block};
use super::*;
use flectar_mail_core::files::progress::{self, Progress};
use flectar_mail_core::links::SharedLink;
use std::sync::Arc;

/// How often the files on their way say how far along they are.
const PROGRESS_EVERY: Duration = Duration::from_millis(400);
/// A file the message sends by link.
pub(super) struct LinkedFile {
    id: u64,
    pub(super) name: String,
    pub(super) size: u64,
    pub(super) state: LinkState,
    /// The picked document's copy, kept until its upload ends.
    _import: Option<documents::ImportedDocument>,
}

pub(super) enum LinkState {
    Uploading(Arc<Progress>),
    Shared(SharedLink),
    Failed(String),
}

impl Composers {
    /// Shows or hides Share files by link in every message, as a storage service was connected
    /// or removed.
    pub(crate) fn link_services_changed(&self) {
        for session in self.sessions.borrow().iter() {
            if let Some(host) = session.host() {
                host.ui().set_link_sharing(session.can_share_by_link());
            }
        }
    }
}

impl ComposeSession {
    /// Opens the file picker for files to send by link.
    pub(super) fn pick_link_files(self: &Rc<Self>) {
        let Some(composers) = self.composers() else {
            return;
        };
        let Some(app) = composers.app.upgrade() else {
            return;
        };
        if self.picking.replace(true) {
            return;
        }
        let dialog = documents::DocumentDialog::titled(translated(
            &app,
            &UiMessage::plain("Share files by link"),
        ));
        let request = composers
            .documents
            .import_many(MAX_COMPOSE_ATTACHMENT_FILES, dialog);
        let weak = Rc::downgrade(self);
        let picked_in = self.generation.get();
        documents::run_request(&composers.runtime, request, move |imported| {
            let Some(session) = weak.upgrade() else {
                return;
            };
            session.picking.set(false);
            if session.generation.get() != picked_in {
                return;
            }
            match imported {
                Ok(documents) => session.share_by_link(
                    documents
                        .into_iter()
                        .map(|document| NewAttachment {
                            path: document.path().to_path_buf(),
                            filename: document.name(),
                            import: Some(document),
                        })
                        .collect(),
                ),
                Err(error) => {
                    if let Some(host) = session.host() {
                        host.ui().set_notice(
                            UiMessage::detail("Could not add attachments: {}", error),
                            true,
                        );
                    }
                }
            }
        });
    }

    /// Sends `files` by link from the storage service the message's account uses, uploading
    /// them now. Sending waits for them.
    pub(super) fn share_by_link(self: &Rc<Self>, files: Vec<NewAttachment>) {
        let (Some(composers), Some(host)) = (self.composers(), self.host()) else {
            return;
        };
        let ui = host.ui();
        let Some(core) = composers.core() else {
            return;
        };
        let Some(service) = self.link_service().map(|service| service.id) else {
            ui.set_notice(
                UiMessage::plain("Connect a storage service in Settings to send files by link."),
                true,
            );
            return;
        };
        for file in files {
            let Some(name) = file.filename.clone() else {
                continue;
            };
            let size = std::fs::metadata(&file.path)
                .map(|metadata| metadata.len())
                .unwrap_or_default();
            let id = self.next_link.get();
            self.next_link.set(id + 1);
            let observer = Arc::new(Progress::default());
            self.links.borrow_mut().push(LinkedFile {
                id,
                name: name.clone(),
                size,
                state: LinkState::Uploading(Arc::clone(&observer)),
                _import: file.import,
            });
            self.count_attaching(true);
            let core = core.clone();
            let path = file.path.clone();
            let request: documents::DocumentFuture<_> = Box::pin(async move {
                Ok(progress::track(observer, core.share_by_link(service, path, name)).await)
            });
            let weak = Rc::downgrade(self);
            let started_in = self.generation.get();
            documents::run_request(&composers.runtime, request, move |shared| {
                let Some(session) = weak.upgrade() else {
                    return;
                };
                if session.generation.get() != started_in {
                    // The message is gone: so is what went up for it.
                    if let Ok(Ok(link)) = shared {
                        session.unshare_later(link);
                    }
                    return;
                }
                session.count_attaching(false);
                session.link_finished(id, shared.and_then(|shared| shared));
            });
        }
        self.refresh_links();
        self.watch_link_progress();
    }

    /// Records how the upload of the file with `id` ended.
    fn link_finished(&self, id: u64, shared: Result<SharedLink, String>) {
        let mut notice = None;
        {
            let mut links = self.links.borrow_mut();
            let Some(file) = links.iter_mut().find(|file| file.id == id) else {
                // Removed while it went up: it comes back down.
                drop(links);
                if let Ok(link) = shared {
                    self.unshare_later(link);
                }
                return;
            };
            file.state = match shared {
                Ok(link) => {
                    if let Some(password) = &link.password {
                        notice = Some((
                            UiMessage::arguments(
                                "{} asks for the password {}. Pass it on another way.",
                                &file.name,
                                password,
                            ),
                            false,
                        ));
                    }
                    LinkState::Shared(link)
                }
                Err(error) => {
                    notice = Some((
                        UiMessage::arguments("{} couldn't go up: {}", &file.name, &error),
                        true,
                    ));
                    LinkState::Failed(error)
                }
            };
            file._import = None;
        }
        if let (Some((message, error)), Some(host)) = (notice, self.host()) {
            host.ui().set_notice(message, error);
        }
        self.refresh_links();
    }

    /// Takes back a link, and the file it shares, without waiting.
    fn unshare_later(&self, link: SharedLink) {
        let Some(composers) = self.composers() else {
            return;
        };
        let Some(core) = composers.core() else {
            return;
        };
        composers.runtime.spawn(async move {
            if let Err(error) = core.unshare_link(link).await {
                tracing::warn!(%error, "could not take back a link");
            }
        });
    }

    /// Drops the file at `index` among those sent by link, and takes its link back.
    pub(super) fn remove_link(&self, index: i32) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let removed = {
            let mut links = self.links.borrow_mut();
            (index < links.len()).then(|| links.remove(index))
        };
        let Some(file) = removed else {
            return;
        };
        match file.state {
            LinkState::Shared(link) => self.unshare_later(link),
            // The upload's end finds the file gone and takes its link back then.
            LinkState::Uploading(_) | LinkState::Failed(_) => {}
        }
        self.refresh_links();
    }

    /// Shows the files sent by link, and how far along those on their way are.
    pub(super) fn refresh_links(&self) {
        let (Some(composers), Some(host)) = (self.composers(), self.host()) else {
            return;
        };
        let Some(app) = composers.app.upgrade() else {
            return;
        };
        let formats = crate::date_format::current();
        let rows = self
            .links
            .borrow()
            .iter()
            .map(|file| {
                let size = crate::compose_controller::format_file_size(file.size);
                let (detail, failed) = match &file.state {
                    LinkState::Uploading(observer) => {
                        let snapshot = observer.snapshot();
                        let percent = (snapshot.done * 100)
                            .checked_div(snapshot.total)
                            .unwrap_or(0)
                            .min(100);
                        (
                            translated(&app, &UiMessage::detail("Uploading… {}%", percent)),
                            false,
                        )
                    }
                    // The link icon says it's a link: the chip says how big and until when.
                    LinkState::Shared(link) => {
                        let detail = match link.expires_on.as_deref().and_then(|date| {
                            chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
                        }) {
                            Some(date) => translated(
                                &app,
                                &UiMessage::arguments(
                                    "{} · until {}",
                                    &size,
                                    formats
                                        .date(crate::date_format::DateStyle::ShortDayMonth, date),
                                ),
                            ),
                            None => size.as_str().into(),
                        };
                        (detail, false)
                    }
                    LinkState::Failed(error) => (error.as_str().into(), true),
                };
                ComposeLink {
                    name: file.name.as_str().into(),
                    detail,
                    failed,
                }
            })
            .collect::<Vec<_>>();
        host.ui().set_links(ModelRc::new(VecModel::from(rows)));
    }

    /// Keeps the progress of files on their way up to date while any are.
    fn watch_link_progress(self: &Rc<Self>) {
        if self.link_timer.running() {
            return;
        }
        let weak = Rc::downgrade(self);
        self.link_timer
            .start(slint::TimerMode::Repeated, PROGRESS_EVERY, move || {
                let Some(session) = weak.upgrade() else {
                    return;
                };
                let uploading = session
                    .links
                    .borrow()
                    .iter()
                    .any(|file| matches!(file.state, LinkState::Uploading(_)));
                session.refresh_links();
                if !uploading {
                    session.link_timer.stop();
                }
            });
    }

    /// The first file that couldn't go up, which the message can't be sent with.
    pub(super) fn failed_link(&self) -> Option<String> {
        self.links
            .borrow()
            .iter()
            .find_map(|file| matches!(file.state, LinkState::Failed(_)).then(|| file.name.clone()))
    }

    /// The message's links as blocks for after what's written: HTML, kept in a draft with
    /// each link's details, and plain text.
    pub(super) fn link_blocks(&self, draft: bool) -> Option<(String, String)> {
        let composers = self.composers()?;
        let app = composers.app.upgrade()?;
        let links = self.links.borrow();
        let shared = links
            .iter()
            .filter_map(|file| match &file.state {
                LinkState::Shared(link) => Some(link),
                _ => None,
            })
            .collect::<Vec<_>>();
        if shared.is_empty() {
            return None;
        }
        let words = BlockWords {
            header: translated(
                &app,
                &UiMessage::detail("{} files shared by link", shared.len()),
            )
            .to_string(),
            password: translated(&app, &UiMessage::plain("password protected")).to_string(),
        };
        let formats = crate::date_format::current();
        let until = |date: &str| {
            let date = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .map(|date| formats.date(crate::date_format::DateStyle::Medium, date))
                .unwrap_or_else(|_| date.to_owned());
            translated(&app, &UiMessage::detail("available until {}", date)).to_string()
        };
        Some((
            block_html(&shared, &words, until, draft),
            block_text(&shared, &words, until),
        ))
    }

    /// Takes a draft's block of links out of `html`, as the files it holds.
    pub(super) fn restore_links(&self, html: &str) -> String {
        let (html, restored) = take_block(html);
        if restored.is_empty() {
            return html;
        }
        {
            let mut links = self.links.borrow_mut();
            for link in restored {
                let id = self.next_link.get();
                self.next_link.set(id + 1);
                links.push(LinkedFile {
                    id,
                    name: link.name.clone(),
                    size: link.size,
                    state: LinkState::Shared(link),
                    _import: None,
                });
            }
        }
        self.refresh_links();
        html
    }
}
