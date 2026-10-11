fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("ios") {
        cc::Build::new()
            .file("platform/ios/Documents.m")
            .file("platform/ios/PdfPreview.m")
            .flag("-fobjc-arc")
            .compile("flectar_documents");
        println!("cargo:rustc-link-lib=framework=UIKit");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rerun-if-changed=platform/ios/Documents.m");
        println!("cargo:rerun-if-changed=platform/ios/PdfPreview.m");
    }
    if std::env::var_os("CARGO_CFG_TARGET_OS").as_deref() == Some(std::ffi::OsStr::new("windows")) {
        winresource::WindowsResource::new()
            .set_icon("resources/app-icon/flectar-mail.ico")
            .compile()
            .expect("failed to embed the Windows application icon");
        println!("cargo:rerun-if-changed=resources/app-icon/flectar-mail.ico");
    }

    // Application controls are painted from primitives in app.slint. Fluent is
    // only the deterministic implementation for the remaining layout/scroll
    // infrastructure and never selects a platform widget toolkit.
    let mut config = slint_build::CompilerConfiguration::new()
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
    let is_ohos = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("ohos");
    let source_dir = if is_ohos {
        // Keep upstream UI/layout sources intact. The fork's generated build
        // replaces product branding, translations and notice text space while
        // retaining the existing controls.
        let overlay = out_dir.join("feihong-ui");
        // Watch the real trees, including added/deleted UI and translations.
        // Generated overlay files cannot be build-script inputs: writing them
        // after Cargo's invocation timestamp makes every later build dirty.
        for directory in ["ui", "lang", "resources"] {
            println!(
                "cargo:rerun-if-changed={}",
                manifest_dir.join(directory).display()
            );
        }
        copy_brand_overlay(&manifest_dir.join("ui"), &overlay.join("ui"), true);
        copy_brand_overlay(&manifest_dir.join("lang"), &overlay.join("lang"), true);
        copy_brand_overlay(
            &manifest_dir.join("resources"),
            &overlay.join("resources"),
            false,
        );
        // OHOS never constructs a tray and must not link its D-Bus backend.
        // Preserve the optional typed handle used by shared controllers.
        std::fs::write(overlay.join("ui/tray.slint"),
            "export component FlectarTray inherits Window { in property <bool> enabled; callback show-window(); callback quit-app(); }\n").expect("OHOS tray type");
        let icons = overlay.join("resources/app-icon");
        std::fs::copy(
            manifest_dir.join("platform/ohos/branding/feihong.svg"),
            icons.join("flectar-mail-masked.svg"),
        )
        .expect("fork vector icon");
        std::fs::copy(
            manifest_dir.join("platform/ohos/AppScope/resources/base/media/app_icon.png"),
            icons.join("flectar-mail-masked.png"),
        )
        .expect("fork window icon");
        println!("cargo:rerun-if-changed=platform/ohos/branding/feihong.svg");
        println!("cargo:rerun-if-changed=platform/ohos/AppScope/resources/base/media/app_icon.png");
        config = config.with_bundled_translations(overlay.join("lang"));
        overlay
    } else {
        manifest_dir.clone()
    };
    let tray_dependencies = slint_build::compile_with_output_path(
        source_dir.join("ui/tray.slint"),
        out_dir.join("tray.rs"),
        config
            .clone()
            .with_bundled_translations(source_dir.join("lang")),
    )
    .expect("failed to compile Slint tray UI");
    for dependency in tray_dependencies {
        if !is_ohos || !dependency.starts_with(&out_dir) {
            println!("cargo:rerun-if-changed={}", dependency.display());
        }
    }

    if is_ohos {
        // The lower-level API lets us exclude our own generated inputs while
        // retaining Slint's normal environment and include_modules contract.
        let app_output = out_dir.join("app.rs");
        let dependencies = slint_build::compile_with_output_path(
            source_dir.join("ui/app.slint"),
            &app_output,
            config,
        )
        .expect("failed to compile Slint UI");
        for dependency in dependencies {
            if !dependency.starts_with(&out_dir) {
                println!("cargo:rerun-if-changed={}", dependency.display());
            }
        }
        for variable in [
            "SLINT_STYLE",
            "SLINT_FONT_SIZES",
            "SLINT_SCALE_FACTOR",
            "SLINT_ASSET_SECTION",
            "SLINT_EMBED_RESOURCES",
            "SLINT_EMIT_DEBUG_INFO",
            "SLINT_LIVE_PREVIEW",
            "SLINT_BUNDLE_TRANSLATIONS",
        ] {
            println!("cargo:rerun-if-env-changed={variable}");
        }
        println!(
            "cargo:rustc-env=SLINT_INCLUDE_GENERATED={}",
            app_output.display()
        );
    } else {
        slint_build::compile_with_config(source_dir.join("ui/app.slint"), config)
            .expect("failed to compile Slint UI");
    }
}

fn copy_brand_overlay(source: &std::path::Path, destination: &std::path::Path, replace: bool) {
    std::fs::create_dir_all(destination).expect("create brand overlay");
    for item in std::fs::read_dir(source).expect("read upstream UI") {
        let item = item.expect("read UI entry");
        let target = destination.join(item.file_name());
        if item.file_type().expect("UI file type").is_dir() {
            copy_brand_overlay(&item.path(), &target, replace);
        } else {
            println!("cargo:rerun-if-changed={}", item.path().display());
            if replace
                && matches!(
                    item.path().extension().and_then(|e| e.to_str()),
                    Some("slint" | "po")
                )
            {
                let text = fork_brand_text(
                    &item.path(),
                    &std::fs::read_to_string(item.path()).expect("read UI source"),
                );
                std::fs::write(target, text).expect("write fork UI overlay");
            } else {
                std::fs::copy(item.path(), target).expect("copy UI asset");
            }
        }
    }
}

const FORK_NOTICES: [(&str, &str, &str); 3] = [
    (
        "Built by Flectar with help from the open-source community.",
        "Based on Flectar Mail; unofficial port.",
        "基于 Flectar Mail，非官方移植。",
    ),
    ("Company website", "Upstream project", "上游项目"),
    (
        "Third-party components retain their own licenses and notices.",
        "This port is based on Flectar Mail and is not produced, sponsored, or supported by Flectar. Slint, Sequoia OpenPGP, OpenSSL and JMAP retain their own licenses; texts are included in the app.",
        "飞鸿邮箱基于 Flectar Mail，非 Flectar 出品、赞助或支持。Slint、Sequoia OpenPGP、OpenSSL、JMAP 许可文本随应用附带。",
    ),
];

fn fork_brand_text(source: &std::path::Path, upstream: &str) -> String {
    let mut text = upstream.replace("Flectar Mail", "飞鸿邮箱");
    for (original, notice, _) in FORK_NOTICES {
        text = text.replace(original, notice);
    }
    if source
        .extension()
        .is_some_and(|extension| extension == "po")
    {
        let chinese = source
            .components()
            .any(|part| part.as_os_str() == "zh_Hans");
        for (_, notice, translation) in FORK_NOTICES {
            // Rewriting msgid alone retains a translation claiming the fork
            // was made by Flectar. Replace that selected msgstr too, including
            // its multiline continuation. Other languages use the new English
            // source until an accurate fork translation is supplied.
            text =
                rewrite_notice_translation(&text, notice, if chinese { translation } else { "" });
        }
    }
    if source.ends_with("settings-dialog.slint") {
        text = text
            .replace("https://flectar.com", "https://github.com/flectar/mail")
            // Keep the existing card and controls, with enough room to read
            // the fork attribution and actual embedded-component notice.
            .replace(
                "height: root.compact ? 164px : 132px;",
                "height: root.compact ? 216px : 164px;",
            )
            .replace(
                "height: root.compact ? 38px : 18px;",
                "height: root.compact ? 88px : 44px;",
            );
    }
    text
}

fn rewrite_notice_translation(catalog: &str, notice: &str, translation: &str) -> String {
    let key = format!("msgid \"{notice}\"");
    catalog
        .split("\n\n")
        .map(|entry| {
            if !entry.lines().any(|line| line == key) {
                return entry.to_owned();
            }
            let mut output = Vec::new();
            let mut lines = entry.lines().peekable();
            while let Some(line) = lines.next() {
                if line.starts_with("msgstr ") {
                    output.push(format!("msgstr \"{translation}\""));
                    while lines.peek().is_some_and(|line| line.starts_with('"')) {
                        lines.next();
                    }
                } else {
                    output.push(line.to_owned());
                }
            }
            output.join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
