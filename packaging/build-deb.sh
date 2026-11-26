#!/bin/sh
# Builds target/debian/parla-server_<version>_<arch>.deb from the release binary.
# Needs dpkg-deb, or ar + tar + gzip when dpkg is not installed. Run from anywhere: packaging/build-deb.sh
# Set SKIP_BUILD=1 to package an existing target/release binary without running cargo.
# Set DEB_VERSION to give the package another version than Cargo.toml.
set -e

HERE=$(cd "$(dirname "$0")" && pwd)
SERVER=$(cd "$HERE/.." && pwd)
cd "$SERVER"

[ -n "$SKIP_BUILD" ] || cargo build --release --locked
BIN="$SERVER/target/release/parla-server"
[ -x "$BIN" ] || { echo "build-deb: $BIN not found" >&2; exit 1; }

VERSION=${DEB_VERSION:-$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n 1)}
# Debian architecture name; without dpkg derived from uname.
deb_arch() {
    if command -v dpkg >/dev/null 2>&1; then
        dpkg --print-architecture
        return
    fi
    case "$(uname -m)" in
        x86_64) echo amd64 ;;
        aarch64) echo arm64 ;;
        armv7l) echo armhf ;;
        *) uname -m ;;
    esac
}
ARCH=$(deb_arch)

# Builds the .deb from a package tree with DEBIAN/ inside. Uses dpkg-deb when it is there; otherwise builds the
# same archive by hand (a .deb is an ar archive of debian-binary, control.tar.gz and data.tar.gz), so build
# machines without dpkg only need ar (binutils), tar and gzip.
build_deb() { # <package tree> <output .deb>
    if command -v dpkg-deb >/dev/null 2>&1; then
        dpkg-deb --build --root-owner-group -Zxz "$1" "$2" >&2
        return
    fi
    command -v ar >/dev/null 2>&1 || { echo "build-deb: neither dpkg-deb nor ar found (install dpkg or binutils)" >&2; exit 1; }
    parts=$(mktemp -d)
    echo "2.0" > "$parts/debian-binary"
    (cd "$1/DEBIAN" && tar --owner=0 --group=0 --numeric-owner -czf "$parts/control.tar.gz" .)
    (cd "$1" && tar --owner=0 --group=0 --numeric-owner --exclude=./DEBIAN -czf "$parts/data.tar.gz" .)
    rm -f "$2"
    (cd "$parts" && ar rc "$2" debian-binary control.tar.gz data.tar.gz)
    rm -rf "$parts"
    echo "build-deb: built $2 without dpkg-deb" >&2
}

# Runtime dependencies: SQLite is built in, so only glibc and libgcc remain.
GLIBC_MIN=$(objdump -T "$BIN" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -V | tail -n 1)
DEPENDS="libc6 (>= ${GLIBC_MIN:-2.31}), libgcc-s1"

ROOT=$(mktemp -d)
trap 'rm -rf "$ROOT"' EXIT
PKG="$ROOT/parla-server"

install -D -m 0755 "$BIN" "$PKG/usr/bin/parla-server"
install -D -m 0644 "$HERE/parla.toml" "$PKG/etc/parla/parla.toml"
install -D -m 0644 "$HERE/parla.service" "$PKG/usr/lib/systemd/system/parla.service"
install -D -m 0644 "$SERVER/README.md" "$PKG/usr/share/doc/parla-server/README.md"
install -D -m 0644 "$SERVER/content/README.md" "$PKG/usr/share/doc/parla-server/course-format.md"
install -D -m 0644 "$SERVER/deploy/nginx-parla.conf" "$PKG/usr/share/doc/parla-server/examples/nginx-parla.conf"
cat > "$PKG/usr/share/doc/parla-server/copyright" <<COPYRIGHT
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: parla
Source: https://github.com/Parla-Studio/parla-server

Files: *
Copyright: $(date +%Y) Niklas Linz <niklas.linz@mirranet.de>
License: MIT
COPYRIGHT

mkdir -p "$PKG/DEBIAN"
SIZE=$(du -sk --exclude=DEBIAN "$PKG" | cut -f1)
sed -e "s/@VERSION@/$VERSION/" -e "s/@ARCH@/$ARCH/" -e "s/@SIZE@/$SIZE/" -e "s/@DEPENDS@/$DEPENDS/" \
    "$HERE/debian/control.in" > "$PKG/DEBIAN/control"
install -m 0644 "$HERE/debian/conffiles" "$PKG/DEBIAN/conffiles"
for script in postinst prerm postrm; do
    install -m 0755 "$HERE/debian/$script" "$PKG/DEBIAN/$script"
done

OUT="$SERVER/target/debian"
mkdir -p "$OUT"
DEB="$OUT/parla-server_${VERSION}_${ARCH}.deb"
build_deb "$PKG" "$DEB"
echo "$DEB"
