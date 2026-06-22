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

    buildTypes {
        release {
            isMinifyEnabled = false
            // M0 is self-signed via the default debug key for sideload bring-up (ADR
            // Decision 3); a real release signingConfig is wired in M4 (RR29-FR2).
            signingConfig = signingConfigs.getByName("debug")
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
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
}
