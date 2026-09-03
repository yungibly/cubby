#!/usr/bin/env bash
# Package a release build into dist/cubby-<version>-<target>.tar.gz containing
# the binary, shell completions, README, and LICENSE.
set -euo pipefail

version="${1:?usage: package.sh VERSION TARGET}"
target="${2:?usage: package.sh VERSION TARGET}"
root="$(cd "$(dirname "$0")/.." && pwd)"
bin="$root/target/$target/release/cubby"
name="cubby-$version-$target"
stage="$root/dist/$name"

[ -x "$bin" ] || { echo "no binary at $bin" >&2; exit 1; }

rm -rf "$stage"
mkdir -p "$stage/completions"
cp "$bin" "$stage/cubby"
cp "$root/README.md" "$root/LICENSE" "$stage/"
for shell in bash zsh fish; do
  "$bin" completion "$shell" > "$stage/completions/cubby.$shell"
done

tar -C "$root/dist" -czf "$root/dist/$name.tar.gz" "$name"
rm -rf "$stage"
echo "dist/$name.tar.gz"
