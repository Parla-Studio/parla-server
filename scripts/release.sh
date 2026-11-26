#!/usr/bin/env bash
# Prepare a release: set the version, commit and tag.
#   scripts/release.sh 2.2.0
# Normally run by the Release workflow (Actions › Release › Run workflow).
set -euo pipefail
version="${1:?usage: scripts/release.sh X.Y.Z}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "version must be X.Y.Z"; exit 1; }
cd "$(dirname "$0")/.."
git diff --quiet || { echo "working tree not clean"; exit 1; }

sed -i -E "0,/^version = \"[^\"]+\"/s//version = \"$version\"/" Cargo.toml
cargo update --workspace >/dev/null 2>&1 || cargo metadata --format-version 1 >/dev/null

git commit -am "Release $version"
git tag -a "v$version" -m "Parla Server $version"
echo "Tagged v$version. Push with: git push origin main v$version"
