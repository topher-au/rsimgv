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
# makepkg sets RUSTFLAGS, which replaces the target-cpu in the source tree's cargo config. svpflow1 only imports
# some SSE2 intrinsics when SSSE3 or AVX2 is enabled, so SSSE3 is turned on here. It is in every x86-64 CPU since 2008.
RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-feature=+ssse3" \
CARGO_TARGET_DIR="$target_dir" "$cargo" build --release --locked -p svpflow1 -p svpflow2
cp "$target_dir/release/libsvpflow1_vs.so" "$target_dir/release/libsvpflow2_vs.so" "$output/"
