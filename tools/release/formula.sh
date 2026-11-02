#!/bin/sh
# Write Formula/fac.rb for a published release:
#   tools/release/formula.sh 0.1.0 [arm64.sha256 intel.sha256]
# Without file arguments, the two .sha256 files are downloaded from the GitHub
# release v<version> with `gh`. With them, no network is used (the files are the
# ones the release workflow wrote, arm64 first). The formula goes to
# Formula/fac.rb in the current directory, or to $FORMULA_OUT when set.
set -eu

[ $# -eq 1 ] || [ $# -eq 3 ] || { echo "usage: $0 <version> [arm64.sha256 intel.sha256]" >&2; exit 2; }
version=$1
case $version in
  *[!0-9A-Za-z.+-]* | '') echo "bad version: $version" >&2; exit 2 ;;
esac

repo=${FAC_REPO:-itaikeren/fac}
here=$(cd "$(dirname "$0")" && pwd)
template="$here/../../packaging/homebrew/fac.rb.in"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ $# -eq 3 ]; then
  cp "$2" "$work/arm.sha256"
  cp "$3" "$work/intel.sha256"
else
  gh release download "v$version" --repo "$repo" --dir "$work" \
    --pattern "fac-$version-aarch64-apple-darwin.tar.gz.sha256" \
    --pattern "fac-$version-x86_64-apple-darwin.tar.gz.sha256"
  mv "$work/fac-$version-aarch64-apple-darwin.tar.gz.sha256" "$work/arm.sha256"
  mv "$work/fac-$version-x86_64-apple-darwin.tar.gz.sha256" "$work/intel.sha256"
fi

# A .sha256 file is "<64 hex>  <archive name>".
arm=$(awk '{ print $1; exit }' "$work/arm.sha256")
intel=$(awk '{ print $1; exit }' "$work/intel.sha256")
for sum in "$arm" "$intel"; do
  printf '%s' "$sum" | grep -Eq '^[0-9a-f]{64}$' || { echo "not a sha256: $sum" >&2; exit 1; }
done

out=${FORMULA_OUT:-Formula/fac.rb}
mkdir -p "$(dirname "$out")"
sed -e "s|@VERSION@|$version|g" -e "s|@SHA256_ARM@|$arm|" -e "s|@SHA256_INTEL@|$intel|" -e "s|@REPO@|$repo|g" "$template" > "$out"
echo "wrote $out"
