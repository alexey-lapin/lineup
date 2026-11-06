#!/usr/bin/env bash
# Release notes for a tag, written to stdout.
#
# usage: release-notes.sh <tag> <image>
#
# GitHub's generated notes (configured by .github/release.yml) list merged pull requests. When the
# range has none, as with commits pushed straight to main, the commits since the previous tag are
# listed instead. Needs full git history and, for the generated notes, an authenticated gh.
set -euo pipefail

tag=$1
image=$2
version=${tag#v}
prev=$(git describe --tags --abbrev=0 --match 'v*' "$tag^" 2>/dev/null || true)

# Hand-written notes for this release (breaking changes, upgrade steps), read from the tag itself.
if notes=$(git show "$tag:docs/releases/$tag.md" 2>/dev/null); then
  printf '%s\n\n' "$notes"
fi

cat <<MD
## Install

\`\`\`sh
docker pull $image:$version   # linux/amd64, linux/arm64
\`\`\`

The CLI for macOS and Linux is attached below as \`lineup-$tag-<target>.tar.gz\`; \`SHA256SUMS\` lists their checksums.

MD

generated=""
if [[ -n ${GITHUB_REPOSITORY:-} ]]; then
  args=(-f "tag_name=$tag")
  [[ -n $prev ]] && args+=(-f "previous_tag_name=$prev")
  generated=$(gh api "repos/$GITHUB_REPOSITORY/releases/generate-notes" "${args[@]}" --jq .body || true)
fi

# Generated notes list pull requests as "* title by @author in <url>".
if grep -q '^\* .* in https://' <<<"$generated"; then
  echo "$generated"
else
  echo "## Changes"
  echo
  range=${prev:+$prev..}$tag
  git log --no-merges --reverse --format='- %s (%h)' "$range" | grep -v '^- Release [0-9]' || true
  echo
  if [[ -n $prev && -n ${GITHUB_REPOSITORY:-} ]]; then
    echo "**Full changelog**: https://github.com/$GITHUB_REPOSITORY/compare/$prev...$tag"
  fi
fi
