#!/usr/bin/env bash
#
# Copyright (c) 2026 MirraNET, Niklas Linz. All rights reserved.
#
# This file is part of the MirraNET project and is licensed under the
# GNU Lesser General Public License v3.0 (LGPLv3).
#
# You may use, distribute and modify this code under the terms
# of the LGPLv3 license. You should have received a copy of the
# license along with this file. If not, see <https://www.gnu.org/licenses/lgpl-3.0.html>
# or contact: niklas.linz@mirranet.de
#
# Adds Debian packages to the APT repository with reprepro:
#
#   publish.sh <repository directory> <suite> <package.deb>...
#
# The repository directory holds conf/distributions and conf/options (templates next to this
# script). reprepro signs the index with the key named in conf/distributions (SignWith), from
# the GnuPG keyring of the user running this script, usually jenkins. The repository keeps one
# version of each package per suite: a newer version replaces the older one, and a version that
# is already published cannot be published again with different contents. See
# docs/apt-repository.md.

set -euo pipefail

if [ "$#" -lt 3 ]; then
    echo "usage: publish.sh <repository directory> <suite> <package.deb>..." >&2
    exit 2
fi
REPO="$1"
SUITE="$2"
shift 2

command -v reprepro >/dev/null || { echo "publish: reprepro is missing (sudo apt install reprepro)" >&2; exit 1; }
[ -f "$REPO/conf/distributions" ] || { echo "publish: $REPO/conf/distributions not found (see docs/apt-repository.md)" >&2; exit 1; }
grep -qE "^(Codename|Suite):[[:space:]]*$SUITE[[:space:]]*$" "$REPO/conf/distributions" \
    || { echo "publish: suite '$SUITE' is not defined in $REPO/conf/distributions" >&2; exit 1; }

for deb in "$@"; do
    [ -f "$deb" ] || { echo "publish: $deb not found" >&2; exit 1; }
    reprepro -b "$REPO" includedeb "$SUITE" "$deb"
    echo "publish: $(basename "$deb") -> $SUITE"
done

# What the repository offers now.
reprepro -b "$REPO" list "$SUITE"
