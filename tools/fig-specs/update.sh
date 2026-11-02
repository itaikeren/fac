#!/bin/sh
# Download the pinned @withfig/autocomplete tarball named in
# vendor/fig-autocomplete/VERSION, check its integrity, and pack it (plus
# specs/overrides) into specs/fig.zst, specs/index.txt and
# specs/pack-report.txt.
set -eu

repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"

version_file=vendor/fig-autocomplete/VERSION
url=$(sed -n 2p "$version_file")
integrity=$(sed -n 3p "$version_file")
expected=${integrity#sha512-}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' INT TERM

echo "Downloading $url"
curl -fsSL -o "$tmp/package.tgz" "$url"

actual=$(openssl dgst -sha512 -binary "$tmp/package.tgz" | base64 | tr -d '\n')
if [ "$actual" != "$expected" ]; then
    echo "integrity mismatch: expected sha512-$expected, got sha512-$actual" >&2
    exit 1
fi

tar -xzf "$tmp/package.tgz" -C "$tmp"

# fac embeds the archive, so the crate needs these files to build the packer;
# empty ones do on a clean checkout.
[ -f specs/fig.zst ] || : >specs/fig.zst
[ -f specs/index.txt ] || : >specs/index.txt

cargo run --release --features pack --bin fac-pack-specs -- \
    "$tmp/package/build" specs/overrides specs >/dev/null

sed -n '/^Source modules/,/^Overrides/p' specs/pack-report.txt
sed -n '/^Entries/,$p' specs/pack-report.txt
