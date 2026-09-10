plugins {
    id("com.android.application")
}

val repositoryAndroidTarget = rootProject.layout.projectDirectory.dir("../../../target/android")
layout.buildDirectory = repositoryAndroidTarget.dir("gradle/app")

android {
    namespace = "com.flectar.mail"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.flectar.mail"
        minSdk = 26
        targetSdk = 36
        versionCode = providers.environmentVariable("FLECTAR_ANDROID_VERSION_CODE")
            .orElse("1").get().toInt()
        versionName = providers.environmentVariable("FLECTAR_ANDROID_VERSION_NAME")
            .orElse("0.1.0").get()
    }

    sourceSets["main"].jniLibs.srcDir(repositoryAndroidTarget.dir("gradle-jni"))
    sourceSets["main"].res.srcDir(layout.buildDirectory.dir("generated/flectar-res"))
    sourceSets["main"].assets.srcDirs(
        "../../assets",
        layout.buildDirectory.dir("generated/flectar-assets"),
    )

    // PDFium is loaded by its absolute installed nativeLibraryDir path. Let the
    // package manager extract and protect it; no temporary executable files.
    packaging.jniLibs.useLegacyPackaging = true

    signingConfigs {
        create("releaseFromEnvironment") {
            val keystore = providers.environmentVariable("FLECTAR_ANDROID_KEYSTORE").orNull
            if (!keystore.isNullOrBlank()) {
                storeFile = file(keystore)
                storePassword = providers.environmentVariable("FLECTAR_ANDROID_KEYSTORE_PASSWORD").get()
                keyAlias = providers.environmentVariable("FLECTAR_ANDROID_KEY_ALIAS").get()
                keyPassword = providers.environmentVariable("FLECTAR_ANDROID_KEY_PASSWORD").get()
            }
        }
    }

    buildTypes {
        getByName("release") {
            isDebuggable = false
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("releaseFromEnvironment")
        }
    }
}

val generateFlectarLicenseAssets by tasks.registering(Copy::class) {
    into(layout.buildDirectory.dir("generated/flectar-assets/licenses/flectar-mail"))
    from("../../../../LICENSE")
    from("../../../../THIRD_PARTY_NOTICES.md")
    from("../../../../LICENSES") {
        into("LICENSES")
    }
}

val generatePdfiumLicenseAssets by tasks.registering(Copy::class) {
    into(layout.buildDirectory.dir("generated/flectar-assets/licenses/PDFium"))
    from(repositoryAndroidTarget.dir("gradle-jni")) {
        include("*/pdfium-licenses/**")
    }
}
val verifyPdfiumRuntime by tasks.registering {
    doLast {
        fileTree(repositoryAndroidTarget.dir("gradle-jni")).matching {
            include("*/libflectar_mail_android.so")
        }.forEach { appLibrary ->
            check(appLibrary.resolveSibling("libpdfium.so").isFile) {
                "Missing bundled PDFium for ${appLibrary.parentFile.name}; run scripts/stage-pdfium.py for this ABI."
            }
            check(appLibrary.parentFile.resolve("pdfium-licenses/build.json").isFile) {
                "Missing PDFium provenance and license notices."
            }
        }
    }
}
tasks.named("preBuild").configure {
    dependsOn(generateFlectarLicenseAssets, generatePdfiumLicenseAssets, verifyPdfiumRuntime)
}

dependencies {
    // AuthorizationClient is Google's supported API for authorizing an
    // Android application to Gmail and other Google user data.
    implementation("com.google.android.gms:play-services-auth:21.6.0")
}
