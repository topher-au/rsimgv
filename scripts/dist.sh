#!/bin/bash
# Packs the source tree into rsimgv-VERSION.tar.gz, for packaging such as packaging/arch/PKGBUILD.
#
#   scripts/dist.sh [--arch] [OUTPUT_DIR]     default: packaging/arch
#
#   --arch   also builds the Arch package from scratch with makepkg in packaging/arch, from the archive just written.
#            It takes no OUTPUT_DIR, as makepkg needs the archive beside the PKGBUILD.
#
# The archive holds the files on disk, so uncommitted changes are included. Untracked files that are
# not ignored are included too. Editor settings, the local cargo config and deleted files are left out.
#
# Submodules are the exception: each must be checked out at the commit the superproject records, with no
# local changes, so the archive builds the versions that the superproject pins.
set -euo pipefail
cd "$(dirname "$0")/.."

arch=false
out=packaging/arch
for arg in "$@"; do
  case "$arg" in
    --arch) arch=true ;;
    -*) echo "usage: scripts/dist.sh [--arch] [OUTPUT_DIR]" >&2; exit 2 ;;
    *) out="$arg" ;;
  esac
done
if [ "$arch" = true ] && [ "$out" != packaging/arch ]; then
  echo "error: --arch builds from packaging/arch; give no OUTPUT_DIR with it" >&2
  exit 2
fi

version=$(sed -n "s/^  version: '\(.*\)',$/\1/p" meson.build)
prefix="rsimgv-$version"

# `git submodule status` prints a leading space when the checkout matches the recorded commit, '+' when it
# differs, '-' when it is not initialised and 'U' when it has merge conflicts.
while IFS= read -r line; do
  [ -n "$line" ] || continue
  state="${line:0:1}"
  read -r sha path _ <<< "${line:1}"
  if [ "$state" != " " ]; then
    echo "error: submodule $path is not at its recorded commit $sha (run: git submodule update --init $path)" >&2
    exit 1
  fi
  if [ -n "$(git -C "$path" status --porcelain --untracked-files=no)" ]; then
    echo "error: submodule $path has uncommitted changes; commit or discard them before packaging" >&2
    exit 1
  fi
done < <(git submodule status)

# The submodule's files are listed as tracked by its own repository, as its build needs them.
mapfile -d '' listed < <(git ls-files -co --exclude-standard -z; git ls-files -z --recurse-submodules -- third_party/)
files=()
for file in "${listed[@]}"; do
  [ -f "$file" ] || continue
  case "$file" in
    .idea/*|.claude/*|.vscode/*|.cargo/*|.mcp.json) continue ;;
  esac
  files+=("$file")
done

mkdir -p "$out"
tar -czf "$out/$prefix.tar.gz" --transform "s,^,$prefix/," "${files[@]}"
echo "$out/$prefix.tar.gz"

if [ "$arch" = true ]; then
  # -C empties the work directories left by an earlier build, so the package is built from scratch.
  rm -f packaging/arch/*.pkg.tar.*
  (cd packaging/arch && makepkg -Cf)
fi
