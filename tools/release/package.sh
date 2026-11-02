#!/bin/sh
# Build the release `fac` for one target and package it:
#   tools/release/package.sh aarch64-apple-darwin
# Writes target/release-test/fac-<version>-<target>.tar.gz and a .sha256 next to it.
# The release workflow runs this same script. The target's Rust standard library
# must be installed (rustup target add <target>).
set -eu

[ $# -eq 1 ] || { echo "usage: $0 <target-triple>" >&2; exit 2; }
target=$1
case $target in
  aarch64-apple-darwin | x86_64-apple-darwin) ;;
  *) echo "unsupported target: $target" >&2; exit 2 ;;
esac

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

version=$(awk -F'"' '/^\[package\]/ { p = 1; next } /^\[/ { p = 0 } p && /^version[[:space:]]*=/ { print $2; exit }' Cargo.toml)
[ -n "$version" ] || { echo "no version in Cargo.toml" >&2; exit 1; }

cargo build --release --locked --bin fac --target "$target"

out="$root/target/release-test"
name="fac-$version-$target"
stage="$out/stage/$name"
rm -rf "$stage"
mkdir -p "$stage"
cp "target/$target/release/fac" LICENSE README.md THIRD_PARTY_NOTICES.md "$stage/"

archive="$out/$name.tar.gz"
tar -C "$stage" -czf "$archive" fac LICENSE README.md THIRD_PARTY_NOTICES.md
rm -rf "$out/stage"

# The checksum file names the archive without a directory, so `shasum -c` works beside it.
(cd "$out" && shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256")

echo "$archive"
cat "$archive.sha256"
