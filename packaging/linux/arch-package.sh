#!/usr/bin/env bash
# Repackage a PlutarchPages Linux tarball as an Arch Linux package:
#
#   $DIST/pdfcraft-<version>-linux-<arch>.pkg.tar.zst
#
# Usage: packaging/linux/arch-package.sh <x86_64|aarch64> [TARBALL]
#
# TARBALL defaults to $DIST/pdfcraft-<version>-linux-<arch>.tar.gz from package.sh. No Rust build, and
# nothing in the tarball runs here, so an x86_64 machine can package aarch64. Needs makepkg: on another
# distribution run this as root in an archlinux:base-devel container, because makepkg refuses root
# and this script then re-runs itself as an unprivileged user.
#
# The package is `plutarchpages`, not upstream's `ai.storyteller.pdfcraft`. Both install
# /usr/bin/pdfcraft, so it provides and conflicts with `pdfcraft`, which upstream's provides too.
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"

ARCH="${1:?usage: arch-package.sh <x86_64|aarch64> [TARBALL]}"
case "$ARCH" in
  x86_64 | aarch64) ;;
  *) echo "unsupported architecture $ARCH" >&2; exit 2 ;;
esac
TARBALL="${2:-$DIST/pdfcraft-$VERSION-linux-$ARCH.tar.gz}"
[[ -f "$TARBALL" ]] || { echo "error: $TARBALL not found (run packaging/linux/package.sh --formats tar first)" >&2; exit 1; }

if [[ "$(id -u)" = 0 ]]; then
  id -u builder >/dev/null 2>&1 || useradd -m builder
  chmod 777 "$DIST"
  exec runuser -u builder -- env HOME=/home/builder bash "${BASH_SOURCE[0]}" "$@"
fi

# Arch's pkgver can't hold a hyphen, and 0.2.0rc.1 sorts before 0.2.0 as 0.2.0-rc.1 does.
PKGVER="${VERSION//-/}"
[[ "$PKGVER" =~ ^[A-Za-z0-9._+]+$ ]] || { echo "error: can't make a pkgver from $VERSION" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir "$WORK/stage"
tar -xzf "$TARBALL" -C "$WORK/stage" --strip-components=1

cat >"$WORK/makepkg.conf" <<EOF
source /etc/makepkg.conf
CARCH=$ARCH
CHOST=$ARCH-unknown-linux-gnu
PKGEXT=.pkg.tar.zst
EOF

cat >"$WORK/PKGBUILD" <<EOF
pkgname=plutarchpages
pkgver=$PKGVER
pkgrel=1
pkgdesc='Read, organize, combine, split and secure PDFs'
arch=($ARCH)
url='https://github.com/EquityBot/PlutarchPages'
license=('MIT OR Apache-2.0')
depends=(glibc gcc-libs hicolor-icon-theme libxkbcommon vulkan-icd-loader)
optdepends=(
  'wayland: native Wayland display support'
  'libx11: X11 and XWayland display support'
  'libxcursor: X11 cursor handling'
  'libxi: X11 input handling'
  'libxrandr: X11 display resolution handling'
  'libglvnd: GPU rendering through EGL'
  'mesa: GPU rendering through EGL'
  'vulkan-driver: GPU rendering through Vulkan'
  'xdg-desktop-portal: native desktop integration and file dialogs'
  'cups: printing support'
)
provides=("pdfcraft=$PKGVER")
conflicts=(pdfcraft)
# Stripped in package.sh, and strip here couldn't read the other architecture's binaries.
options=(!strip !debug)

package() {
  install -d "\$pkgdir/usr" "\$pkgdir/usr/share/licenses/\$pkgname"
  cp -dr --no-preserve=ownership "$WORK/stage/bin" "$WORK/stage/share" "\$pkgdir/usr/"
  find "\$pkgdir/usr/share/doc/pdfcraft" -maxdepth 1 -type f \( -name 'LICENSE*' -o -name NOTICE -o -name 'OFL-*' \) \
    -exec cp -t "\$pkgdir/usr/share/licenses/\$pkgname" {} +
}
EOF

(cd "$WORK" && makepkg --config "$WORK/makepkg.conf" --nodeps --force)
built=("$WORK"/plutarchpages-*.pkg.tar.zst)
[[ "${#built[@]}" = 1 ]] || { echo "error: expected 1 package, found: ${built[*]}" >&2; exit 1; }
OUT="$DIST/pdfcraft-$VERSION-linux-$ARCH.pkg.tar.zst"
cp -f "${built[0]}" "$OUT"

pacman -Qip "$OUT"
listing="$(pacman -Qlp "$OUT")"
for path in usr/bin/pdfcraft usr/bin/pdfcraft-cli usr/share/applications/ai.storyteller.pdfcraft.desktop \
  usr/share/metainfo/ai.storyteller.pdfcraft.metainfo.xml usr/share/pdfcraft/models/ATTRIBUTION.txt \
  usr/share/licenses/plutarchpages/LICENSE-MIT; do
  grep -qF "/$path" <<<"$listing" || { echo "error: $OUT has no $path" >&2; exit 1; }
done
echo "wrote $OUT"
