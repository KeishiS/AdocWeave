#!/usr/bin/env bash
set -euo pipefail

readonly root="$(git rev-parse --show-toplevel)"
cd "$root"

readonly revision_file="security/rustsec-advisory-db-revision.txt"
readonly revision="$(tr -d '[:space:]' < "$revision_file")"
if [[ ! "$revision" =~ ^[0-9a-f]{40}$ ]]; then
  echo "invalid RustSec advisory database revision: $revision" >&2
  exit 1
fi

readonly database="${CARGO_TARGET_DIR:-target}/rustsec-advisory-db"
readonly metadata="$(mktemp "${TMPDIR:-/tmp}/adocweave-dependencies.XXXXXX.json")"
readonly zed_metadata="$(mktemp "${TMPDIR:-/tmp}/adocweave-zed-dependencies.XXXXXX.json")"
readonly notice="$(mktemp "${TMPDIR:-/tmp}/adocweave-third-party-notices.XXXXXX.adoc")"
trap 'rm -f "$metadata" "$zed_metadata" "$notice"' EXIT
if [[ ! -d "$database/.git" ]]; then
  rm -rf "$database"
  git init --quiet "$database"
  git -C "$database" remote add origin https://github.com/RustSec/advisory-db.git
fi
if [[ "$(git -C "$database" remote get-url origin)" != "https://github.com/RustSec/advisory-db.git" ]]; then
  echo "unexpected RustSec advisory database remote" >&2
  exit 1
fi
if [[ "${ADOCWEAVE_ADVISORY_DB_OFFLINE:-0}" != 1 ]]; then
  git -C "$database" fetch --quiet --depth=1 origin "$revision"
  git -C "$database" checkout --quiet --detach FETCH_HEAD
fi
test "$(git -C "$database" rev-parse HEAD)" = "$revision"

audit_args=(--db "$database" --no-fetch)
while IFS= read -r advisory; do
  audit_args+=(--ignore "$advisory")
done < <(node tools/verify-dependency-boundaries.mjs --audit-ignores)
cargo audit "${audit_args[@]}" --file Cargo.lock
cargo audit "${audit_args[@]}" --file editors/zed/Cargo.lock

cargo deny --config deny.toml --manifest-path Cargo.toml --all-features check licenses bans sources
cargo deny --config deny.toml --manifest-path editors/zed/Cargo.toml --all-features check licenses bans sources

node tools/verify-dependency-boundaries.mjs
cargo metadata --locked --format-version=1 > "$metadata"
cargo metadata --manifest-path editors/zed/Cargo.toml --locked --format-version=1 > "$zed_metadata"
node tools/verify-duplicate-dependencies.mjs "$metadata" "$zed_metadata"
node tools/generate-third-party-notices.mjs "$notice"
