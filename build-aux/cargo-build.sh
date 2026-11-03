#!/bin/sh
# Build the Rust crate and copy the binary to the Meson output path.
# Usage: cargo-build.sh <cargo> <manifest> <target-dir> <profile> <gresource> <svp-plugins> <output>
set -e

cargo="$1"
manifest="$2"
target_dir="$3"
profile="$4"
gresource="$(realpath "$5")"
svp_plugins="$(realpath "$6")"
output="$7"

flags=""
if [ "$profile" = release ]; then
  flags="--release"
fi

RSIMGV_GRESOURCE="$gresource" RSIMGV_SVP_PLUGINS="$svp_plugins" CARGO_TARGET_DIR="$target_dir" \
  "$cargo" build --manifest-path "$manifest" $flags
cp "$target_dir/$profile/rsimgv" "$output"
