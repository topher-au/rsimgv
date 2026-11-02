#!/bin/sh
# Run a built rsimgv inside a sandbox so screenshots show only the demo files, and nothing touches your real settings.
#
#   launch.sh DEMO_HOME RSIMGV_CHECKOUT STATE_DIR [rsimgv arguments...]
#
# DEMO_HOME is a folder made by make_demo_media.py; it appears inside the sandbox as /home/demo. STATE_DIR is
# wiped and used for config, cache and runtime files. Set SEED to a YAML file to start from those settings, for
# example the window size. Needs bubblewrap, and an X display in $DISPLAY (Xvfb works: `Xvfb :99 -screen 0 1400x900x24`).
#
# Why bubblewrap: the folder tree is rooted at /, so without it the tree would list the real machine's folders.
# A fresh, empty STATE_DIR matters because launching rsimgv rewrites last-directory and last-file in its config.
set -eu
demo=$(realpath "$1"); repo=$(realpath "$2"); state=$3; shift 3
rm -rf "$state"; mkdir -p "$state/config/rsimgv" "$state/cache" "$state/data" "$state/run"; chmod 700 "$state/run"
state=$(realpath "$state")
[ -n "${SEED:-}" ] && cp "$SEED" "$state/config/rsimgv/rsimgv.conf"
export HOME=/home/demo XDG_CONFIG_HOME="$state/config" XDG_CACHE_HOME="$state/cache" XDG_DATA_HOME="$state/data" XDG_RUNTIME_DIR="$state/run"
export GSETTINGS_SCHEMA_DIR="$repo/builddir/data" RSIMGV_GRESOURCE="$repo/builddir/rsimgv.gresource"
export GDK_BACKEND=x11 GSK_RENDERER=cairo
exec bwrap --ro-bind / / --dev-bind /dev /dev --proc /proc --tmpfs /home --tmpfs /mnt \
  --bind "$demo" /home/demo --tmpfs /tmp --bind /tmp/.X11-unix /tmp/.X11-unix \
  --bind "$state" "$state" --bind "$repo" "$repo" "$repo/builddir/rsimgv" "$@"
