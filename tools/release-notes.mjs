import { readFileSync } from "node:fs";
import process from "node:process";

import {
  PUBLIC_PROTOCOL_SCHEMA_VERSION,
  RELEASE_NOTES_VERSION,
} from "./release-policy.mjs";

const ROOT = new URL("../", import.meta.url);
const manifest = JSON.parse(readFileSync(new URL("release-manifest.json", ROOT), "utf8"));
const plan = JSON.parse(readFileSync(new URL("release/distribution-plan.json", ROOT), "utf8"));
const protocol = JSON.parse(readFileSync(new URL("protocol/public-api.json", ROOT), "utf8"));
export { RELEASE_NOTES_VERSION };
export const RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION = PUBLIC_PROTOCOL_SCHEMA_VERSION;

// The release manifest schema version this release train started from.
//
// An earlier release shipped notes announcing a manifest change that had not
// been made: the same body stated one schema version in one line and a
// different transition in another. Deriving both numbers from the manifest
// keeps a sentence from outliving the change it describes.
export const PREVIOUS_RELEASE_MANIFEST_SCHEMA_VERSION = 3;

export const REQUIRED_RELEASE_NOTE_HEADINGS = [
  "## 対応環境",
  "## 公開契約と破壊的変更",
  `## v${RELEASE_NOTES_VERSION}への移行`,
  "## 既知の制約",
  "## 配布物の検証",
  "## 更新とロールバック",
];

const highlights = [
  "``javascript``と``vbscript``のURLを、利用側アプリが許可schemeへ加えても出力しないようにしました。これらはbrowserがcodeとして実行するURLです。",
  "``ifeval``などのdirectiveが、``\\{name}``を文書本文と同じくエスケープとして読むようにしました。これまでは属性として展開しており、同じ記法が本文と条件式で違う意味を持っていました。",
  "lint規則の内部の欠陥で、CLIやLanguage Serverのプロセスが終わらないようにしました。修正候補だけを落として診断は返します。",
  "ファイル検査の件数上限を、すべてのプラットフォームで適用するようにしました。これまでmacOSとWindowsでは上限が働かず、同じ文書がLinuxでだけ拒否されていました。",
  "Language Serverの初期化が、workspace全体の走査を待たずに応答するようにしました。8,000文書のworkspaceで414ミリ秒から1ミリ秒になります。",
  "日本語文書の一般名詞を日本語表記へそろえました。表記の基準は「用語の表記」に定めています。",
];

const contractNotes = [
  `統一package version：${RELEASE_NOTES_VERSION}`,
  `release manifest schema version：${manifest.schemaVersion}、distribution plan schema version：${plan.schemaVersion}、配布manifest schema version：2。`,
  `WASM protocol schema version：${RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION}、Worker protocol version：${protocol.workerProtocolVersion}。v0.23.0から変更していません。`,
  "挙動の変更：``javascript``と``vbscript``のURLを、``ActiveUrlPolicy.allowed_schemes``へ加えても出力しません。``data``はこの扱いに含めず、``allow_data_uris``に従います。",
  "挙動の変更：``include``と``ifeval``のdirectiveが``\\{name}``をエスケープとして読みます。以前は属性として展開していました。",
  "挙動の変更：``resources.max-files``の上限が、macOSとWindowsでも読み込み経路に適用されます。以前はLinuxだけでした。",
  "``adocweave_config::ProjectScopeId``を追加しました。CLIとLanguage Serverが同じ型でプロジェクト範囲を識別します。",
  `release manifestへ\`\`nodeVersion\`\`を追加し、schema versionを${PREVIOUS_RELEASE_MANIFEST_SCHEMA_VERSION}から${manifest.schemaVersion}へ更新しました。Nixを使えないrunnerも、この値でNode.jsを選びます。`,
  "公開Rust APIのそのほかの型、WASM protocol、CLI引数、Language Server protocolおよび設定schemaはv0.24.0から変更していません。",
  "GitHub Release以外のregistryへpackageまたは拡張を公開しません。",
];

const migrationNotes = [
  "設定の移行は不要です。診断code、終了コードおよび公開projectionは変わりません。",
  "``javascript``または``vbscript``を許可schemeへ加えていた場合、そのURLは出力されなくなります。これらのURLをHTMLへ出したい場合、AdocWeaveは対応しません。",
  "``\\{name}``をdirectiveの中で属性として展開させていた場合、その記法は文字列``{name}``になります。属性として展開するには``\\``を外してください。",
  "macOSまたはWindowsで``resources.max-files``を超える数のファイルを読んでいた場合、``local-target-limit-exceeded``になります。上限を上げるか、対象を絞ってください。Linuxでは以前から同じ動作です。",
  `release manifestを機械的に読んでいる場合は、\`\`schemaVersion\`\`が${manifest.schemaVersion}になり\`\`nodeVersion\`\`が加わります。`,
  `CLI、LSP、browser、ZedおよびVS Code向け配布物のversionを${RELEASE_NOTES_VERSION}へそろえてください。`,
];

const knownConstraints = [
  `対応Rust toolchain：${manifest.rustVersion}。このreleaseのflake.lockで固定しています。`,
  "native binaryは配布計画に定義したLinux、macOSおよびWindows環境へ提供します。macOSとWindowsのbinaryはOSのsystem libraryへ動的linkします。",
  "macOS binaryへDeveloper ID署名とnotarizationを行わず、Windows binaryへAuthenticode署名を行いません。OSの警告が表示された場合はchecksumとattestationを確認してください。",
  "Zed拡張はdevelopment extension、VS Code拡張はVSIXとして手動導入します。拡張registryへは公開しません。",
  "公式Playgroundはこのreleaseに含みません。`adocweave preview`は利用者の端末で実行するローカル機能です。",
  "packageはcrates.io、npmまたはOS package registryへ公開しません。Nix packageはこのrepositoryのflakeから直接buildします。",
  "AdocWeaveはBibTeXの保存・解析やCSL相当の書誌の組版を行いません。citation keyの解決と引用表示の組み立ては利用側アプリの責務です。",
  "解決結果を渡さない引用の表示は`unresolved_references`の設定に従い、`hidden`では出力しません。ただし文書内の`[bibliography]`項目を指すkeyは、設定にかかわらずその項目へのlinkとして出力します。",
  "引用の解決結果は文書全体の並べ替えを行いません。番号付きの引用styleで通し番号を振る場合は、利用側アプリが出現順を見て文字列を決めてください。出現順は公開projectionの`citations`から取得できます。",
  "単一ファイルのworkspaceでは、同じディレクトリの別のAsciiDocファイルとinclude先を自動では読み込みません。複数ファイルの解析にはディレクトリのworkspace folderが必要です。",
  "Language Serverはworkspaceの走査を初期化の応答後に行います。走査の完了前は、開いた文書の解析にworkspace内のほかの文書が反映されません。走査の完了後に再解析します。",
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
    "consumerは記載されたpackage versionを厳密に一致させてください。異なるversionのCLI、LSP、browser、ZedまたはVS Code向け配布物を混在させないでください。\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[2]}\n\n${markdownList(migrationNotes)}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[3]}\n\n${markdownList(knownConstraints)}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[4]}\n\n` +
    "すべてのrelease assetをdownloadし、`sha256sum --check sha256.sum`を実行してください。その後、必要なassetを`gh attestation verify <asset> --repo KeishiS/adocweave`で検証してください。\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[5]}\n\n` +
    `native archiveはversion別directoryへ展開し、\`--version --json\`が\`${RELEASE_NOTES_VERSION}\`を返すことを確認してから選択先を切り替えてください。\n\n` +
    "VS Codeでは検証済みVSIXを手動導入し、拡張とLanguage Serverのversion一致を確認してください。受入確認が成功するまで以前のVSIXとnative directoryを保持します。\n\n" +
    "Zedでは新versionのmanaged Language Server取得とeditor機能を確認するまで旧versionのZed directoryを保持します。rollback時は旧directoryをdev extensionとして選び直し、Zedを再起動してください。\n\n" +
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
