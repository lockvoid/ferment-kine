// The Kotlin binding of the kine core, beside the header it mirrors
// (`../crate/include/kine.h`). Build the host library with `./build.sh host`
// before running `./gradlew :kine:test`.
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
