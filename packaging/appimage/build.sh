#!/bin/bash
# Builds rsimgv-VERSION-x86_64.AppImage. Run it inside an Arch Linux environment (CI uses the archlinux
# container), as the build needs GTK >= 4.22 and libadwaita >= 1.9, which only current distributions ship.
# The AppImage bundles GTK, libadwaita, mpv and OpenCV, but not glibc, so it runs only on systems whose
# glibc is as new as the build environment's. Python plugin support is left out, as it would bundle CPython.
#
#   packaging/appimage/build.sh [OUTPUT_DIR]     default: dist
#
# Needs: base-devel, cargo, clang, meson, ninja, sassc, gtk4, libadwaita, mpv, opencv, wget, file, patchelf,
# desktop-file-utils, squashfs-tools. Downloads linuxdeploy and its GTK plugin.
set -euo pipefail
cd "$(dirname "$0")/../.."
root=$PWD
out=$(realpath -m "${1:-dist}")

version=$(sed -n "s/^  version: '\(.*\)',$/\1/p" meson.build)
work=$root/build-appimage
rm -rf "$work"
mkdir -p "$work/tools" "$out"

meson setup "$work/build" --prefix=/usr --buildtype=release -Dpython=disabled
meson compile -C "$work/build"
DESTDIR="$work/AppDir" meson install -C "$work/build"

cd "$work/tools"
wget -q https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20250213-2/linuxdeploy-x86_64.AppImage
wget -q https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh
chmod +x linuxdeploy-x86_64.AppImage linuxdeploy-plugin-gtk.sh

# The AppImage tools are run extracted, as CI containers have no FUSE.
export APPIMAGE_EXTRACT_AND_RUN=1
export PATH="$work/tools:$PATH"
export DEPLOY_GTK_VERSION=4
# linuxdeploy's bundled strip is too old to read the RELR relocations in current Arch libraries.
export NO_STRIP=1
export ARCH=x86_64
export VERSION=$version
export LINUXDEPLOY_OUTPUT_VERSION=$version

./linuxdeploy-x86_64.AppImage \
  --appdir "$work/AppDir" \
  --plugin gtk \
  --desktop-file "$work/AppDir/usr/share/applications/au.topher.rsimgv.desktop" \
  --icon-file "$root/data/icons/hicolor/256x256/apps/au.topher.rsimgv.png" \
  --output appimage

mv ./*.AppImage "$out/rsimgv-$version-x86_64.AppImage"
echo "$out/rsimgv-$version-x86_64.AppImage"
