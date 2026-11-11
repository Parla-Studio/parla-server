#!/bin/sh
# Minimal Maven wrapper: downloads the Maven distribution named in
# .mvn/wrapper/maven-wrapper.properties once and runs it, so build machines
# only need a JDK. Respects MAVEN_USER_HOME (default ~/.m2).
set -e

BASEDIR=$(cd "$(dirname "$0")" && pwd)
PROPS="$BASEDIR/.mvn/wrapper/maven-wrapper.properties"
URL=$(sed -n 's/^distributionUrl=//p' "$PROPS" | tr -d '\r')
if [ -z "$URL" ]; then
  echo "mvnw: distributionUrl missing in $PROPS" >&2
  exit 1
fi

ARCHIVE=$(basename "$URL")
NAME=${ARCHIVE%-bin.tar.gz}
DISTS="${MAVEN_USER_HOME:-$HOME/.m2}/wrapper/dists"
MAVEN_HOME="$DISTS/$NAME"

if [ ! -x "$MAVEN_HOME/bin/mvn" ]; then
  echo "mvnw: downloading $URL" >&2
  mkdir -p "$DISTS"
  TMP=$(mktemp -d "$DISTS/.download.XXXXXX")
  trap 'rm -rf "$TMP"' EXIT
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL -o "$TMP/$ARCHIVE" "$URL"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$TMP/$ARCHIVE" "$URL"
  else
    echo "mvnw: neither curl nor wget is available to download Maven" >&2
    exit 1
  fi
  tar -xzf "$TMP/$ARCHIVE" -C "$TMP"
  # Atomic install, so a parallel build never sees a half-extracted Maven.
  if [ ! -x "$MAVEN_HOME/bin/mvn" ]; then
    mv "$TMP/$NAME" "$MAVEN_HOME" 2>/dev/null || true
  fi
fi

cd "$BASEDIR"
exec "$MAVEN_HOME/bin/mvn" "$@"
