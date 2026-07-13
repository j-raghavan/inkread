// Root build file (RR1-FR2). Plugin versions are declared but not applied here.
// AGP is pinned to 8.5.2 (with Gradle 8.9): AGP 9.x registers its own `kotlin` extension and
// conflicts with the standalone org.jetbrains.kotlin.android plugin ("Cannot add extension with
// name 'kotlin'"), so the AGP-9 / Gradle-9 Dependabot bump broke the release build. The AGP-9
// upgrade is a deliberate migration, not an auto-bump (Dependabot is told to ignore those majors).
plugins {
    id("com.android.application") version "8.13.2" apply false
    id("org.jetbrains.kotlin.android") version "2.4.0" apply false
}
