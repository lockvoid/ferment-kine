// The Kotlin binding of the kine core, beside the header it mirrors
// (`../crate/include/kine.h`). Standalone: `./gradlew :kine:test`. The Android
// app includes these two modules into its own build from `KINE_ROOT`.
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

plugins {
    id("org.gradle.toolchains.foojay-resolver-convention") version "1.0.0"
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "kine-kotlin"

include(":kine")
include(":kine-android")
