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
# The build directory and the downloaded tools are kept between runs, so that only what changed is rebuilt: the
# Rust build alone, with the OpenCV bindings, takes minutes from scratch. Only the bundle is made afresh.
rm -rf "$work/AppDir" "$work/tools/squashfs-root"
mkdir -p "$work/tools" "$out"

if [ -d "$work/build/meson-private" ]; then
  meson setup "$work/build" --reconfigure --prefix=/usr --buildtype=release -Dpython=disabled
else
  meson setup "$work/build" --prefix=/usr --buildtype=release -Dpython=disabled
fi
meson compile -C "$work/build"
DESTDIR="$work/AppDir" meson install -C "$work/build"

cd "$work/tools"
wget -nc -q https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20250213-2/linuxdeploy-x86_64.AppImage
wget -nc -q https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh
chmod +x linuxdeploy-x86_64.AppImage linuxdeploy-plugin-gtk.sh

# The AppImage tools are run extracted, as CI containers have no FUSE.
export APPIMAGE_EXTRACT_AND_RUN=1
export PATH="$work/tools:$PATH"
export DEPLOY_GTK_VERSION=4
export ARCH=x86_64
export VERSION=$version
export LINUXDEPLOY_OUTPUT_VERSION=$version

# linuxdeploy bundles patchelf 0.15 and an old strip, which cannot handle the RELR relocations in current Arch
# libraries: strip fails on them, and patchelf corrupts them so that the loader segfaults. Swap in the system tools.
./linuxdeploy-x86_64.AppImage --appimage-extract > /dev/null
ln -sf "$(command -v patchelf)" squashfs-root/usr/bin/patchelf
ln -sf "$(command -v strip)" squashfs-root/usr/bin/strip

deploy_args=(
  --appdir "$work/AppDir"
  --desktop-file "$work/AppDir/usr/share/applications/au.topher.rsimgv.desktop"
  --icon-file "$root/data/icons/hicolor/256x256/apps/au.topher.rsimgv.png"
)
# The GTK plugin copies <libdir>/gtk-4.0 unconditionally, but on Arch that
# directory only exists when a package such as ibus installs modules into it.
mkdir -p "$(pkg-config --variable=libdir gtk4)/gtk-4.0"
squashfs-root/AppRun "${deploy_args[@]}" --plugin gtk

# The plugin's AppRun hook forces GTK_THEME, which makes GTK load its plain Adwaita theme instead of libadwaita's
# stylesheet and so breaks the app's styling, and forces the X11 backend even on Wayland. Neither is wanted.
sed -i '/^export GTK_THEME=/d; /^export GDK_BACKEND=/d' "$work/AppDir/apprun-hooks/linuxdeploy-plugin-gtk.sh"

# The GTK plugin copies the build machine's schemas into the bundle, replacing ours with any older copy installed
# there (such as a distribution package of rsimgv). Put ours back and recompile before the AppImage is written.
schemas=$work/AppDir/usr/share/glib-2.0/schemas
install -m644 "$root/data/au.topher.rsimgv.gschema.xml" "$schemas/"
glib-compile-schemas "$schemas"

squashfs-root/AppRun "${deploy_args[@]}" --output appimage

mv "rsimgv-$version-x86_64.AppImage" "$out/"
echo "$out/rsimgv-$version-x86_64.AppImage"
