plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "com.onkey.android"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.onkey.android"
        minSdk = 29          // Android 10: AudioPlaybackCapture availability (live capture, phase 2)
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
    }

    // The Rust core's .so files are placed into src/main/jniLibs/<abi> by cargo-ndk in CI
    // before this build runs. Only arm64-v8a in the MVP.
    sourceSets {
        getByName("main") {
            jniLibs.srcDir("src/main/jniLibs")
        }
    }

    packaging {
        jniLibs.useLegacyPackaging = false
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
    buildFeatures {
        compose = true
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2024.10.01")
    implementation(composeBom)
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-graphics")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.activity:activity-compose:1.9.3")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.8.7")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.8.7")
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")
    // ONNX Runtime is linked by the Rust core (ort crate); no Maven AAR needed.
}
