#!/usr/bin/env bash
#
# Adds Debian packages to an APT repository with reprepro:
#
#   publish.sh <repository directory> <suite> <package.deb>...
#
# The repository directory holds conf/distributions. reprepro signs the index with the key named
# there (SignWith), taken from the GnuPG keyring of the user running this script. The repository
# keeps one version of each package per suite: a newer version replaces the older one, and a
# version that is already published cannot be published again with different contents.
set -euo pipefail

if [ "$#" -lt 3 ]; then
    echo "usage: publish.sh <repository directory> <suite> <package.deb>..." >&2
    exit 2
fi
REPO="$1"
SUITE="$2"
shift 2

command -v reprepro >/dev/null || { echo "publish: reprepro is missing (sudo apt install reprepro)" >&2; exit 1; }
[ -f "$REPO/conf/distributions" ] || { echo "publish: $REPO/conf/distributions not found" >&2; exit 1; }
grep -qE "^(Codename|Suite):[[:space:]]*$SUITE[[:space:]]*$" "$REPO/conf/distributions" \
    || { echo "publish: suite '$SUITE' is not defined in $REPO/conf/distributions" >&2; exit 1; }

for deb in "$@"; do
    [ -f "$deb" ] || { echo "publish: $deb not found" >&2; exit 1; }
    reprepro -b "$REPO" includedeb "$SUITE" "$deb"
    echo "publish: $(basename "$deb") -> $SUITE"
done

reprepro -b "$REPO" list "$SUITE"
