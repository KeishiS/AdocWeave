#!/usr/bin/env bash
set -euo pipefail

readonly root="${ADOCWEAVE_SOURCE_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$root"

readonly version="$(node --input-type=module -e "import manifest from './release-manifest.json' with { type: 'json' }; process.stdout.write(manifest.packageVersion)")"
readonly archive_name="adocweave-textlint-plugin-asciidoc-$version.tgz"
readonly output_directory="${ADOCWEAVE_TEXTLINT_PLUGIN_OUTPUT_DIRECTORY:-target/distrib}"
readonly archive="$output_directory/$archive_name"
readonly cargo_target_directory="${ADOCWEAVE_TEXTLINT_PLUGIN_CARGO_TARGET_DIRECTORY:-target/textlint-plugin-wasm-build}"
readonly npm_cache="${ADOCWEAVE_TEXTLINT_PLUGIN_NPM_CACHE:-target/npm-cache}"
readonly scratch="$(mktemp -d "${TMPDIR:-/tmp}/adocweave-textlint-plugin.XXXXXX")"
readonly stage="$scratch/package"
readonly wasm_output="${ADOCWEAVE_TEXTLINT_PLUGIN_WASM_OUTPUT_DIRECTORY:-$scratch/wasm-bindgen}"
readonly notice="$scratch/THIRD_PARTY_NOTICES.adoc"
trap 'rm -rf "$scratch"' EXIT

tools/build-textlint-wasm-node.sh "$wasm_output" "$cargo_target_directory"

mkdir -p "$output_directory"
node tools/generate-third-party-notices.mjs --textlint-plugin "$notice"
node tools/stage-textlint-plugin-package.mjs "$stage" "$wasm_output" "$notice"

pack_result="$(npm --cache "$npm_cache" pack --ignore-scripts --json --pack-destination "$scratch" "$stage")"
packed_name="$(node --input-type=module -e '
  const result = JSON.parse(process.argv[1]);
  if (!Array.isArray(result) || result.length !== 1) throw new Error("npm pack produced an unexpected result");
  process.stdout.write(result[0].filename);
' "$pack_result")"
if [[ "$packed_name" != "$archive_name" ]]; then
  echo "unexpected textlint plugin archive name: $packed_name" >&2
  exit 1
fi
node tools/verify-textlint-plugin-package.mjs "$scratch/$packed_name"
cp "$scratch/$packed_name" "$archive"

echo "textlint plugin release package built: $archive"
