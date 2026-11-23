#!/bin/sh
# mapaki installer.
#
#   Server:          curl -fsSL https://raw.githubusercontent.com/Auxnon/mapaki/main/install.sh | sh -s -- daemon
#   Desktop client:  curl -fsSL https://raw.githubusercontent.com/Auxnon/mapaki/main/install.sh | sh -s -- client
#
# Options:
#   --version vX.Y.Z   install a specific release instead of the latest
#   --no-service       just install the binary, don't set up systemd
#
# The daemon runs as the user who invoked the installer (sudo is used only to
# place the binary and the system unit) and stores files in that user's
# ~/mapaki. The client installs to ~/.local/bin with a systemd *user* unit.
set -eu

REPO="Auxnon/mapaki"
what=""
version=""
service=1

while [ $# -gt 0 ]; do
    case "$1" in
        daemon|server) what=daemon ;;
        client|desktop) what=client ;;
        --version) version="$2"; shift ;;
        --no-service) service=0 ;;
        -h|--help) sed -n '2,16p' "$0" 2>/dev/null || true; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 1 ;;
    esac
    shift
done

if [ -z "$what" ]; then
    echo "usage: install.sh daemon|client [--version vX.Y.Z] [--no-service]" >&2
    exit 1
fi

say() { printf 'mapaki: %s\n' "$*"; }
die() { printf 'mapaki: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = "Linux" ] || die "this installer supports Linux only (for now)"
case "$(uname -m)" in
    x86_64|amd64) arch=x86_64 ;;
    aarch64|arm64) arch=aarch64 ;;
    *) die "unsupported architecture: $(uname -m)" ;;
esac

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO "$2" "$1"; }
else
    die "need curl or wget"
fi

if [ -n "${MAPAKI_RELEASE_BASE:-}" ]; then
    base="$MAPAKI_RELEASE_BASE" # testing against a local mirror of release assets
elif [ -n "$version" ]; then
    base="https://github.com/$REPO/releases/download/$version"
else
    base="https://github.com/$REPO/releases/latest/download"
fi

if [ "$what" = daemon ]; then asset="mapaki-daemon-linux-$arch"; bin=mapaki-daemon; else asset="mapaki-linux-$arch"; bin=mapaki; fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $asset ${version:-(latest)}"
fetch "$base/$asset" "$tmp/$bin" || die "download failed: $base/$asset"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "download failed: $base/SHA256SUMS"
expected="$(grep " $asset\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)"
actual="$(sha256sum "$tmp/$bin" | cut -d' ' -f1)"
[ -n "$expected" ] && [ "$expected" = "$actual" ] || die "checksum mismatch for $asset"
chmod 755 "$tmp/$bin"

as_root() {
    if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo "$@"; fi
}

has_systemd() { command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; }

install_daemon() {
    run_user="${SUDO_USER:-$(id -un)}"
    run_home="$(getent passwd "$run_user" | cut -d: -f6)"
    [ -n "$run_home" ] || run_home="$HOME"

    as_root install -m 755 "$tmp/$bin" /usr/local/bin/mapaki-daemon
    say "installed /usr/local/bin/mapaki-daemon ($(/usr/local/bin/mapaki-daemon --version))"

    if [ "$service" -eq 0 ] || ! has_systemd; then
        say "start it with: MAPAKI_DIR=$run_home/mapaki mapaki-daemon"
        return
    fi

    cat > "$tmp/mapaki-daemon.service" <<EOF
[Unit]
Description=mapaki-daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$run_user
ExecStart=/usr/local/bin/mapaki-daemon
Environment=MAPAKI_DIR=$run_home/mapaki
Environment=MAPAKI_PORT=8420
Environment=RUST_LOG=info
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF
    as_root install -m 644 "$tmp/mapaki-daemon.service" /etc/systemd/system/mapaki-daemon.service
    as_root systemctl daemon-reload
    as_root systemctl enable mapaki-daemon >/dev/null 2>&1
    as_root systemctl restart mapaki-daemon
    say "service running as $run_user, storing files in $run_home/mapaki, port 8420"

    sleep 1
    say "your first device:"
    if [ "$(id -un)" = "$run_user" ]; then
        MAPAKI_DIR="$run_home/mapaki" /usr/local/bin/mapaki-daemon pair-code 2>/dev/null
    else
        sudo -u "$run_user" env MAPAKI_DIR="$run_home/mapaki" /usr/local/bin/mapaki-daemon pair-code 2>/dev/null
    fi
    say "more devices later: mapaki-daemon pair-code   (status, devices, revoke: mapaki-daemon --help)"
}

install_client() {
    dest="$HOME/.local/bin"
    mkdir -p "$dest"
    install -m 755 "$tmp/$bin" "$dest/mapaki"
    say "installed $dest/mapaki ($("$dest/mapaki" --version))"
    case ":$PATH:" in *":$dest:"*) ;; *) say "note: add $dest to your PATH" ;; esac

    if [ "$service" -eq 1 ] && command -v systemctl >/dev/null 2>&1; then
        unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
        mkdir -p "$unit_dir"
        cat > "$unit_dir/mapaki.service" <<EOF
[Unit]
Description=mapaki folder sync
After=network-online.target

[Service]
ExecStart=$dest/mapaki run
Restart=on-failure
RestartSec=30

[Install]
WantedBy=default.target
EOF
        systemctl --user daemon-reload 2>/dev/null || true
        say "installed user service $unit_dir/mapaki.service"
    fi

    say "next:"
    say "  mapaki pair http://<server>:8420 <pairing-code>"
    say "  mapaki sync"
    say "  systemctl --user enable --now mapaki"
}

if [ "$what" = daemon ]; then install_daemon; else install_client; fi
