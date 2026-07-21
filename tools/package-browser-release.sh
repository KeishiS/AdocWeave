#!/usr/bin/env bash
set -euo pipefail

version="$(node --input-type=module -e "import manifest from './release-manifest.json' with { type: 'json' }; process.stdout.write(manifest.packageVersion)")"
package="adocweave-browser-$version"
stage="target/distrib/$package"
archive="target/distrib/$package.tar.xz"

rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
cargo build -p adocweave-wasm --release --target wasm32-unknown-unknown

if command -v wasm-bindgen >/dev/null 2>&1; then
  wasm_bindgen="$(command -v wasm-bindgen)"
else
  tool_root="target/release-tools/wasm-bindgen-cli-0.2.121"
  cargo install --locked wasm-bindgen-cli --version 0.2.121 --root "$tool_root"
  wasm_bindgen="$tool_root/bin/wasm-bindgen"
fi
"$wasm_bindgen" \
  --target web \
  --out-dir target/adocweave-wasm \
  target/wasm32-unknown-unknown/release/adocweave_wasm.wasm

rm -rf "$stage"
mkdir -p "$stage/wasm" "$stage/worker"
cp target/adocweave-wasm/adocweave_wasm.js "$stage/wasm/"
cp target/adocweave-wasm/adocweave_wasm_bg.wasm "$stage/wasm/"
if [[ -f target/adocweave-wasm/adocweave_wasm.d.ts ]]; then
  cp target/adocweave-wasm/adocweave_wasm.d.ts "$stage/wasm/"
fi
cp web-worker/client.mjs web-worker/contracts.mjs web-worker/controller.mjs web-worker/worker.mjs "$stage/worker/"
cp web-worker/package.json web-worker/README.adoc LICENSE-MIT LICENSE-APACHE THIRD_PARTY_NOTICES.adoc "$stage/"

tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner \
  -cJf "$archive" -C target/distrib "$package"
rm -rf "$stage"
echo "browser release artifact: $archive"
