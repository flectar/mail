use cfg_aliases::cfg_aliases;

fn main() {
    generate_notification_words();
    // Shared code selects behaviour by capability, never by naming a mobile
    // operating system. A new platform joins these groups here (and in the
    // core's build.rs) instead of editing call sites. See platform/README.md.
    cfg_aliases! {
        mobile: { any(target_os = "android", target_os = "ios", target_env = "ohos") },
        desktop: { not(mobile) },
        // OpenHarmony reports target_os = "linux" without a desktop session,
        // portals, X11 or a Secret Service.
        linux_desktop: { all(desktop, target_os = "linux", not(target_env = "ohos")) },
        glibc: { all(target_os = "linux", target_env = "gnu") },
        // New-mail notifications through Apple's User Notifications.
        user_notifications: { any(target_os = "macos", target_os = "ios") },
        // The system suspends the app in the background and wakes it to
        // check for new mail.
        background_checks: { any(target_os = "ios", target_os = "android") },
        // The system may start the app without its window, for a check for
        // new mail or a button on a notification.
        background_engine: { target_os = "android" },
    }

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("ios") {
        cc::Build::new()
            .file("platform/ios/Documents.m")
            .file("platform/ios/PdfPreview.m")
            .file("platform/ios/AppIcon.m")
            .file("platform/ios/WebAuthentication.m")
            .file("platform/ios/MemoryPressure.m")
            .file("platform/ios/BackgroundChecks.m")
            .flag("-fobjc-arc")
            .compile("flectar_documents");
        println!("cargo:rustc-link-lib=framework=UIKit");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=framework=AuthenticationServices");
        println!("cargo:rustc-link-lib=framework=BackgroundTasks");
        // iOS gives the main thread a 1 MB stack, and Slint's generated code
        // for the main window takes a 930 KiB frame when the mailbox first
        // appears (slint-ui/slint#14817). Ask for the 8 MiB the same code gets
        // on macOS and Linux; the kernel sizes the main stack from LC_MAIN.
        println!("cargo:rustc-link-arg-bins=-Wl,-stack_size,0x800000");
        println!("cargo:rerun-if-changed=platform/ios/Documents.m");
        println!("cargo:rerun-if-changed=platform/ios/PdfPreview.m");
        println!("cargo:rerun-if-changed=platform/ios/AppIcon.m");
        println!("cargo:rerun-if-changed=platform/ios/WebAuthentication.m");
        println!("cargo:rerun-if-changed=platform/ios/MemoryPressure.m");
        println!("cargo:rerun-if-changed=platform/ios/BackgroundChecks.m");
    }
    if std::env::var_os("CARGO_CFG_TARGET_OS").as_deref() == Some(std::ffi::OsStr::new("windows")) {
        winresource::WindowsResource::new()
            .set_icon("resources/app-icon/flectar-mail.ico")
            // Opts into the segment heap (Windows 10 2004 and later), which
            // Microsoft says generally reduces memory use. Nothing else is set:
            // declaring DPI awareness or an OS version would change behaviour.
            .set_manifest_file("platform/windows/flectar-mail.manifest.xml")
            .compile()
            .expect("failed to embed the Windows icon and manifest");
        println!("cargo:rerun-if-changed=resources/app-icon/flectar-mail.ico");
        println!("cargo:rerun-if-changed=platform/windows/flectar-mail.manifest.xml");
    }

    // Application controls are painted from primitives in app.slint. Fluent is
    // only the deterministic implementation for the remaining layout/scroll
    // infrastructure and never selects a platform widget toolkit.
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        // A source phrase has one meaning throughout this application. Keeping
        // catalogs context-free avoids duplicate entries for shared controls.
        .with_default_translation_context(slint_build::DefaultTranslationContext::None)
        // Bundle the small gettext catalogs so desktop and mobile builds use
        // the same translations without depending on a system gettext install.
        .with_bundled_translations("lang");

    // Compile the non-visual tray separately from app.slint. Slint otherwise
    // registers app.slint's embedded font through the tray's shared
    // globals, which creates an unintended native WindowAdapter for a
    // SystemTrayIcon-rooted component. Keep the same style and translation
    // configuration for both compilation units.
    let manifest_dir = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
    );
    let out_dir =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let tray_dependencies = slint_build::compile_with_output_path(
        manifest_dir.join("ui/tray.slint"),
        out_dir.join("tray.rs"),
        config
            .clone()
            .with_bundled_translations(manifest_dir.join("lang")),
    )
    .expect("failed to compile Slint tray UI");
    for dependency in tray_dependencies {
        println!("cargo:rerun-if-changed={}", dependency.display());
    }

    slint_build::compile_with_config("ui/app.slint", config).expect("failed to compile Slint UI");
}

/// The words of new-mail notifications from every bundled catalog, for a
/// process that shows them without the window (src/notifications/words.rs).
/// Slint bundles the same catalogs (`with_bundled_translations`); this reads
/// them the way it does, so the words come out the same.
fn generate_notification_words() {
    use rspolib::TranslatedEntry;
    use std::fmt::Write;

    // As app.slint's I18n asks for them: context, message, plural message.
    // Keep in step with WORDS in src/notifications/words.rs.
    const SINGLES: &[(&str, &str)] = &[
        ("", "New message"),
        ("", "(No subject)"),
        ("", "Open"),
        ("", "Mark as read"),
        ("", "Archive"),
        ("", "Delete"),
        ("", "New mail"),
        ("", "New mail without sound"),
    ];
    const PLURALS: &[(&str, &str)] = &[
        ("", "{n} new message"),
        (
            "the first senders of new mail, then how many others",
            "{0} and {n} more",
        ),
        ("", "{n} unread message"),
    ];

    let mut languages = std::fs::read_dir("lang")
        .expect("reading lang/")
        .map(|entry| entry.expect("reading lang/").path())
        .filter(|path| path.join("LC_MESSAGES/flectar-mail.po").exists())
        .collect::<Vec<_>>();
    languages.sort();
    let mut out = String::from(
        "// Generated by build.rs from lang/*/LC_MESSAGES/flectar-mail.po.\n\
         static CATALOGS: &[Catalog] = &[\n",
    );
    for directory in &languages {
        let path = directory.join("LC_MESSAGES/flectar-mail.po");
        println!("cargo:rerun-if-changed={}", path.display());
        let catalog = rspolib::pofile(path.as_path())
            .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()));
        let rule = catalog
            .metadata
            .get("Plural-Forms")
            .and_then(|header| {
                header.split(';').find_map(|part| {
                    let (key, expression) = part.split_once('=')?;
                    (key.trim() == "plural").then(|| expression.trim().to_owned())
                })
            })
            .unwrap_or_else(|| "n != 1".to_owned());
        let language = directory.file_name().unwrap().to_string_lossy();
        let find = |context: &str, message: &str| {
            catalog
                .find_by_msgid_msgctxt(message, context)
                .filter(|entry| entry.translated())
        };
        write!(out, "    Catalog {{\n        language: {language:?},\n        plural: {rule:?},\n        singles: &[").unwrap();
        for (context, message) in SINGLES {
            match find(context, message).and_then(|entry| entry.msgstr) {
                Some(text) => write!(out, "Some({text:?}), ").unwrap(),
                None => out.push_str("None, "),
            }
        }
        out.push_str("],\n        plurals: &[");
        for (context, message) in PLURALS {
            match find(context, message)
                .map(|entry| entry.msgstr_plural)
                .filter(|forms| !forms.is_empty())
            {
                Some(forms) => write!(out, "Some(&{forms:?}), ").unwrap(),
                None => out.push_str("None, "),
            }
        }
        out.push_str("],\n    },\n");
    }
    out.push_str("];\n");
    let path = std::path::Path::new(&std::env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"))
        .join("notification_words.rs");
    std::fs::write(path, out).expect("writing the notification words");
}
