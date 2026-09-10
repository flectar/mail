//! Files owns one serialized async session. UI callbacks only capture input and
//! schedule work; protocol, database, dialogs and disk I/O never block Slint.
use crate::document_preview::{Preview, preview};
use crate::documents::safe_name;
use crate::{AppWindow, FileRow, FilesUi, InboxState, OperationRow};
use flectar_mail_core::files::{
    self, FileClient,
    service::{
        Entry, FilesService as Directory, Output as ServiceOutput, Preview as ServicePreview,
    },
};
use slint::{ComponentHandle, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use std::{cell::RefCell, rc::Rc, sync::Arc};
use tokio::sync::Mutex;

struct SyncWorker(tokio::task::JoinHandle<()>);
impl Drop for SyncWorker {
    fn drop(&mut self) {
        self.0.abort();
    }
}
#[cfg(any(target_os = "android", target_os = "ios"))]
static SUSPEND: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> = std::sync::OnceLock::new();

#[cfg(any(target_os = "android", target_os = "ios"))]
pub(crate) fn suspend() {
    crate::pdf_preview::cancel_active();
    if let Some(suspend) = SUSPEND.get() {
        suspend();
    }
}

pub(crate) fn register(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &Rc<tokio::runtime::Runtime>,
    documents: Arc<dyn crate::documents::DocumentProvider>,
    scratch_root: std::path::PathBuf,
) {
    app.global::<FilesUi>().set_pdf_supported(true);
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let weak = app.as_weak();
        let _ = SUSPEND.set(Box::new(move || {
            let _ = weak.upgrade_in_event_loop(|app| {
                let ui = app.global::<FilesUi>();
                ui.invoke_command("release-preview".into(), "".into(), "".into());
                ui.set_preview_open(false);
            });
        }));
        #[cfg(target_os = "ios")]
        unsafe {
            install_pdf_lifecycle_observers();
        }
    }
    let directory = Arc::new(Mutex::new((
        (0usize, 0u64),
        Directory {
            attachments: true,
            ..Default::default()
        },
    )));
    let pdf_document = Arc::new(std::sync::Mutex::new(None::<Arc<Vec<u8>>>));
    let profile_pdf = pdf_document.clone();
    let weak = app.as_weak();
    let inbox = Rc::clone(state);
    let handle = runtime.handle().clone();
    let worker = Rc::new(RefCell::new(
        None::<(Arc<flectar_mail_core::Core>, SyncWorker)>,
    ));
    let preview_pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pending = Rc::new(RefCell::new(None::<tokio::task::AbortHandle>));
    let generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let account_epoch = Rc::new(std::cell::Cell::new(0_u64));
    let account_state = state.clone();
    let account_pending = pending.clone();
    let account_generation = generation.clone();
    let account_pdf = pdf_document.clone();
    let epoch = account_epoch.clone();
    let previous_accounts = Rc::new(RefCell::new(Vec::<i64>::new()));
    let window = app.as_weak();
    app.global::<FilesUi>().on_context_changed(move || {
        let ids = account_state.borrow().connected_accounts.iter().map(|a| a.id).collect::<Vec<_>>();
        if *previous_accounts.borrow() == ids { return; }
        *previous_accounts.borrow_mut() = ids;
        epoch.set(epoch.get().wrapping_add(1));
        account_generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(task) = account_pending.borrow_mut().take() { task.abort(); }
        *account_pdf.lock().unwrap() = None;
        if let Some(app) = window.upgrade() { reset_profile(&app.global::<FilesUi>()); }
    });
    let profile_pending = pending.clone();
    let profile_generation = generation.clone();
    let profile_ui = weak.clone();
    let worker_timer = slint::Timer::default();
    let worker_state = inbox.clone();
    let worker_slot = worker.clone();
    let worker_runtime = handle.clone();
    worker_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(500),
        move || {
            let core = worker_state.borrow().core.as_ref().map(|c| c.file_core());
            if let Some(core) = core {
                if worker_slot
                    .borrow()
                    .as_ref()
                    .is_none_or(|(prior, _)| !Arc::ptr_eq(prior, &core))
                {
                    profile_generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if let Some(task) = profile_pending.borrow_mut().take() {
                        task.abort();
                    }
                    if let Some(app) = profile_ui.upgrade() {
                        reset_profile(&app.global::<FilesUi>());
                        *profile_pdf.lock().unwrap() = None;
                    }
                    *worker_slot.borrow_mut() = Some((
                        core.clone(),
                        SyncWorker(worker_runtime.spawn(files::sync::run((*core).clone()))),
                    ));
                }
            } else if worker_slot.borrow_mut().take().is_some() {
                profile_generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if let Some(task) = profile_pending.borrow_mut().take() {
                    task.abort();
                }
                if let Some(app) = profile_ui.upgrade() {
                    reset_profile(&app.global::<FilesUi>());
                    *profile_pdf.lock().unwrap() = None;
                }
            }
        },
    );
    let progress = Arc::new(files::progress::Progress::default());
    let progress_timer = slint::Timer::default();
    let observed = progress.clone();
    let progress_ui = weak.clone();
    progress_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(100),
        move || {
            if let Some(app) = progress_ui.upgrade() {
                let ui = app.global::<FilesUi>();
                if ui.get_busy() {
                    let p = observed.snapshot();
                    let phase = match p.phase {
                        1 => "Uploading",
                        2 => "Downloading",
                        3 => "Copying",
                        _ => "Working",
                    };
                    ui.set_transfer_status(
                        if p.phase == 0 {
                            String::new()
                        } else if p.total > 0 {
                            format!("{phase} · {} of {}", size(p.done), size(p.total))
                        } else {
                            format!("{phase} · {}", size(p.done))
                        }
                        .into(),
                    );
                }
            }
        },
    );

    app.global::<FilesUi>().on_command(move |action, a, b| {
        let _keep_worker_timer_alive = (&worker_timer,&progress_timer);
        let Some(app) = weak.upgrade() else {
            return;
        };
        let ui = app.global::<FilesUi>();
        if action.as_str() == "release-preview" {
            if preview_pending.swap(false, std::sync::atomic::Ordering::SeqCst) {
                generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if let Some(task) = pending.borrow_mut().take() { task.abort(); }
                ui.set_busy(false);
            }
            *pdf_document.lock().unwrap() = None;
            ui.set_pdf_pages(0);
            ui.set_preview_image(slint::Image::default());
            ui.set_preview_text("".into());
            return;
        }
        if action.as_str() == "cancel" {
            generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(task) = pending.borrow_mut().take() {
                task.abort();
            }
            ui.set_busy(false);
            if preview_pending.swap(false, std::sync::atomic::Ordering::SeqCst) {
                ui.set_status("".into());
                return;
            }
            ui.set_selected(-1);
            ui.set_can_create(false);
            ui.set_can_more(false);
            ui.set_status(
                "Cancelled. Refresh storage before retrying; a server change may have completed."
                    .into(),
            );
            return;
        }
        if ui.get_busy() {
            return;
        }
        let Some(core) = inbox.borrow().core.as_ref().map(|c| c.file_core()) else {
            ui.set_status("Mail is still starting. Try again shortly.".into());
            return;
        };
        let identity = (Arc::as_ptr(&core) as usize, account_epoch.get());
        if worker
            .borrow()
            .as_ref()
            .is_none_or(|(prior, _)| !Arc::ptr_eq(prior, &core))
        {
            *worker.borrow_mut() = Some((
                core.clone(),
                SyncWorker(handle.spawn(files::sync::run((*core).clone()))),
            ));
        }
        if matches!(action.as_str(), "scope" | "account" | "account-id" | "space" | "load" | "search" | "filter" | "refresh" | "enter" | "up" | "attachment-source") {
            ui.set_dialog("".into());
            ui.set_preview_open(false);
            ui.set_preview_image(slint::Image::default());
            ui.set_pdf_pages(0);
            *pdf_document.lock().unwrap() = None;
        }
        if matches!(action.as_str(), "preview" | "details" | "notifications" | "clear-notifications") {
            ui.set_preview_image(slint::Image::default());
            ui.set_preview_text("".into());
            ui.set_preview_is_image(false);
            ui.set_activity_open(matches!(action.as_str(), "notifications" | "clear-notifications"));
            ui.set_preview_open(true);
        }
        if matches!(action.as_str(), "preview" | "details" | "notifications" | "clear-notifications") {
            ui.set_pdf_pages(0);
            ui.set_pdf_zoom(100);
            *pdf_document.lock().unwrap() = None;
        }
        let pdf_source = pdf_document.lock().unwrap().clone();
        let pdf_document = pdf_document.clone();
        let selected = ui.get_selected();
        let collision = ui.get_collision();
        let case_insensitive = ui.get_case_insensitive();
        let action = action.to_string();
        let a = if action.as_str()=="filter" {
            let size = |s:slint::SharedString|->Option<u32> {s.parse().ok()};
            let min=ui.get_min_size();let max=ui.get_max_size();
            if (!min.is_empty()&&size(min.clone()).is_none())||(!max.is_empty()&&size(max.clone()).is_none()) {
                ui.set_status("Enter file sizes as whole bytes between 0 and 4294967295.".into());return;
            }
            serde_json::json!({"nodeType":match ui.get_file_kind(){1=>Some("file"),2=>Some("directory"),_=>None},"minSize":size(min),"maxSize":size(max),"sort":match ui.get_file_sort(){1=>"size",2=>"nodeType",_=>"name"},"descending":ui.get_descending()}).to_string()
        } else {a.to_string()};
        let b = b.to_string();
        let documents = documents.clone();
        let scratch_root = scratch_root.clone();
        let directory = Arc::clone(&directory);
        let weak = weak.clone();
        progress.reset();
        ui.set_transfer_status("".into());
        let progress=progress.clone();
        preview_pending.store(matches!(action.as_str(), "preview" | "pdf-page" | "details" | "notifications" | "clear-notifications"), std::sync::atomic::Ordering::SeqCst);
        let preview_pending = preview_pending.clone();
        ui.set_busy(true);
        ui.set_status("".into());
        let current_generation = generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let generation = Arc::clone(&generation);
        let task = handle.spawn(async move {
            let mut workspace = directory.lock().await;
            if workspace.0!=identity {workspace.0=identity;workspace.1=Directory{attachments:true,..Default::default()};}
            let dir=&mut workspace.1;
            dir.collision = match collision {
                1 => files::CollisionPolicy::Rename,
                2 => files::CollisionPolicy::Replace,
                3 => files::CollisionPolicy::Newest,
                _ => files::CollisionPolicy::Reject,
            };
            dir.case_insensitive = case_insensitive;
            let mut scratch = None;
            let selection = if action == "download" {
                let _ = tokio::fs::create_dir_all(&scratch_root).await;
                match tempfile::tempdir_in(&scratch_root) {
                    Ok(dir) => {
                        let path = dir.path().join("download");
                        scratch = Some(dir);
                        Ok(Some(crate::documents::ImportedDocument::source(path)))
                    }
                    Err(e) => Err(e.to_string()),
                }
            } else if matches!(action.as_str(), "upload" | "replace") {
                documents.import().await
            } else {
                Ok(None)
            };
            let export_path = selection.as_ref().ok().and_then(|v| v.as_ref()).map(|v| v.path().to_owned());
            let mut result = match selection {
                Err(error) => Err(error),
                Ok(None) if matches!(action.as_str(), "upload" | "replace") => {
                    Ok(ServiceOutput::Status("File selection cancelled.".into()))
                }
                Ok(_) if action == "pdf-page" => Ok(ServiceOutput::Status(String::new())),
                Ok(path) => files::progress::track(progress,dir.execute(&core, &action, &a, &b, selected, path.as_ref().map(|v| v.path().to_owned()))).await,
            };
            if action == "download" && result.is_ok()
                && let Some(path) = export_path {
                    let name = dir
                        .selected(selected)
                        .map(|e| match e {
                            Entry::Attachment(a) => a.filename,
                            Entry::Remote(n) => n.name,
                        })
                        .unwrap_or_else(|_| "download".into());
                    result = documents.export(path, safe_name(&name)).await.map(|saved| {
                        ServiceOutput::Status(
                            if saved {
                                "File downloaded."
                            } else {
                                "Export cancelled; the cached file is still available."
                            }
                            .into(),
                        )
                    });
                }
            drop(scratch);
            let result = if action == "pdf-page" {
                match (pdf_source, a.parse::<u32>(), b.parse::<u32>()) {
                    (Some(data), Ok(index), Ok(zoom)) => crate::pdf_preview::render(data.clone(), index, zoom).await
                        .map(|page| Output::Preview(Preview::Pdf(data, page))),
                    _ => Err("Select a PDF to preview.".into()),
                }
            } else { match result {
                Ok(ServiceOutput::Operations(rows)) => Ok(Output::Operations(rows)),
                Ok(ServiceOutput::Status(s)) => Ok(Output::Status(s)),
                Ok(ServiceOutput::Preview(ServicePreview::Text(s))) => {
                    Ok(Output::Preview(Preview::Text(s)))
                }
                Ok(ServiceOutput::Preview(ServicePreview::Data(data, media))) => {
                    preview(data, &media).await.map(Output::Preview)
                }
                Err(e) => Err(e),
            }};
            let snapshot = Snapshot::new(dir);
            let reset_selection = !matches!(
                action.as_str(),
                "preview"
                    | "pdf-page"
                    | "details"
                    | "download"
                    | "more"
                    | "notifications"
                    | "clear-notifications"
            );
            let _ = weak.upgrade_in_event_loop(move |app| {
                if generation.load(std::sync::atomic::Ordering::SeqCst) != current_generation {
                    return;
                }
                let ui = app.global::<FilesUi>();
                snapshot.apply(&ui);
                if action == "connect" && result.is_ok() { ui.set_connection_revision(ui.get_connection_revision().wrapping_add(1)); }
                if action == "more" || result.is_ok() {
                    ui.set_pagination_failed(action == "more" && result.is_err());
                }
                if result.is_ok() && !matches!(action.as_str(), "preview" | "pdf-page" | "details" | "download" | "notifications" | "clear-notifications" | "transfers") {
                    ui.set_content_revision(ui.get_content_revision().wrapping_add(1));
                }
                if reset_selection {
                    ui.set_selected(-1);
                }
                match result {
                    Ok(Output::Operations(rows)) => {
                        ui.set_operations(ModelRc::new(VecModel::from(
                            rows.into_iter()
                                .map(|r| OperationRow {
                                    id: r.id.to_string().into(),
                                    action: format!("#{} · {} · {}",r.id,r.action,r.description).into(),
                                    state: r.state.into(),
                                    error: r.error.into(),
                                    bytes: if r.bytes>0 {size(r.bytes).into()}else{slint::SharedString::default()},
                                })
                                .collect::<Vec<_>>(),
                        )));
                        ui.set_transfers_open(true);
                    }
                    Ok(Output::Preview(preview)) => {
                        ui.set_activity_open(matches!(
                            action.as_str(),
                            "notifications" | "clear-notifications"
                        ));
                        match preview {
                            Preview::Text(text) => {
                                ui.set_preview_text(text.into());
                                ui.set_preview_is_image(false);
                            }
                            Preview::Pdf(data, page) => {
                                *pdf_document.lock().unwrap() = Some(data);
                                ui.set_pdf_page(page.index as i32);
                                ui.set_pdf_pages(page.count as i32);
                                let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&page.pixels, page.width, page.height);
                                ui.set_preview_image(slint::Image::from_rgba8(buffer));
                                ui.set_preview_is_image(true);
                            }
                            Preview::Image(data, w, h) => {
                                let buffer =
                                    SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&data, w, h);
                                ui.set_preview_image(slint::Image::from_rgba8(buffer));
                                ui.set_preview_is_image(true);
                            }
                        }
                        ui.set_preview_open(true);
                    }
                    Ok(Output::Status(message)) => ui.set_status(message.into()),
                    Err(error) => ui.set_status(error.into()),
                }
                preview_pending.store(false, std::sync::atomic::Ordering::SeqCst);
                ui.set_busy(false);
            });
        });
        *pending.borrow_mut() = Some(task.abort_handle());
    });
}
enum Output {
    Operations(Vec<files::store::Operation>),
    Status(String),
    Preview(Preview),
}

fn reset_profile(ui: &FilesUi) {
    ui.set_rows(ModelRc::new(VecModel::from(Vec::<FileRow>::new())));
    ui.set_operations(ModelRc::new(VecModel::from(Vec::<OperationRow>::new())));
    ui.set_accounts(ModelRc::new(VecModel::from(vec![
        slint::SharedString::from("All accounts"),
    ])));
    ui.set_storage_accounts(ModelRc::new(VecModel::from(
        Vec::<slint::SharedString>::new(),
    )));
    ui.set_account(0);
    ui.set_selected_account_id(-1);
    ui.set_storage_account(0);
    ui.set_selected(-1);
    ui.set_pdf_pages(0);
    ui.set_pdf_zoom(100);
    ui.set_pagination_failed(false);
    ui.set_busy(false);
    ui.set_connected(false);
    ui.set_can_create(false);
    ui.set_can_more(false);
    ui.set_can_up(false);
    ui.set_preview_open(false);
    ui.set_preview_text("".into());
    ui.set_preview_image(slint::Image::default());
    ui.set_dialog("".into());
    ui.set_transfers_open(false);
    ui.set_attachments(true);
    ui.set_server_attachments(false);
    ui.set_query("".into());
    ui.set_endpoint("".into());
    ui.set_quota("".into());
    ui.set_status("".into());
}
fn size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024. * 1024.))
    }
}

struct Snapshot {
    rows: Vec<FileRow>,
    accounts: Vec<String>,
    account: i32,
    account_id: i32,
    spaces: Vec<String>,
    space: i32,
    attachments: bool,
    connected: bool,
    endpoint: String,
    webdav: bool,
    can_create: bool,
    can_lock: bool,
    can_more: bool,
    can_up: bool,
    path: String,
    query: String,
    quota: String,
}
impl Snapshot {
    fn new(dir: &Directory) -> Self {
        let rows = dir
            .entries
            .iter()
            .map(|entry| match entry {
                Entry::Attachment(a) => FileRow {
                    identity: match &a.remote {
                        Some(remote) => format!(
                            "mail:{}:{}:{}:{}",
                            a.account_id, remote.account, remote.email, remote.blob
                        ),
                        None => format!("cached:{}:{}", a.account_id, a.id),
                    }
                    .into(),
                    name: a.filename.clone().into(),
                    detail: format!("{} · {}", a.sender, a.subject).into(),
                    size: size(a.size).into(),
                    date: chrono::DateTime::from_timestamp_millis(a.date)
                        .map(|d| d.format("%b %d, %Y").to_string())
                        .unwrap_or_default()
                        .into(),
                    attachment: true,
                    readable: true,
                    ..Default::default()
                },
                Entry::Remote(n) => FileRow {
                    identity: format!("storage:{}:{}:{}", dir.account, dir.settings.endpoint, n.id)
                        .into(),
                    created: n.created.clone().unwrap_or_default().into(),
                    modified: n.modified.clone().unwrap_or_default().into(),
                    accessed: n.accessed.clone().unwrap_or_default().into(),
                    executable: n.executable,
                    subscribed: n.is_subscribed.unwrap_or(true),
                    name: n.name.clone().into(),
                    detail: if n.is_directory() {
                        "Folder".into()
                    } else {
                        n.media_type.clone().unwrap_or_default().into()
                    },
                    size: n.size.map(size).unwrap_or_default().into(),
                    date: n
                        .modified
                        .as_deref()
                        .unwrap_or("")
                        .chars()
                        .take(10)
                        .collect::<String>()
                        .into(),
                    directory: n.is_directory(),
                    readable: n.my_rights.may_read,
                    writable: n.my_rights.modify(),
                    renameable: n.my_rights.rename(),
                    deletable: n.my_rights.delete(),
                    shareable: n.my_rights.may_share,
                    locked: n.locked,
                    owns_lock: matches!(&dir.client,Some(FileClient::Dav(c)) if c.owns_lock(&n.id)),
                    ..Default::default()
                },
            })
            .collect();
        let (spaces, space) = if dir.offline {
            (
                dir.cached_spaces
                    .iter()
                    .map(|(_, name, _)| name.clone())
                    .collect(),
                dir.cached_spaces
                    .iter()
                    .position(|(id, _, _)| Some(*id) == dir.cached_space)
                    .unwrap_or(0) as i32,
            )
        } else {
            match &dir.client {
                Some(FileClient::Jmap(c)) => (
                    c.accounts.iter().map(|a| a.name.clone()).collect(),
                    c.accounts
                        .iter()
                        .position(|a| a.id == c.account_id)
                        .unwrap_or(0) as i32,
                ),
                _ => (Vec::new(), 0),
            }
        };
        Self {
            rows,
            accounts: std::iter::once("All accounts".into())
                .chain(dir.accounts.iter().map(|a| a.1.clone()))
                .collect(),
            account: dir.account as i32,
            account_id: dir
                .account_id()
                .and_then(|id| i32::try_from(id).ok())
                .unwrap_or(-1),
            spaces,
            space,
            attachments: dir.attachments,
            connected: dir.client.is_some() || dir.cached_space.is_some(),
            endpoint: dir.settings.endpoint.clone(),
            webdav: dir.settings.webdav,
            can_create: dir.can_create,
            can_lock: matches!(&dir.client,Some(FileClient::Dav(c)) if c.supports_locks),
            can_more: dir.next.is_some(),
            can_up: !dir.history.is_empty(),
            path: if dir.attachments {
                "Files from your email, in one place".into()
            } else {
                format!(
                    "/{}",
                    dir.history
                        .iter()
                        .map(|n| n.name.as_str())
                        .collect::<Vec<_>>()
                        .join(" / ")
                )
            },
            query: dir.query.clone(),
            quota: dir.quota.clone(),
        }
    }
    fn apply(self, ui: &FilesUi) {
        // Reuse Mail's reconciliation so appending pages retains the viewport
        // and existing row delegates instead of rebuilding the entire list.
        let retained = ui.get_rows();
        if let Some(model) = retained.as_any().downcast_ref::<VecModel<FileRow>>() {
            crate::reconcile_model_rows_by(
                model,
                self.rows,
                |row| row.identity.clone(),
                PartialEq::eq,
            );
        } else {
            ui.set_rows(ModelRc::new(VecModel::from(self.rows)));
        }
        ui.set_accounts(ModelRc::new(VecModel::from(
            self.accounts
                .into_iter()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )));
        ui.set_account(self.account);
        ui.set_selected_account_id(self.account_id);
        ui.set_storage_accounts(ModelRc::new(VecModel::from(
            self.spaces.into_iter().map(Into::into).collect::<Vec<_>>(),
        )));
        ui.set_storage_account(self.space);
        ui.set_attachments(self.attachments);
        ui.set_connected(self.connected);
        ui.set_endpoint(self.endpoint.into());
        ui.set_webdav(self.webdav);
        ui.set_can_create(self.can_create);
        ui.set_can_lock(self.can_lock);
        ui.set_can_more(self.can_more);
        ui.set_can_up(self.can_up);
        ui.set_path(self.path.into());
        ui.set_query(self.query.into());
        ui.set_quota(self.quota.into());
    }
}

#[cfg(target_os = "ios")]
unsafe extern "C" {
    fn install_pdf_lifecycle_observers();
}
