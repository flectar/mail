//! Report issue in About email rendering. It opens the GitHub form for
//! rendering problems with this copy's version and reader settings filled in
//! and hands over the email's HTML for the reporter to review and include,
//! the way Zed and VS Code prefill their bug reports.
use super::*;
use crate::documents::{DocumentDialog, DocumentFuture, DocumentProvider};
use flectar_mail_core::Core;
use std::{io::Write, path::Path};

/// The form in .github/ISSUE_TEMPLATE/email-rendering.yml. GitHub fills a
/// form field from the query parameter named after the field's `id`.
const ISSUE_FORM: &str = "https://github.com/flectar/mail/issues/new?template=email-rendering.yml";

/// GitHub rejects issues longer than 65,536 characters. HTML up to this
/// length goes on the clipboard and leaves room for the rest of the form.
/// Longer HTML is saved as a file to attach, as VS Code does with data too
/// long for an issue.
const PASTE_LIMIT: usize = 60_000;

/// Attachments show their file name on the issue, so it never carries the
/// subject.
const FILE_NAME: &str = "email.html";

/// `core` returns the open mail data, whose settings choose the folder a
/// saved file starts in.
pub(super) fn register(
    app: &AppWindow,
    core: impl Fn() -> Option<Arc<Core>> + 'static,
    runtime: &tokio::runtime::Handle,
    documents: Arc<dyn DocumentProvider>,
    temp_dir: PathBuf,
    opener: ExternalUrlOpener,
) {
    let weak = app.as_weak();
    let runtime = runtime.clone();
    let saving = Rc::new(Cell::new(false));
    app.on_report_rendering_issue(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let url = issue_url(&Environment::from_app(&app).describe());
        let html = app.get_selected_source();
        if fits_in_issue(&html) {
            if !html.is_empty() {
                crate::renderer_input_controller::copy_text(&app, html.into());
                app.set_render_status(UiMessage::plain(
                    "Copied HTML source. Paste it into the issue.",
                ));
            }
            open_form(&app, &opener, &url);
            return;
        }
        // The browser would cover a save dialog opened after it, so the form
        // opens once the file is saved.
        if saving.replace(true) {
            return;
        }
        let request = save_html(
            core(),
            Arc::clone(&documents),
            temp_dir.clone(),
            html.into(),
        );
        let weak = app.as_weak();
        let opener = opener.clone();
        let saving = Rc::clone(&saving);
        crate::documents::run_request(&runtime, request, move |result| {
            saving.set(false);
            if let Some(app) = weak.upgrade() {
                finish_saving(&app, &opener, &url, result);
            }
        });
    });
}

fn open_form(app: &AppWindow, opener: &ExternalUrlOpener, url: &str) {
    if let Err(error) = opener.open_external(url) {
        app.set_render_status(UiMessage::detail("Could not open link: {}", error));
    }
}

fn fits_in_issue(html: &str) -> bool {
    html.chars().count() <= PASTE_LIMIT
}

/// Saves HTML too long for an issue as a file for the reporter to attach.
fn save_html(
    core: Option<Arc<Core>>,
    documents: Arc<dyn DocumentProvider>,
    temp_dir: PathBuf,
    html: String,
) -> DocumentFuture<bool> {
    Box::pin(async move {
        let file = tokio::task::spawn_blocking(move || stage(&temp_dir, &html))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        // Start in the download folder from Settings, as attachments do.
        let dialog = match core {
            Some(core) => crate::documents::download_dialog_for(&core).await,
            None => DocumentDialog::default(),
        };
        let dialog = DocumentDialog {
            title: "Save email HTML".into(),
            filter: Some(("HTML".into(), &["html"])),
            ..dialog
        };
        documents
            .export_with(file.to_path_buf(), FILE_NAME.into(), dialog)
            .await
    })
}

fn finish_saving(
    app: &AppWindow,
    opener: &ExternalUrlOpener,
    url: &str,
    result: Result<bool, String>,
) {
    match result {
        Ok(true) => app.set_render_status(UiMessage::plain(
            "Saved HTML source. Attach the file to the issue.",
        )),
        // Cancelled. The form still opens for a report without the HTML.
        Ok(false) => {}
        Err(error) => {
            app.set_render_status(UiMessage::detail("Could not save HTML source: {}", error))
        }
    }
    open_form(app, opener, url);
}

/// Writes the HTML below the cache directory, which every document provider
/// accepts as an export source. The file goes away when the path drops.
fn stage(temp_dir: &Path, html: &str) -> std::io::Result<tempfile::TempPath> {
    std::fs::create_dir_all(temp_dir)?;
    let mut file = tempfile::Builder::new()
        .prefix("email-")
        .suffix(".html")
        .tempfile_in(temp_dir)?;
    file.write_all(html.as_bytes())?;
    Ok(file.into_temp_path())
}

fn issue_url(environment: &str) -> String {
    let mut url = url::Url::parse(ISSUE_FORM).expect("the issue form address is valid");
    url.query_pairs_mut()
        .append_pair("environment", environment);
    url.into()
}

/// What a report says about this copy of the app and the reader's settings.
struct Environment {
    version: &'static str,
    package: Option<&'static str>,
    system: &'static str,
    architecture: &'static str,
    distribution: Option<String>,
    gpu: bool,
    zoom: f32,
    fit: bool,
    colors: &'static str,
    remote_images_blocked: bool,
}

impl Environment {
    fn from_app(app: &AppWindow) -> Self {
        let reader = app.global::<EmailReader>();
        let colors = app.global::<MessageColors>();
        Self {
            version: env!("CARGO_PKG_VERSION"),
            package: package(),
            system: system(),
            architecture: std::env::consts::ARCH,
            distribution: distribution(),
            gpu: app.global::<RendererSettings>().get_active() == "gpu",
            zoom: reader.get_zoom(),
            fit: reader.get_auto_fit(),
            colors: if colors.get_original() {
                "original"
            } else if colors.get_dark() {
                "dark"
            } else {
                "light"
            },
            remote_images_blocked: app.get_remote_images_blocked(),
        }
    }

    fn describe(&self) -> String {
        let package = self
            .package
            .map(|package| format!(" ({package})"))
            .unwrap_or_default();
        let distribution = self
            .distribution
            .as_ref()
            .map(|distribution| format!(" ({distribution})"))
            .unwrap_or_default();
        let renderer = if self.gpu { "GPU" } else { "CPU" };
        let fit = if self.fit { " (fit to width)" } else { "" };
        let images = if self.remote_images_blocked {
            ", remote images blocked"
        } else {
            ""
        };
        format!(
            "Flectar Mail {}{package}\n{} {}{distribution}\n{renderer} renderer, {:.0}% zoom{fit}, {} message colors{images}",
            self.version,
            self.system,
            self.architecture,
            self.zoom * 100.0,
            self.colors,
        )
    }
}

fn system() -> &'static str {
    #[cfg(mobile)]
    let name = crate::platform::NAME;
    #[cfg(desktop)]
    let name = std::env::consts::OS;
    match name {
        "linux" => "Linux",
        "windows" => "Windows",
        "macos" => "macOS",
        "android" => "Android",
        "ios" => "iOS",
        "ohos" => "OpenHarmony",
        other => other,
    }
}

/// Linux packages bring their own libraries and sandboxes, which change how
/// links, fonts and files behave.
fn package() -> Option<&'static str> {
    if cfg!(feature = "flatpak") {
        return Some("Flatpak");
    }
    #[cfg(linux_desktop)]
    if std::env::var_os("APPIMAGE").is_some() {
        return Some("AppImage");
    }
    None
}

#[cfg(linux_desktop)]
fn distribution() -> Option<String> {
    // Inside Flatpak /etc/os-release names the runtime. Flatpak mounts the
    // host's file at /run/host/os-release, as os-release(5) suggests.
    let files: &[&str] = if cfg!(feature = "flatpak") {
        &["/run/host/os-release"]
    } else {
        &["/etc/os-release", "/usr/lib/os-release"]
    };
    // os-release(5): use only the first file that exists.
    files
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| pretty_name(&text))
}

#[cfg(not(linux_desktop))]
fn distribution() -> Option<String> {
    None
}

/// `PRETTY_NAME` from an os-release file, whose values follow shell quoting.
#[cfg(any(linux_desktop, test))]
fn pretty_name(os_release: &str) -> Option<String> {
    let value = os_release
        .lines()
        .find_map(|line| line.trim().strip_prefix("PRETTY_NAME="))?
        .trim();
    let value = ['"', '\'']
        .into_iter()
        .find_map(|quote| value.strip_prefix(quote)?.strip_suffix(quote))
        .unwrap_or(value);
    let mut name = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        match character {
            '\\' => name.extend(characters.next()),
            character => name.push(character),
        }
    }
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::platform::{
        Platform, WindowAdapter, WindowEvent,
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    };
    use std::{sync::Mutex, time::Duration};

    fn environment() -> Environment {
        Environment {
            version: "0.1.0-alpha.7",
            package: Some("AppImage"),
            system: "Linux",
            architecture: "x86_64",
            distribution: Some("Arch Linux".into()),
            gpu: true,
            zoom: 0.85,
            fit: true,
            colors: "dark",
            remote_images_blocked: true,
        }
    }

    #[test]
    fn describes_the_app_system_and_reader() {
        assert_eq!(
            environment().describe(),
            "Flectar Mail 0.1.0-alpha.7 (AppImage)\n\
             Linux x86_64 (Arch Linux)\n\
             GPU renderer, 85% zoom (fit to width), dark message colors, remote images blocked"
        );
        let plain = Environment {
            package: None,
            system: "Windows",
            distribution: None,
            gpu: false,
            zoom: 1.25,
            fit: false,
            colors: "original",
            remote_images_blocked: false,
            ..environment()
        };
        assert_eq!(
            plain.describe(),
            "Flectar Mail 0.1.0-alpha.7\n\
             Windows x86_64\n\
             CPU renderer, 125% zoom, original message colors"
        );
    }

    #[test]
    fn prefills_the_form_field_named_environment() {
        let environment = environment().describe();
        let url = issue_url(&environment);
        let parsed = url::Url::parse(&url).unwrap();
        assert_eq!(parsed.host_str(), Some("github.com"));
        assert_eq!(parsed.path(), "/flectar/mail/issues/new");
        let query: Vec<(String, String)> = parsed.query_pairs().into_owned().collect();
        assert_eq!(
            query,
            [
                ("template".to_owned(), "email-rendering.yml".to_owned()),
                ("environment".to_owned(), environment),
            ]
        );
        // Well below the length VS Code keeps its prefilled issue links under.
        assert!(url.len() < 7_500, "{}", url.len());
    }

    #[test]
    fn issue_form_has_the_prefilled_field() {
        let form = include_str!("../.github/ISSUE_TEMPLATE/email-rendering.yml");
        let template = ISSUE_FORM.rsplit_once("template=").unwrap().1;
        assert_eq!(template, "email-rendering.yml");
        assert!(form.contains("\n    id: environment\n"), "{form}");
    }

    #[test]
    fn long_html_is_saved_instead_of_pasted() {
        assert!(fits_in_issue(""));
        assert!(fits_in_issue(&"é".repeat(PASTE_LIMIT)));
        assert!(!fits_in_issue(&"a".repeat(PASTE_LIMIT + 1)));
    }

    #[test]
    fn reads_the_pretty_name_from_os_release() {
        let arch = "NAME=\"Arch Linux\"\nPRETTY_NAME=\"Arch Linux\"\nID=arch\n";
        assert_eq!(pretty_name(arch).as_deref(), Some("Arch Linux"));
        assert_eq!(
            pretty_name("PRETTY_NAME='Debian GNU/Linux 13 (trixie)'").as_deref(),
            Some("Debian GNU/Linux 13 (trixie)")
        );
        assert_eq!(pretty_name("PRETTY_NAME=Gentoo").as_deref(), Some("Gentoo"));
        assert_eq!(
            pretty_name(r#"PRETTY_NAME="Say \"hi\" \$HOME""#).as_deref(),
            Some(r#"Say "hi" $HOME"#)
        );
        assert_eq!(pretty_name("NAME=Linux\n"), None);
        assert_eq!(pretty_name("PRETTY_NAME=\"\""), None);
    }

    /// Records what an export received while its source file still existed.
    #[derive(Default)]
    struct Exports(Mutex<Vec<(PathBuf, String, String, DocumentDialog)>>);

    impl DocumentProvider for Exports {
        fn import(&self) -> DocumentFuture<Option<crate::documents::ImportedDocument>> {
            Box::pin(async { Ok(None) })
        }
        fn export(&self, path: PathBuf, name: String) -> DocumentFuture<bool> {
            self.export_with(path, name, DocumentDialog::default())
        }
        fn export_with(
            &self,
            path: PathBuf,
            name: String,
            dialog: DocumentDialog,
        ) -> DocumentFuture<bool> {
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            self.0.lock().unwrap().push((path, content, name, dialog));
            Box::pin(async { Ok(true) })
        }
    }

    #[test]
    fn saves_long_html_as_a_file_to_attach() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let temp_dir = directory.path().join("tmp");
        let exports = Arc::new(Exports::default());
        let html = format!("<p>{}</p>", "a".repeat(PASTE_LIMIT));
        let saved = runtime.block_on(save_html(
            None,
            exports.clone(),
            temp_dir.clone(),
            html.clone(),
        ));
        assert_eq!(saved, Ok(true));
        let exports = exports.0.lock().unwrap();
        let [(path, content, name, dialog)] = exports.as_slice() else {
            panic!("expected one export");
        };
        assert!(path.starts_with(&temp_dir), "{path:?}");
        assert_eq!(content, &html);
        assert_eq!(name, FILE_NAME);
        assert_eq!(dialog.title, "Save email HTML");
        assert_eq!(dialog.filter, Some(("HTML".to_owned(), &["html"][..])));
        assert!(!path.exists(), "the staged copy is removed after saving");
    }

    /// A window with a clipboard and a clock the test moves.
    struct Headless(
        Rc<MinimalSoftwareWindow>,
        Rc<RefCell<String>>,
        Rc<Cell<Duration>>,
    );

    impl Platform for Headless {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
        fn set_clipboard_text(&self, text: &str, _: slint::platform::Clipboard) {
            *self.1.borrow_mut() = text.to_owned();
        }
        fn duration_since_start(&self) -> Duration {
            self.2.get()
        }
    }

    const SOURCE: &str = "<p>Download your data</p>";

    /// Opens About email rendering in a window `width` wide and presses
    /// Return, which activates Report issue. Returns the app, the addresses
    /// it opened and the clipboard.
    fn report_with_return(width: u32, single_pane: bool) -> (AppWindow, Vec<String>, String) {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        let clipboard = Rc::new(RefCell::new(String::new()));
        let clock = Rc::new(Cell::new(Duration::ZERO));
        slint::platform::set_platform(Box::new(Headless(
            window.clone(),
            clipboard.clone(),
            clock.clone(),
        )))
        .unwrap();
        let app = AppWindow::new().unwrap();
        app.set_startup_ready(true);
        app.set_startup_hydrated(true);
        app.set_connected_accounts(ModelRc::new(VecModel::from(vec![AccountRow {
            id: 1,
            ..Default::default()
        }])));
        app.set_active_view("mail".into());
        app.window().set_size(slint::PhysicalSize::new(width, 900));
        app.show().unwrap();
        if single_pane {
            // A tab with a conversation opens the single pane on the message.
            app.global::<TabsUi>().set_shown_conversation(true);
            app.global::<TabsUi>().set_shown(1);
            slint::platform::update_timers_and_animations();
            // Let the message slide in. The window drops keys aimed at
            // elements that are still out of view.
            clock.set(Duration::from_secs(1));
            slint::platform::update_timers_and_animations();
        }
        app.set_selected_source(SOURCE.into());
        app.set_remote_images_blocked(true);
        app.global::<RendererSettings>().set_active("gpu".into());
        app.global::<EmailReader>().set_zoom(0.85);
        app.global::<EmailReader>().set_auto_fit(true);
        app.global::<MessageColors>().set_preference("dark".into());

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let opened = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded = opened.clone();
        register(
            &app,
            || None,
            runtime.handle(),
            Arc::new(Exports::default()),
            std::env::temp_dir(),
            ExternalUrlOpener::new(move |url| {
                recorded.lock().unwrap().push(url.to_owned());
                Ok(())
            }),
        );

        // The dialog selects Report issue when it opens.
        app.set_rendering_info_open(true);
        let key = slint::SharedString::from(slint::platform::Key::Return);
        app.window()
            .dispatch_event(WindowEvent::KeyPressed { text: key.clone() });
        app.window()
            .dispatch_event(WindowEvent::KeyReleased { text: key });
        // The reading pane copies to the clipboard from a change handler.
        slint::platform::update_timers_and_animations();
        let opened = opened.lock().unwrap().clone();
        let copied = clipboard.borrow().clone();
        (app, opened, copied)
    }

    #[test]
    fn report_issue_copies_the_html_and_opens_the_prefilled_form() {
        let (app, opened, copied) = report_with_return(1280, false);
        let [url] = opened.as_slice() else {
            panic!("expected the issue form once, got {opened:?}");
        };
        let environment = url::Url::parse(url)
            .unwrap()
            .query_pairs()
            .find(|(key, _)| key == "environment")
            .unwrap()
            .1
            .into_owned();
        assert!(
            environment.starts_with(&format!("Flectar Mail {}", env!("CARGO_PKG_VERSION"))),
            "{environment}"
        );
        assert!(
            environment.ends_with(
                "\nGPU renderer, 85% zoom (fit to width), dark message colors, remote images blocked"
            ),
            "{environment}"
        );
        assert_eq!(copied, SOURCE);
        assert_eq!(
            app.get_render_message().key,
            "Copied HTML source. Paste it into the issue."
        );
    }

    /// Both layouts hold the dialog. The hidden copy must not take the focus
    /// from the one on screen.
    #[test]
    fn report_issue_answers_the_keyboard_in_the_single_pane_layout() {
        let (_app, opened, copied) = report_with_return(900, true);
        assert_eq!(opened.len(), 1, "{opened:?}");
        assert_eq!(copied, SOURCE);
    }
}
