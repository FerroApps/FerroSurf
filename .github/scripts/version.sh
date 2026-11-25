#!/usr/bin/env bash
# Version helpers used by .github/workflows/version.yml (and by the local
# two-cycle simulation in the PR). The version lives in Cargo.toml.
#
#   version.sh current                  print the Cargo.toml version
#   version.sh bump patch|minor         bump Cargo.toml + Cargo.lock and commit
#   version.sh sync-to-development      merge origin/main into development so the
#                                       release commit becomes an ancestor of
#                                       development; version = max(main, dev)
set -euo pipefail

# Every workspace crate shares the [workspace.package] version.
PKGS="${PKGS:-ferrosurf ferrosurf-core ferrosurf-extensions ferrosurf-mcp}"

current() {
  grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "(.*)"/\1/'
}

set_version() {
  local new="$1"
  sed -i "0,/^version = \".*\"/s//version = \"$new\"/" Cargo.toml
  if [ -f Cargo.lock ]; then
    for pkg in $PKGS; do
      sed -i "/^name = \"$pkg\"$/{n;s/^version = \".*\"/version = \"$new\"/}" Cargo.lock
    done
  fi
}

# Prints the higher of two x.y.z versions.
max_version() {
  printf '%s\n%s\n' "$1" "$2" | sort -t. -k1,1n -k2,2n -k3,3n | tail -1
}

bump() {
  local kind="$1" cur major minor patch new
  cur=$(current)
  IFS=. read -r major minor patch <<< "$cur"
  case "$kind" in
    minor) minor=$((minor + 1)); patch=0 ;;
    patch) patch=$((patch + 1)) ;;
    *) echo "unknown bump kind $kind" >&2; exit 2 ;;
  esac
  new="$major.$minor.$patch"
  set_version "$new"
  git add Cargo.toml Cargo.lock 2>/dev/null || git add Cargo.toml
  git commit -q -m "chore(release): v$new"
  echo "$new"
}

sync_to_development() {
  git fetch -q origin main development
  git checkout -q -B development origin/development
  local dev_ver main_ver target
  dev_ver=$(current)
  main_ver=$(git show origin/main:Cargo.toml | grep -m1 '^version = ' | sed -E 's/version = "(.*)"/\1/')
  target=$(max_version "$dev_ver" "$main_ver")
  # A real merge: the release commit on main becomes an ancestor of
  # development, so the next development -> main PR merges cleanly.
  # -X theirs only affects conflicting hunks (the version lines); every other
  # change on development is kept. The version is then forced to max().
  git merge -q --no-ff -X theirs origin/main \
    -m "chore(release): sync v$main_ver from main into development"
  if [ "$(current)" != "$target" ]; then
    set_version "$target"
    git add Cargo.toml Cargo.lock 2>/dev/null || git add Cargo.toml
    git commit -q --amend --no-edit
  fi
  echo "$target"
}

case "${1:-}" in
  current) current ;;
  bump) bump "${2:?patch|minor}" ;;
  sync-to-development) sync_to_development ;;
  max) max_version "$2" "$3" ;;
  *) echo "usage: $0 current|bump <kind>|sync-to-development|max a b" >&2; exit 2 ;;
esac
