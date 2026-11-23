#!/bin/bash
# Package an already-built x86_64 Linux binary on Linux; fail if it needs a newer ZOS ABI.
set -euo pipefail
if [[ $# != 3 || ! $1 =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
    echo "usage: build-deb.sh MAJOR.MINOR.PATCH LINUX_AMD64_BINARY OUTPUT_DIRECTORY" >&2
    exit 2
fi
version=$1
binary=$2
output=$3
here=$(cd "$(dirname "$0")" && pwd)
web=$here/../../../frontend/dist
test -s "$web/index.html"
if [[ $("$binary" --version) != "meshshare-headless $version" ]]; then
    echo "MESHSHARE_VERSION must match the Debian package version" >&2
    exit 1
fi
export LC_ALL=C
readelf -h "$binary" | grep -E 'Machine:.*X86-64' >/dev/null
required=$(readelf --version-info "$binary" | sed -nE 's/.*Name: GLIBC_([0-9.]+).*/\1/p' | sort -Vu | tail -n 1)
if [[ -z $required ]] || ! dpkg --compare-versions "$required" le 2.35; then
    echo "Expected a glibc binary requiring at most GLIBC_2.35; found $required" >&2
    exit 1
fi
if readelf --dynamic "$binary" | grep -Ei 'NEEDED.*(webkit|gtk|gdk|ayatana|wayland|libX11|libxcb)' >/dev/null; then
    echo "Unexpected desktop runtime dependency" >&2
    exit 1
fi

root=$(mktemp -d)
trap 'rm -rf -- "$root"' EXIT
mkdir -p "$root"/{DEBIAN,usr/bin,lib/systemd/system,usr/share/meshshare-headless/web,usr/share/doc/meshshare-headless} "$output"
install -m 0755 "$binary" "$root/usr/bin/meshshare-headless"
install -m 0644 "$here"/*.service "$here"/*.socket "$root/lib/systemd/system/"
install -m 0644 "$here/../../../LICENSE" "$root/usr/share/doc/meshshare-headless/copyright"
cp -R "$web/." "$root/usr/share/meshshare-headless/web/"
find "$root/usr/share" -type d -exec chmod 0755 {} +
find "$root/usr/share" -type f -exec chmod 0644 {} +
install -m 0755 "$here"/{postinst,prerm,postrm} "$root/DEBIAN/"
cat >"$root/DEBIAN/control" <<CONTROL
Package: meshshare-headless
Version: $version
Architecture: amd64
Maintainer: MeshShare <maintainers@users.noreply.github.com>
Section: net
Priority: optional
Depends: libc6 (>= $required), libgcc-s1, systemd (>= 249), init-system-helpers, adduser, ca-certificates
Description: MeshShare headless LAN clipboard and file sharing node
 Runs the MeshShare node and temporary mobile web page as a system service.
CONTROL
chmod 0755 "$root"
package=meshshare-headless_${version}_amd64.deb
dpkg-deb --build --root-owner-group -Zxz -z4 "$root" "$output/$package"
(cd "$output" && sha256sum "$package" > "$package.sha256")
