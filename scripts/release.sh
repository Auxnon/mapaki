#!/usr/bin/env bash
# Cut a release: bump every version to <version>, commit, and tag v<version>.
# Pushing the tag (--push, or `git push origin v<version>` later) triggers
# .github/workflows/release.yml, which builds and publishes all artifacts.
#
#   scripts/release.sh 0.2.0 [--push]
set -euo pipefail

version="${1:-}"
push="${2:-}"
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "usage: scripts/release.sh <major.minor.patch> [--push]" >&2
    exit 1
fi
cd "$(dirname "$0")/.."

if [ -n "$(git status --porcelain)" ]; then
    echo "working tree not clean; commit or stash first" >&2
    exit 1
fi
if git rev-parse -q --verify "refs/tags/v$version" >/dev/null; then
    echo "tag v$version already exists" >&2
    exit 1
fi

echo "$version" > VERSION
for crate in daemon client; do
    # Only the [package] version line (the first `version =` in the file).
    sed -i "0,/^version = \".*\"/s//version = \"$version\"/" "$crate/Cargo.toml"
    (cd "$crate" && cargo update --workspace --offline >/dev/null 2>&1 || cargo update --workspace >/dev/null)
    (cd "$crate" && cargo test --quiet)
done

git add VERSION daemon/Cargo.toml daemon/Cargo.lock client/Cargo.toml client/Cargo.lock
git commit -m "release: v$version"
git tag -a "v$version" -m "mapaki v$version"
echo "tagged v$version"

if [ "$push" = "--push" ]; then
    git push origin HEAD "v$version"
else
    echo "push with: git push origin HEAD v$version"
fi
