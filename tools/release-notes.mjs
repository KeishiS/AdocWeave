import { readFileSync } from "node:fs";
import process from "node:process";

import {
  PUBLIC_PROTOCOL_SCHEMA_VERSION,
  RELEASE_NOTES_VERSION,
} from "./release-policy.mjs";
import { loadTextlintPluginPackageContract } from "./textlint-plugin-package-contract.mjs";

const ROOT = new URL("../", import.meta.url);
const manifest = JSON.parse(readFileSync(new URL("release-manifest.json", ROOT), "utf8"));
const plan = JSON.parse(readFileSync(new URL("release/distribution-plan.json", ROOT), "utf8"));
const protocol = JSON.parse(readFileSync(new URL("protocol/public-api.json", ROOT), "utf8"));
const textlintContract = loadTextlintPluginPackageContract();
export { RELEASE_NOTES_VERSION };
export const RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION = PUBLIC_PROTOCOL_SCHEMA_VERSION;

const releaseVersionParts = RELEASE_NOTES_VERSION.split(".").map(Number);
if (releaseVersionParts.length !== 3 || releaseVersionParts.some((part) => !Number.isInteger(part))) {
  throw new Error(`Release NotesのversionがSemVerではありません：${RELEASE_NOTES_VERSION}`);
}
export const PREVIOUS_RELEASE_VERSION = "0.30.0";

// The release manifest schema version the previous stable release shipped.
//
// An earlier release shipped notes announcing a manifest change that had not
// been made: the same body stated one schema version in one line and a
// different transition in another. Deriving the sentence from this value and
// the manifest keeps a claim from outliving the change it describes, and keeps
// a release that changes nothing from announcing a transition.
export const PREVIOUS_RELEASE_MANIFEST_SCHEMA_VERSION = 4;

const manifestSchemaNote =
  manifest.schemaVersion === PREVIOUS_RELEASE_MANIFEST_SCHEMA_VERSION
    ? `release manifestのschema versionは${manifest.schemaVersion}のままで、項目を追加も削除もしていません。`
    : `release manifestのschema versionを${PREVIOUS_RELEASE_MANIFEST_SCHEMA_VERSION}から${manifest.schemaVersion}へ更新しました。`;

export const REQUIRED_RELEASE_NOTE_HEADINGS = [
  "## 対応環境",
  "## 公開仕様と破壊的変更",
  `## v${RELEASE_NOTES_VERSION}への移行`,
  "## 既知の制約",
  "## 配布物の検証",
  "## 更新とロールバック",
];

const highlights = [
  "textlint用パッケージの識別情報、対応環境、収録ファイル、WebAssemblyの制約、容量上限および受入検査の組合せを、一つの機械可読な契約へ集約しました。",
  "公開用``package.json``と収録ディレクトリを契約とrelease versionから生成し、開発用``package.json``を公開パッケージの情報源から分離しました。",
  "生成処理と実装を共有しないarchive検証器を追加しました。tarballのfile種別とpath、許可file集合、実際の展開量、WebAssemblyの公開関数、memory上限および機械固有pathを検査します。",
  "固定したconsumer依存とinstall後のfile treeを専用fixtureで検査し、別々の構築環境から同じbyte列のtarballが得られることを確認します。",
  "公開前のcandidateと公開後のGitHub Release assetを``npx``で実行する検査を追加し、単体検査、契約検査、consumer検査、再現性検査および文書校正のtaskを分離しました。",
];

/// Public contracts this release states are unchanged since the previous stable tag.
///
/// The sentence in the notes is built from this list rather than written beside
/// it. v0.27.2 announced that the configuration schema had not changed while the
/// same release changed it: the claim was prose, so nothing compared it with the
/// diff. `tools/release-claims.mjs` reads this list and checks every entry that
/// has a single machine-readable source of truth.
export const UNCHANGED_CONTRACTS = [
  "WASM protocol",
  "CLI引数",
  "Language Server protocol",
];

/// The file that decides whether a named contract changed.
///
/// A contract without an entry here is stated but not checked: CLI arguments and
/// the Language Server protocol are spread across the sources that implement
/// them, and a file diff would report every unrelated edit. The tool names the
/// unchecked contracts in its output so the reader knows how far the check goes.
export const CONTRACT_SOURCES = {
  "WASM protocol": "protocol/public-api.json",
  設定schema: "config/adocweave.schema.json",
};

/// Fields that carry the release version rather than the contract's shape.
export const CONTRACT_VERSION_FIELDS = ["packageVersion"];

const contractNotes = [
  `統一package version：${RELEASE_NOTES_VERSION}`,
  `release manifest schema version：${manifest.schemaVersion}、distribution plan schema version：${plan.schemaVersion}、配布manifest schema version：2。`,
  `WASM protocol schema version：${RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION}、Worker protocol version：${protocol.workerProtocolVersion}。v0.23.0から変更していません。`,
  manifestSchemaNote,
  "破壊的変更：ありません。",
  `WASM protocolのschema version、Worker protocol versionおよびfield構造は変えず、\`\`packageVersion\`\`だけを${RELEASE_NOTES_VERSION}へ更新しました。Node.js向けの\`\`parseText\`\`は専用の\`\`adocweave-textlint-wasm\`\`だけに含み、Browser packageには含めません。`,
  "パーサAPI、HTMLコンパイラ、Language ServerおよびBrowser APIの動作は変更していません。",
  "textlint Processorの公開API、TxtASTへの変換結果および自動修正を行わない保証は変更していません。",
  `${UNCHANGED_CONTRACTS.join("、")}は変更していません。`,
  "GitHub Release以外のregistryへpackageまたは拡張を公開しません。",
];

const migrationNotes = [
  "CLI、パーサ、HTMLコンパイラ、Language Server、Browser packageおよびtextlint Processorの利用方法に変更はなく、移行作業は不要です。",
  `textlint用Processorを更新する場合は、GitHub Releaseにある${RELEASE_NOTES_VERSION}のtarballへ依存を更新します。package名は\`\`${textlintContract.identity.packageName}\`\`、textlintのplugin名は\`\`${textlintContract.identity.pluginName}\`\`です。`,
  "プロジェクトへ依存を追加せず試す場合は、利用手順に記載した``npx --package``の実行方法を使用できます。",
  "``cargo make docs-prose-lint``は再設計後のProcessorを使用します。AdocWeave固有の日本語規則、用語集および対象文書一覧は公開パッケージへ含めません。",
  `release manifestを機械的に読んでいる場合も追随は不要です。\`\`schemaVersion\`\`は${manifest.schemaVersion}のままです。`,
  `CLI、LSP、browser、Zed、VS Codeおよびtextlint向け配布物のversionを${RELEASE_NOTES_VERSION}へそろえてください。バージョンの異なる配布物を混ぜて使えないため、更新する場合はすべてを入れ替えます。`,
];

const knownConstraints = [
  `対応Rust toolchain：${manifest.rustVersion}。このreleaseのflake.lockで固定しています。`,
  "native binaryは配布計画に定義したLinux、macOSおよびWindows環境へ提供します。macOSとWindowsのbinaryはOSのsystem libraryへ動的linkします。",
  "macOS binaryへDeveloper ID署名とnotarizationを行わず、Windows binaryへAuthenticode署名を行いません。OSの警告が表示された場合はchecksumとattestationを確認してください。",
  "Zed拡張はdevelopment extension、VS Code拡張はVSIXとして手動導入します。拡張registryへは公開しません。",
  "ZedがLanguage Serverの導入中に異常終了すると、安全のため導入ロックを自動削除しません。すべてのZedプロセスを終了してから、エラーに表示されたロックのpathを削除して再試行してください。",
  "公式Playgroundはこのreleaseに含みません。`adocweave preview`は利用者の端末で実行するローカル機能です。",
  "packageはcrates.io、npmまたはOS package registryへ公開しません。Nix packageはこのrepositoryのflakeから直接buildします。",
  `textlint用Processorの対応範囲はNode.js \`\`${textlintContract.compatibility.nodeEngine}\`\`、textlint \`\`${textlintContract.compatibility.textlintVersion}\`\`です。includeは展開せず、入力した一つの物理ファイルだけを検査します。`,
  "AdocWeaveはBibTeXの保存・解析やCSL相当の書誌の組版を行いません。citation keyの解決と引用表示の組み立ては利用側アプリの責務です。",
  "解決結果を渡さない引用の表示は`unresolved_references`の設定に従い、`hidden`では出力しません。ただし文書内の`[bibliography]`項目を指すkeyは、設定にかかわらずその項目へのlinkとして出力します。",
  "引用の解決結果は文書全体の並べ替えを行いません。番号付きの引用styleで通し番号を振る場合は、利用側アプリが出現順を見て文字列を決めてください。出現順は公開projectionの`citations`から取得できます。",
  "単一ファイルのworkspaceでは、同じディレクトリの別のAsciiDocファイルとinclude先を自動では読み込みません。複数ファイルの解析にはディレクトリのworkspace folderが必要です。",
  "Language Serverはworkspaceの走査を初期化の応答後に、要求へ応答するthreadの外で行います。走査中もほかの要求へ応答しますが、走査の完了前は、開いた文書の解析にworkspace内のほかの文書が反映されません。走査の完了後に再解析します。",
];

function markdownList(items) {
  return items.map((item) => `- ${item}`).join("\n");
}

const MINIMUM_OS_DESCRIPTIONS = {
  "darwin:14.0": "macOS 14.0以降",
  "win32:10.0.17763": "Windows 10 version 1809（build 10.0.17763）以降",
};

function minimumOsDescription(target) {
  if (target.minimumOsVersion === null) return "";
  const description = MINIMUM_OS_DESCRIPTIONS[`${target.os}:${target.minimumOsVersion}`];
  if (!description) {
    throw new Error(`最小対応OS版の説明がありません：${target.os} ${target.minimumOsVersion}`);
  }
  return `、${description}`;
}

export function buildReleaseNotes(tag) {
  if (tag !== `v${RELEASE_NOTES_VERSION}`) {
    throw new Error(`Release Notesはv${RELEASE_NOTES_VERSION}専用です`);
  }
  const osNames = { darwin: "macOS", linux: "Linux", win32: "Windows" };
  const targets = plan.targets
    .map(
      (target) =>
        `- ${osNames[target.os]} ${target.architecture}（\`${target.triple}\`${minimumOsDescription(target)}）`,
    )
    .join("\n");
  const notes = `## 主な変更\n\n${markdownList(highlights)}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[0]}\n\n${targets}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[1]}\n\n${markdownList(contractNotes)}\n\n` +
    "consumerは記載されたpackage versionを厳密に一致させてください。異なるversionのCLI、LSP、browser、Zed、VS Codeまたはtextlint向け配布物を混在させないでください。\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[2]}\n\n${markdownList(migrationNotes)}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[3]}\n\n${markdownList(knownConstraints)}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[4]}\n\n` +
    "すべてのrelease assetをdownloadし、`sha256sum --check sha256.sum`を実行してください。その後、必要なassetを`gh attestation verify <asset> --repo KeishiS/adocweave`で検証してください。\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[5]}\n\n` +
    `native archiveはversion別directoryへ展開し、\`--version --json\`が\`${RELEASE_NOTES_VERSION}\`を返すことを確認してから選択先を切り替えてください。\n\n` +
    "VS Codeでは検証済みVSIXを手動導入し、拡張とLanguage Serverのversion一致を確認してください。受入確認が成功するまで以前のVSIXとnative directoryを保持します。\n\n" +
    "Zedでは新versionのmanaged Language Server取得とeditor機能を確認するまで旧versionのZed directoryを保持します。rollback時は旧directoryをdev extensionとして選び直し、Zedを再起動してください。\n\n" +
    "textlint用Processorは新しいReleaseのtarball URLへ変更してlockfileを更新します。rollback時は以前の検証済みURLへ戻し、lockfileから依存を再導入してください。\n\n" +
    "rollback時は以前のversion別directoryまたはVSIXへ戻します。詳細は`docs/user-guide/release-installation.adoc`を参照してください。\n";
  return notes;
}

export function validateReleaseNotes(body) {
  for (const heading of REQUIRED_RELEASE_NOTE_HEADINGS) {
    if (!body.includes(heading)) throw new Error(`Release Notesに必須見出しがありません：${heading}`);
  }
}

if (process.argv[1] && import.meta.url === new URL(process.argv[1], "file:").href) {
  const tag = process.argv[2];
  // cargo-distの自動生成本文は英語であるため読み捨て、単一の日本語本文を生成します。
  for await (const _chunk of process.stdin) {
    // 標準入力を最後まで読み、呼出側のpipeを正常に終了させます。
  }
  const output = buildReleaseNotes(tag);
  validateReleaseNotes(output);
  process.stdout.write(output);
}
