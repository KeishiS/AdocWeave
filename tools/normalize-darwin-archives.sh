#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "使用方法: tools/normalize-darwin-archives.sh ARTIFACT_DIRECTORY TARGET" >&2
  exit 2
fi

artifact_directory="$(cd "$1" && pwd)"
target="$2"
case "$target" in
  *-apple-darwin) ;;
  *)
    echo "Darwin以外のtargetは正規化できません: $target" >&2
    exit 2
    ;;
esac

executable_count="$(
  jq --arg target "$target" \
    '[.assets[] | select(.target == $target and .executable != null)] | length' \
    release/distribution-plan.json
)"
if [ "$executable_count" -eq 0 ]; then
  echo "配布計画にDarwin実行fileがありません: $target" >&2
  exit 1
fi

scratch="$(mktemp -d "${TMPDIR:-/tmp}/adocweave-darwin-archives.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

while IFS=$'\t' read -r archive_name executable; do
  archive="$artifact_directory/$archive_name"
  if [ ! -f "$archive" ]; then
    echo "Darwin archiveがありません: $archive" >&2
    exit 1
  fi

  destination="$scratch/$executable"
  mkdir "$destination"
  unzip -q "$archive" -d "$destination"
  binary="$destination/$executable"

  while IFS= read -r dependency; do
    case "$dependency" in
      /nix/store/*-libiconv-*/lib/libiconv.*.dylib)
        install_name_tool -change "$dependency" /usr/lib/libiconv.2.dylib "$binary"
        ;;
    esac
  done < <(otool -L "$binary" | tail -n +2 | awk '{print $1}')

  if otool -L "$binary" | tail -n +2 | awk '{print $1}' | grep -q '^/nix/store/'; then
    echo "Darwin実行fileにNix storeの動的依存が残っています: $executable" >&2
    exit 1
  fi

  normalized="$scratch/normalized-$archive_name"
  (
    cd "$destination"
    zip -q -X "$normalized" ./*
  )
  mv "$normalized" "$archive"
done < <(
  jq -r --arg target "$target" \
    '.assets[] | select(.target == $target and .executable != null) | [.name, .executable] | @tsv' \
    release/distribution-plan.json
)
