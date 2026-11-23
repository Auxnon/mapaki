# mapaki

A small self-hosted file drop: one big folder of files, notes, links and images that
lives on a server at home and stays in sync with your phone and computers. It's like
a stripped-down ownCloud, with git-style versioning so concurrent edits are merged
or flagged, never silently lost.

- **`daemon/`**: `mapaki-daemon`, the server (Rust/axum). It stores the folder, the
  version history and the change feed.
- **`client/`**: `mapaki`, the desktop sync client (Rust). It keeps a local folder in
  sync and runs as a background service.
- **`app/`**: the Android app (Kotlin/Compose). Share anything to it; every file is
  kept on the device.

## Install

### Server (Linux)

```sh
curl -fsSL https://raw.githubusercontent.com/Auxnon/mapaki/main/install.sh | sh -s -- daemon
```

This installs `/usr/local/bin/mapaki-daemon` and a systemd service. The service runs
as you, stores files in `~/mapaki`, listens on port 8420 and starts on boot. At the
end the installer prints a **pairing code** for your first device. Each code is valid
for 10 minutes and works once. On the server:

```sh
mapaki-daemon pair-code          # a code for another device, plus the address to enter
mapaki-daemon status             # running? at what address? which files?
mapaki-daemon devices            # paired devices and when each was last seen
mapaki-daemon revoke 2           # unpair device #2 (or by name); its access stops at once
```

You can also drop files straight into `~/mapaki` on the server, or edit them there.
The daemon notices right away and records them as revisions. A file that's still
being copied in is left alone until it has stopped changing for 2 seconds, so a
half-written copy never reaches your devices.

### Linux desktop client

```sh
curl -fsSL https://raw.githubusercontent.com/Auxnon/mapaki/main/install.sh | sh -s -- client
mapaki pair http://<server-ip>:8420 <pairing-code>   # syncs ~/Mapaki by default (--folder DIR to change)
mapaki sync                                          # first sync, asks about any conflicts
systemctl --user enable --now mapaki                 # keep it running in the background
```

To send files without keeping a folder in sync (handy over ssh or in scripts):

```sh
mapaki example.txt photo.jpg      # same as `mapaki send example.txt photo.jpg`
mapaki send -i example.txt        # ask first if the name is taken
```

A name that's already on the server gets a free one (`example-2.txt`), so nothing is
ever overwritten. With `-i` you choose: **overwrite** (saved as a new revision, so the
old content is still in `mapaki history`), **rename**, or **cancel**. Pressing Enter
renames. Sent files are also copied into your local folder. A local copy with edits
that haven't synced yet is left alone, and the next `mapaki sync` sorts it out like
any other conflict.

See what's on the server and whether each file is in sync on this computer:

```sh
mapaki ls
```

Browse, search and edit your notes in the terminal:

```sh
mapaki notes
```

`/` searches, `Enter` reads, `e` edits the note in `$EDITOR` (the title on the
first line, then the body), `n` writes a new one, and `o` opens a note's images in
your image viewer. If `mapaki run` is active, it syncs your edits. Otherwise
`mapaki notes` syncs when it opens and after each save.

If the server is reachable at more than one address (say, LAN first and a public
hostname as fallback), add the others. They are tried in order:

```sh
mapaki url add https://mapaki.example.com
mapaki url list
```

### Android

Download `mapaki-android.apk` from the
[latest release](https://github.com/Auxnon/mapaki/releases/latest) and install it.
You'll need to allow installs from your browser or file manager. Open the app and
enter a pairing code.

Share links, text, images or any other file to **Mapaki**. A shared URL is saved as
a `.link` with the page's title and a small preview image. Shared text becomes a
`.note`. Tap **+** to write a note (with photos) or import files. Tap a note to read
it, then the pencil to edit it. Tap a link to open it in your browser. Tap anything
else to open it in the app for its type: an `.apk` opens the installer (Android asks
once to allow installs from mapaki), a PDF your PDF reader, and so on. **Export** in a
tile's menu still saves a copy elsewhere.

Sharing doesn't open the app. A card slides up over the app you shared from, showing
the link's title and preview (or the image, or the start of the note) and whether it
reached the server, then slides away by itself. Tap it to open mapaki, or tap anywhere
else to dismiss it. If the server isn't reachable, the share is saved on the phone and uploaded
on the next sync (when you open the app, share again, or the background check runs).
A share always arrives as a new file. If its name is taken by then, it gets a free
one, so a share that waited offline never overwrites anything.

Both installer commands take `--version vX.Y.Z` to install a specific release, and
`--no-service` to install only the binary.

## How sync works

**Versioning.** Every file has its own revision history, like a git log. Each
revision records its content hash, its parent revision, and the device that made it.
A device remembers which revision its copy is based on, and sends that base along
with every upload. The server then decides:

| The uploader's base is… | Result |
|---|---|
| the current head | **fast-forward**: saved, no questions |
| an older revision, the file is text, and the edits don't overlap | **merged** automatically (3-way merge; history gets a two-parent merge revision) |
| an older revision, and the file is binary or the edits overlap | **conflict**: you choose keep **local**, **remote**, or **both** (yours is renamed `name (conflict copy).ext`) |
| unknown (a new file that happens to share a name) | conflict, unless the content is identical |

Deletes work the same way. A delete based on a stale revision is refused, so a file
someone else just edited isn't removed. Deleted files are kept as tombstones on the
server. On desktops, files removed by sync go to `~/.local/share/mapaki/trash`
rather than being deleted outright.

The server keeps the content of the last 20 revisions per file. Older revisions keep
their metadata, so ancestry checks still work, but you can't fetch their content.

```sh
mapaki history notes.txt              # list revisions (* marks the one you have)
mapaki restore notes.txt <revision>   # bring an old revision back as a new edit
mapaki conflicts                      # conflicts the background service couldn't ask about
mapaki resolve notes.txt both         # local | remote | both
```

**When devices check in.** The server never connects to clients. Clients always
connect out to the server, which works behind NAT, firewalls and Android's
background limits.

- **Desktop (`mapaki run`):**
  - Remote changes: the client holds one idle connection open to `/events`
    (server-sent events) and the server pushes a notice when anything changes.
    While connected, nothing is polled.
  - Local changes: the OS file watcher reports them, and the client syncs after a
    2-second quiet period.
  - When the server is unreachable, the client waits 1, 4, 16, then 60 minutes
    between attempts, capped at an hour. The wait resets to 1 minute when you edit
    a file locally or the machine wakes from sleep. On wake it also syncs right away.
- **Android:**
  - The app syncs every time it opens and every time you share to it.
  - While the app is on screen, it holds the same live connection as the desktop
    client.
  - In the background, WorkManager checks after 15 minutes, then 1 hour, then every
    4 hours. Only unmetered (Wi-Fi) networks are used. Any change found resets the
    wait to 15 minutes.

## Notes and links

Notes (`.note`) and links (`.link`) are small TOML files, so they sync, version and
merge like any other text file, and you can edit them by hand. Images are embedded
as base64, so a note and its pictures are always one file.

```toml
# Groceries.note
title = "Groceries"
created = "2026-11-22T18:00:00Z"
body = """
Milk, eggs.
![receipt](receipt.jpg)
"""

[[images]]
name = "receipt.jpg"
type = "image/jpeg"
data = "/9j/4AAQSkZJRgABAQ..."
```

A body line that is just `![alt](name)` shows that image there. Images the body
doesn't place show at the end. The app shrinks large photos to at most 1600 px
before embedding them.

A note's tile in the app shows its cover image cropped into the top, with the title
and the start of the text below; the text is cut off past 160 characters. The cover
is the first image the body places, or the first attached one. `mapaki notes` shows
the same snippet under each title.

```toml
# Some article.link
url = "https://example.com/article"
title = "Some article"
description = "What the page says about itself"
site = "example.com"
saved = "2026-11-22T18:00:00Z"

[preview]                  # optional: the page's og:image, 320 px wide
name = "preview.jpg"
type = "image/jpeg"
data = "/9j/4AAQSkZJRgABAQ..."
```

Every client writes exactly the same layout (both test suites check against
`fixtures/sample.note`). Edits to different parts of a note from two devices
therefore merge cleanly on the server. Reading is lenient: any valid TOML works, a
plain text file opens as a note, and v0.1 `.link` files (just the URL) still open.

## Building from source

```sh
just test            # daemon + client tests
just daemon-build    # daemon/target/release/mapaki-daemon
just client-build    # client/target/release/mapaki
just build           # Android debug APK (downloads a local JDK + SDK the first time)
```

## Releasing

Versions are kept in sync across all three parts. `VERSION` is the source of truth;
the Android `versionName` and `versionCode` are derived from it.

```sh
scripts/release.sh 0.2.0 --push    # bump VERSION + Cargo.toml files, run tests, commit, tag v0.2.0, push
```

Pushing a `v*` tag runs `.github/workflows/release.yml`. It builds static
x86_64/aarch64 Linux binaries of the daemon and client plus the Android APK, and
publishes them to a GitHub release with `SHA256SUMS`. The installer verifies
downloads against that file.

Every APK should be signed with the same key, so each one installs over the last
(debug builds over releases too) without uninstalling:

```sh
scripts/android-signing.sh init      # once: creates the key in ~/.config/mapaki; local builds use it
gh auth login
scripts/android-signing.sh upload    # stores it as repository secrets, so CI releases use it too
```

Back up `~/.config/mapaki/android-release.jks` and `android-signing.properties`. If
the key is lost, the next APK can't update the installed app. Without the secrets, CI
signs each release with a throwaway key, so you'd have to uninstall the app (losing
anything not yet synced) and pair again before installing a new version.

## API

All endpoints except `/health`, `/version` and `/pair` need `Authorization: Bearer <token>`.

| Method | Path | |
|---|---|---|
| POST | `/pair` | `{otp, device}` → `{token}` |
| GET | `/files` | live files in display order |
| POST | `/files` | multipart upload (`name`, `file`), creates a new file |
| POST | `/files/new?name=` | raw-body upload, creates a new file |
| PUT | `/files/:id/content?base=&force=` | new content; 200 `{result: fast_forward\|merged\|unchanged, entry}` or 409 `{entry}` |
| GET | `/files/:id/content[?rev=]` | current or past content |
| GET | `/files/:id/history` | revisions, oldest first |
| PATCH | `/files/:id` | `{name}` rename |
| DELETE | `/files/:id[?base=]` | delete; 409 if the file changed since `base` |
| PUT | `/order` | `{ids}` reorder |
| GET | `/changes?since=N` | `{seq, entries}`: everything changed after cursor `N`, tombstones included |
| GET | `/events` | server-sent events: `seq` on connect and on every change |

## Status

- **v0.1:** server, Linux desktop client and Android app are working.
- **Windows and macOS:** the client is written to be portable (it uses each OS's
  file watcher, and sends notifications on macOS) but isn't packaged or tested on
  those systems yet.
- **Android note edits need the server:** saving an edit offline keeps the
  editor open rather than queueing it. New notes and shares do queue.
- **Not yet supported:**
  - Subfolders: the sync folder is one flat folder.
  - Rename tracking: a rename is detected only when the content is unchanged.
