import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// Version from the workspace Cargo.toml: 0.2.5 -> versionCode 205.
val cargoVersion: String = rootProject.file("../Cargo.toml").readLines()
    .first { it.startsWith("version = ") }.substringAfter('"').substringBefore('"')
val (vMajor, vMinor, vPatch) = cargoVersion.split('.').map { it.takeWhile(Char::isDigit).toInt() }

android {
    namespace = "com.ospab.overnet"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.ospab.overnet"
        minSdk = 26
        targetSdk = 36
        versionCode = vMajor * 10000 + vMinor * 100 + vPatch
        versionName = cargoVersion
    }

    // Release key from the environment (GitHub Actions secrets); without it the
    // release build is signed with the debug key, for local testing.
    val keystore = System.getenv("OVERNET_KEYSTORE")
    signingConfigs {
        if (keystore != null) {
            create("release") {
                storeFile = file(keystore)
                storePassword = System.getenv("OVERNET_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("OVERNET_KEY_ALIAS") ?: "overnet"
                keyPassword = System.getenv("OVERNET_KEYSTORE_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.findByName("release") ?: signingConfigs.getByName("debug")
        }
    }

    // One APK per processor: GeckoView is large, a universal APK would be ~3x.
    splits {
        abi {
            isEnable = true
            reset()
            include("arm64-v8a", "armeabi-v7a", "x86_64")
            isUniversalApk = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

kotlin {
    compilerOptions { jvmTarget.set(JvmTarget.JVM_17) }
}

dependencies {
    implementation("org.mozilla.geckoview:geckoview:157.0.20260924084938")
}
