#!/bin/bash
# Packs the source tree into rsimgv-VERSION.tar.gz, for packaging such as packaging/arch/PKGBUILD.
#
#   scripts/dist.sh [OUTPUT_DIR]     default: packaging/arch
#
# The archive holds the files on disk, so uncommitted changes are included. Untracked files that are
# not ignored are included too. Editor settings, the local cargo config and deleted files are left out.
set -euo pipefail
cd "$(dirname "$0")/.."

out="${1:-packaging/arch}"
version=$(sed -n "s/^  version: '\(.*\)',$/\1/p" meson.build)
prefix="rsimgv-$version"

mapfile -d '' listed < <(git ls-files -co --exclude-standard -z)
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
