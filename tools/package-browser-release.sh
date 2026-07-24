#!/usr/bin/env bash
set -euo pipefail

version="$(node --input-type=module -e "import manifest from './release-manifest.json' with { type: 'json' }; process.stdout.write(manifest.packageVersion)")"
package="adocweave-browser-$version"
stage="target/distrib/$package"
archive="target/distrib/$package.tar.xz"

export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$(pwd)=. --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=cargo-home"

cargo build -p adocweave-wasm --profile browser --target wasm32-unknown-unknown

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
  target/wasm32-unknown-unknown/browser/adocweave_wasm.wasm

rm -rf "$stage"
mkdir -p "$stage/wasm" "$stage/worker" "$stage/example"
cp target/adocweave-wasm/adocweave_wasm.js "$stage/wasm/"
cp target/adocweave-wasm/adocweave_wasm_bg.wasm "$stage/wasm/"
if [[ -f target/adocweave-wasm/adocweave_wasm.d.ts ]]; then
  cp target/adocweave-wasm/adocweave_wasm.d.ts "$stage/wasm/"
fi
cp web-worker/client.mjs web-worker/contracts.mjs web-worker/controller.mjs web-worker/index.mjs \
  web-worker/index.d.mts web-worker/protocol.generated.mjs web-worker/protocol.generated.d.mts \
  web-worker/worker.mjs "$stage/worker/"
cp web-worker/example/index.html web-worker/example/app.mjs "$stage/example/"
cp web-worker/package.json web-worker/README.adoc LICENSE-MIT LICENSE-APACHE "$stage/"
node tools/generate-third-party-notices.mjs "$stage/THIRD_PARTY_NOTICES.adoc"

tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner \
  -cJf "$archive" -C target/distrib "$package"
if tar -xOf "$archive" | LC_ALL=C grep -a -E '(/workspace/|/home/|/tmp/)' >/dev/null; then
  echo "browser release artifact contains a machine-local absolute path" >&2
  exit 1
fi
rm -rf "$stage"
echo "browser release artifact: $archive"
