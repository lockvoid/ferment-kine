// kine-android — the arm64 .so payload for :kine plus the JNA aar. No sources of
// its own beyond Android-only helpers; jniLibs are produced by kotlin/build.sh android.
plugins {
    alias(libs.plugins.android.library)
}

android {
    namespace = "run.ferment.kine.android"
    compileSdk = 37
    compileSdkMinor = 2

    defaultConfig {
        minSdk = 33
        ndk { abiFilters += listOf("arm64-v8a") }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    packaging {
        jniLibs { useLegacyPackaging = false }
    }
}

dependencies {
    api(project(":kine"))
    implementation("${libs.jna.get()}@aar")
}
