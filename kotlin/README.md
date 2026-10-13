# Kine for Kotlin and Android

The Kotlin binding calls the shared Rust renderer through JNA. `:kine` is the
JVM wrapper; `:kine-android` packages the Android native library and platform
helpers. The document format is defined in the [schema](../crate/docs/SCHEMA.md).

## Build and test on the host

The current host build targets an Apple Silicon Mac. Install the pinned Rust
toolchain through rustup and use JDK 17 for Gradle.

From the repository root:

```sh
./kotlin/build.sh host
cd kotlin
./gradlew :kine:test
```

The build stages `libkine.dylib` in `kine/build/host/`; tests set the JNA library
path to that directory. The host tests do not require an Android SDK.

## Build for Android

The Android library supports API 33 and later and currently packages arm64-v8a.
Its build targets Android SDK 37.2. Install `cargo-ndk` and NDK r28 or later,
then set `ANDROID_NDK_HOME` to the NDK directory.

```sh
# From the repository root, with ANDROID_NDK_HOME set:
./kotlin/build.sh android
cd kotlin
./gradlew :kine-android:assembleRelease
```

The script builds the core with `gpu-gles` and stages `libkine.so` in
`kine-android/src/main/jniLibs/arm64-v8a/`. Generated libraries and Gradle build
output are excluded from Git. Rebuild after changing the core or C header.

Include both modules in your application's Gradle build and depend on
`:kine-android`. JVM consumers use `:kine` and supply JNA plus the matching
native library on the library search path. These modules do not configure a
remote Maven publication.

## Render a document

This example assumes you loaded the demo JSON and its font bytes from your
application's resources:

```kotlin
import run.ferment.kine.Kine

Kine.registerFont(fontBytes)
val frame = Kine.Document(documentJson).use { document ->
    document.renderRGBA(
        t = 0.5,
        signals = mapOf("progress" to 0.5),
        width = 800,
        height = 260,
    )
}
```

Reuse a document for repeated renders and close it when finished. RGBA output
is premultiplied. GPU rendering additionally needs a host-owned EGL context
and GL texture; the wrapper and [C header](../crate/include/kine.h) document
that contract.

Source licensing and font notices are listed in the [root README](../README.md#license).
