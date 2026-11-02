#!/bin/sh
# Builds an optimised rsimgv and installs it with Meson.
#
#   scripts/install.sh                       install under /usr/local (needs root for that prefix)
#   scripts/install.sh --prefix /usr         install under another prefix
#   scripts/install.sh --destdir DIR         stage the install in DIR instead of the system
#
# Staging with --destdir writes the same files a package would contain, without touching the system.
# BUILD_DIR selects another build directory.
set -eu
cd "$(dirname "$0")/.."

builddir="${BUILD_DIR:-builddir}"
prefix=/usr/local
destdir=""

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) prefix="$2"; shift ;;
    --prefix=*) prefix="${1#--prefix=}" ;;
    --destdir) destdir="$2"; shift ;;
    --destdir=*) destdir="${1#--destdir=}" ;;
    -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
    *) echo "install.sh: unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

BUILD_DIR="$builddir" "$(dirname "$0")/build.sh" --release --prefix "$prefix"

set -- --no-rebuild
if [ -n "$destdir" ]; then
  set -- "$@" --destdir "$destdir"
fi
meson install -C "$builddir" "$@"
