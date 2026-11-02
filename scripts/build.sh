#!/bin/sh
# Configures the Meson build directory, then compiles rsimgv into it.
#
#   scripts/build.sh                   debug build in builddir/
#   scripts/build.sh --release         optimised build
#   scripts/build.sh --prefix DIR      install prefix recorded in the build directory
#   scripts/build.sh --clean           delete the build directory first
#
# BUILD_DIR selects another build directory.
set -eu
cd "$(dirname "$0")/.."

builddir="${BUILD_DIR:-builddir}"
buildtype=debug
prefix=""
clean=false

while [ $# -gt 0 ]; do
  case "$1" in
    --release) buildtype=release ;;
    --prefix) prefix="$2"; shift ;;
    --prefix=*) prefix="${1#--prefix=}" ;;
    --clean) clean=true ;;
    -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
    *) echo "build.sh: unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

if $clean; then
  rm -rf "$builddir"
fi

set -- --buildtype="$buildtype"
if [ -n "$prefix" ]; then
  set -- "$@" --prefix="$prefix"
fi

if [ -f "$builddir/build.ninja" ]; then
  meson setup --reconfigure "$builddir" "$@"
else
  meson setup "$builddir" "$@"
fi
meson compile -C "$builddir"
