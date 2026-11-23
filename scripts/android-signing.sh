#!/usr/bin/env bash
# One stable signing key for the Android app, so every APK (debug or release,
# local or CI) installs over the previous one instead of needing an uninstall.
#
#   scripts/android-signing.sh init     create the key (once) in ~/.config/mapaki
#   scripts/android-signing.sh upload   store it as GitHub Actions secrets (needs `gh auth login`)
#
# Back up ~/.config/mapaki/android-release.jks and its .properties file. If the
# key is lost, the next APK can't update the installed app: users would have
# to uninstall (losing anything not yet synced) and pair again.
set -euo pipefail

dir="$HOME/.config/mapaki"
jks="$dir/android-release.jks"
props="$dir/android-signing.properties"
repo="${MAPAKI_REPO:-Auxnon/mapaki}"
root="$(cd "$(dirname "$0")/.." && pwd)"

keytool_bin() {
    if [ -x "$root/app/.jdk/bin/keytool" ]; then echo "$root/app/.jdk/bin/keytool"
    elif command -v keytool >/dev/null; then command -v keytool
    else echo "keytool not found: run \`just build\` once (it downloads a JDK) or install one" >&2; exit 1
    fi
}

prop() { sed -n "s/^$1=//p" "$props"; }

case "${1:-}" in
init)
    if [ -f "$jks" ]; then
        echo "key already exists: $jks"
        exit 0
    fi
    mkdir -p "$dir"
    chmod 700 "$dir"
    pass="$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')"
    (umask 077
     "$(keytool_bin)" -genkeypair -keystore "$jks" -storetype PKCS12 -alias mapaki \
         -keyalg RSA -keysize 4096 -validity 36500 -dname "CN=mapaki" \
         -storepass "$pass" -keypass "$pass" >/dev/null 2>&1
     printf 'storeFile=%s\nstorePassword=%s\nkeyAlias=mapaki\nkeyPassword=%s\n' "$jks" "$pass" "$pass" > "$props")
    echo "created $jks"
    echo "local builds now sign with it. Back up both files in $dir."
    echo "next: scripts/android-signing.sh upload   (so CI releases use the same key)"
    ;;
upload)
    [ -f "$props" ] || { echo "no key yet: run scripts/android-signing.sh init" >&2; exit 1; }
    gh auth status >/dev/null 2>&1 || { echo "gh isn't signed in: run \`gh auth login\` first" >&2; exit 1; }
    base64 -w0 "$jks" | gh secret set ANDROID_KEYSTORE_BASE64 --repo "$repo"
    prop storePassword | gh secret set ANDROID_KEYSTORE_PASSWORD --repo "$repo"
    prop keyAlias | gh secret set ANDROID_KEY_ALIAS --repo "$repo"
    prop keyPassword | gh secret set ANDROID_KEY_PASSWORD --repo "$repo"
    echo "secrets set on $repo; the next release APK is signed with this key"
    ;;
*)
    sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
