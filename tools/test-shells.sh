#!/bin/sh
# Fills target/test-shells/ with the shells the bash and fish tests run
# under, pinned, and changes nothing outside it (`cargo clean` removes it):
#   fish/base/            the fish program from fish's official release
#                         (its SHA-256 checked); the .app is not kept
#   bash-completion/      scop/bash-completion at a release tag
# bash itself is Homebrew's (/opt/homebrew/bin/bash, 5.3+); the tests take
# another with FAC_TEST_BASH (and another fish with FAC_TEST_FISH).
# Safe to run again: what is already there and right is kept.
set -eu

FISH_VERSION=4.9.3
FISH_URL="https://github.com/fish-shell/fish-shell/releases/download/$FISH_VERSION/fish-$FISH_VERSION.app.zip"
FISH_SHA256=3619fa92d540ed0dff4816594429290550d21b92226e0523457e5f0242c5e9a8

BASH_COMPLETION_URL=https://github.com/scop/bash-completion.git
BASH_COMPLETION_TAG=2.18.0
BASH_COMPLETION_COMMIT=b80d38cce5645fb98acd4eba857843fdf66be570

root=$(cd "$(dirname "$0")/.." && pwd)
dir="$root/target/test-shells"
mkdir -p "$dir"

# fish: only the program is kept, never a launchable bundle. The .app's
# Contents/MacOS/fish is a launcher: running it, or anything that makes
# macOS launch the bundle, opens Terminal.app windows running fish in the
# real HOME. So its Contents/Resources/base (the plain fish program, which
# finds its data relative to itself) is copied out to fish/base, and the
# .app is unregistered from Launch Services and deleted. An .app left by
# the old layout is converted the same way.
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
drop_fish_apps() {
  for app in "$dir"/fish/*.app "$dir"/fish/.unpack/*.app; do
    [ -d "$app" ] || continue
    if [ -x "$lsregister" ]; then
      "$lsregister" -u "$app" 2>/dev/null || true
    fi
    rm -rf "$app"
  done
  rm -rf "$dir/fish/.unpack"
}
fish="$dir/fish/base/usr/local/bin/fish"
old_base="$dir/fish/fish-$FISH_VERSION.app/Contents/Resources/base"
if [ ! -d "$dir/fish/base" ] && [ -d "$old_base" ]; then
  echo "fish $FISH_VERSION: keeping only the program from the old .app"
  ditto "$old_base" "$dir/fish/base.part"
  mv "$dir/fish/base.part" "$dir/fish/base"
fi
drop_fish_apps
if [ -x "$fish" ] && [ "$(env -i HOME=/var/empty "$fish" --no-config --version 2>/dev/null)" = "fish, version $FISH_VERSION" ]; then
  echo "fish $FISH_VERSION: already in $dir/fish/base"
else
  zip="$dir/fish-$FISH_VERSION.app.zip"
  echo "fish $FISH_VERSION: downloading $FISH_URL"
  curl --fail --location --silent --show-error --output "$zip.part" "$FISH_URL"
  sum=$(shasum -a 256 "$zip.part" | cut -d ' ' -f 1)
  if [ "$sum" != "$FISH_SHA256" ]; then
    rm -f "$zip.part"
    echo "fish: SHA-256 $sum, expected $FISH_SHA256; not unpacked" >&2
    exit 1
  fi
  mv "$zip.part" "$zip"
  rm -rf "$dir/fish"
  mkdir -p "$dir/fish/.unpack"
  ditto -x -k "$zip" "$dir/fish/.unpack"
  rm -f "$zip"
  ditto "$dir/fish/.unpack/fish-$FISH_VERSION.app/Contents/Resources/base" "$dir/fish/base"
  drop_fish_apps
  env -i HOME=/var/empty "$fish" --no-config --version
fi

# bash-completion: a shallow clone of the pinned tag.
bc="$dir/bash-completion"
if [ "$(git -C "$bc" rev-parse HEAD 2>/dev/null)" = "$BASH_COMPLETION_COMMIT" ]; then
  echo "bash-completion $BASH_COMPLETION_TAG: already in $bc"
else
  rm -rf "$bc"
  git -c advice.detachedHead=false clone --quiet --depth 1 --branch "$BASH_COMPLETION_TAG" \
    "$BASH_COMPLETION_URL" "$bc"
  head=$(git -C "$bc" rev-parse HEAD)
  if [ "$head" != "$BASH_COMPLETION_COMMIT" ]; then
    rm -rf "$bc"
    echo "bash-completion: $BASH_COMPLETION_TAG is $head, expected $BASH_COMPLETION_COMMIT; removed" >&2
    exit 1
  fi
  echo "bash-completion $BASH_COMPLETION_TAG: cloned into $bc"
fi
