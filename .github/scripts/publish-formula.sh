#!/usr/bin/env bash
# Commit a rendered formula to the Homebrew tap.
#
#   publish-formula.sh TAG FORMULA
#
# Needs GH_TOKEN (to list this repo's releases) and TAP_GITHUB_TOKEN (contents: read/write on
# alexey-lapin/homebrew-tap). Skips, without failing, when a newer stable release exists, so a
# rebuilt older tag never downgrades the tap, and when the tap already has the same formula.
set -euo pipefail

tag=$1 formula=$2
version=${tag#v}
tap=alexey-lapin/homebrew-tap
dest=Formula/lineup.rb

newest=$(gh release list --exclude-pre-releases --exclude-drafts --limit 100 --json tagName --jq '.[].tagName' |
  grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | sed 's/^v//' | sort -V | tail -n 1)
if [ -n "$newest" ] && [ "$(printf '%s\n%s\n' "$version" "$newest" | sort -V | tail -n 1)" != "$version" ]; then
  echo "skipped: $tap $dest stays on newer $newest"
  exit 0
fi

[ -n "${TAP_GITHUB_TOKEN:-}" ] || { echo "TAP_GITHUB_TOKEN is not set" >&2; exit 1; }
url="https://x-access-token:$TAP_GITHUB_TOKEN@github.com/$tap.git"
work=$(mktemp -d)
git clone -q --depth 1 "$url" "$work"
mkdir -p "$work/Formula"
cp "$formula" "$work/$dest"
git -C "$work" add "$dest"
if git -C "$work" diff --cached --quiet; then
  echo "unchanged: $tap already has this $dest for $tag"
  exit 0
fi
git -C "$work" -c user.name="github-actions[bot]" \
  -c user.email="github-actions[bot]@users.noreply.github.com" commit -qm "lineup $tag"
git -C "$work" push -q origin HEAD
echo "published: $tap $dest for $tag"
