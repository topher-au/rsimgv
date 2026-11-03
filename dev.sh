#!/bin/sh
# Development helper: rebuilds rsimgv and runs it from the build directory.
#
#   ./dev.sh             rebuild, then run rsimgv (extra arguments are passed to it)
#   ./dev.sh test        rebuild, then run the test suite
#   ./dev.sh gdb         rebuild, then run rsimgv under gdb (extra arguments are passed to it)
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

if [ "${1:-}" = "gdb" ]; then
    shift
    # gdb starts the program without the session's D-Bus and runtime variables, so they are given to it here.
    runtime_dir="/run/user/$(id -u)"
    exec rust-gdb \
        -ex "set environment DBUS_SESSION_BUS_ADDRESS unix:path=$runtime_dir/bus" \
        -ex "set environment XDG_RUNTIME_DIR $runtime_dir" \
        -ex run -ex bt -ex "info threads" \
        --args "$PWD/builddir/rsimgv" "$@"
fi

exec "$PWD/builddir/rsimgv" "$@"
