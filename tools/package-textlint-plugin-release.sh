#!/usr/bin/env bash
set -euo pipefail

readonly root="$(git rev-parse --show-toplevel)"
cd "$root"

readonly version="$(node --input-type=module -e "import manifest from './release-manifest.json' with { type: 'json' }; process.stdout.write(manifest.packageVersion)")"
readonly package_source="packages/textlint-plugin-asciidoc"
readonly archive_name="adocweave-textlint-plugin-asciidoc-$version.tgz"
readonly output_directory="target/distrib"
readonly archive="$output_directory/$archive_name"
readonly scratch="$(mktemp -d "${TMPDIR:-/tmp}/adocweave-textlint-plugin.XXXXXX")"
readonly stage="$scratch/package"
readonly wasm_output="$scratch/wasm-bindgen"
trap 'rm -rf "$scratch"' EXIT

node --input-type=module -e '
  import manifest from "./packages/textlint-plugin-asciidoc/package.json" with { type: "json" };
  const version = process.argv[1];
  if (manifest.name !== "@adocweave/textlint-plugin-asciidoc" || manifest.version !== version || manifest.private !== true) {
    throw new Error("textlint plugin package identity does not match the release train");
  }
  for (const field of ["dependencies", "optionalDependencies", "bundledDependencies"]) {
    const value = manifest[field];
    if (value && (Array.isArray(value) ? value.length : Object.keys(value).length) !== 0) {
      throw new Error(`textlint plugin must not declare ${field}`);
    }
  }
  for (const name of ["preinstall", "install", "postinstall", "prepare", "prepack", "postpack"]) {
    if (manifest.scripts?.[name]) throw new Error(`textlint plugin must not define ${name}`);
  }
' "$version"

tools/build-textlint-wasm-node.sh "$wasm_output" target/textlint-plugin-wasm-build

mkdir -p "$stage/wasm" "$output_directory"
for name in adapter.mjs bridge.mjs index.d.mts index.mjs package.json position.mjs processor.mjs README.adoc; do
  cp "$package_source/$name" "$stage/$name"
done
cp LICENSE-APACHE LICENSE-MIT "$stage/"
cp "$wasm_output/adocweave_textlint_wasm.js" "$stage/wasm/adocweave_textlint_wasm.cjs"
cp "$wasm_output/adocweave_textlint_wasm_bg.wasm" "$stage/wasm/"
node tools/generate-third-party-notices.mjs --textlint-plugin "$stage/THIRD_PARTY_NOTICES.adoc"

pack_result="$(npm --cache target/npm-cache pack --ignore-scripts --json --pack-destination "$scratch" "$stage")"
packed_name="$(node --input-type=module -e '
  const result = JSON.parse(process.argv[1]);
  if (!Array.isArray(result) || result.length !== 1) throw new Error("npm pack produced an unexpected result");
  if (result[0].files.length !== 13 || result[0].unpackedSize > 16 * 1024 * 1024 || result[0].size > 8 * 1024 * 1024) {
    throw new Error("textlint plugin package exceeds its file count or size budget");
  }
  process.stdout.write(result[0].filename);
' "$pack_result")"
if [[ "$packed_name" != "$archive_name" ]]; then
  echo "unexpected textlint plugin archive name: $packed_name" >&2
  exit 1
fi
cp "$scratch/$packed_name" "$archive"

actual="$(mktemp "${TMPDIR:-/tmp}/adocweave-textlint-plugin-actual.XXXXXX")"
expected="$(mktemp "${TMPDIR:-/tmp}/adocweave-textlint-plugin-expected.XXXXXX")"
trap 'rm -rf "$scratch"; rm -f "$actual" "$expected"' EXIT
tar -tzf "$archive" | LC_ALL=C sort > "$actual"
printf '%s\n' \
  package/LICENSE-APACHE \
  package/LICENSE-MIT \
  package/README.adoc \
  package/THIRD_PARTY_NOTICES.adoc \
  package/adapter.mjs \
  package/bridge.mjs \
  package/index.d.mts \
  package/index.mjs \
  package/package.json \
  package/position.mjs \
  package/processor.mjs \
  package/wasm/adocweave_textlint_wasm.cjs \
  package/wasm/adocweave_textlint_wasm_bg.wasm | LC_ALL=C sort > "$expected"
diff -u "$expected" "$actual"

if tar -tvzf "$archive" | awk 'substr($0, 1, 1) != "-" { exit 1 }'; then
  :
else
  echo "textlint plugin archive contains a symlink or unsupported member type" >&2
  exit 1
fi
if strings "$stage/wasm/adocweave_textlint_wasm_bg.wasm" | rg -q '/(workspace|home|Users|tmp|private/tmp|builds?|runner|__w)/|[A-Za-z]:\\'; then
  echo "textlint plugin WebAssembly contains a machine-specific path" >&2
  exit 1
fi

echo "textlint plugin release package built: $archive"
