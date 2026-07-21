#!/usr/bin/env bash
set -euo pipefail

bash tools/package-browser-release.sh
commit="${GITHUB_SHA:-$(git rev-parse HEAD)}"
node tools/release-metadata.mjs generate target/distrib "$commit"
node tools/release-metadata.mjs verify target/distrib "$commit"
