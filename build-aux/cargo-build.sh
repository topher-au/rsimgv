#!/bin/sh
# Build the Rust crate and copy the binary to the Meson output path.
# Usage: cargo-build.sh <cargo> <manifest> <target-dir> <profile> <gresource> <output>
set -e

cargo="$1"
manifest="$2"
target_dir="$3"
profile="$4"
gresource="$(realpath "$5")"
output="$6"

flags=""
if [ "$profile" = release ]; then
  flags="--release"
fi

RSIMGV_GRESOURCE="$gresource" CARGO_TARGET_DIR="$target_dir" \
  "$cargo" build --manifest-path "$manifest" $flags
cp "$target_dir/$profile/rsimgv" "$output"
