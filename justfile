# Mapaki build tooling.
#
# `just build` produces a debug-signed APK you can sideload straight away —
# no keystore setup needed. First run downloads a project-local JDK and
# Android SDK (nothing touches your system) plus generates the Gradle
# wrapper if it's ever missing; that only happens once.
#
# Daemon deploy: on the server, run the installer from the README
# (`curl ... install.sh | sh -s -- daemon`), or by hand:
#   1. `just daemon-build`, copy the binary over, install
#      daemon/mapaki-daemon.service (edit the CHANGE_ME user/paths first),
#      `systemctl enable --now mapaki-daemon`
#   2. Get a pairing code: `mapaki-daemon pair-code` (as the service user)
#   3. On matriarch (site-delta host), create `mapaki-upstream.txt` next to
#      the site-delta binary containing beex's LAN IP, e.g. `192.168.1.40:8420`
#      (must be an IP, not `beex.local` — see daemon/README notes), add the
#      Cloudflare DNS record for mapaki.makeavoy.com, redeploy site-delta.

android_sdk_dir := "app/app/.android-sdk"
jdk_dir := "app/.jdk"
cmdline_tools_version := "11076708"
temurin_url := "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.5%2B11/OpenJDK21U-jdk_x64_linux_hotspot_21.0.5_11.tar.gz"

default:
    @just --list

# One-time (idempotent) setup: local JDK + project-local Android SDK for app/.
bootstrap:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -x "{{jdk_dir}}/bin/javac" ]; then
        echo "mapaki: downloading a local JDK 21 (no usable system javac found)..."
        mkdir -p "{{jdk_dir}}"
        curl -sSL -o /tmp/mapaki-jdk.tar.gz "{{temurin_url}}"
        tar -xzf /tmp/mapaki-jdk.tar.gz -C "{{jdk_dir}}" --strip-components=1
        rm -f /tmp/mapaki-jdk.tar.gz
    fi
    if [ ! -d "{{android_sdk_dir}}/platforms/android-35" ]; then
        echo "mapaki: downloading Android cmdline-tools + platform 35 + build-tools 35..."
        mkdir -p "{{android_sdk_dir}}/cmdline-tools"
        curl -sSL -o /tmp/mapaki-cmdline-tools.zip \
          "https://dl.google.com/android/repository/commandlinetools-linux-{{cmdline_tools_version}}_latest.zip"
        rm -rf /tmp/mapaki-cmdline-tools && mkdir /tmp/mapaki-cmdline-tools
        unzip -q /tmp/mapaki-cmdline-tools.zip -d /tmp/mapaki-cmdline-tools
        rm -rf "{{android_sdk_dir}}/cmdline-tools/latest"
        mv /tmp/mapaki-cmdline-tools/cmdline-tools "{{android_sdk_dir}}/cmdline-tools/latest"
        rm -rf /tmp/mapaki-cmdline-tools /tmp/mapaki-cmdline-tools.zip
        export JAVA_HOME="$(pwd)/{{jdk_dir}}"
        yes | "{{android_sdk_dir}}/cmdline-tools/latest/bin/sdkmanager" \
          --sdk_root="$(pwd)/{{android_sdk_dir}}" --licenses > /dev/null || true
        "{{android_sdk_dir}}/cmdline-tools/latest/bin/sdkmanager" \
          --sdk_root="$(pwd)/{{android_sdk_dir}}" "platform-tools" "platforms;android-35" "build-tools;35.0.0"
    fi
    echo "sdk.dir=$(pwd)/{{android_sdk_dir}}" > app/local.properties
    echo "mapaki: bootstrap complete"

# Build a debug APK (sideload-ready, no keystore needed).
build: bootstrap
    #!/usr/bin/env bash
    set -euo pipefail
    export JAVA_HOME="$(pwd)/{{jdk_dir}}"
    cd app && ./gradlew :app:assembleDebug --console=plain
    echo
    echo "APK: app/app/build/outputs/apk/debug/app-debug.apk"

# Print the path to the last built debug APK.
apk:
    @echo app/app/build/outputs/apk/debug/app-debug.apk

# Install the debug APK on a connected/adb-paired device (enable USB debugging first).
install: build
    ./{{android_sdk_dir}}/platform-tools/adb install -r app/app/build/outputs/apk/debug/app-debug.apk

# Build the daemon (release, native target).
daemon-build:
    cd daemon && cargo build --release

# Run the daemon locally against a scratch folder for testing.
daemon-run:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p daemon/.scratch-mapaki
    MAPAKI_DIR="$(pwd)/daemon/.scratch-mapaki" MAPAKI_PORT=8420 cargo run --manifest-path daemon/Cargo.toml

# Run the daemon's tests.
daemon-test:
    cd daemon && cargo test

# Build the desktop sync client (release, native target).
client-build:
    cd client && cargo build --release

# Run the desktop client's tests.
client-test:
    cd client && cargo test

# All Rust tests.
test: daemon-test client-test

# Bump every version, commit and tag v<version>; CI publishes the release on push.
release version:
    scripts/release.sh {{version}}
