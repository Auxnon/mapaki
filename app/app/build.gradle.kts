plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
}

// Single source of truth for the release version, shared with the Rust crates
// (scripts/release.sh bumps it). versionCode is derived so it always increases.
val mapakiVersion = rootProject.file("../VERSION").readText().trim()
val mapakiVersionCode = mapakiVersion.split(".").map { it.toInt() }.let { (major, minor, patch) ->
    major * 10_000 + minor * 100 + patch
}

android {
    namespace = "com.makeavoy.mapaki"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.makeavoy.mapaki"
        minSdk = 26
        targetSdk = 35
        versionCode = mapakiVersionCode
        versionName = mapakiVersion
    }

    // CI signs releases from secrets (see .github/workflows/release.yml). Without
    // them the release build falls back to the debug key so it's still installable.
    val releaseKeystore = System.getenv("MAPAKI_KEYSTORE_PATH")?.let { file(it) }?.takeIf { it.exists() }
    signingConfigs {
        if (releaseKeystore != null) {
            create("release") {
                storeFile = releaseKeystore
                storePassword = System.getenv("MAPAKI_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("MAPAKI_KEY_ALIAS")
                keyPassword = System.getenv("MAPAKI_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.findByName("release") ?: signingConfigs.getByName("debug")
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
    implementation(libs.core.ktx)
    implementation(libs.lifecycle.runtime.ktx)
    implementation(libs.lifecycle.viewmodel.compose)
    implementation(libs.activity.compose)

    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.graphics)
    implementation(libs.compose.ui.tooling.preview)
    implementation(libs.compose.material3)
    implementation(libs.compose.material.icons.extended)
    debugImplementation(libs.compose.ui.tooling)

    implementation(libs.security.crypto)
    implementation(libs.okhttp)
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.work.runtime.ktx)

    testImplementation(libs.junit)
}
