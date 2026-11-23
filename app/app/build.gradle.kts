import java.util.Properties

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

val localSigning = Properties().apply {
    val f = File(System.getProperty("user.home"), ".config/mapaki/android-signing.properties")
    if (f.exists()) f.inputStream().use { load(it) }
}
fun signing(env: String, prop: String): String? = System.getenv(env) ?: localSigning.getProperty(prop)

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

    // One signing key for every build, so any APK installs over any other
    // (debug over release included) without uninstalling. CI reads it from
    // secrets (see .github/workflows/release.yml); locally it comes from
    // ~/.config/mapaki/android-signing.properties, which
    // `scripts/android-signing.sh init` creates. With neither, builds fall
    // back to the throwaway debug key.
    val keystore = signing("MAPAKI_KEYSTORE_PATH", "storeFile")?.let { file(it) }?.takeIf { it.exists() }
    signingConfigs {
        if (keystore != null) {
            create("mapaki") {
                storeFile = keystore
                storePassword = signing("MAPAKI_KEYSTORE_PASSWORD", "storePassword")
                keyAlias = signing("MAPAKI_KEY_ALIAS", "keyAlias")
                keyPassword = signing("MAPAKI_KEY_PASSWORD", "keyPassword")
            }
        }
    }

    buildTypes {
        val mapaki = signingConfigs.findByName("mapaki")
        debug {
            if (mapaki != null) signingConfig = mapaki
        }
        release {
            isMinifyEnabled = false
            signingConfig = mapaki ?: signingConfigs.getByName("debug")
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
