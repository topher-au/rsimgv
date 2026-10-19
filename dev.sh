#!/bin/sh
# Development helper: rebuilds rsimgv and runs it from the build directory.
#
#   ./dev.sh             rebuild, then run rsimgv (extra arguments are passed to it)
#   ./dev.sh test        rebuild, then run the test suite
#
# The GSettings schema and the GResource bundle are both produced in builddir/, so the schema
# directory is pointed there and the test binary is given the bundle's path.
set -e
cd "$(dirname "$0")"

if [ ! -d builddir ]; then
    meson setup builddir
fi
meson compile -C builddir

# Compiled every time, so a renamed or edited schema is never stale.
mkdir -p builddir/data
glib-compile-schemas --targetdir="$PWD/builddir/data" data
export GSETTINGS_SCHEMA_DIR="$PWD/builddir/data"
export RSIMGV_GRESOURCE="$PWD/builddir/rsimgv.gresource"

if [ "${1:-}" = "test" ]; then
    shift
    exec cargo test -- --test-threads=1 "$@"
fi

exec "$PWD/builddir/rsimgv" "$@"
