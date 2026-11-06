#!/usr/bin/env bash
# Runs next-version.sh against throwaway repos and checks the version it picks. Used by CI; run it
# locally with scripts/next-version.test.sh.
set -euo pipefail

NEXT=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/next-version.sh
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

fail() { echo "FAIL: $*" >&2; exit 1; }
# expect <version> <commit message>...: a fresh repo tagged v1.2.3, then one commit per message.
expect() {
  local want=$1 repo got
  shift
  repo=$(mktemp -d -p "$T")
  git -C "$repo" init -q
  git -C "$repo" commit -q --allow-empty -m "feat: start"
  git -C "$repo" tag v1.2.3
  for msg in "$@"; do git -C "$repo" commit -q --allow-empty -m "$msg"; done
  got=$(cd "$repo" && "$NEXT")
  [ "$got" = "$want" ] || fail "$* -> $got, want $want"
}

expect 1.2.4 "fix: a" "docs: b" "Merge pull request #1 from x/y"
expect 1.2.4 "chore: release notes mention feat: in passing"
expect 1.3.0 "fix: a" "feat(judge): b"
expect 2.0.0 "feat: a" "fix(status)!: b"
expect 2.0.0 "refactor: a

BREAKING CHANGE: the env file moved"
# A re-run on an already released commit keeps its version.
expect 1.2.3

# The first release counts the whole history from 0.0.0, and ignores tags that aren't versions.
repo=$T/fresh
git init -q "$repo"
git -C "$repo" commit -q --allow-empty -m "feat: start"
git -C "$repo" tag latest
[ "$(cd "$repo" && "$NEXT")" = 0.1.0 ] || fail "first release"

echo "next-version: ok"
