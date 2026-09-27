// kine — the Kotlin binding over the core's C ABI (../crate/include/kine.h).
// Host-JVM module: tests load kine/build/host/libkine.dylib through JNA.
plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

kotlin {
    jvmToolchain(17)
}

dependencies {
    compileOnly(libs.jna)
    implementation(libs.kotlinx.serialization.json)
    testImplementation(libs.jna)
    testImplementation(libs.kotlin.test.junit)
}

// The real core, built by kotlin/build.sh — never a stub. Declaring it an
// INPUT is what stops `gradle clean` (which deletes it, since it is not a task
// output) leaving the test task UP-TO-DATE; the guard is what stops a missing
// dylib reporting one UnsatisfiedLinkError as "1 test" instead of 19.
tasks.test {
    useJUnit()
    val library = layout.projectDirectory.file("build/host/libkine.dylib").asFile
    inputs.files(library).withPropertyName("kineHostLibrary")
    systemProperty("jna.library.path", library.parent)
    doFirst {
        require(library.exists()) { "$library is missing — run `kotlin/build.sh host`" }
    }
    testLogging { events("failed"); showStandardStreams = false }
}
