#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: tools/build-textlint-wasm-node.sh OUTPUT_DIRECTORY TARGET_DIRECTORY" >&2
  exit 2
fi

readonly root="${ADOCWEAVE_SOURCE_ROOT:-$(git rev-parse --show-toplevel)}"
readonly output_directory="$1"
readonly target_directory="$2"

cd "$root"
readonly maximum_memory_bytes="$(node --input-type=module -e '
  import { loadTextlintPluginPackageContract } from "./tools/textlint-plugin-package-contract.mjs";
  process.stdout.write(String(loadTextlintPluginPackageContract().wasm.maximumMemoryBytes));
')"
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$root=. --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=cargo-home -C link-arg=--max-memory=$maximum_memory_bytes"
cargo build \
  -p adocweave-textlint-wasm \
  --release \
  --target wasm32-unknown-unknown \
  --target-dir "$target_directory"
wasm-bindgen \
  --target nodejs \
  --out-dir "$output_directory" \
  "$target_directory/wasm32-unknown-unknown/release/adocweave_textlint_wasm.wasm"
node tools/verify-textlint-wasm-memory.mjs \
  "$output_directory/adocweave_textlint_wasm_bg.wasm" \
  "$maximum_memory_bytes"
