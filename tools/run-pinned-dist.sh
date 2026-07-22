#!/usr/bin/env bash
set -euo pipefail

readonly expected_version="$(
  sed -n 's/^cargo-dist-version = "\([^"]*\)"$/\1/p' dist-workspace.toml
)"
if [[ -z "$expected_version" ]]; then
  echo "missing cargo-dist-version in dist-workspace.toml" >&2
  exit 1
fi

dist_bin="${ADOCWEAVE_DIST_BIN:-}"
if [[ -z "$dist_bin" ]]; then
  dist_bin="$(nix develop --command bash -c 'printf %s "$ADOCWEAVE_DIST_BIN"')"
fi
readonly dist_bin
if [[ "$dist_bin" != /nix/store/*/bin/dist ]]; then
  echo "cargo-dist did not resolve from the locked Nix store: $dist_bin" >&2
  exit 1
fi

readonly actual_version="$($dist_bin --version)"
if [[ "$actual_version" != "cargo-dist $expected_version" ]]; then
  echo "cargo-dist version mismatch: expected $expected_version, got $actual_version" >&2
  exit 1
fi

exec "$dist_bin" "$@"
