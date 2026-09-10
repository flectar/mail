//! Mail attachment metadata and an independent dialog session. Files and Mail
//! share bounded decoders/PDF workers; opening Mail never changes Files state.
use crate::document_preview::{Preview, preview};
use crate::{
    AppWindow, EmailReader, InboxState, MailAttachment, MailAttachments, mail::MailMessage,
};
use slint::{ComponentHandle, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

#[cfg(any(target_os = "android", target_os = "ios"))]
static SUSPEND: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> = std::sync::OnceLock::new();
#[cfg(any(target_os = "android", target_os = "ios"))]
pub(crate) fn suspend() {
    if let Some(suspend) = SUSPEND.get() {
        suspend();
    }
}

pub(crate) fn clear(app: &AppWindow) {
    let ui = app.global::<MailAttachments>();
    ui.invoke_command("close".into(), "".into());
    ui.set_rows(ModelRc::default());
}
pub(crate) fn project(app: &AppWindow, email: &MailMessage, same: bool) {
    if !same {
        clear(app);
    }
    let rows = attachment_rows(&email.attachments);
    app.global::<MailAttachments>()
        .set_rows(ModelRc::new(VecModel::from(rows)));
}
fn attachment_rows(
    attachments: &[flectar_mail_core::models::AttachmentMeta],
) -> Vec<MailAttachment> {
    attachments
        .iter()
        .filter(|a| {
            !a.is_inline
                || a.filename
                    .as_ref()
                    .is_some_and(|name| !name.trim().is_empty())
        })
        .map(|a| {
            let name = a
                .filename
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "attachment".into());
            let media = a
                .mime_type
                .as_deref()
                .unwrap_or("")
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            MailAttachment {
                id: a.id.to_string().into(),
                name: name.clone().into(),
                detail: media.clone().into(),
                previewable: media == "application/pdf"
                    || media.starts_with("image/")
                    || media.starts_with("text/")
                    || matches!(media.as_str(), "application/json" | "application/xml")
                    || name.to_ascii_lowercase().ends_with(".pdf"),
            }
        })
        .collect::<Vec<_>>()
}

struct Session {
    task: Option<tokio::task::AbortHandle>,
    document: Option<Arc<Vec<u8>>>,
    selected: Option<flectar_mail_core::models::AttachmentMeta>,
}
impl Session {
    fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.document = None;
        self.selected = None;
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

fn close(ui: &MailAttachments<'_>, generation: &AtomicU64, session: &RefCell<Session>) {
    generation.fetch_add(1, Ordering::SeqCst);
    session.borrow_mut().stop();
    ui.set_open(false);
    ui.set_busy(false);
    ui.set_image(Default::default());
    ui.set_text("".into());
    ui.set_status("".into());
    ui.set_pages(0);
    ui.set_is_image(false);
}

pub(crate) fn register(
    app: &AppWindow,
    inbox: &Rc<RefCell<InboxState>>,
    runtime: &Rc<tokio::runtime::Runtime>,
    documents: Arc<dyn crate::documents::DocumentProvider>,
) {
    let session = Rc::new(RefCell::new(Session {
        task: None,
        document: None,
        selected: None,
    }));
    let generation = Arc::new(AtomicU64::new(0));
    let weak = app.as_weak();
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let weak = weak.clone();
        let _ = SUSPEND.set(Box::new(move || {
            let _ = weak.upgrade_in_event_loop(|app| {
                app.global::<MailAttachments>()
                    .invoke_command("close".into(), "".into())
            });
        }));
    }
    // Profile IDs are not globally unique: compare Core identity as well as
    // message identity before allowing an old session to remain actionable.
    let context = Rc::new(RefCell::new(None::<(Arc<flectar_mail_core::Core>, i32)>));
    let timer = slint::Timer::default();
    let state = inbox.clone();
    let observed = context.clone();
    let window = weak.clone();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(100),
        move || {
            let Some(app) = window.upgrade() else {
                return;
            };
            let current = state.borrow().core.as_ref().map(|core| {
                (
                    core.file_core(),
                    app.global::<EmailReader>().get_message_id(),
                )
            });
            let changed = match (&*observed.borrow(), &current) {
                (Some((a, id)), Some((b, next))) => !Arc::ptr_eq(a, b) || id != next,
                (None, None) => false,
                _ => true,
            };
            if changed {
                app.global::<MailAttachments>()
                    .invoke_command("close".into(), "".into());
                *observed.borrow_mut() = current;
            }
        },
    );
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<(
        u64,
        i32,
        Arc<flectar_mail_core::Core>,
        Result<Option<Preview>, String>,
    )>(2);
    let state = inbox.clone();
    let session_for_delivery = session.clone();
    let current = generation.clone();
    let window = weak.clone();
    app.global::<MailAttachments>().on_deliver(move || {
        let Some(app) = window.upgrade() else {
            return;
        };
        let session = &session_for_delivery;
        while let Ok((revision, message, core, result)) = receiver.try_recv() {
            if current.load(Ordering::SeqCst) != revision
                || app.global::<EmailReader>().get_message_id() != message
            {
                continue;
            }

            if !state
                .borrow()
                .core
                .as_ref()
                .is_some_and(|c| Arc::ptr_eq(&core, &c.file_core()))
            {
                continue;
            }

            let ui = app.global::<MailAttachments>();
            ui.set_busy(false);
            match result {
                Ok(Some(Preview::Pdf(data, page))) => {
                    session.borrow_mut().document = Some(data);
                    ui.set_page(page.index as i32);
                    ui.set_pages(page.count as i32);
                    ui.set_image(slint::Image::from_rgba8(
                        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                            &page.pixels,
                            page.width,
                            page.height,
                        ),
                    ));
                    ui.set_is_image(true);
                }
                Ok(Some(Preview::Image(data, w, h))) => {
                    ui.set_image(slint::Image::from_rgba8(
                        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&data, w, h),
                    ));
                    ui.set_is_image(true);
                }
                Ok(Some(Preview::Text(text))) => ui.set_text(text.into()),
                Ok(None) => close(&ui, &current, session),
                Err(error) => ui.set_status(error.into()),
            }
        }
    });
    let state = inbox.clone();
    let handle = runtime.handle().clone();
    app.global::<MailAttachments>()
        .on_command(move |action, value| {
            let _keep_timer = &timer;
            let Some(app) = weak.upgrade() else {
                return;
            };
            let ui = app.global::<MailAttachments>();
            if action == "close" {
                close(&ui, &generation, &session);
                return;
            }
            if ui.get_busy() {
                return;
            }
            let Some(core) = state.borrow().core.as_ref().map(|core| core.file_core()) else {
                return;
            };
            let message = app.global::<EmailReader>().get_message_id();
            // A profile switch may happen before the lifecycle timer's next
            // tick. Never use a retained attachment ID with a different Core.
            if (action == "page" || action == "download-current")
                && !context
                    .borrow()
                    .as_ref()
                    .is_some_and(|(owner, id)| Arc::ptr_eq(owner, &core) && *id == message)
            {
                close(&ui, &generation, &session);
                return;
            }
            let selected = if action == "page" || action == "download-current" {
                session.borrow().selected.clone()
            } else {
                value.parse::<i64>().ok().and_then(|id| {
                    state
                        .borrow()
                        .messages
                        .iter()
                        .find(|m| m.id == message)
                        .and_then(|m| m.attachments.iter().find(|a| a.id == id))
                        .cloned()
                })
            };
            let Some(selected) = selected else {
                return;
            };
            let page = value.parse::<u32>().unwrap_or(0);
            if action == "page" && page >= ui.get_pages().max(0) as u32 {
                return;
            }
            if !matches!(
                action.as_str(),
                "preview" | "download" | "download-current" | "page"
            ) {
                return;
            }
            let revision = generation.fetch_add(1, Ordering::SeqCst) + 1;
            let data = session.borrow().document.clone();
            session.borrow_mut().selected = Some(selected.clone());
            *context.borrow_mut() = Some((core.clone(), message));
            if action != "page" {
                ui.set_name(
                    selected
                        .filename
                        .clone()
                        .unwrap_or_else(|| "attachment".into())
                        .into(),
                );
                ui.set_pages(0);
                ui.set_image(Default::default());
                ui.set_text("".into());
                ui.set_is_image(false);
                ui.set_revision(ui.get_revision().wrapping_add(1));
                session.borrow_mut().document = None;
            }
            ui.set_open(true);
            ui.set_busy(true);
            ui.set_status("".into());
            let weak = weak.clone();
            let sender = sender.clone();
            let documents = documents.clone();
            let task = handle.spawn(async move {
                let result: Result<Option<Preview>, String> = async {
                    if action == "page" {
                        let data = data.ok_or("PDF preview has been closed")?;
                        return crate::pdf_preview::render(data.clone(), page, 200)
                            .await
                            .map(|page| Some(Preview::Pdf(data, page)));
                    }
                    let download = action == "download" || action == "download-current";
                    if !download && selected.size.is_some_and(|size| size > 16 * 1024 * 1024) {
                        return Err(
                            "This attachment is too large to preview. Download it to view locally."
                                .into(),
                        );
                    }
                    let path = core
                        .get_attachment(selected.id)
                        .await
                        .map_err(|e| e.to_string())?;
                    if download {
                        documents
                            .export(
                                path.into(),
                                crate::documents::safe_name(
                                    selected.filename.as_deref().unwrap_or("attachment"),
                                ),
                            )
                            .await?;
                        return Ok(None);
                    }
                    use tokio::io::AsyncReadExt;
                    let file = tokio::fs::File::open(path)
                        .await
                        .map_err(|e| e.to_string())?;
                    let mut bytes = Vec::new();
                    file.take(16 * 1024 * 1024 + 1)
                        .read_to_end(&mut bytes)
                        .await
                        .map_err(|e| e.to_string())?;
                    if bytes.len() > 16 * 1024 * 1024 {
                        return Err(
                            "This attachment is too large to preview. Download it to view locally."
                                .into(),
                        );
                    }
                    preview(
                        bytes,
                        selected
                            .mime_type
                            .as_deref()
                            .unwrap_or("application/octet-stream"),
                    )
                    .await
                    .map(Some)
                }
                .await;
                if sender.send((revision, message, core, result)).await.is_ok() {
                    let _ = weak.upgrade_in_event_loop(|app| {
                        app.global::<MailAttachments>().invoke_deliver()
                    });
                }
            });
            session.borrow_mut().task = Some(task.abort_handle());
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attachment_rows_preserve_large_ids_and_offer_safe_previews() {
        use flectar_mail_core::models::AttachmentMeta;
        let files = vec![
            AttachmentMeta {
                id: i64::MAX,
                filename: Some("Report.PDF".into()),
                mime_type: None,
                size: Some(1024),
                is_inline: false,
            },
            AttachmentMeta {
                id: 2,
                filename: Some("photo.png".into()),
                mime_type: Some("image/png".into()),
                size: Some(42),
                is_inline: false,
            },
            AttachmentMeta {
                id: 3,
                filename: None,
                mime_type: Some("image/png".into()),
                size: Some(42),
                is_inline: true,
            },
            AttachmentMeta {
                id: 4,
                filename: Some("archive.zip".into()),
                mime_type: Some("application/zip".into()),
                size: Some(42),
                is_inline: false,
            },
        ];
        let rows = attachment_rows(&files);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].id.as_str(), i64::MAX.to_string());
        assert!(rows[0].previewable && rows[1].previewable);
        assert!(!rows[2].previewable);
    }
}
