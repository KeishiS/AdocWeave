import { readFileSync } from "node:fs";

const packageJson = JSON.parse(readFileSync("package.json", "utf8"));
const release = JSON.parse(readFileSync("../../release-manifest.json", "utf8"));
const language = JSON.parse(readFileSync("language-configuration.json", "utf8"));
const grammar = JSON.parse(readFileSync("syntaxes/asciidoc.tmLanguage.json", "utf8"));

if (packageJson.version !== release.packageVersion) {
  throw new Error("VS Code拡張のversionがrelease manifestと一致しません");
}
if (packageJson.private !== true || packageJson.scripts?.publish || packageJson.publishConfig) {
  throw new Error("VS Code拡張はregistry公開を無効にする必要があります");
}
if (
  packageJson.homepage !== "https://github.com/KeishiS/adocweave" ||
  packageJson.repository?.url !== "https://github.com/KeishiS/adocweave.git"
) {
  throw new Error("VS Code拡張のrepository URLがcanonical名と一致しません");
}
if (
  packageJson.main !== "./dist/extension.cjs" ||
  packageJson.contributes?.languages?.[0]?.id !== "asciidoc"
) {
  throw new Error("VS Code拡張のentry pointまたはlanguage登録が不正です");
}
if (!Array.isArray(language.brackets) || grammar.scopeName !== "text.asciidoc") {
  throw new Error("AsciiDoc language設定またはTextMate grammarが不正です");
}

for (const repository of Object.values(grammar.repository ?? {})) {
  for (const pattern of repository.patterns ?? []) {
    for (const field of ["begin", "end", "match"]) {
      if (pattern[field]) new RegExp(pattern[field].replaceAll("\\1", "----"), "u");
    }
  }
}

process.stdout.write("VS Code拡張manifestとgrammarを検証しました。\n");
