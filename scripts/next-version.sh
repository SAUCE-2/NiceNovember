#!/usr/bin/env bash
# Prints the semantic version to release HEAD as, from the Conventional Commits since the last
# vX.Y.Z tag: a breaking change (`type!:` or a `BREAKING CHANGE:` footer) bumps major, a `feat`
# minor, anything else patch. HEAD already tagged keeps its version; no tag yet counts from 0.0.0.
set -euo pipefail

last=$(git describe --tags --abbrev=0 --match 'v[0-9]*.[0-9]*.[0-9]*' 2>/dev/null || true)
IFS=. read -r major minor patch <<<"${last:-v0.0.0}"
major=${major#v}
log=$(git log --format=%B "${last:+$last..}HEAD")

if [ -z "$log" ]; then
  echo "$major.$minor.$patch"
elif grep -qE '^[a-z]+(\([^)]*\))?!:|^BREAKING[ -]CHANGE:' <<<"$log"; then
  echo "$((major + 1)).0.0"
elif grep -qE '^feat(\([^)]*\))?:' <<<"$log"; then
  echo "$major.$((minor + 1)).0"
else
  echo "$major.$minor.$((patch + 1))"
fi
