#!/bin/sh
# Builds the GSettings schema and GResource bundle, then runs the test suite.
#
#   scripts/test.sh                 run every test
#   scripts/test.sh NAME            run the tests whose names contain NAME
#
# The tests need a display. Without one, they run under xvfb-run.
set -eu
cd "$(dirname "$0")/.."

builddir="${BUILD_DIR:-builddir}"
if [ ! -f "$builddir/build.ninja" ]; then
  "$(dirname "$0")/build.sh"
else
  meson compile -C "$builddir"
fi

export GSETTINGS_SCHEMA_DIR="$PWD/$builddir/data"
export RSIMGV_GRESOURCE="$PWD/$builddir/rsimgv.gresource"

if [ -n "${DISPLAY:-}" ] || [ -n "${WAYLAND_DISPLAY:-}" ]; then
  exec cargo test -- --test-threads=1 "$@"
fi
exec xvfb-run -a cargo test -- --test-threads=1 "$@"
