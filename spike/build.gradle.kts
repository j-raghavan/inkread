// inkread pen-latency spike module (RR19-FR4b) — a SEPARATE measurement APK, NOT the reader.
//
// Package: dev.jraghavan.inkread.penspike. This module is device-specific and MAY name the
// vendor (IR-7 constrains only reader-core; this is a throwaway measurement tool). It depends
// on NOTHING in reader-core / device-eink. A tiny C native helper (CMake externalNativeBuild)
// provides the /dev/ebc open()+ioctl() that Kotlin cannot do.
//
// Build:  ./gradlew :spike:assembleDebug
// Output: spike/build/outputs/apk/debug/spike-debug.apk
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.jraghavan.inkread.penspike"
    compileSdk = 34

    defaultConfig {
        applicationId = "dev.jraghavan.inkread.penspike"
        minSdk = 24
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0-spike"
        ndk {
            // RK3566 is arm64; the spike targets the Supernote only.
            abiFilters += "arm64-v8a"
        }
        externalNativeBuild {
            cmake {
                // C11, no STL needed — keeps the helper tiny and trustworthy.
                cppFlags += "-std=c11"
                arguments += "-DANDROID_STL=none"
            }
        }
    }

    externalNativeBuild {
        cmake {
            path = file("src/main/cpp/CMakeLists.txt")
            version = "3.22.1"
        }
    }

    buildTypes {
        debug {
            isMinifyEnabled = false
        }
        release {
            isMinifyEnabled = false
            // Self-signed via the debug key — this is a measurement tool, never shipped.
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
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
}
