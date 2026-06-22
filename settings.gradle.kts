// inkread — single-module Android project (RR1-FR2). The Rust core is built separately by
// buildApk.sh (cargo-ndk) and bundled from app/src/main/jniLibs/.
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "inkread"
include(":app")
// RR19-FR4b pen-latency spike — a SEPARATE measurement APK (not the reader). Standalone
// module so it builds/installs independently of the M0 reader bring-up.
include(":spike")
