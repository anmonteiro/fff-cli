#!/bin/bash
set -euo pipefail

VERSION="${1:?Usage: ./scripts/release.sh <version> (e.g. 0.1.0)}"
VERSION="${VERSION#v}"
TAG="v${VERSION}"

git pull --ff-only

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "Error: Working tree is not clean. Commit or stash changes first."
  exit 1
fi

if git rev-parse "$TAG" >/dev/null 2>&1; then
  echo "Error: Tag $TAG already exists"
  exit 1
fi

echo "Releasing $VERSION..."

if ! command -v cargo-set-version >/dev/null 2>&1 && ! cargo set-version --help >/dev/null 2>&1; then
  echo "cargo-edit not found, installing..."
  cargo install cargo-edit
fi

echo "→ Updating Cargo.toml version to $VERSION"
cargo set-version "$VERSION"

echo "→ Updating lockfile"
cargo check --locked || cargo check

git add Cargo.toml Cargo.lock
git commit -m "chore: release $VERSION"

echo "→ Creating tag $TAG"
git tag -a "$TAG" -m "Release $VERSION"

echo "→ Pushing to origin"
git push origin
git push origin "$TAG"

echo ""
echo "Release $VERSION created and pushed."
echo "CI will build artifacts and publish the GitHub release."
