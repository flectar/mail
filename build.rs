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
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        // A source phrase has one meaning throughout this application. Keeping
        // catalogs context-free avoids duplicate entries for shared controls.
        .with_default_translation_context(slint_build::DefaultTranslationContext::None)
        // Bundle the small gettext catalogs so desktop and mobile builds use
        // the same translations without depending on a system gettext install.
        .with_bundled_translations("lang");

    // Compile the non-visual tray separately from app.slint. Slint 1.17
    // otherwise registers app.slint's embedded font through the tray's shared
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
