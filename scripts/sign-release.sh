#!/usr/bin/env bash
# Signs the draft release that release.yml made for a tag, then publishes it:
#   scripts/sign-release.sh v0.2.0
# Needs gh, signed in, minisign and sha256sum. MINISIGN_KEY overrides the
# secret key, ~/.minisign/domestique.key.
set -euo pipefail

tag=${1:?usage: scripts/sign-release.sh vX.Y.Z}
repo=mikaellecam/domestique
pub=$(cd "$(dirname "$0")/.." && pwd)/minisign.pub
archives=(domestique-windows-x86_64.zip domestique-linux-x86_64.tar.gz domestique-macos-arm64.zip)

if [ "$(gh release view "$tag" --repo "$repo" --json isDraft --jq .isDraft)" != true ]; then
  echo "$tag is not a draft release" >&2
  exit 1
fi
echo "$tag is $(gh api "repos/$repo/commits/$tag" --jq '.sha + " " + (.commit.message | split("\n")[0])')"

dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
cd "$dir"
for a in "${archives[@]}"; do
  gh release download "$tag" --repo "$repo" --pattern "$a"
done
sha256sum "${archives[@]}" > SHA256SUMS
minisign -S -s "${MINISIGN_KEY:-$HOME/.minisign/domestique.key}" -m SHA256SUMS -t "domestique $tag"
# A wrong key fails here, before anything is published.
minisign -V -p "$pub" -m SHA256SUMS

gh release upload "$tag" --repo "$repo" SHA256SUMS SHA256SUMS.minisig
gh release edit "$tag" --repo "$repo" --draft=false
echo "Published https://github.com/$repo/releases/tag/$tag"
