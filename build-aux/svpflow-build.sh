#!/bin/sh
# Build open-svpflow's VapourSynth plugins and copy them to the Meson output directory.
# Usage: svpflow-build.sh <cargo> <source directory> <target directory> <output directory>
set -e

cargo="$1"
source="$2"
target_dir="$3"
output="$(realpath "$4")"

# Run from the source tree, so its own cargo config and lock file are used.
cd "$source"
CARGO_TARGET_DIR="$target_dir" "$cargo" build --release --locked -p svpflow1 -p svpflow2
cp "$target_dir/release/libsvpflow1_vs.so" "$target_dir/release/libsvpflow2_vs.so" "$output/"
