// inkread Android app module (RR1-FR2). minSdk 24 (NeoReader floor); arm64 only (RK3566).
// The Rust core (libreader.so) is produced by buildApk.sh via cargo-ndk into
// src/main/jniLibs/ BEFORE this assembles — Gradle just packages it.
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.jraghavan.inkread"
    compileSdk = 34

    defaultConfig {
        applicationId = "dev.jraghavan.inkread"
        minSdk = 24
        targetSdk = 34
        // Version is injected by CI on a release build: release.yml passes the git tag via
        // ORG_GRADLE_PROJECT_inkread* env vars, which Gradle maps to these project properties.
        // Local/dev builds fall back to a clearly-marked -dev version with code 1.
        versionCode = (project.findProperty("inkreadVersionCode") as String?)?.toInt() ?: 1
        versionName = (project.findProperty("inkreadVersionName") as String?) ?: "0.1.0-m0"
        ndk {
            // RK3566 is arm64; M0 ships arm64 only (RR29-FR1).
            abiFilters += "arm64-v8a"
        }
    }

    // libreader.so + libpdfium.so are staged into src/main/jniLibs/ by buildApk.sh.
    sourceSets["main"].jniLibs.srcDirs("src/main/jniLibs")

    // Release signing (RR29-FR2 / ADR-INKREAD-0014 Decision 0). The keystore is supplied by CI via
    // env vars (release.yml decodes a repo secret to a file); local/dev builds leave them unset and
    // fall back to the debug key for sideload bring-up. A STABLE release key is the prerequisite for
    // the in-app self-updater: Android rejects an update whose signer differs from the installed app.
    val keystorePath = System.getenv("INKREAD_KEYSTORE_FILE")
        ?: project.findProperty("inkreadKeystoreFile") as String?
    signingConfigs {
        create("release") {
            if (!keystorePath.isNullOrBlank()) {
                storeFile = file(keystorePath)
                storePassword = System.getenv("INKREAD_KEYSTORE_PASSWORD")
                    ?: project.findProperty("inkreadKeystorePassword") as String?
                keyAlias = System.getenv("INKREAD_KEY_ALIAS")
                    ?: project.findProperty("inkreadKeyAlias") as String?
                keyPassword = System.getenv("INKREAD_KEY_PASSWORD")
                    ?: project.findProperty("inkreadKeyPassword") as String?
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // Production-sign when a keystore is configured (CI); otherwise self-sign with the debug
            // key for local sideload bring-up. The self-updater stays inert (signer-pin fails closed)
            // until a release-key build is the installed one.
            signingConfig = if (!keystorePath.isNullOrBlank()) {
                signingConfigs.getByName("release")
            } else {
                signingConfigs.getByName("debug")
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }

    // Don't let Gradle strip/recompress the prebuilt .so (it is already stripped).
    packaging {
        jniLibs.useLegacyPackaging = false
    }
}

dependencies {
    // Pinned to the last versions that build against compileSdk 34 (AGP 8.5.2). Newer androidx.core
    // (1.16+) demands compileSdk 35+ and AGP 9.1+, which is a deliberate migration — see dependabot
    // ignores. Bump these together with compileSdk + AGP, not piecemeal.
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.1")
    // Background scheduler for the daily auto-compile (#66). 2.9.x is the last line that builds
    // against compileSdk 34 (2.10+ needs SDK 35) — bump with compileSdk + AGP, not piecemeal.
    implementation("androidx.work:work-runtime-ktx:2.11.2")

    // Host JVM unit tests for pure logic (e.g. PalmFilter) — run via :app:testDebugUnitTest, no
    // emulator/device needed (an emulator can't simulate the Supernote EMR pen anyway).
    testImplementation("junit:junit:4.13.2")
}
