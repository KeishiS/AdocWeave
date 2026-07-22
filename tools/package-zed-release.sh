#!/usr/bin/env bash
set -euo pipefail

version="$(node --input-type=module -e "import manifest from './release-manifest.json' with { type: 'json' }; process.stdout.write(manifest.packageVersion)")"
package="adocweave-zed-$version"
stage="target/zed-release/$package"
archive="target/distrib/$package.tar.xz"

rm -rf "target/zed-release"
mkdir -p "$stage/src" "$stage/languages/asciidoc" "target/distrib"
cp editors/zed/Cargo.toml editors/zed/Cargo.lock editors/zed/extension.toml "$stage/"
cp editors/zed/src/*.rs "$stage/src/"
cp editors/zed/languages/asciidoc/config.toml "$stage/languages/asciidoc/"
cp editors/zed/README.adoc "$stage/"
cp LICENSE-MIT LICENSE-APACHE THIRD_PARTY_NOTICES.adoc "$stage/"

tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner \
  -cJf "$archive" -C target/zed-release "$package"
test -s "$archive"
echo "Zed release artifact: $archive"
